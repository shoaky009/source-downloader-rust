use crate::components::holding_task_trigger::HoldingTaskTrigger;
use parking_lot::Mutex;
use serde::Deserialize;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ProcessTask, SdComponent,
    SdComponentMetadata, Stateful, Trigger, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, Weak};

pub struct WebhookTriggerSupplier;
pub const SUPPLIER: WebhookTriggerSupplier = WebhookTriggerSupplier;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct WebhookTriggerConfig {
    path: String,
    #[serde(default = "default_webhook_method")]
    method: String,
}

fn default_webhook_method() -> String {
    "GET".to_owned()
}

impl ComponentSupplier for WebhookTriggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trigger("webhook".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<WebhookTriggerConfig>(props)?;
        WebhookMethod::parse(&config.method).map_err(|error| {
            ComponentError::new(format!("Invalid configuration at 'method': {error}"))
        })?;
        let trigger =
            WebhookTrigger::new(config.path, config.method).map_err(|error| {
                ComponentError::new(format!("Invalid configuration at 'path': {error}"))
            })?;
        Ok(Arc::new(trigger))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Triggers processing tasks through an HTTP webhook.".to_owned(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "path":{"type":"string"},
                    "method":{"type":"string","default":"GET"}
                },
                "required":["path"]
            })),
            props_ui_schema: None,
            #[rustfmt::skip]
            state_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "running":{"type":"boolean"},
                    "tasks":{
                        "type":"object",
                        "additionalProperties":{
                            "type":"array",
                            "items":{
                                "type":"object",
                                "properties":{
                                    "processName":{"type":"string"}
                                },
                                "required":["processName"]
                            }
                        }
                    }
                }
            })),
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WebhookMethod(String);

