use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::IntoResponse,
};
use source_downloader_core::{
    application::CorePluginContext, component_manager::ComponentManager,
    config::YamlConfigOperator, instance_manager::InstanceManager, plugin::PluginManager,
    processor_manager::ProcessorManager, processor_run_manager::ProcessorRunManager,
};
use std::collections::VecDeque;
use storage_sqlite::SeaProcessingStorage;
use tower::ServiceExt;

struct Fixture {
    router: Router,
    core: Arc<CoreApplication>,
    _directory: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let config =
        Arc::new(YamlConfigOperator::new_path(&directory.path().join("config.yaml")));
    config.init().unwrap();
    let instances = Arc::new(InstanceManager::new(config.clone()));
    let components = Arc::new(ComponentManager::with_create_context(
        config.clone(),
        instances.clone(),
    ));
    let runs = Arc::new(ProcessorRunManager::default());
    let storage =
        Arc::new(SeaProcessingStorage::new_with_wal("sqlite::memory:").await.unwrap());
    let core = Arc::new(CoreApplication {
        config_operator: config,
        component_manager: components.clone(),
        instance_manager: instances,
        processor_manager: Arc::new(ProcessorManager::new(
            components,
            storage.clone(),
            runs.clone(),
        )),
        run_manager: runs,
        plugin_manager: PluginManager::new(Arc::new(CorePluginContext {
            data_location: directory.path().into(),
        })),
        data_location: directory.path().into(),
        plugin_location: None,
        webhook_adapter: None,
    });
    core.plugin_manager.register_plugin(Box::new(common::PLUGIN));
    core.start().unwrap();
    let router =
        register_routers(Arc::new(ApplicationContext { core: core.clone(), storage }));
    Fixture { router, core, _directory: directory }
}

async fn request(
    router: &Router,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, String) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024).await.unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

fn last_session(stream: &str) -> Value {
    stream
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .rfind(|event| event["type"] == "session")
        .unwrap_or_else(|| panic!("No session in {stream}"))["session"]
        .clone()
}

fn local_plan() -> Value {
    json!({"summary":"整理本地动画到 /tmp/output，手动触发，默认启用。", "instances":[],
        "components":[{"rootType":"source","typeName":"system-file","name":"local-input","props":{"path":"/tmp/input"}}],
        "processors":[{"name":"local-anime","source":"system-file:local-input","downloader":"system-file:local-input","item-file-resolver":"system-file","file-mover":"general","save-path":"/tmp/output","triggers":[],"options":{"filename-pattern":"{file.name}"}}]})
}

fn tool_response(protocol: &str, name: &str, arguments: Value) -> String {
    if protocol == "responses" {
        format!(
            "data: {}\n\n",
            json!({"type":"response.completed","response":{"output":[{"type":"reasoning","id":"r1","encrypted_content":"test-reasoning"},{"type":"function_call","call_id":"call1","name":name,"arguments":arguments.to_string()}]}})
        )
    } else {
        format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call1","type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}]})
        )
    }
}

type MockState = Arc<Mutex<(VecDeque<String>, Vec<Value>)>>;
async fn mock_model(
    State(state): State<MockState>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    let mut state = state.lock().await;
    state.1.push(body);
    let response = state.0.pop_front().unwrap_or_default();
    ([("content-type", "text/event-stream")], response)
}

