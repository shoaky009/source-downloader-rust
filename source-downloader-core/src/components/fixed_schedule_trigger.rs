use crate::components::holding_task_trigger::state_detail_for_tasks;
use iso8601_duration::Duration as Iso8601Duration;
use parking_lot::Mutex;
use serde::Deserialize;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ProcessTask, SdComponent,
    SdComponentMetadata, Stateful, TaskRegistry, Trigger, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::AbortHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info};
type TaskGroups = Vec<(Option<String>, Vec<Arc<dyn ProcessTask>>)>;

pub struct FixedScheduleTriggerSupplier;
pub const SUPPLIER: FixedScheduleTriggerSupplier = FixedScheduleTriggerSupplier {};

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct FixedScheduleTriggerConfig {
    interval: String,
    #[serde(default)]
    on_start_run_tasks: bool,
}

impl ComponentSupplier for FixedScheduleTriggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trigger("fixed".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<FixedScheduleTriggerConfig>(props)?;
        let interval = parse_duration(&config.interval).map_err(|error| {
            ComponentError::new(format!("Invalid configuration at 'interval': {error}"))
        })?;

        Ok(Arc::new(FixedScheduleTrigger::new(interval, config.on_start_run_tasks)))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Runs processing tasks at a fixed interval.".to_owned(),
            props_json_schema: Some(
                json!({"type":"object","properties":{"interval":{"type":"string"},"on-start-run-tasks":{"type":"boolean","default":false}},"required":["interval"]}),
            ),
            props_ui_schema: None,
            state_json_schema: Some(
                json!({"type":"object","properties":{"tasks":{"type":"object","additionalProperties":{"type":"array","items":{"type":"object","properties":{"processName":{"type":"string"}},"required":["processName"]}}}}}),
            ),
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let duration = Iso8601Duration::parse(value)
        .map_err(|error| format!("Invalid ISO 8601 duration '{value}': {error:?}"))?;
    duration
        .to_std()
        .ok_or_else(|| format!("ISO 8601 duration '{value}' contains years or months"))
}

#[derive(SdComponent)]
#[component(Trigger, Stateful)]
struct FixedScheduleTrigger {
    interval: Duration,
    on_start_run_tasks: bool,
    task_registry: TaskRegistry,
    worker_handle: Mutex<Option<AbortHandle>>,
}

impl FixedScheduleTrigger {
    pub fn new(interval: Duration, on_start_run_tasks: bool) -> Self {
        Self {
            interval,
            on_start_run_tasks,
            task_registry: TaskRegistry::new(),
            worker_handle: Mutex::new(None),
        }
    }

    async fn run_tasks_once(tasks: Vec<Arc<dyn ProcessTask>>) {
        let mut groups: TaskGroups = Vec::new();
        for task in tasks {
            let group = task.group();
            if let Some((_, grouped)) =
                groups.iter_mut().find(|(known, _)| known == &group)
            {
                grouped.push(task);
            } else {
                groups.push((group, vec![task]));
            }
        }

        futures_util::future::join_all(groups.into_iter().map(|(_, tasks)| async move {
            for task in tasks {
                let result = task.run().await;
                if let Err(error) = &result {
                    tracing::error!(task = %task.name(), error = %error, "Task processing failed");
                } else {
                    debug!("Task {} finished successfully", task.name());
                }
            }
        }))
        .await;
    }
}

impl Stateful for FixedScheduleTrigger {
    fn get_state_detail(&self) -> Option<Map<String, Value>> {
        Some(state_detail_for_tasks(&self.task_registry.tasks.read()))
    }
}

impl Trigger for FixedScheduleTrigger {
    fn start(&self) {
        let mut handle_lock = self.worker_handle.lock();

        let tasks = self.task_registry.tasks.clone();
        let duration = self.interval;
        let run_on_start = self.on_start_run_tasks;

        let join_handle = tokio::spawn(async move {
            let mut interval_timer = tokio::time::interval(duration);
            interval_timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
            if !run_on_start {
                interval_timer.tick().await;
            }

            loop {
                interval_timer.tick().await;
                tokio::spawn(Self::run_tasks_once(tasks.read().clone()));
            }
        });

        *handle_lock = Some(join_handle.abort_handle());

        info!(
            "Trigger started, interval={} on_start_run_tasks={}",
            humantime::format_duration(duration).to_string(),
            run_on_start
        );
    }