impl WebhookMethod {
    pub fn parse(method: &str) -> Result<Self, String> {
        if method.is_empty() || !method.bytes().all(is_http_token_byte) {
            return Err(format!("Invalid webhook method '{method}'"));
        }
        Ok(Self(method.to_ascii_uppercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_http_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WebhookEndpoint {
    path: String,
    method: WebhookMethod,
}

impl WebhookEndpoint {
    pub fn new(path: impl Into<String>, method: impl AsRef<str>) -> Result<Self, String> {
        let path = path.into();
        if path.is_empty() {
            return Err("Webhook path cannot be empty".to_owned());
        }
        if path.starts_with('/') {
            return Err(format!("Webhook path must be relative: {path}"));
        }
        if path.ends_with('/') {
            return Err(format!("Webhook path cannot end with '/': {path}"));
        }
        if path.contains('?') {
            return Err(format!("Webhook path cannot contain '?': {path}"));
        }
        if path.contains('#') {
            return Err(format!("Webhook path cannot contain '#': {path}"));
        }
        if path
            .chars()
            .any(|character| character.is_ascii_control() || character.is_whitespace())
        {
            return Err(format!("Webhook path contains invalid whitespace: {path}"));
        }
        Ok(Self { path, method: WebhookMethod::parse(method.as_ref())? })
    }

    pub fn from_route_path(path: &str, method: &str) -> Result<Option<Self>, String> {
        let Some(path) = path.strip_prefix("/webhook/") else {
            return Ok(None);
        };
        if path.is_empty() {
            return Ok(None);
        }
        Self::new(path, method).map(Some)
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn method(&self) -> &WebhookMethod {
        &self.method
    }

    pub fn route_path(&self) -> String {
        format!("/webhook/{}", self.path)
    }
}

pub trait WebhookRequestHandler: Send + Sync {
    fn handle_request(&self) -> bool;
}

pub trait WebhookAdapter: Send + Sync {
    fn register_endpoint(
        &self,
        endpoint: &WebhookEndpoint,
        handler: Arc<dyn WebhookRequestHandler>,
    ) -> Result<(), String>;
    fn unregister_endpoint(&self, endpoint: &WebhookEndpoint) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WebhookState {
    Stopped,
    Running,
}

struct WebhookLifecycle {
    adapter: Option<Arc<dyn WebhookAdapter>>,
    registered_adapter: Option<Arc<dyn WebhookAdapter>>,
    state: WebhookState,
}

struct WebhookHandler {
    holding: HoldingTaskTrigger,
    lifecycle: Weak<Mutex<WebhookLifecycle>>,
}

#[derive(source_downloader_sdk::SdComponent)]
#[component(Trigger, Stateful)]
pub struct WebhookTrigger {
    endpoint: WebhookEndpoint,
    holding: HoldingTaskTrigger,
    lifecycle: Arc<Mutex<WebhookLifecycle>>,
    handler: Arc<WebhookHandler>,
}

impl WebhookTrigger {
    pub fn new(
        path: impl Into<String>,
        method: impl Into<String>,
    ) -> Result<Self, String> {
        let endpoint = WebhookEndpoint::new(path, method.into())?;
        Ok(Self::from_endpoint(endpoint))
    }

    fn from_endpoint(endpoint: WebhookEndpoint) -> Self {
        let holding = HoldingTaskTrigger::new();
        let lifecycle = Arc::new(Mutex::new(WebhookLifecycle {
            adapter: None,
            registered_adapter: None,
            state: WebhookState::Stopped,
        }));
        let handler = Arc::new(WebhookHandler {
            holding: holding.clone(),
            lifecycle: Arc::downgrade(&lifecycle),
        });
        Self { endpoint, holding, lifecycle, handler }
    }

    pub fn with_adapter(
        path: impl Into<String>,
        method: impl Into<String>,
        adapter: Arc<dyn WebhookAdapter>,
    ) -> Result<Self, String> {
        let trigger = Self::new(path, method)?;
        trigger.set_adapter(adapter)?;
        Ok(trigger)
    }

    pub fn set_adapter(&self, adapter: Arc<dyn WebhookAdapter>) -> Result<(), String> {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.state == WebhookState::Running {
            return Err(format!(
                "Cannot replace adapter while webhook endpoint {} is running",
                self.endpoint.route_path()
            ));
        }
        if lifecycle.registered_adapter.is_some() {
            return Err(format!(
                "Cannot replace adapter while webhook endpoint {} is still registered",
                self.endpoint.route_path()
            ));
        }
        lifecycle.adapter = Some(adapter);
        Ok(())
    }

    pub fn endpoint(&self) -> (&str, &str) {
        (self.endpoint.path(), self.endpoint.method().as_str())
    }

    pub fn endpoint_spec(&self) -> &WebhookEndpoint {
        &self.endpoint
    }

    fn is_running(&self) -> bool {
        self.lifecycle.lock().state == WebhookState::Running
    }

    pub fn start_checked(&self) -> Result<(), String> {
        let mut lifecycle = self.lifecycle.lock();
        if lifecycle.state == WebhookState::Running {
            return Ok(());
        }
        if lifecycle.registered_adapter.is_some() {
            return Err(format!(
                "Cannot start webhook endpoint {} while its previous registration is active",
                self.endpoint.route_path()
            ));
        }
        let Some(adapter) = lifecycle.adapter.clone() else {
            tracing::warn!(
                path = %self.endpoint.route_path(),
                method = %self.endpoint.method().as_str(),
                "Webhook trigger has no HTTP adapter"
            );
            return Ok(());
        };
        adapter.register_endpoint(&self.endpoint, self.handler.clone())?;
        lifecycle.registered_adapter = Some(adapter);
        lifecycle.state = WebhookState::Running;
        Ok(())
    }

    pub fn stop_checked(&self) -> Result<(), String> {
        let mut lifecycle = self.lifecycle.lock();
        let Some(adapter) = lifecycle.registered_adapter.take() else {
            lifecycle.state = WebhookState::Stopped;
            return Ok(());
        };
        let result = adapter.unregister_endpoint(&self.endpoint);
        if result.is_err() {
            lifecycle.registered_adapter = Some(adapter);
        }
        lifecycle.state = WebhookState::Stopped;
        result
    }

    /// Executes the registered tasks for one incoming webhook request.
    pub fn handle_request(&self) {
        let _ = self.handler.handle_request();
    }
}

impl WebhookRequestHandler for WebhookHandler {
    fn handle_request(&self) -> bool {
        let Some(lifecycle) = self.lifecycle.upgrade() else {
            return false;
        };
        let running = lifecycle.lock().state == WebhookState::Running;
        if !running {
            return false;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            tracing::warn!("Webhook request received without a Tokio runtime");
            return false;
        };
        for task in self.holding.tasks() {
            handle.spawn(async move {
                if let Err(error) = task.run().await {
                    tracing::error!(task = %task.name(), error = %error, "Task processing failed");
                }
            });
        }
        true
    }
}

impl Debug for WebhookTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookTrigger")
            .field("path", &self.endpoint.path())
            .field("method", &self.endpoint.method().as_str())
            .field("task_count", &self.holding.tasks().len())
            .field("running", &self.is_running())
            .finish()
    }
}

impl Display for WebhookTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "webhook:{} {}", self.endpoint.method().as_str(), self.endpoint.path())
    }
}

impl Stateful for WebhookTrigger {
    fn get_state_detail(&self) -> Option<Map<String, Value>> {
        let mut state = self.holding.state_detail();
        state.insert(String::from("running"), Value::Bool(self.is_running()));
        Some(state)
    }
}

impl Trigger for WebhookTrigger {
    fn start(&self) {
        if let Err(error) = self.start_checked() {
            tracing::error!(
                path = %self.endpoint.route_path(),
                method = %self.endpoint.method().as_str(),
                %error,
                "Failed to register webhook endpoint"
            );
        }
    }

