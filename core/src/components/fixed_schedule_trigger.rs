use sdk::SdComponent;
use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ProcessTask, SdComponent,
    SdComponentMetadata, Stateful, TaskRegistry, Trigger,
};
use sdk::serde_json::{Map, Value};
use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use std::time::Duration;
use tokio::task::AbortHandle;
use tokio::time::MissedTickBehavior;
use tracing::{debug, info};

pub struct FixedScheduleTriggerSupplier;
pub const SUPPLIER: FixedScheduleTriggerSupplier = FixedScheduleTriggerSupplier {};

impl ComponentSupplier for FixedScheduleTriggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trigger("fixed".to_string())]
    }

    fn apply(&self, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let interval_str = props
            .get("interval")
            .ok_or_else(|| ComponentError::from("Missing 'interval' property"))?
            .as_str()
            .ok_or_else(|| ComponentError::from("Invalid 'interval' property"))?;
        let interval = humantime::parse_duration(interval_str)
            .map_err(|e| ComponentError::from(e.to_string() + " for 'interval' property"))?;

        let on_start_run_tasks = match props.get("on-start-run-tasks") {
            None => false,
            Some(v) => match v {
                Value::Bool(b) => *b,
                _ => return Err("on-start-run-tasks 必须是 true or false".into()),
            },
        };

        Ok(Arc::new(FixedScheduleTrigger::new(
            interval,
            on_start_run_tasks,
        )))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
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
}

impl Stateful for FixedScheduleTrigger {
    fn get_state_detail(&self) -> Option<Map<String, Value>> {
        let mut state = Map::new();
        state.insert(
            "running".to_string(),
            Value::Bool(self.worker_handle.lock().unwrap().is_some()),
        );
        Some(state)
    }
}

impl Trigger for FixedScheduleTrigger {
    fn start(&self) {
        let mut handle_lock = self.worker_handle.lock().unwrap();
        if handle_lock.is_some() {
            info!("Trigger is already running.");
            return;
        }

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
                for task in tasks.read().clone() {
                    //TODO grouping tasks, then run them in parallel await task.execute()
                    tokio::spawn(async move {
                        let result = task.run().await;
                        tracing::info!("Task {} finished with result {:?}", task.name(), result);
                    });
                }
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
        let mut handle_lock = self.worker_handle.lock().unwrap();
        if let Some(handle) = handle_lock.take() {
            handle.abort();
            info!("Trigger stopped, interval: {}s", self.interval.as_secs(),);
        }
    }

    fn add_task(&self, task: Arc<dyn ProcessTask>) {
        self.task_registry.add(task);
        debug!(
            "Current task count: {}",
            self.task_registry.tasks.read().len()
        );
    }

    fn remove_task(&self, task: Arc<dyn ProcessTask>) {
        self.task_registry.remove(task);
        debug!(
            "Current task count: {}",
            self.task_registry.tasks.read().len()
        );
    }
}