async fn reviewed_flow(protocol: &str) {
    let fixture = fixture().await;
    let mut workflow_plan = local_plan();
    workflow_plan["components"].as_array_mut().unwrap().push(json!({
        "rootType":"source", "typeName":"fanbox", "name":"private-account",
        "props":{"cookie":"$secret:credential"}
    }));
    let mock: MockState = Arc::new(Mutex::new((
        VecDeque::from([
            tool_response(
                protocol,
                "ask_user",
                json!({"questions":[{"id":"credential","title":"填写服务凭据","options":[],"secret":true}]}),
            ),
            tool_response(
                protocol,
                "propose_plan",
                json!({"configuration":workflow_plan.to_string()}),
            ),
        ]),
        vec![],
    )));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/responses", post(mock_model))
        .route("/chat/completions", post(mock_model))
        .with_state(mock.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let (status, _) = request(&fixture.router, "PUT", "/agent/settings", json!({"protocol":protocol,"baseUrl":format!("http://{address}"),"model":"mock-model","apiKey":"model-secret"})).await;
    assert_eq!(status, StatusCode::OK);
    let (_, settings) =
        request(&fixture.router, "GET", "/agent/settings", json!({})).await;
    assert!(!settings.contains("model-secret"));
    let (_, created) =
        request(&fixture.router, "POST", "/agent/sessions", json!({})).await;
    let created: Value = serde_json::from_str(&created).unwrap();
    let id = created["id"].as_str().unwrap();
    let before = configuration_snapshot(&fixture.core);
    let (_, stream) = request(
        &fixture.router,
        "POST",
        &format!("/agent/sessions/{id}/messages"),
        json!({"text":"整理本地动画"}),
    )
    .await;
    assert_eq!(last_session(&stream)["status"], "questions", "{stream}");
    let (_, stream) = request(
        &fixture.router,
        "POST",
        &format!("/agent/sessions/{id}/messages"),
        json!({"answers":{"credential":"private-cookie-value"}}),
    )
    .await;
    let review = last_session(&stream);
    assert_eq!(review["status"], "review", "{stream}");
    assert_eq!(review["plan"]["processors"][0]["enabled"], true);
    assert_eq!(
        configuration_snapshot(&fixture.core),
        before,
        "Review must not write config"
    );
    assert!(
        fixture.core.component_manager.get_all_component().is_empty(),
        "Review must not construct components"
    );
    assert!(!stream.contains("private-cookie-value"));
    let calls = &mock.lock().await.1;
    assert!(!json!(calls).to_string().contains("private-cookie-value"));
    assert!(json!(calls).to_string().contains("$secret:credential"));
    if protocol == "responses" {
        assert!(
            calls[1]["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["type"] == "reasoning")
        );
    }
    let revision = review["revision"].as_u64().unwrap();
    let (status, _) = request(
        &fixture.router,
        "POST",
        &format!("/agent/sessions/{id}/approve"),
        json!({"revision":revision - 1}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(configuration_snapshot(&fixture.core), before);
    let (status, stream) = request(
        &fixture.router,
        "POST",
        &format!("/agent/sessions/{id}/approve"),
        json!({"revision":revision}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let complete = last_session(&stream);
    assert_eq!(complete["status"], "complete", "{stream}");
    assert_eq!(complete["steps"][0]["status"], "done");
    let config =
        fixture.core.config_operator.get_processor_config("local-anime").unwrap();
    assert!(config.enabled);
    assert!(
        fixture
            .core
            .processor_manager
            .get_processor("local-anime")
            .unwrap()
            .processor
            .is_some()
    );
    let account = fixture
        .core
        .config_operator
        .get_component_config(
            &source_downloader_sdk::component::ComponentType::source("fanbox".into()),
            "private-account",
        )
        .unwrap();
    assert_eq!(account.props["cookie"], "private-cookie-value");
    assert!(!stream.contains("private-cookie-value"));
    let completed = configuration_snapshot(&fixture.core);
    let (status, _) = request(
        &fixture.router,
        "POST",
        &format!("/agent/sessions/{id}/approve"),
        json!({"revision":revision}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(configuration_snapshot(&fixture.core), completed);
    request(&fixture.router, "DELETE", &format!("/agent/sessions/{id}"), json!({})).await;
    let (status, _) =
        request(&fixture.router, "GET", &format!("/agent/sessions/{id}"), json!({}))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let paths: Vec<_> = std::fs::read_dir(fixture._directory.path())
        .unwrap()
        .map(|p| p.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        paths.iter().all(|p| p == "config.yaml" || p == "agent-model.json"),
        "No chat storage: {paths:?}"
    );
    fixture.core.shutdown();
    server.abort();
}

#[tokio::test]
async fn responses_review_then_create_real_enabled_processor() {
    reviewed_flow("responses").await;
}

#[tokio::test]
async fn chat_completions_review_then_create_real_enabled_processor() {
    reviewed_flow("chat-completions").await;
}

#[tokio::test]
async fn shared_component_aliases_and_missing_references_are_validated() {
    let fixture = fixture().await;
    let mut plan: ConfigurationPlan = serde_json::from_value(local_plan()).unwrap();
    plan.normalize();
    plan.validate(&fixture.core).unwrap();
    plan.processors[0]["downloader"] = json!("system-file:missing");
    assert!(plan.validate(&fixture.core).unwrap_err().contains("Missing component"));
    assert!(fixture.core.component_manager.get_all_component().is_empty());
    fixture.core.shutdown();
}

#[tokio::test]
async fn component_failure_keeps_created_resources_and_retries_do_not_duplicate() {
    let fixture = fixture().await;
    let mut plan: ConfigurationPlan = serde_json::from_value(local_plan()).unwrap();
    plan.normalize();
    plan.execute_step(&fixture.core, 0, false).unwrap();
    assert!(
        plan.execute_step(&fixture.core, 0, false).is_err(),
        "Unrelated existing resource cannot be overwritten"
    );
    plan.execute_step(&fixture.core, 0, true).unwrap();
    assert_eq!(
        fixture.core.config_operator.get_all_component_config()["source"].len(),
        1
    );
    fixture.core.shutdown();
}

async fn reviewed_state(fixture: &Fixture, plan: Value) -> (Shared, String, u64) {
    let id = "review-test".to_owned();
    let mut session = Session::new(id.clone());
    tools::execute(
        &fixture.core,
        &mut session,
        "propose_plan",
        &json!({"configuration":plan.to_string()}).to_string(),
    )
    .unwrap();
    let revision = session.revision;
    let state = Arc::new(AgentState {
        core: fixture.core.clone(),
        sessions: Mutex::new(HashMap::from([(
            id.clone(),
            Arc::new(Mutex::new(session)),
        )])),
        settings: RwLock::new(()),
        apply: Arc::new(Mutex::new(())),
        settings_path: fixture._directory.path().join("unused-settings.json"),
    });
    (state, id, revision)
}

#[tokio::test]
async fn existing_config_change_invalidates_review_before_any_creation() {
    let fixture = fixture().await;
    let (state, id, revision) = reviewed_state(&fixture, local_plan()).await;
    fixture
        .core
        .config_operator
        .save_instance(source_downloader_core::config::InstanceConfig {
            name: "external-change".into(),
            props: Default::default(),
        })
        .unwrap();
    assert!(approve(State(state), Path(id), Json(Approval { revision })).await.is_err());
    assert!(fixture.core.config_operator.get_all_component_config().is_empty());
    assert!(fixture.core.config_operator.get_all_processor_config().is_empty());
    fixture.core.shutdown();
}

struct FailOnceSupplier(std::sync::atomic::AtomicBool);
impl source_downloader_sdk::component::ComponentSupplier for FailOnceSupplier {
    fn get_metadata(
        &self,
    ) -> Option<Box<source_downloader_sdk::component::SdComponentMetadata>> {
        None
    }
    fn supply_types(&self) -> Vec<source_downloader_sdk::component::ComponentType> {
        use source_downloader_sdk::component::ComponentType;
        vec![
            ComponentType::source("retry-source".into()),
            ComponentType::downloader("retry-source".into()),
        ]
    }
    fn apply(
        &self,
        context: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &serde_json::Map<String, Value>,
    ) -> Result<
        Arc<dyn source_downloader_sdk::component::SdComponent>,
        source_downloader_sdk::component::ComponentError,
    > {
        if !self.0.swap(true, Ordering::SeqCst) {
            return Err(source_downloader_sdk::component::ComponentError::new(
                "temporary failure",
            ));
        }
        source_downloader_core::components::system_file_source::SUPPLIER
            .apply(context, props)
    }
}

#[tokio::test]
async fn failed_creation_resumes_reviewed_plan_and_skips_completed_steps() {
    let fixture = fixture().await;
    fixture
        .core
        .component_manager
        .register_supplier(Arc::new(FailOnceSupplier(
            std::sync::atomic::AtomicBool::new(false),
        )))
        .unwrap();
    let mut plan = local_plan();
    plan["components"].as_array_mut().unwrap().push(json!({"rootType":"source","typeName":"retry-source","name":"retry","props":{"path":"/tmp/retry"}}));
    plan["processors"][0]["source"] = json!("retry-source:retry");
    plan["processors"][0]["downloader"] = json!("retry-source:retry");
    let (state, id, revision) = reviewed_state(&fixture, plan).await;
    let response =
        approve(State(state.clone()), Path(id.clone()), Json(Approval { revision }))
            .await
            .unwrap()
            .into_response();
    let stream = String::from_utf8(
        to_bytes(response.into_body(), 1_000_000).await.unwrap().to_vec(),
    )
    .unwrap();
    let failed = last_session(&stream);
    assert_eq!(failed["status"], "failed", "{stream}");
    assert_eq!(failed["steps"][0]["status"], "done");
    assert_eq!(failed["steps"][1]["status"], "failed");
    assert!(fixture.core.config_operator.get_all_processor_config().is_empty());
    let response = approve(State(state), Path(id), Json(Approval { revision }))
        .await
        .unwrap()
        .into_response();
    let stream = String::from_utf8(
        to_bytes(response.into_body(), 1_000_000).await.unwrap().to_vec(),
    )
    .unwrap();
    assert_eq!(last_session(&stream)["status"], "complete", "{stream}");
    assert_eq!(
        fixture.core.config_operator.get_all_component_config()["source"].len(),
        2
    );
    fixture.core.shutdown();
}

#[tokio::test]
async fn mikan_and_fanbox_plans_resolve_real_registered_capabilities_without_creation() {
    let fixture = fixture().await;
    let plans = [
        json!({"summary":"Mikan", "instances":[], "components":[
            {"rootType":"source","typeName":"mikan","name":"mikan-feed","props":{"url":"https://example.com/rss"}},
            {"rootType":"downloader","typeName":"qbittorrent","name":"qb","props":{"endpoint":"http://127.0.0.1:8081","username":"test","password":"example"}},
            {"rootType":"trigger","typeName":"fixed","name":"20min","props":{"interval":"PT20M"}}
        ], "processors":[{"name":"mikan","save-path":"/tmp/anime","triggers":["fixed:20min"],"source":"mikan:mikan-feed","item-file-resolver":"torrent","downloader":"qbittorrent:qb","file-mover":"qbittorrent:qb","options":{"variable-providers":["mikan","episode"],"save-path-pattern":"{name}/Season {season}","filename-pattern":"{nameCn} - S{season}E{episode}"}}]}),
        json!({"summary":"Fanbox", "instances":[], "components":[
            {"rootType":"source","typeName":"fanbox","name":"account","props":{"cookie":"example-cookie"}},
            {"rootType":"downloader","typeName":"http","name":"http","props":{"download-path":"/tmp/downloads"}}
        ], "processors":[{"name":"fanbox","save-path":"/tmp/fanbox","source":"fanbox:account","item-file-resolver":"fanbox:account","downloader":"http","file-mover":"general","options":{"variable-providers":["sequence"],"save-path-pattern":"{item.attrs.username}/{item.date} {item.title}-{item.attrs.postId}","filename-pattern":"{sequence}","file-grouping":[{"expression-matching":"file.attrs.type == 'file'","filename-pattern":""}]}}]}),
    ];
    for value in plans {
        let mut plan: ConfigurationPlan = serde_json::from_value(value).unwrap();
        plan.normalize();
        plan.validate(&fixture.core).unwrap();
    }
    assert!(fixture.core.component_manager.get_all_component().is_empty());
    assert!(fixture.core.config_operator.get_all_component_config().is_empty());
    fixture.core.shutdown();
}