    fn stop(&self) {
        if let Err(error) = self.stop_checked() {
            tracing::error!(
                path = %self.endpoint.route_path(),
                method = %self.endpoint.method().as_str(),
                %error,
                "Failed to unregister webhook endpoint"
            );
        }
    }

    fn add_task(&self, task: Arc<dyn ProcessTask>) {
        self.holding.add_task(task);
    }

    fn remove_task(&self, task: Arc<dyn ProcessTask>) {
        self.holding.remove_task(&task);
    }
}

impl Drop for WebhookTrigger {
    fn drop(&mut self) {
        self.stop();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[derive(Default)]
    struct RecordingAdapter {
        registered: Mutex<Vec<WebhookEndpoint>>,
        unregistered: Mutex<Vec<WebhookEndpoint>>,
        active: Mutex<HashSet<WebhookEndpoint>>,
        handlers: Mutex<HashMap<WebhookEndpoint, Arc<dyn WebhookRequestHandler>>>,
        fail_registration: AtomicBool,
        fail_unregistration: AtomicBool,
    }

    impl WebhookAdapter for RecordingAdapter {
        fn register_endpoint(
            &self,
            endpoint: &WebhookEndpoint,
            handler: Arc<dyn WebhookRequestHandler>,
        ) -> Result<(), String> {
            if self.fail_registration.load(Ordering::Acquire) {
                return Err("registration failed".to_owned());
            }
            if !self.active.lock().insert(endpoint.clone()) {
                return Err("duplicate endpoint".to_owned());
            }
            self.registered.lock().push(endpoint.clone());
            self.handlers.lock().insert(endpoint.clone(), handler);
            Ok(())
        }

        fn unregister_endpoint(&self, endpoint: &WebhookEndpoint) -> Result<(), String> {
            if self.fail_unregistration.load(Ordering::Acquire) {
                return Err("unregistration failed".to_owned());
            }
            self.active.lock().remove(endpoint);
            self.unregistered.lock().push(endpoint.clone());
            self.handlers.lock().remove(endpoint);
            Ok(())
        }
    }

    fn endpoint() -> WebhookEndpoint {
        WebhookEndpoint::new("updates", "POST").unwrap()
    }

    fn trigger(adapter: Arc<RecordingAdapter>) -> WebhookTrigger {
        WebhookTrigger::with_adapter("updates", "POST", adapter).unwrap()
    }

    #[test]
    fn start_without_adapter_keeps_trigger_stopped() {
        let trigger = WebhookTrigger::new("updates", "POST").unwrap();

        trigger.start();

        assert!(!trigger.is_running());
    }

    #[test]
    fn registration_failure_keeps_trigger_stopped() {
        let adapter = Arc::new(RecordingAdapter::default());
        adapter.fail_registration.store(true, Ordering::Release);
        let trigger = trigger(adapter.clone());

        assert!(trigger.start_checked().is_err());
        assert!(!trigger.is_running());
        assert!(adapter.active.lock().is_empty());
    }

    #[test]
    fn successful_registration_marks_trigger_running() {
        let adapter = Arc::new(RecordingAdapter::default());
        let trigger = trigger(adapter.clone());

        trigger.start_checked().unwrap();

        assert!(trigger.is_running());
        assert!(adapter.active.lock().contains(&endpoint()));
    }

    #[test]
    fn repeated_start_is_idempotent() {
        let adapter = Arc::new(RecordingAdapter::default());
        let trigger = trigger(adapter.clone());

        trigger.start_checked().unwrap();
        trigger.start_checked().unwrap();

        assert_eq!(adapter.registered.lock().len(), 1);
    }

    #[test]
    fn repeated_stop_is_idempotent() {
        let adapter = Arc::new(RecordingAdapter::default());
        let trigger = trigger(adapter.clone());
        trigger.start_checked().unwrap();

        trigger.stop_checked().unwrap();
        trigger.stop_checked().unwrap();

        assert_eq!(adapter.unregistered.lock().len(), 1);
        assert!(!trigger.is_running());
    }

    #[test]
    fn adapter_cannot_be_replaced_while_running() {
        let first = Arc::new(RecordingAdapter::default());
        let second = Arc::new(RecordingAdapter::default());
        let trigger = trigger(first.clone());
        trigger.start_checked().unwrap();

        assert!(trigger.set_adapter(second.clone()).is_err());
        trigger.stop_checked().unwrap();
        trigger.set_adapter(second.clone()).unwrap();
        trigger.start_checked().unwrap();

        assert!(first.active.lock().is_empty());
        assert!(second.active.lock().contains(&endpoint()));
    }

    #[test]
    fn stop_removes_registered_endpoint() {
        let adapter = Arc::new(RecordingAdapter::default());
        let trigger = trigger(adapter.clone());
        trigger.start_checked().unwrap();

        trigger.stop_checked().unwrap();

        assert!(adapter.active.lock().is_empty());
    }

    #[test]
    fn endpoint_method_is_case_insensitive() {
        let endpoint = WebhookEndpoint::new("updates", "post").unwrap();

        assert_eq!(endpoint.method().as_str(), "POST");
    }

    #[test]
    fn duplicate_endpoint_is_rejected() {
        let adapter = Arc::new(RecordingAdapter::default());
        let first = trigger(adapter.clone());
        let second = trigger(adapter.clone());
        first.start_checked().unwrap();

        assert!(second.start_checked().is_err());
        assert!(!second.is_running());
    }

    #[test]
    fn unregister_missing_endpoint_is_idempotent() {
        let adapter = RecordingAdapter::default();

        adapter.unregister_endpoint(&endpoint()).unwrap();
        adapter.unregister_endpoint(&endpoint()).unwrap();
    }

    #[test]
    fn same_path_different_method_is_allowed() {
        let adapter = Arc::new(RecordingAdapter::default());
        let post =
            WebhookTrigger::with_adapter("updates", "POST", adapter.clone()).unwrap();
        let get =
            WebhookTrigger::with_adapter("updates", "GET", adapter.clone()).unwrap();

        post.start_checked().unwrap();
        get.start_checked().unwrap();

        assert_eq!(adapter.active.lock().len(), 2);
    }

    #[test]
    fn empty_path_is_rejected() {
        assert!(WebhookEndpoint::new("", "GET").is_err());
    }

    #[test]
    fn leading_slash_path_is_rejected() {
        assert!(WebhookEndpoint::new("/updates", "GET").is_err());
    }

    #[test]
    fn invalid_method_is_rejected_during_component_creation() {
        let mut props = Map::new();
        props.insert("path".to_owned(), Value::String("updates".to_owned()));
        props.insert("method".to_owned(), Value::String("not valid".to_owned()));

        let error = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props,
            )
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Invalid configuration at 'method': Invalid webhook method 'not valid'"
        );
    }