    fn stop(&self) {
        let mut handle_lock = self.worker_handle.lock();
        if let Some(handle) = handle_lock.take() {
            handle.abort();
            info!("Trigger stopped, interval: {}s", self.interval.as_secs(),);
        }
    }

    fn add_task(&self, task: Arc<dyn ProcessTask>) {
        let mut tasks = self.task_registry.tasks.write();
        if !tasks.iter().any(|known| Arc::ptr_eq(known, &task)) {
            tasks.push(task);
        }
        debug!("Current task count: {}", tasks.len());
    }

    fn remove_task(&self, task: Arc<dyn ProcessTask>) {
        self.task_registry.remove(task);
        debug!("Current task count: {}", self.task_registry.tasks.read().len());
    }
}

impl Debug for FixedScheduleTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixedScheduleTrigger")
            .field("interval", &self.interval)
            .field("on_start_run_tasks", &self.on_start_run_tasks)
            .field("tasks", &self.task_registry.tasks.read().len())
            .field("worker_handle", &self.worker_handle.lock().is_some())
            .finish()
    }
}

impl Display for FixedScheduleTrigger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "fixed")
    }
}

impl Drop for FixedScheduleTrigger {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

    fn create_counting_task(counter: Arc<AtomicUsize>) -> Arc<dyn ProcessTask> {
        Arc::new(TestTask { counter, run_sender: None })
    }

    fn create_recording_task(
        counter: Arc<AtomicUsize>,
    ) -> (Arc<dyn ProcessTask>, UnboundedReceiver<()>) {
        let (run_sender, run_receiver) = unbounded_channel();
        (Arc::new(TestTask { counter, run_sender: Some(run_sender) }), run_receiver)
    }

    async fn expect_next_run(run_receiver: &mut UnboundedReceiver<()>) {
        tokio::time::timeout(Duration::from_secs(1), run_receiver.recv())
            .await
            .expect("task was not scheduled")
            .expect("task stopped reporting runs");
    }

    struct TestTask {
        counter: Arc<AtomicUsize>,
        run_sender: Option<UnboundedSender<()>>,
    }
    #[async_trait]
    impl ProcessTask for TestTask {
        async fn run(&self) -> Result<(), String> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            if let Some(run_sender) = &self.run_sender {
                let _ = run_sender.send(());
            }
            Ok(())
        }

        fn name(&self) -> &str {
            "TestTask"
        }

