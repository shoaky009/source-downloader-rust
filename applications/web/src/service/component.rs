use crate::ApplicationContext;
use crate::error_handle::AppError;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Sse;
use axum::response::sse::Event;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use core::application::CoreApplication;
use core::component_manager::ComponentManager;
use core::config::ComponentConfig;
use futures_util::Stream;
use sdk::component::ComponentRootType::Trigger;
use sdk::component::{ComponentError, ComponentId, ComponentRootType, ComponentType};
use sdk::serde_json::{Map, Value};
use serde::Deserialize;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashSet;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tracing::info;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    Router::new()
        .nest(
            "/component",
            Router::new()
                .route("/", get(query_components).post(save_component))
                .route(
                    "/{root_type}/{type_name}/{name}",
                    put(save_component_props).delete(delete_component),
                )
                .route(
                    "/{root_type}/{type_name}/{name}/reload",
                    post(reload_component),
                )
                .route("/types", get(all_types))
                .route("/schema", get(component_schema))
                .route("/state-stream", get(state_stream)),
        )
        .with_state(ctx.core.clone())
}

#[axum::debug_handler]
async fn query_components(
    State(_core): State<Arc<CoreApplication>>,
    Query(query): Query<ComponentQuery>,
) -> Json<Vec<ComponentInfo>> {
    info!(
        "query_components: root_type={} type_name={} name={}",
        query.root_type.as_ref().unwrap_or(&"*".to_string()),
        query.type_name.as_ref().unwrap_or(&"*".to_string()),
        query.name.as_ref().unwrap_or(&"*".to_string())
    );

    let all_configs = _core.config_operator.get_all_component_config();
    let all_components = _core.component_manager.get_all_component();

    let mut results: Vec<ComponentInfo> = all_configs
        .into_iter()
        .filter(|(root_type_str, _)| {
            query
                .root_type
                .as_ref()
                .map_or(true, |t| t == root_type_str)
        })
        .flat_map(|(root_type_str, configs)| {
            let all_components = all_components.clone();
            configs
                .into_iter()
                .filter(|config| {
                    query
                        .type_name
                        .as_ref()
                        .map_or(true, |t| t == &config.component_type)
                })
                .filter(|config| query.name.as_ref().map_or(true, |n| n == &config.name))
                .map(move |config| {
                    let all_components = all_components.clone();
                    let root_type = ComponentRootType::parse(&root_type_str)
                        .unwrap_or(ComponentRootType::Source);
                    let component_id = ComponentId::new(
                        ComponentType {
                            root_type: root_type.clone(),
                            name: config.component_type.clone(),
                        },
                        &config.name,
                    );

                    let wrapper = all_components
                        .iter()
                        .find(|w| w.id == component_id)
                        .cloned();

                    let state_detail = wrapper.as_ref().and_then(|w| {
                        if w.primary {
                            w.component
                                .as_ref()
                                .cloned()
                                .and_then(|c| c.as_stateful())
                                .and_then(|s| s.get_state_detail())
                                .map(|m| Value::Object(m))
                        } else {
                            None
                        }
                    });

                    let (running, error_message, refs, modifiable) = wrapper
                        .as_ref()
                        .map(|w| {
                            (
                                w.component.is_some(),
                                w.creation_error.clone(),
                                w.get_refs().iter().cloned().collect::<Vec<_>>(),
                                w.component.is_some(),
                            )
                        })
                        .unwrap_or((false, None, vec![], true));

                    ComponentInfo {
                        root_type,
                        type_name: config.component_type,
                        name: config.name,
                        props: config.props,
                        state_detail,
                        primary: wrapper.as_ref().map_or(true, |w| w.primary),
                        running,
                        refs,
                        modifiable,
                        error_message,
                    }
                })
        })
        .collect();

    results.sort_by(|a, b| a.type_name.cmp(&b.type_name));
    Json(results)
}

#[axum::debug_handler]
async fn save_component(
    State(_core): State<Arc<CoreApplication>>,
    Json(request): Json<ComponentSaveRequest>,
) -> Result<(), AppError> {
    _save_component(_core, request)
}

fn _save_component(
    core: Arc<CoreApplication>,
    request: ComponentSaveRequest,
) -> Result<(), AppError> {
    check_request(&core, &request)?;
    core.config_operator.save_component(
        &request.root_type,
        ComponentConfig {
            component_type: request.type_name,
            name: request.name,
            props: request.props,
        },
    )?;
    Ok(())
}

#[axum::debug_handler]
async fn save_component_props(
    State(_core): State<Arc<CoreApplication>>,
    Path(_path): Path<ComponentIdPath>,
    Json(props): Json<Map<String, Value>>,
) -> Result<(), AppError> {
    let req = ComponentSaveRequest {
        root_type: _path.root_type,
        type_name: _path.type_name,
        name: _path.name,
        props,
    };
    _save_component(_core, req)
}

#[axum::debug_handler]
async fn delete_component(
    State(_core): State<Arc<CoreApplication>>,
    Path(path): Path<ComponentIdPath>,
) -> Result<(), AppError> {
    let id = ComponentId::new(
        ComponentType {
            root_type: path.root_type.to_owned(),
            name: path.type_name.to_owned(),
        },
        &path.name,
    );
    let wp = _core.component_manager.get_component(&id);
    if let Ok(wp) = wp {
        let refs = wp.get_refs();
        if !refs.is_empty() {
            return Err(AppError::BadRequest(format!(
                "Component has been referenced by other processor, can not be deleted. {}",
                refs.iter().cloned().collect::<Vec<String>>().join(", ")
            )));
        }
    }
    _core
        .config_operator
        .delete_component(&path.root_type, &path.type_name, &path.name)?;
    Ok(())
}