    #[test]
    fn non_string_method_is_rejected_during_component_creation() {
        let mut props = Map::new();
        props.insert("path".to_owned(), Value::String("updates".to_owned()));
        props.insert("method".to_owned(), Value::Bool(true));

        let error = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props,
            )
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Invalid configuration at 'method': invalid type: boolean `true`, expected a string"
        );
    }

    #[test]
    fn query_string_in_path_is_rejected() {
        assert!(WebhookEndpoint::new("updates?x=1", "GET").is_err());
    }

    #[test]
    fn fragment_in_path_is_rejected() {
        assert!(WebhookEndpoint::new("updates#part", "GET").is_err());
    }

    #[test]
    fn concurrent_start_calls_register_once() {
        let adapter = Arc::new(RecordingAdapter::default());
        let trigger = Arc::new(trigger(adapter.clone()));
        let barrier = Arc::new(Barrier::new(8));

        thread::scope(|scope| {
            for _ in 0..8 {
                let trigger = trigger.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    trigger.start_checked().unwrap();
                });
            }
        });

        assert_eq!(adapter.registered.lock().len(), 1);
        assert!(trigger.is_running());
    }

    #[test]
    fn concurrent_stop_calls_unregister_once() {
        let adapter = Arc::new(RecordingAdapter::default());
        let trigger = Arc::new(trigger(adapter.clone()));
        trigger.start_checked().unwrap();
        let barrier = Arc::new(Barrier::new(8));

        thread::scope(|scope| {
            for _ in 0..8 {
                let trigger = trigger.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    trigger.stop_checked().unwrap();
                });
            }
        });

        assert_eq!(adapter.unregistered.lock().len(), 1);
        assert!(!trigger.is_running());
    }

    #[test]
    fn concurrent_start_and_stop_keep_state_and_registry_consistent() {
        let adapter = Arc::new(RecordingAdapter::default());
        let trigger = Arc::new(trigger(adapter.clone()));
        let barrier = Arc::new(Barrier::new(2));

        thread::scope(|scope| {
            let start_trigger = trigger.clone();
            let start_barrier = barrier.clone();
            scope.spawn(move || {
                start_barrier.wait();
                start_trigger.start_checked().unwrap();
            });
            let stop_trigger = trigger.clone();
            let stop_barrier = barrier.clone();
            scope.spawn(move || {
                stop_barrier.wait();
                stop_trigger.stop_checked().unwrap();
            });
        });

        let active = adapter.active.lock().contains(&endpoint());
        assert_eq!(active, trigger.is_running());
    }
    #[test]
    fn unregistration_failure_keeps_trigger_stopped() {
        let adapter = Arc::new(RecordingAdapter::default());
        let trigger = trigger(adapter.clone());
        trigger.start_checked().unwrap();
        adapter.fail_unregistration.store(true, Ordering::Release);

        assert!(trigger.stop_checked().is_err());
        assert!(!trigger.is_running());
    }

    #[test]
    fn failed_unregistration_is_retryable_before_adapter_replacement() {
        let first = Arc::new(RecordingAdapter::default());
        let second = Arc::new(RecordingAdapter::default());
        let trigger = trigger(first.clone());
        trigger.start_checked().unwrap();
        first.fail_unregistration.store(true, Ordering::Release);

        assert!(trigger.stop_checked().is_err());
        assert!(trigger.set_adapter(second.clone()).is_err());

        first.fail_unregistration.store(false, Ordering::Release);
        trigger.stop_checked().unwrap();
        trigger.set_adapter(second.clone()).unwrap();
        trigger.start_checked().unwrap();

        assert!(first.active.lock().is_empty());
        assert!(second.active.lock().contains(&endpoint()));
    }

    #[test]
    fn reload_removes_old_endpoint() {
        let adapter = Arc::new(RecordingAdapter::default());
        {
            let old = trigger(adapter.clone());
            old.start_checked().unwrap();
        }

        assert!(adapter.active.lock().is_empty());
    }

    #[test]
    fn reload_registers_new_endpoint() {
        let adapter = Arc::new(RecordingAdapter::default());
        {
            let old = trigger(adapter.clone());
            old.start_checked().unwrap();
        }
        let new =
            WebhookTrigger::with_adapter("changed", "POST", adapter.clone()).unwrap();
        new.start_checked().unwrap();

        assert!(
            adapter
                .active
                .lock()
                .contains(&WebhookEndpoint::new("changed", "POST").unwrap())
        );
    }

    #[test]
    fn reload_does_not_leave_duplicate_registration() {
        let adapter = Arc::new(RecordingAdapter::default());
        {
            let old = trigger(adapter.clone());
            old.start_checked().unwrap();
        }
        let new = trigger(adapter.clone());

        new.start_checked().unwrap();

        assert_eq!(adapter.active.lock().len(), 1);
    }

    #[test]
    fn destroyed_trigger_is_not_dispatchable() {
        let adapter = Arc::new(RecordingAdapter::default());
        let old = trigger(adapter.clone());
        old.start_checked().unwrap();
        let handler = adapter.handlers.lock().get(&endpoint()).cloned().unwrap();
        adapter.fail_unregistration.store(true, Ordering::Release);

        drop(old);

        assert!(!handler.handle_request());
    }
}