        fn group(&self) -> Option<String> {
            None
        }
    }

    struct GroupedTask {
        group: &'static str,
        group_active: Arc<AtomicUsize>,
        group_max_active: Arc<AtomicUsize>,
        global_active: Arc<AtomicUsize>,
        global_max_active: Arc<AtomicUsize>,
        barrier: Arc<tokio::sync::Barrier>,
    }

    #[async_trait]
    impl ProcessTask for GroupedTask {
        async fn run(&self) -> Result<(), String> {
            let group_active = self.group_active.fetch_add(1, Ordering::SeqCst) + 1;
            self.group_max_active.fetch_max(group_active, Ordering::SeqCst);
            let global_active = self.global_active.fetch_add(1, Ordering::SeqCst) + 1;
            self.global_max_active.fetch_max(global_active, Ordering::SeqCst);
            self.barrier.wait().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
            self.global_active.fetch_sub(1, Ordering::SeqCst);
            self.group_active.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }

        fn name(&self) -> &str {
            self.group
        }

        fn group(&self) -> Option<String> {
            Some(self.group.to_owned())
        }
    }

    #[tokio::test]
    async fn tasks_run_sequentially_within_groups_and_groups_run_concurrently() {
        let global_active = Arc::new(AtomicUsize::new(0));
        let global_max_active = Arc::new(AtomicUsize::new(0));
        let first_group_active = Arc::new(AtomicUsize::new(0));
        let first_group_max_active = Arc::new(AtomicUsize::new(0));
        let second_group_active = Arc::new(AtomicUsize::new(0));
        let second_group_max_active = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let make_task = |group, group_active, group_max_active| {
            Arc::new(GroupedTask {
                group,
                group_active,
                group_max_active,
                global_active: global_active.clone(),
                global_max_active: global_max_active.clone(),
                barrier: barrier.clone(),
            }) as Arc<dyn ProcessTask>
        };
        let tasks = vec![
            make_task(
                "first",
                first_group_active.clone(),
                first_group_max_active.clone(),
            ),
            make_task(
                "second",
                second_group_active.clone(),
                second_group_max_active.clone(),
            ),
            make_task("first", first_group_active, first_group_max_active.clone()),
            make_task("second", second_group_active, second_group_max_active.clone()),
        ];

        tokio::time::timeout(
            Duration::from_secs(1),
            FixedScheduleTrigger::run_tasks_once(tasks),
        )
        .await
        .expect("task groups did not make progress");

        assert_eq!(first_group_max_active.load(Ordering::SeqCst), 1);
        assert_eq!(second_group_max_active.load(Ordering::SeqCst), 1);
        assert_eq!(global_max_active.load(Ordering::SeqCst), 2);
    }
    #[test]
    fn test_add_remove_task() {
        // 测试基本的增删逻辑，不涉及异步运行
        let trigger = FixedScheduleTrigger::new(Duration::from_secs(1), false);
        let counter = Arc::new(AtomicUsize::new(0));
        let task = create_counting_task(counter);

        // 添加
        trigger.add_task(task.clone());
        {
            let tasks = trigger.task_registry.tasks.read();
            assert_eq!(tasks.len(), 1);
        }

        // 删除
        trigger.remove_task(task.clone());
        {
            let tasks = trigger.task_registry.tasks.read();
            assert_eq!(tasks.len(), 0);
        }
    }

    #[tokio::test]
    async fn test_run_on_start() {
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(100), true);
        let counter = Arc::new(AtomicUsize::new(0));
        let (task, mut run_receiver) = create_recording_task(counter.clone());

        trigger.add_task(task);
        trigger.start();

        expect_next_run(&mut run_receiver).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        trigger.stop();
    }

    #[tokio::test(start_paused = true)]
    async fn test_wait_on_start() {
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(50), false);
        let counter = Arc::new(AtomicUsize::new(0));
        let (task, mut run_receiver) = create_recording_task(counter.clone());

        trigger.add_task(task);
        trigger.start();
        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_millis(49)).await;
        assert!(run_receiver.try_recv().is_err(), "Should NOT run immediately");

        tokio::time::advance(Duration::from_millis(1)).await;
        expect_next_run(&mut run_receiver).await;
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        trigger.stop();
    }

    #[tokio::test(start_paused = true)]
    async fn test_scheduled_execution() {
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(20), true);
        let counter = Arc::new(AtomicUsize::new(0));
        let (task, mut run_receiver) = create_recording_task(counter.clone());

        trigger.add_task(task);
        trigger.start();
        expect_next_run(&mut run_receiver).await;

        for _ in 0..5 {
            tokio::time::advance(Duration::from_millis(20)).await;
            expect_next_run(&mut run_receiver).await;
        }

        assert_eq!(counter.load(Ordering::SeqCst), 6);
        trigger.stop();
    }

    #[tokio::test(start_paused = true)]
    async fn test_stop_trigger() {
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(10), true);
        let counter = Arc::new(AtomicUsize::new(0));
        let (task, mut run_receiver) = create_recording_task(counter.clone());

        trigger.add_task(task);
        trigger.start();
        expect_next_run(&mut run_receiver).await;

        tokio::time::advance(Duration::from_millis(10)).await;
        expect_next_run(&mut run_receiver).await;
        trigger.stop();

        let count_before_advance = counter.load(Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        assert!(run_receiver.try_recv().is_err());
        assert_eq!(counter.load(Ordering::SeqCst), count_before_advance);
    }

    #[tokio::test(start_paused = true)]
    async fn test_dynamic_add_task() {
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(20), true);
        let counter1 = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::new(AtomicUsize::new(0));
        let (task1, mut run_receiver1) = create_recording_task(counter1.clone());
        let (task2, mut run_receiver2) = create_recording_task(counter2.clone());

        trigger.start();
        tokio::task::yield_now().await;

        trigger.add_task(task1);
        tokio::time::advance(Duration::from_millis(20)).await;
        expect_next_run(&mut run_receiver1).await;

        trigger.add_task(task2);
        tokio::time::advance(Duration::from_millis(20)).await;
        expect_next_run(&mut run_receiver1).await;
        expect_next_run(&mut run_receiver2).await;

        trigger.stop();
        assert_eq!(counter1.load(Ordering::SeqCst), 2);
        assert_eq!(counter2.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn parse_duration_accepts_iso_8601_values() {
        assert_eq!(parse_duration("PT1.5S").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_duration("P1DT2H3M4S").unwrap(), Duration::from_secs(93_784));
    }
}
