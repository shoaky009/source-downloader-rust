use crate::components::holding_task_trigger::HoldingTaskTrigger;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ProcessTask, SdComponent,
    SdComponentMetadata, Stateful, Trigger,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;
use tokio_cron_scheduler::{Job, JobScheduler, JobSchedulerError};

pub struct CronTriggerSupplier;
pub const SUPPLIER: CronTriggerSupplier = CronTriggerSupplier;

impl ComponentSupplier for CronTriggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trigger("cron".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let expression = props
            .get("expression")
            .and_then(Value::as_str)
            .ok_or_else(|| ComponentError::from("Missing 'expression' property"))?;
        validate_cron_expression(expression).map_err(ComponentError::new)?;
        Ok(Arc::new(CronTrigger::new(expression.to_owned())))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
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
