use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::routing::any;
use source_downloader_core::components::webhook_trigger::{
    WebhookAdapter, WebhookEndpoint, WebhookRequestHandler,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Clone, Default)]
pub(super) struct AxumWebhookAdapter {
    endpoints: Arc<RwLock<HashMap<WebhookEndpoint, Arc<dyn WebhookRequestHandler>>>>,
}

enum WebhookLookup {
    Found(Arc<dyn WebhookRequestHandler>),
    MethodNotAllowed,
    NotFound,
}

impl AxumWebhookAdapter {
    fn lookup(&self, path: &str, method: &str) -> Result<WebhookLookup, String> {
        let endpoint = match WebhookEndpoint::from_route_path(path, method) {
            Ok(Some(endpoint)) => endpoint,
            Ok(None) | Err(_) => return Ok(WebhookLookup::NotFound),
        };
        let endpoints = self
            .endpoints
            .read()
            .map_err(|_| "Webhook endpoint registry is poisoned".to_owned())?;
        if let Some(handler) = endpoints.get(&endpoint) {
            return Ok(WebhookLookup::Found(handler.clone()));
        }
        if endpoints.keys().any(|known| known.path() == endpoint.path()) {
            Ok(WebhookLookup::MethodNotAllowed)
        } else {
            Ok(WebhookLookup::NotFound)
        }
    }

    pub(super) fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/webhook/{*path}", any(handle_webhook_request))
            .with_state(WebhookRouteState { adapter: self })
    }
}

impl WebhookAdapter for AxumWebhookAdapter {
    fn register_endpoint(
        &self,
        endpoint: &WebhookEndpoint,
        handler: Arc<dyn WebhookRequestHandler>,
    ) -> Result<(), String> {
        let mut endpoints = self
            .endpoints
            .write()
            .map_err(|_| "Webhook endpoint registry is poisoned".to_owned())?;
        if endpoints.contains_key(endpoint) {
            return Err(format!(
                "Webhook endpoint already registered: {} {}",
                endpoint.method().as_str(),
                endpoint.route_path()
            ));
        }
        endpoints.insert(endpoint.clone(), handler);
        Ok(())
    }

    fn unregister_endpoint(&self, endpoint: &WebhookEndpoint) -> Result<(), String> {
        let mut endpoints = self
            .endpoints
            .write()
            .map_err(|_| "Webhook endpoint registry is poisoned".to_owned())?;
        endpoints.remove(endpoint);
        Ok(())
    }
}

#[derive(Clone)]
struct WebhookRouteState {
    adapter: Arc<AxumWebhookAdapter>,
}

async fn handle_webhook_request(
    State(state): State<WebhookRouteState>,
    request: Request,
) -> StatusCode {
    let lookup =
        match state.adapter.lookup(request.uri().path(), request.method().as_str()) {
            Ok(lookup) => lookup,
            Err(error) => {
                tracing::error!(%error, "Failed to inspect webhook endpoint registry");
                return StatusCode::INTERNAL_SERVER_ERROR;
            }
        };
    match lookup {
        WebhookLookup::Found(handler) => {
            if handler.handle_request() {
                StatusCode::NO_CONTENT
            } else {
                StatusCode::NOT_FOUND
            }
        }
        WebhookLookup::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
        WebhookLookup::NotFound => StatusCode::NOT_FOUND,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TestHandler {
        calls: AtomicUsize,
    }

    impl WebhookRequestHandler for TestHandler {
        fn handle_request(&self) -> bool {
            self.calls.fetch_add(1, Ordering::AcqRel);
            true
        }
    }

    fn webhook_request(method: &str, path: &str) -> Request {
        Request::builder().method(method).uri(path).body(Body::empty()).unwrap()
    }

    fn endpoint(method: &str) -> WebhookEndpoint {
        WebhookEndpoint::new("updates", method).unwrap()
    }

    #[test]
    fn endpoint_method_is_case_insensitive() {
        let adapter = AxumWebhookAdapter::default();
        let endpoint = endpoint("post");
        let handler = Arc::new(TestHandler { calls: AtomicUsize::new(0) });

        adapter.register_endpoint(&endpoint, handler).unwrap();

        assert!(matches!(
            adapter.lookup("/webhook/updates", "POST").unwrap(),
            WebhookLookup::Found(_)
        ));
    }

    #[test]
    fn duplicate_endpoint_is_rejected() {
        let adapter = AxumWebhookAdapter::default();
        let first = endpoint("POST");
        let second = endpoint("post");
        let handler = || Arc::new(TestHandler { calls: AtomicUsize::new(0) });

        adapter.register_endpoint(&first, handler()).unwrap();
        assert!(adapter.register_endpoint(&second, handler()).is_err());
    }

    #[test]
    fn unregister_missing_endpoint_is_idempotent() {
        let adapter = AxumWebhookAdapter::default();
        let endpoint = endpoint("POST");

        adapter.unregister_endpoint(&endpoint).unwrap();
        adapter.unregister_endpoint(&endpoint).unwrap();
    }

    #[test]
    fn same_path_different_method_is_allowed() {
        let adapter = AxumWebhookAdapter::default();
        let post = endpoint("POST");
        let get = endpoint("GET");

        adapter
            .register_endpoint(
                &post,
                Arc::new(TestHandler { calls: AtomicUsize::new(0) }),
            )
            .unwrap();
        adapter
            .register_endpoint(&get, Arc::new(TestHandler { calls: AtomicUsize::new(0) }))
            .unwrap();

        assert!(matches!(
            adapter.lookup("/webhook/updates", "POST").unwrap(),
            WebhookLookup::Found(_)
        ));
        assert!(matches!(
            adapter.lookup("/webhook/updates", "GET").unwrap(),
            WebhookLookup::Found(_)
        ));
    }

    #[test]
    fn path_registered_with_wrong_method_returns_method_not_allowed() {
        let adapter = AxumWebhookAdapter::default();
        let endpoint = endpoint("POST");
        adapter
            .register_endpoint(
                &endpoint,
                Arc::new(TestHandler { calls: AtomicUsize::new(0) }),
            )
            .unwrap();

        assert!(matches!(
            adapter.lookup("/webhook/updates", "GET").unwrap(),
            WebhookLookup::MethodNotAllowed
        ));
        assert!(matches!(
            adapter.lookup("/webhook/missing", "GET").unwrap(),
            WebhookLookup::NotFound
        ));
    }

    #[tokio::test]
    async fn registered_request_returns_no_content_and_dispatches() {
        let adapter = Arc::new(AxumWebhookAdapter::default());
        let endpoint = endpoint("POST");
        let handler = Arc::new(TestHandler { calls: AtomicUsize::new(0) });
        adapter.register_endpoint(&endpoint, handler.clone()).unwrap();

        let status = handle_webhook_request(
            State(WebhookRouteState { adapter: adapter.clone() }),
            webhook_request("POST", "/webhook/updates"),
        )
        .await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(handler.calls.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn missing_or_wrong_method_returns_expected_status() {
        let adapter = Arc::new(AxumWebhookAdapter::default());
        let endpoint = endpoint("POST");
        adapter
            .register_endpoint(
                &endpoint,
                Arc::new(TestHandler { calls: AtomicUsize::new(0) }),
            )
            .unwrap();
        let state = State(WebhookRouteState { adapter });

        let wrong_method = handle_webhook_request(
            state.clone(),
            webhook_request("GET", "/webhook/updates"),
        )
        .await;
        let missing =
            handle_webhook_request(state, webhook_request("POST", "/webhook/missing"))
                .await;

        assert_eq!(wrong_method, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(missing, StatusCode::NOT_FOUND);
    }
}
