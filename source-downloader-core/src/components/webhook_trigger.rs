use crate::components::holding_task_trigger::HoldingTaskTrigger;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ProcessTask, SdComponent,
    SdComponentMetadata, Stateful, Trigger,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;

pub struct WebhookTriggerSupplier;
pub const SUPPLIER: WebhookTriggerSupplier = WebhookTriggerSupplier;

impl ComponentSupplier for WebhookTriggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trigger("webhook".to_owned())]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError>
    {
        let path = props
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| ComponentError::from("Missing 'path' property"))?;
        let method = props.get("method").and_then(Value::as_str).unwrap_or("GET");
        Ok(Arc::new(WebhookTrigger::new(path, method)))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

/// Integration seam for the HTTP application hosting webhook endpoints.
///
/// The core component does not own an HTTP server. Applications can provide an
/// adapter and construct a trigger with [`WebhookTrigger::with_adapter`].
pub trait WebhookAdapter: Send + Sync {
    fn register_endpoint(&self, path: &str, method: &str) -> Result<(), String>;
    fn unregister_endpoint(&self, path: &str, method: &str) -> Result<(), String>;
}

#[derive(source_downloader_sdk::SdComponent)]
#[component(Trigger, Stateful)]
pub struct WebhookTrigger {
    path: String,
    method: String,
    holding: HoldingTaskTrigger,
    adapter: Option<Arc<dyn WebhookAdapter>>,
    running: std::sync::atomic::AtomicBool,
}

impl WebhookTrigger {
    pub fn new(path: impl Into<String>, method: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            method: method.into(),
            holding: HoldingTaskTrigger::new(),
            adapter: None,
            running: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn with_adapter(
        path: impl Into<String>,
        method: impl Into<String>,
        adapter: Arc<dyn WebhookAdapter>,
    ) -> Self {
        let mut trigger = Self::new(path, method);
        trigger.adapter = Some(adapter);
        trigger
    }

    pub fn endpoint(&self) -> (&str, &str) {
        (&self.path, &self.method)
    }

    /// Executes the registered tasks for one incoming webhook request.
    pub fn handle_request(&self) {
        let tasks = self.holding.tasks();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(run_webhook_tasks(tasks));
        } else {
            tracing::warn!(path = %self.path, "Webhook request received without a Tokio runtime");
        }
    }
}

async fn run_webhook_tasks(tasks: Vec<Arc<dyn ProcessTask>>) {
    for task in tasks {
        tokio::spawn(async move {
            if let Err(error) = task.run().await {
                tracing::error!(task = %task.name(), error = %error, "Task processing failed");
            }
        });
    }
}

impl Debug for WebhookTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookTrigger")
            .field("path", &self.path)
            .field("method", &self.method)
            .field("task_count", &self.holding.tasks().len())
            .field("running", &self.running.load(std::sync::atomic::Ordering::Relaxed))
            .finish()
    }
}

impl Display for WebhookTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "webhook:{} {}", self.method, self.path)
    }
}

impl Stateful for WebhookTrigger {
    fn get_state_detail(&self) -> Option<Map<String, Value>> {
        let mut state = self.holding.state_detail();
        state.insert(
            String::from("running"),
            Value::Bool(self.running.load(std::sync::atomic::Ordering::Relaxed)),
        );
        Some(state)
    }
}

impl Trigger for WebhookTrigger {
    fn start(&self) {
        if self.running.swap(true, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        if let Some(adapter) = &self.adapter {
            let endpoint = format!("/webhook/{}", self.path);
            if let Err(error) = adapter.register_endpoint(&endpoint, &self.method) {
                self.running.store(false, std::sync::atomic::Ordering::Release);
                tracing::error!(
                    path = %endpoint,
                    method = %self.method,
                    %error,
                    "Failed to register webhook endpoint"
                );
                return;
            }
        } else {
            tracing::warn!(
                path = %self.path,
                method = %self.method,
                "Webhook trigger has no HTTP adapter"
            );
        }
    }

    fn stop(&self) {
        if !self.running.swap(false, std::sync::atomic::Ordering::AcqRel) {
            return;
        }
        if let Some(adapter) = &self.adapter {
            let endpoint = format!("/webhook/{}", self.path);
            if let Err(error) = adapter.unregister_endpoint(&endpoint, &self.method) {
                tracing::error!(
                    path = %endpoint,
                    method = %self.method,
                    %error,
                    "Failed to unregister webhook endpoint"
                );
            }
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
    use parking_lot::Mutex;

    #[derive(Default)]
    struct RecordingAdapter {
        registered: Mutex<Vec<(String, String)>>,
        unregistered: Mutex<Vec<(String, String)>>,
    }

    impl WebhookAdapter for RecordingAdapter {
        fn register_endpoint(&self, path: &str, method: &str) -> Result<(), String> {
            self.registered.lock().push((path.to_owned(), method.to_owned()));
            Ok(())
        }

        fn unregister_endpoint(&self, path: &str, method: &str) -> Result<(), String> {
            self.unregistered.lock().push((path.to_owned(), method.to_owned()));
            Ok(())
        }
    }

    #[test]
    fn start_and_stop_register_the_configured_endpoint() {
        let adapter = Arc::new(RecordingAdapter::default());
        let trigger = WebhookTrigger::with_adapter("updates", "POST", adapter.clone());

        assert_eq!(trigger.endpoint(), ("updates", "POST"));
        trigger.start();
        assert_eq!(
            adapter.registered.lock().as_slice(),
            &[("/webhook/updates".to_owned(), String::from("POST"))]
        );
        assert_eq!(
            trigger.get_state_detail().unwrap().get("running"),
            Some(&Value::Bool(true))
        );

        trigger.stop();
        assert_eq!(
            adapter.unregistered.lock().as_slice(),
            &[("/webhook/updates".to_owned(), String::from("POST"))]
        );
        assert_eq!(
            trigger.get_state_detail().unwrap().get("running"),
            Some(&Value::Bool(false))
        );
    }
}