#[axum::debug_handler]
async fn reload_component(
    State(_core): State<Arc<CoreApplication>>,
    Path(path): Path<ComponentIdPath>,
) -> Result<(), AppError> {
    let id = ComponentId::parse(&format!(
        "{}:{}:{}",
        path.root_type, path.type_name, path.name
    ))?;
    let removed = _core.component_manager.destroy(&id);
    if removed.is_none() {
        return Err(AppError::NotFound(
            "Component instance not found".to_string(),
        ));
    };

    for name in removed.unwrap().get_refs() {
        let processor = _core.processor_manager.get_processor(&name);
        if processor.is_none() {
            continue;
        }

        let config = _core.config_operator.get_processor_config(&name);
        if config.is_none() {
            continue;
        }
        _core.processor_manager.destroy_processor(&name);
        _core.processor_manager.create_processor(&config.unwrap());
    }

    if path.root_type == Trigger {
        let trigger = _core
            .component_manager
            .get_component(&id)?
            .require_component()?
            .as_trigger()?;
        trigger.start();
    };
    Ok(())
}

#[axum::debug_handler]
async fn all_types(
    State(core): State<Arc<CoreApplication>>,
    Query(q): Query<TypesQuery>,
) -> Json<Vec<ComponentTypeInfo>> {
    Json(
        core.component_manager
            .get_all_suppliers()
            .iter()
            .flat_map(|x| x.supply_types())
            .filter(|x| q.root_type.as_ref().map_or(true, |t| *t == x.root_type))
            .map(|x| ComponentTypeInfo {
                root_type: x.root_type,
                name: x.name,
            })
            .collect::<Vec<_>>(),
    )
}

#[axum::debug_handler]
async fn component_schema(State(_core): State<Arc<CoreApplication>>) -> Json<Vec<String>> {
    info!("component_schema");
    Json(vec![])
}

#[axum::debug_handler]
async fn state_stream(
    State(_core): State<Arc<CoreApplication>>,
    Qs(query): Qs<ComponentIds>,
) -> Sse<ComponentStateStream> {
    info!("state_stream");
    let component_ids = query
        .id
        .iter()
        .map(|x| ComponentId::parse(x))
        .collect::<Result<HashSet<_>, _>>()
        .unwrap();
    let stream = ComponentStateStream::new(_core.component_manager.clone(), component_ids);
    Sse::new(stream)
}

fn check_request(
    core: &Arc<CoreApplication>,
    request: &ComponentSaveRequest,
) -> Result<(), ComponentError> {
    let c_type = ComponentType {
        root_type: request.root_type.clone(),
        name: request.type_name.clone(),
    };
    let supplier = core.component_manager.get_supplier(&c_type);
    if supplier.is_none() {
        return Err(ComponentError::new(format!(
            "ComponentType {} not register",
            c_type
        )));
    }
    let supplier = supplier.unwrap();
    supplier.apply(&request.props).map(|_| ())
}

struct ComponentStateStream {
    component_manager: Arc<ComponentManager>,
    component_ids: HashSet<ComponentId>,
    interval: tokio::time::Interval,
}

impl ComponentStateStream {
    fn new(component_manager: Arc<ComponentManager>, component_ids: HashSet<ComponentId>) -> Self {
        Self {
            component_manager,
            component_ids,
            interval: tokio::time::interval(Duration::from_secs(1)),
        }
    }
}

impl Stream for ComponentStateStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.interval).poll_tick(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(_) => {
                let components = self
                    .component_manager
                    .get_all_component()
                    .into_iter()
                    .filter(|x| self.component_ids.contains(&x.id))
                    .collect::<Vec<_>>();
                for wrapper in &components {
                    if let Some(state) = &wrapper
                        .component
                        .clone()
                        .and_then(|x| x.as_stateful())
                        .and_then(|x| x.get_state_detail())
                    {
                        let event = Event::default()
                            .id(wrapper.id.display())
                            .event("component-state")
                            .data(sdk::serde_json::to_string(&state).unwrap_or("{}".to_string()));
                        return Poll::Ready(Some(Ok(event)));
                    }
                }
                Poll::Ready(None)
            }
        }
    }
}

#[derive(Deserialize)]
struct ComponentIdPath {
    root_type: ComponentRootType,
    type_name: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentQuery {
    #[serde(rename = "type")]
    root_type: Option<String>,
    type_name: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
struct ComponentIds {
    id: Vec<String>,
}

#[derive(Serialize)]
struct ComponentTypeInfo {
    #[serde(rename = "type")]
    root_type: ComponentRootType,
    name: String,
}

#[derive(Deserialize)]
struct TypesQuery {
    #[serde(rename = "type")]
    root_type: Option<ComponentRootType>,
}

#[derive(Deserialize)]
struct ComponentSaveRequest {
    #[serde(rename = "type")]
    root_type: ComponentRootType,
    #[serde(rename = "typeName")]
    type_name: String,
    name: String,
    #[serde(default)]
    props: Map<String, Value>,
}

#[derive(Serialize)]
struct ComponentInfo {
    #[serde(rename = "type")]
    root_type: ComponentRootType,
    #[serde(rename = "typeName")]
    type_name: String,
    name: String,
    props: Map<String, Value>,
    #[serde(rename = "stateDetail")]
    state_detail: Option<Value>,
    primary: bool,
    running: bool,
    refs: Vec<String>,
    modifiable: bool,
    #[serde(rename = "errorMessage")]
    error_message: Option<String>,
}

struct Qs<T>(pub T);

impl<S, T> FromRequestParts<S> for Qs<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or("");
        let value =
            serde_qs::from_str(query).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
        Ok(Qs(value))
    }
}
