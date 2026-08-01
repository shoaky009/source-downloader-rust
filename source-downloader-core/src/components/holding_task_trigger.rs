use source_downloader_sdk::component::ProcessTask;
use source_downloader_sdk::component::TaskRegistry;
use source_downloader_sdk::serde_json::{Map, Value};
use std::sync::Arc;

/// Shared task storage and grouped execution used by holding triggers.
#[derive(Clone, Default)]
pub struct HoldingTaskTrigger {
    pub registry: TaskRegistry,
}

impl HoldingTaskTrigger {
    pub fn new() -> Self {
        Self { registry: TaskRegistry::new() }
    }

    pub fn add_task(&self, task: Arc<dyn ProcessTask>) {
        let mut tasks = self.registry.tasks.write();
        if tasks.iter().any(|known| Arc::ptr_eq(known, &task)) {
            return;
        }
        tasks.push(task);
    }

    pub fn remove_task(&self, task: &Arc<dyn ProcessTask>) {
        self.registry.tasks.write().retain(|known| !Arc::ptr_eq(known, task));
    }

    pub fn tasks(&self) -> Vec<Arc<dyn ProcessTask>> {
        self.registry.tasks.read().clone()
    }

    pub fn state_detail(&self) -> Map<String, Value> {
        state_detail_for_tasks(&self.registry.tasks.read())
    }
}

pub fn state_detail_for_tasks(tasks: &[Arc<dyn ProcessTask>]) -> Map<String, Value> {
    let mut grouped: Vec<(String, Vec<Value>)> = Vec::new();
    for task in tasks {
        let group = task.group().unwrap_or_else(|| "default".to_owned());
        if let Some((_, values)) = grouped.iter_mut().find(|(known, _)| known == &group) {
            values.push(serde_json::json!({"processName": task.name()}));
        } else {
            grouped.push((group, vec![serde_json::json!({"processName": task.name()})]));
        }
    }
    let tasks = grouped
        .into_iter()
        .map(|(group, values)| (group, Value::Array(values)))
        .collect();
    Map::from_iter([(String::from("tasks"), Value::Object(tasks))])
}

pub async fn run_grouped_tasks(tasks: Vec<Arc<dyn ProcessTask>>) {
    let mut groups: Vec<(String, Vec<Arc<dyn ProcessTask>>)> = Vec::new();
    for task in tasks {
        let group = task.group().unwrap_or_else(|| "default".to_owned());
        if let Some((_, grouped)) = groups.iter_mut().find(|(known, _)| known == &group) {
            grouped.push(task);
        } else {
            groups.push((group, vec![task]));
        }
    }

    futures_util::future::join_all(groups.into_iter().map(|(_, group)| async move {
        for task in group {
            if let Err(error) = task.run().await {
                tracing::error!(task = %task.name(), error = %error, "Task processing failed");
            }
        }
    }))
    .await;
}
#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    struct TestTask {
        name: &'static str,
        group: Option<&'static str>,
    }

    #[async_trait]
    impl ProcessTask for TestTask {
        async fn run(&self) -> Result<(), String> {
            Ok(())
        }

        fn name(&self) -> &str {
            self.name
        }

        fn group(&self) -> Option<String> {
            self.group.map(str::to_owned)
        }
    }

    #[test]
    fn state_detail_groups_tasks_and_uses_default_group() {
        let tasks: Vec<Arc<dyn ProcessTask>> = vec![
            Arc::new(TestTask { name: "first", group: None }),
            Arc::new(TestTask { name: "second", group: Some("custom") }),
        ];

        let state = state_detail_for_tasks(&tasks);
        let groups = state.get("tasks").and_then(Value::as_object).unwrap();

        assert_eq!(
            groups
                .get("default")
                .and_then(Value::as_array)
                .and_then(|tasks| tasks.first())
                .and_then(|task| task.get("processName"))
                .and_then(Value::as_str),
            Some("first")
        );
        assert_eq!(
            groups
                .get("custom")
                .and_then(Value::as_array)
                .and_then(|tasks| tasks.first())
                .and_then(|task| task.get("processName"))
                .and_then(Value::as_str),
            Some("second")
        );
    }
}
