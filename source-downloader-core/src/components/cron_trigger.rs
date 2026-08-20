use crate::components::holding_task_trigger::HoldingTaskTrigger;
use serde::Deserialize;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ProcessTask, SdComponent,
    SdComponentMetadata, Stateful, Trigger, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

pub struct CronTriggerSupplier;
pub const SUPPLIER: CronTriggerSupplier = CronTriggerSupplier;

#[derive(Deserialize)]
struct CronTriggerConfig {
    expression: String,
}

impl ComponentSupplier for CronTriggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trigger("cron".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<CronTriggerConfig>(props)?;
        validate_cron_expression(&config.expression).map_err(|error| {
            ComponentError::new(format!("Invalid configuration at 'expression': {error}"))
        })?;
        Ok(Arc::new(CronTrigger::new(config.expression)))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Runs processing tasks according to a cron expression."
                .to_owned(),
            #[rustfmt::skip]
            props_json_schema: Some(json!({
                "type":"object",
                "properties":{
                    "expression":{"type":"string"}
                },
                "required":["expression"]
            })),
            props_ui_schema: None,
            #[rustfmt::skip]
            state_json_schema: Some(json!({
                "type":"object",
                "properties":{
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

#[derive(source_downloader_sdk::SdComponent)]
#[component(Trigger, Stateful)]
pub struct CronTrigger {
    expression: String,
    holding: HoldingTaskTrigger,
    shutdown_sender: Mutex<Option<oneshot::Sender<()>>>,
}

impl CronTrigger {
    fn new(expression: String) -> Self {
        Self {
            expression,
            holding: HoldingTaskTrigger::new(),
            shutdown_sender: Mutex::new(None),
        }
    }
}

impl Debug for CronTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CronTrigger")
            .field("expression", &self.expression)
            .field("task_count", &self.holding.tasks().len())
            .field(
                "running",
                &self.shutdown_sender.lock().is_ok_and(|guard| guard.is_some()),
            )
            .finish()
    }
}

impl Display for CronTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("cron")
    }
}

impl Stateful for CronTrigger {
    fn get_state_detail(&self) -> Option<Map<String, Value>> {
        Some(self.holding.state_detail())
    }
}

impl Trigger for CronTrigger {
    fn start(&self) {
        let Ok(mut shutdown_sender) = self.shutdown_sender.lock() else {
            return;
        };
        let task_groups = Arc::new(group_tasks(self.holding.tasks()));
        let expression = self.expression.clone();
        let (sender, receiver) = oneshot::channel();

        drop(tokio::spawn(async move {
            if let Err(error) = run_scheduler(expression, task_groups, receiver).await {
                tracing::error!(error = %error, "Cron scheduler failed");
            }
        }));
        *shutdown_sender = Some(sender);
    }

    fn stop(&self) {
        let Ok(mut shutdown_sender) = self.shutdown_sender.lock() else {
            return;
        };
        if let Some(sender) = shutdown_sender.take() {
            let _ = sender.send(());
            tracing::info!("Cron trigger stopped");
        }
    }

    fn add_task(&self, task: Arc<dyn ProcessTask>) {
        self.holding.add_task(task);
    }

    fn remove_task(&self, task: Arc<dyn ProcessTask>) {
        self.holding.remove_task(&task);
    }
}

impl Drop for CronTrigger {
    fn drop(&mut self) {
        self.stop();
    }
}
type TaskGroup = (Option<String>, Vec<Arc<dyn ProcessTask>>);
type TaskGroups = Vec<Vec<Arc<dyn ProcessTask>>>;

fn group_tasks(tasks: Vec<Arc<dyn ProcessTask>>) -> TaskGroups {
    let mut groups: Vec<TaskGroup> = Vec::new();
    for task in tasks {
        let group = task.group();
        if let Some((_, grouped)) = groups.iter_mut().find(|(known, _)| known == &group) {
            grouped.push(task);
        } else {
            groups.push((group, vec![task]));
        }
    }
    groups.into_iter().map(|(_, tasks)| tasks).collect()
}

async fn run_scheduler(
    expression: String,
    task_groups: Arc<TaskGroups>,
    shutdown_receiver: oneshot::Receiver<()>,
) -> Result<(), JobSchedulerError> {
    let mut scheduler = JobScheduler::new().await?;
    let job = Job::new_async(expression.clone(), move |_uuid, _scheduler| {
        let task_groups = Arc::clone(&task_groups);
        Box::pin(async move {
            for group in task_groups.iter() {
                for task in group {
                    if let Err(error) = task.run().await {
                        tracing::error!(
                            task = %task.name(),
                            error = %error,
                            "Task processing failed"
                        );
                    }
                }
            }
        })
    })?;
    scheduler.add(job).await?;
    scheduler.start().await?;
    tracing::info!(expression = %expression, "Cron trigger started");
    let _ = shutdown_receiver.await;
    scheduler.shutdown().await
}

fn validate_cron_expression(expression: &str) -> Result<(), String> {
    if expression.split_whitespace().count() != 6 {
        return Err(String::from("Cron expression must contain six fields"));
    }
    Job::new(expression, |_uuid, _scheduler| {})
        .map(|_| ())
        .map_err(|error| format!("Invalid cron expression: {error}"))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cron_expression_requires_six_fields() {
        let error = validate_cron_expression("* * * * *").unwrap_err();

        assert_eq!(error, "Cron expression must contain six fields");
    }
}
