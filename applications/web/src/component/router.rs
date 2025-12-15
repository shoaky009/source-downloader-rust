use crate::ApplicationContext;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::Sse;
use axum::response::sse::Event;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use core::ComponentManager;
use core::CoreApplication;
use futures_util::Stream;
use sdk::serde::Deserialize;
use sdk::component::ComponentId;
use std::collections::HashSet;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tracing::info;

pub fn register_routers(ctx: Arc<ApplicationContext>) -> Router {
    let core = ctx.core.clone();
    Router::new()
        .nest(
            "/component",
            Router::new()
                .route("/", get(query_components))
                .route("/", post(create_component))
                .route("/{root_type}/{type_name}/{name}", delete(delete_component))
                .route(
                    "/{root_type}/{type_name}/{name}/reload",
                    post(reload_component),
                )
                .route("/types", get(all_types))
                .route("/schema", get(component_schema))
                .route("/state-stream", get(state_stream)),
        )
        .with_state(core)
}

#[axum::debug_handler]
async fn query_components(
    State(_): State<Arc<CoreApplication>>,
    Query(query): Query<ComponentQuery>,
) -> Json<Vec<String>> {
    info!(
        "query_components: root_type={} type_name={} name={}",
        query.root_type.unwrap_or("*".to_string()),
        query.type_name.unwrap_or("*".to_string()),
        query.name.unwrap_or("*".to_string())
    );
    Json(vec![])
}

#[axum::debug_handler]
async fn create_component(State(_): State<Arc<CoreApplication>>) -> Json<String> {
    info!("create_component");
    Json("".to_string())
}

#[axum::debug_handler]
async fn delete_component(
    State(_): State<Arc<CoreApplication>>,
    Path(path): Path<ComponentIdPath>,
) -> () {
    info!(
        "delete_component: {}:{}:{}",
        path.root_type, path.type_name, path.name
    );
}

#[axum::debug_handler]
async fn reload_component(
    State(_): State<Arc<CoreApplication>>,
    Path(path): Path<ComponentIdPath>,
) -> () {
    info!(
        "reload_component: {}:{}:{}",
        path.root_type, path.type_name, path.name
    );
}

#[axum::debug_handler]
async fn all_types(State(_): State<Arc<CoreApplication>>) -> Json<Vec<String>> {
    info!("all_types");
    Json(vec![])
}

#[axum::debug_handler]
async fn component_schema(State(_): State<Arc<CoreApplication>>) -> Json<Vec<String>> {
    info!("component_schema");
    Json(vec![])
}

#[axum::debug_handler]
async fn state_stream(
    State(core): State<Arc<CoreApplication>>,
    Qs(query): Qs<ComponentIds>,
) -> Sse<ComponentStateStream> {
    info!("state_stream");
    let component_ids = query
        .id
        .iter()
        .map(|x| ComponentId::parse(x))
        .collect::<Result<HashSet<_>, _>>()
        .unwrap();
    let stream = ComponentStateStream::new(core.component_manager.clone(), component_ids);
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(1))
            .text("keep-alive-text"),
    )
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
    root_type: String,
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

use serde::de::DeserializeOwned;

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