impl Debug for FixedScheduleTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FixedScheduleTrigger")
            .field("interval", &self.interval)
            .field("on_start_run_tasks", &self.on_start_run_tasks)
            .field("tasks", &self.task_registry.tasks.read().len())
            .field(
                "worker_handle",
                &self.worker_handle.lock().unwrap().is_some(),
            )
            .finish()
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

    // 辅助函数：创建一个会增加计数器的任务
    fn create_counting_task(counter: Arc<AtomicUsize>) -> Arc<dyn ProcessTask> {
        Arc::new(TestTask { counter })
        //
        // Arc::new(ProcessorTask {
        //     process_name: "TestTask".to_string(),
        //     group: None,
        //     runnable: Box::new(move || {
        //         let counter_clone = counter.clone();
        //         Box::pin(async move {
        //             counter_clone.fetch_add(1, Ordering::SeqCst);
        //             println!("Task executed");
        //         })
        //     }),
        // })
    }

    struct TestTask {
        counter: Arc<AtomicUsize>,
    }
    #[async_trait]
    impl ProcessTask for TestTask {
        async fn run(&self) -> Result<(), String> {
            self.counter.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn name(&self) -> &str {
            "TestTask"
        }

        fn group(&self) -> Option<String> {
            None
        }
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
        // 测试：on_start = true，应该立即执行一次
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(100), true);
        let counter = Arc::new(AtomicUsize::new(0));
        let task = create_counting_task(counter.clone());

        trigger.add_task(task);
        trigger.start();

        //稍微等待一下让异步任务跑起来
        tokio::time::sleep(Duration::from_millis(10)).await;

        // 即使时间没到 100ms，因为 run_on_start 是 true，应该已经执行了 1 次
        assert!(
            counter.load(Ordering::SeqCst) >= 1,
            "Should run immediately on start"
        );

        trigger.stop();
    }

    #[tokio::test]
    async fn test_wait_on_start() {
        // 测试：on_start = false，应该等待第一个间隔才执行
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(50), false);
        let counter = Arc::new(AtomicUsize::new(0));
        let task = create_counting_task(counter.clone());

        trigger.add_task(task);
        trigger.start();

        // 刚启动，应该还没执行
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(
            counter.load(Ordering::SeqCst),
            0,
            "Should NOT run immediately"
        );

        // 等待超过 50ms
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert!(
            counter.load(Ordering::SeqCst) >= 1,
            "Should run after interval"
        );

        trigger.stop();
    }

    #[tokio::test]
    async fn test_scheduled_execution() {
        // 测试：任务是否周期性执行
        // 间隔 20ms
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(20), true);
        let counter = Arc::new(AtomicUsize::new(0));
        let task = create_counting_task(counter.clone());

        trigger.add_task(task);
        trigger.start();

        // 等待 110ms，理论上应该执行 5-6 次 (0ms, 20ms, 40ms, 60ms, 80ms, 100ms)
        tokio::time::sleep(Duration::from_millis(110)).await;

        let count = counter.load(Ordering::SeqCst);
        println!("Executed count: {}", count);

        // 由于调度会有微小误差，我们验证一个合理范围
        assert!(count >= 5 && count <= 7);

        trigger.stop();
    }

    #[tokio::test]
    async fn test_stop_trigger() {
        // 测试：stop 后任务不再增加
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(10), true);
        let counter = Arc::new(AtomicUsize::new(0));
        let task = create_counting_task(counter.clone());

        trigger.add_task(task);
        trigger.start();

        // 让它跑一会儿
        tokio::time::sleep(Duration::from_millis(50)).await;
        let count_before_stop = counter.load(Ordering::SeqCst);

        // 停止
        trigger.stop();
        println!("Stopped at count: {}", count_before_stop);

        // 再等待很长一段时间
        tokio::time::sleep(Duration::from_millis(100)).await;
        let count_after_wait = counter.load(Ordering::SeqCst);

        // 验证计数器没有显著增加（考虑到 stop 指令发送和 task 接收的一瞬间可能有 1 次并发误差，通常通过 strict equal 验证，这里稍微放宽一点或者确认它不再无限增长）
        assert_eq!(
            count_before_stop, count_after_wait,
            "Task should stop executing after stop() called"
        );
    }

    #[tokio::test]
    async fn test_dynamic_add_task() {
        // 测试：在运行时动态添加任务
        let trigger = FixedScheduleTrigger::new(Duration::from_millis(20), true);
        let counter1 = Arc::new(AtomicUsize::new(0));
        let counter2 = Arc::new(AtomicUsize::new(0));

        trigger.start();

        // 添加任务 1
        trigger.add_task(create_counting_task(counter1.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;

        // 添加任务 2
        trigger.add_task(create_counting_task(counter2.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;

        trigger.stop();

        let c1 = counter1.load(Ordering::SeqCst);
        let c2 = counter2.load(Ordering::SeqCst);

        // c1 应该比 c2 跑得久，所以次数更多
        assert!(c1 > c2, "Task 1 should have run more times than Task 2");
        assert!(c2 > 0, "Task 2 should have executed at least once");
    }
}
