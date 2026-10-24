mod provider;
mod tools;

use crate::{ApplicationContext, error_handle::AppError};
use axum::{
    Json, Router,
    extract::{Path, State},
    response::{
        Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use source_downloader_core::{
    application::CoreApplication,
    config_agent::{ConfigurationPlan, configuration_snapshot},
};
use std::{
    collections::HashMap,
    convert::Infallible,
    io::Write,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{Mutex, RwLock, mpsc};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSettings {
    pub protocol: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            protocol: "responses".into(),
            base_url: "https://api.openai.com/v1".into(),
            model: String::new(),
            api_key: String::new(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Question {
    pub id: String,
    pub title: String,
    pub options: Vec<String>,
    pub secret: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Message {
    role: String,
    text: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    name: String,
    status: String,
    error: Option<String>,
}

struct Session {
    id: String,
    messages: Vec<Message>,
    history: Vec<Value>,
    protocol: Option<String>,
    questions: Vec<Question>,
    secrets: HashMap<String, String>,
    plan: Option<ConfigurationPlan>,
    snapshot: Option<Value>,
    revision: u64,
    status: String,
    steps: Vec<Step>,
    touched: Instant,
}

impl Session {
    fn new(id: String) -> Self {
        Self {
            id,
            messages: vec![],
            history: vec![],
            protocol: None,
            questions: vec![],
            secrets: HashMap::new(),
            plan: None,
            snapshot: None,
            revision: 0,
            status: "idle".into(),
            steps: vec![],
            touched: Instant::now(),
        }
    }
    fn view(&self) -> Value {
        json!({"id":self.id,"messages":self.messages,"questions":self.questions,"plan":self.plan,"revision":self.revision,"status":self.status,"steps":self.steps})
    }
    fn sanitize(&self, text: &str) -> String {
        let mut text = text.to_owned();
        for secret in self.secrets.values().filter(|v| !v.is_empty()) {
            text = text.replace(secret, "[凭据]");
        }
        text
    }
    fn resolve_plan(
        &self,
        plan: &ConfigurationPlan,
    ) -> Result<ConfigurationPlan, String> {
        let mut value = serde_json::to_value(plan).map_err(|e| e.to_string())?;
        // Credentials may populate props, never resource names, summaries or paths
        // hidden behind a reference in the reviewed processor configuration.
        for section in ["instances", "components"] {
            if let Some(resources) = value[section].as_array_mut() {
                for resource in resources {
                    resolve_secrets(&mut resource["props"], &self.secrets)?;
                }
            }
        }
        if json!(plan.processors).to_string().contains("$secret:") {
            return Err(
                "Credential references are only supported in component/instance props"
                    .into(),
            );
        }
        serde_json::from_value(value).map_err(|e| e.to_string())
    }
    fn invalidate_review(&mut self) {
        self.revision += 1;
        self.plan = None;
        self.snapshot = None;
        self.steps.clear();
        self.status = "thinking".into();
    }
    fn check_approval(&self, revision: u64) -> Result<(), String> {
        if self.revision != revision
            || !matches!(self.status.as_str(), "review" | "failed")
            || self.plan.is_none()
        {
            return Err("方案已变更或不可执行，请重新查看并确认最新方案".into());
        }
        Ok(())
    }
}

fn resolve_secrets(
    value: &mut Value,
    secrets: &HashMap<String, String>,
) -> Result<(), String> {
    match value {
        Value::String(text) if text.starts_with("$secret:") => {
            *text = secrets
                .get(&text[8..])
                .ok_or_else(|| format!("Missing credential reference: {text}"))?
                .clone();
        }
        Value::String(text) if text.contains("$secret:") => {
            return Err(
                "Credential references must be the complete property value".into()
            );
        }
        Value::Object(map) => {
            for value in map.values_mut() {
                resolve_secrets(value, secrets)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                resolve_secrets(value, secrets)?;
            }
        }
        _ => {}
    }
    Ok(())
}

struct AgentState {
    core: Arc<CoreApplication>,
    sessions: Mutex<HashMap<String, Arc<Mutex<Session>>>>,
    settings: RwLock<()>,
    apply: Arc<Mutex<()>>,
    settings_path: PathBuf,
}

type Shared = Arc<AgentState>;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    let state = Arc::new(AgentState {
        settings_path: ctx.core.data_location.join("agent-model.json"),
        core: ctx.core.clone(),
        sessions: Mutex::new(HashMap::new()),
        settings: RwLock::new(()),
        apply: Arc::new(Mutex::new(())),
    });
    Router::new()
        .nest(
            "/agent",
            Router::new()
                .route("/settings", get(get_settings).put(save_settings))
                .route("/sessions", post(new_session))
                .route("/sessions/{id}", get(get_session).delete(delete_session))
                .route("/sessions/{id}/messages", post(chat))
                .route("/sessions/{id}/approve", post(approve)),
        )
        .with_state(state)
}

fn bad(message: impl Into<String>) -> AppError {
    AppError::BadRequest(message.into())
}

async fn read_settings(state: &AgentState) -> Result<ModelSettings, AppError> {
    let _guard = state.settings.read().await;
    match tokio::fs::read(&state.settings_path).await {
        Ok(data) => serde_json::from_slice(&data).map_err(|_| bad("模型配置文件无效")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(ModelSettings::default())
        }
        Err(e) => Err(e.into()),
    }
}

async fn get_settings(State(state): State<Shared>) -> Result<Json<Value>, AppError> {
    let settings = read_settings(&state).await?;
    Ok(Json(
        json!({"protocol":settings.protocol,"baseUrl":settings.base_url,"model":settings.model,"hasApiKey":!settings.api_key.is_empty()}),
    ))
}

async fn save_settings(
    State(state): State<Shared>,
    Json(mut settings): Json<ModelSettings>,
) -> Result<Json<Value>, AppError> {
    if !matches!(settings.protocol.as_str(), "responses" | "chat-completions")
        || settings.model.trim().is_empty()
    {
        return Err(bad("请选择接口协议并填写模型名称"));
    }
    let url =
        reqwest::Url::parse(&settings.base_url).map_err(|_| bad("模型服务地址无效"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(bad("模型地址必须是 HTTP(S) Base URL，不包含账号、查询参数或片段"));
    }
    let _guard = state.settings.write().await;
    if settings.api_key.is_empty()
        && let Ok(bytes) = tokio::fs::read(&state.settings_path).await
    {
        let old: ModelSettings =
            serde_json::from_slice(&bytes).map_err(|_| bad("模型配置文件无效"))?;
        // Never forward an old provider's key to a different endpoint.
        if old.base_url.trim_end_matches('/') == settings.base_url.trim_end_matches('/') {
            settings.api_key = old.api_key;
        }
    }
    let parent = state.settings_path.parent().ok_or_else(|| bad("模型配置路径无效"))?;
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    let bytes = serde_json::to_vec_pretty(&settings).map_err(|e| bad(e.to_string()))?;
    file.write_all(&bytes)?;
    file.as_file().sync_all()?;
    file.persist(&state.settings_path)
        .map_err(|e| AppError::InternalError(e.to_string()))?;
    Ok(Json(json!({"saved":true})))
}

async fn session(state: &AgentState, id: &str) -> Result<Arc<Mutex<Session>>, AppError> {
    state
        .sessions
        .lock()
        .await
        .get(id)
        .cloned()
        .ok_or_else(|| AppError::NotFound("会话已结束，请开始新对话".into()))
}

async fn new_session(State(state): State<Shared>) -> Result<Json<Value>, AppError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut sessions = state.sessions.lock().await;
    sessions.retain(|_, session| {
        session
            .try_lock()
            .map_or(true, |s| s.touched.elapsed() < Duration::from_secs(7200))
    });
    if sessions.len() >= 64 {
        return Err(bad("临时会话过多，请关闭不用的配置助手页面"));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| bad(e.to_string()))?
        .as_nanos();
    let id = format!("{stamp:x}-{:x}", NEXT.fetch_add(1, Ordering::Relaxed));
    let session = Session::new(id.clone());
    let view = session.view();
    sessions.insert(id, Arc::new(Mutex::new(session)));
    Ok(Json(view))
}

async fn get_session(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let session = session(&state, &id).await?;
    let session = session.try_lock().map_err(|_| bad("会话正在处理中"))?;
    Ok(Json(session.view()))
}

async fn delete_session(
    State(state): State<Shared>,
    Path(id): Path<String>,
) -> Json<Value> {
    state.sessions.lock().await.remove(&id);
    Json(json!({"deleted":true}))
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AgentEvent {
    Delta { text: String },
    Status { text: String },
    Session { session: Value },
    Error { message: String },
    Done,
}

fn events(
    mut receiver: mpsc::Receiver<AgentEvent>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    Sse::new(async_stream::stream! {
        while let Some(event) = receiver.recv().await {
            if let Ok(data) = serde_json::to_string(&event) { yield Ok(Event::default().data(data)); }
        }
    }).keep_alive(KeepAlive::default())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChatRequest {
    #[serde(default)]
    text: String,
    #[serde(default)]
    answers: HashMap<String, String>,
}

async fn chat(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(request): Json<ChatRequest>,
) -> Result<impl axum::response::IntoResponse, AppError> {
    let settings = read_settings(&state).await?;
    if settings.model.is_empty() {
        return Err(bad("请先设置模型连接"));
    }
    if request.text.len() > 32_000 || request.answers.values().any(|v| v.len() > 32_000) {
        return Err(bad("输入内容过长"));
    }
    let mut session = session(&state, &id)
        .await?
        .try_lock_owned()
        .map_err(|_| bad("请等待当前操作完成"))?;
    if session.protocol.as_ref().is_some_and(|p| p != &settings.protocol) {
        return Err(bad("接口协议已变更，请开始新对话"));
    }
    if session.messages.len() > 300 {
        return Err(bad("当前对话过长，请开始新对话"));
    }
    let mut text = request.text.trim().to_owned();
    let questions = session.questions.clone();
    for (id, answer) in &request.answers {
        let question = questions
            .iter()
            .find(|q| &q.id == id)
            .ok_or_else(|| bad("问题已过期，请回答当前问题"))?;
        if answer.trim().is_empty() {
            return Err(bad("请补充问题答案"));
        }
        let answer = if question.secret {
            session.secrets.insert(id.clone(), answer.clone());
            format!("$secret:{id}")
        } else {
            answer.clone()
        };
        text.push_str(&format!("\n{}: {}", question.title, answer));
    }
    if text.trim().is_empty() {
        return Err(bad("请输入需求或回答问题"));
    }
    session.invalidate_review();
    session.questions.clear();
    session.protocol = Some(settings.protocol.clone());
    session.touched = Instant::now();
    session.messages.push(Message { role: "user".into(), text: text.clone() });
    session.history.push(json!({"role":"user","content":text}));
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let _ = tx.send(AgentEvent::Session { session: session.view() }).await;
        let checkpoint = session.history.len();
        let outcome = tokio::select! {
            result = converse(&state.core, &settings, &mut session, &tx) => result,
            _ = tx.closed() => Err("已停止生成，可以继续补充需求".into()),
        };
        if let Err(error) = outcome {
            let error = session.sanitize(&error);
            session.history.truncate(checkpoint);
            session.plan = None;
            session.questions.clear();
            session.status = "idle".into();
            session.messages.push(Message { role: "error".into(), text: error.clone() });
            let _ = tx.send(AgentEvent::Error { message: error }).await;
        }
        session.touched = Instant::now();
        let _ = tx.send(AgentEvent::Session { session: session.view() }).await;
        let _ = tx.send(AgentEvent::Done).await;
    });
    Ok(events(rx))
}

async fn converse(
    core: &CoreApplication,
    settings: &ModelSettings,
    session: &mut Session,
    tx: &mpsc::Sender<AgentEvent>,
) -> Result<(), String> {
    let definitions = tools::definitions();
    for _ in 0..16 {
        let reply = provider::complete(
            settings,
            &session.history,
            tools::INSTRUCTIONS,
            &definitions,
            tx,
        )
        .await?;
        if !reply.text.is_empty() {
            session.messages.push(Message {
                role: "assistant".into(),
                text: session.sanitize(&reply.text),
            });
        }
        session.history.extend(reply.items);
        if reply.calls.is_empty() {
            session.status = "idle".into();
            return Ok(());
        }
        let mut pause = false;
        for call in reply.calls {
            let label = match call.name.as_str() {
                "catalog" => "查询可用组件和现有配置",
                "inspect_capability" => "读取组件配置要求",
                "ask_user" => "需要补充信息",
                "propose_plan" => "校验配置方案",
                _ => "检查工具请求",
            };
            let _ = tx.send(AgentEvent::Status { text: label.into() }).await;
            let result = if pause {
                Err("Already awaiting user input; call this tool after their reply"
                    .into())
            } else {
                tools::execute(core, session, &call.name, &call.arguments)
            };
            let value = match result {
                Ok((value, stop)) => {
                    pause |= stop;
                    value
                }
                Err(error) => json!({"error":session.sanitize(&error)}),
            };
            session.messages.push(Message {
                role: "tool".into(),
                text: if let Some(error) = value["error"].as_str() {
                    format!("{label}：{error}")
                } else {
                    format!("{label} · 完成")
                },
            });
            session.history.push(provider::tool_result(
                &settings.protocol,
                &call.id,
                &value,
            ));
        }
        let _ = tx.send(AgentEvent::Session { session: session.view() }).await;
        if pause {
            if !session.questions.is_empty() {
                session.status = "questions".into();
            }
            return Ok(());
        }
    }
    Err("已达到本轮工具调用上限，请补充需求后继续".into())
}

#[derive(Deserialize)]
struct Approval {
    revision: u64,
}

async fn approve(
    State(state): State<Shared>,
    Path(id): Path<String>,
    Json(request): Json<Approval>,
) -> Result<impl axum::response::IntoResponse, AppError> {
    let mut session = session(&state, &id)
        .await?
        .try_lock_owned()
        .map_err(|_| bad("请等待当前操作完成"))?;
    session.check_approval(request.revision).map_err(bad)?;
    let apply = state
        .apply
        .clone()
        .try_lock_owned()
        .map_err(|_| bad("另一份方案正在执行，请稍后重试"))?;
    let plan = session
        .resolve_plan(session.plan.as_ref().ok_or_else(|| bad("没有待确认方案"))?)
        .map_err(bad)?;
    plan.validate(&state.core).map_err(|e| bad(session.sanitize(&e)))?;
    if session.snapshot.as_ref() != Some(&configuration_snapshot(&state.core)) {
        return Err(bad("现有配置已发生变化，请在对话中重新生成方案并 Review"));
    }
    if session.steps.is_empty() {
        plan.check_new_resources(&state.core).map_err(bad)?;
        session.steps = plan
            .steps()
            .into_iter()
            .map(|name| Step { name, status: "pending".into(), error: None })
            .collect();
    }
    session.status = "executing".into();
    let (tx, rx) = mpsc::channel(64);
    tokio::spawn(async move {
        let _apply = apply;
        let _ = tx.send(AgentEvent::Session { session: session.view() }).await;
        for index in 0..session.steps.len() {
            if session.steps[index].status == "done" {
                continue;
            }
            let resume = session.steps[index].status != "pending";
            session.steps[index].status = "running".into();
            session.steps[index].error = None;
            let _ = tx.send(AgentEvent::Session { session: session.view() }).await;
            let core = state.core.clone();
            let plan = plan.clone();
            let result = tokio::task::spawn_blocking(move || {
                plan.execute_step(&core, index, resume)
            })
            .await;
            let result = result
                .unwrap_or_else(|_| Err("创建任务意外中断，请检查配置后重试".into()));
            session.snapshot = Some(configuration_snapshot(&state.core));
            match result {
                Ok(()) => session.steps[index].status = "done".into(),
                Err(error) => {
                    let error = session.sanitize(&error);
                    session.steps[index].status = "failed".into();
                    session.steps[index].error = Some(error.clone());
                    session.status = "failed".into();
                    let _ = tx.send(AgentEvent::Error { message: error }).await;
                    break;
                }
            }
            let _ = tx.send(AgentEvent::Session { session: session.view() }).await;
        }
        if session.steps.iter().all(|s| s.status == "done") {
            session.status = "complete".into();
        }
        let report = format!(
            "方案执行状态：{}。{}",
            session.status,
            session
                .steps
                .iter()
                .map(|s| format!(
                    "{}={}{}",
                    s.name,
                    s.status,
                    s.error.as_deref().unwrap_or_default()
                ))
                .collect::<Vec<_>>()
                .join("；")
        );
        session.history.push(
            json!({"role":"user","content":format!("[后端实际执行结果] {report}")}),
        );
        let text = if session.status == "complete" {
            "组件和处理器已创建，处理器已启用。"
        } else {
            "创建中断，已完成的资源已保留。可重试未完成步骤，或修改需求后重新 Review。"
        };
        session.messages.push(Message { role: "assistant".into(), text: text.into() });
        session.touched = Instant::now();
        let _ = tx.send(AgentEvent::Session { session: session.view() }).await;
        let _ = tx.send(AgentEvent::Done).await;
    });
    Ok(events(rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_requirements_revoke_review() {
        let mut session = Session::new("s".into());
        session.plan = Some(ConfigurationPlan {
            summary: "test".into(),
            instances: vec![],
            components: vec![],
            processors: vec![],
        });
        session.status = "review".into();
        session.revision = 1;
        assert!(session.check_approval(1).is_ok());
        session.invalidate_review();
        assert!(session.check_approval(1).is_err());
    }

    #[test]
    fn model_cannot_create_resources() {
        let tools = tools::definitions();
        assert!(!tools.iter().any(|t| t["name"].as_str().unwrap().starts_with("create")));
    }

    #[test]
    fn secret_answers_are_not_in_session_view() {
        let mut session = Session::new("s".into());
        session.secrets.insert("cookie".into(), "private-value".into());
        assert!(!session.view().to_string().contains("private-value"));
        let mut props = json!({"cookie":"$secret:cookie"});
        resolve_secrets(&mut props, &session.secrets).unwrap();
        assert_eq!(props["cookie"], "private-value");
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod integration_tests;
