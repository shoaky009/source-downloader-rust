use crate::processor_run_state::{
    ActiveProcessorItem, ProcessorItemStage, ProcessorRunProgress, ProcessorRunReporter,
    ProcessorRunStage,
};
use crate::source_processor::SourceProcessor;
use async_trait::async_trait;
use parking_lot::Mutex;
use serde::Serialize;
use source_downloader_sdk::component::ProcessTask;
use source_downloader_sdk::time::OffsetDateTime;
use std::future::Future;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use tokio::sync::{broadcast, mpsc, oneshot};

pub struct ManagedProcessTask {
    manager: Arc<ProcessorRunManager>,
    processor: Arc<SourceProcessor>,
    kind: ProcessorRunKind,
}

#[async_trait]
impl ProcessTask for ManagedProcessTask {
    async fn run(&self) -> Result<(), String> {
        let processor = self.processor.clone();
        self.manager.submit(self.processor.name.clone(), self.kind, async move {
            processor.run().await
        });
        Ok(())
    }

    fn name(&self) -> &str {
        &self.processor.name
    }

    fn group(&self) -> Option<String> {
        self.processor.group()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessorRunKind {
    Automatic,
    ScheduledFull,
    ManualFull,
    Items,
    Rename,
    Reprocess,
    DryRunCollected,
    DryRunStreamed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessorRunStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessorRunSnapshot {
    pub id: u64,
    pub processor_name: String,
    pub kind: ProcessorRunKind,
    pub status: ProcessorRunStatus,
    pub stage: Option<ProcessorRunStage>,
    pub progress: ProcessorRunProgress,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProcessorRunEvent {
    Resync {
        runs: Vec<ProcessorRunSnapshot>,
    },
    Created {
        run: ProcessorRunSnapshot,
    },
    Started {
        run_id: u64,
        #[serde(with = "source_downloader_sdk::time::serde::rfc3339")]
        started_at: OffsetDateTime,
    },
    RunStageChanged {
        run_id: u64,
        stage: ProcessorRunStage,
    },
    TotalItemsChanged {
        run_id: u64,
        total_items: u32,
    },
    ItemStarted {
        run_id: u64,
        item_id: u64,
        item: ActiveProcessorItem,
    },
    ItemStageChanged {
        run_id: u64,
        item_id: u64,
        stage: ProcessorItemStage,
    },
    ItemCompleted {
        run_id: u64,
        item_id: u64,
        completed_items: u32,
    },
    Finished {
        run_id: u64,
        status: ProcessorRunStatus,
        #[serde(with = "source_downloader_sdk::time::serde::rfc3339")]
        finished_at: OffsetDateTime,
        failure: Option<String>,
    },
}

struct Entry {
    snapshot: ProcessorRunSnapshot,
    abort: Option<tokio::task::AbortHandle>,
}

struct State {
    active: std::collections::HashMap<u64, Entry>,
    automatic_rename_tasks: std::collections::HashMap<String, tokio::task::AbortHandle>,
}

#[derive(Clone)]
pub struct ProcessorRunManager {
    next_id: Arc<AtomicU64>,
    state: Arc<Mutex<State>>,
    events: broadcast::Sender<ProcessorRunEvent>,
}

struct ManagedRunReporter {
    manager: ProcessorRunManager,
    run_id: u64,
    next_item_id: AtomicU64,
}

impl ProcessorRunReporter for ManagedRunReporter {
    fn set_run_stage(&self, stage: ProcessorRunStage) {
        let changed = {
            let mut state = self.manager.state.lock();
            let Some(entry) = state.active.get_mut(&self.run_id) else { return };
            if entry.snapshot.stage == Some(stage) {
                false
            } else {
                entry.snapshot.stage = Some(stage);
                true
            }
        };
        if changed {
            self.manager.publish(ProcessorRunEvent::RunStageChanged {
                run_id: self.run_id,
                stage,
            });
        }
    }

    fn set_total_items(&self, total: u32) {
        let changed = {
            let mut state = self.manager.state.lock();
            let Some(entry) = state.active.get_mut(&self.run_id) else { return };
            if entry.snapshot.progress.total_items == Some(total) {
                false
            } else {
                entry.snapshot.progress.total_items = Some(total);
                true
            }
        };
        if changed {
            self.manager.publish(ProcessorRunEvent::TotalItemsChanged {
                run_id: self.run_id,
                total_items: total,
            });
        }
    }

    fn begin_item(&self, title: &str, stage: ProcessorItemStage) -> u64 {
        let item_id = self.next_item_id.fetch_add(1, Ordering::Relaxed);
        let item = ActiveProcessorItem {
            title: title.to_owned(),
            stage,
            started_at: OffsetDateTime::now_utc(),
        };
        {
            let mut state = self.manager.state.lock();
            let Some(entry) = state.active.get_mut(&self.run_id) else { return item_id };
            entry.snapshot.progress.active_items.insert(item_id, item.clone());
        }
        self.manager.publish(ProcessorRunEvent::ItemStarted {
            run_id: self.run_id,
            item_id,
            item,
        });
        item_id
    }

    fn set_item_stage(&self, item_id: u64, stage: ProcessorItemStage) {
        let changed = {
            let mut state = self.manager.state.lock();
            let Some(entry) = state.active.get_mut(&self.run_id) else { return };
            let Some(item) = entry.snapshot.progress.active_items.get_mut(&item_id)
            else {
                return;
            };
            if item.stage == stage {
                false
            } else {
                item.stage = stage;
                true
            }
        };
        if changed {
            self.manager.publish(ProcessorRunEvent::ItemStageChanged {
                run_id: self.run_id,
                item_id,
                stage,
            });
        }
    }

    fn complete_item(&self, item_id: u64) {
        let completed_items = {
            let mut state = self.manager.state.lock();
            let Some(entry) = state.active.get_mut(&self.run_id) else { return };
            if entry.snapshot.progress.active_items.remove(&item_id).is_none() {
                return;
            }
            entry.snapshot.progress.completed_items += 1;
            entry.snapshot.progress.completed_items
        };
        self.manager.publish(ProcessorRunEvent::ItemCompleted {
            run_id: self.run_id,
            item_id,
            completed_items,
        });
    }
}

impl Default for ProcessorRunManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessorRunManager {
    pub fn new() -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            next_id: Arc::new(AtomicU64::new(1)),
            state: Arc::new(Mutex::new(State {
                active: Default::default(),
                automatic_rename_tasks: Default::default(),
            })),
            events,
        }
    }

    fn publish(&self, event: ProcessorRunEvent) {
        if self.events.receiver_count() > 0 {
            let _ = self.events.send(event);
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ProcessorRunEvent> {
        self.events.subscribe()
    }

    pub fn list(&self) -> Vec<ProcessorRunSnapshot> {
        let state = self.state.lock();
        let mut runs =
            state.active.values().map(|entry| entry.snapshot.clone()).collect::<Vec<_>>();
        runs.sort_by_key(|run| run.id);
        runs
    }

    pub fn get(&self, id: u64) -> Option<ProcessorRunSnapshot> {
        self.state.lock().active.get(&id).map(|entry| entry.snapshot.clone())
    }

    pub fn cancel(&self, id: u64) -> bool {
        let abort =
            self.state.lock().active.get(&id).and_then(|entry| entry.abort.clone());
        if let Some(abort) = abort {
            abort.abort();
            self.finish(id, ProcessorRunStatus::Cancelled, None);
            true
        } else {
            false
        }
    }

    fn create(
        &self,
        name: impl Into<String>,
        kind: ProcessorRunKind,
    ) -> ProcessorRunSnapshot {
        let run = ProcessorRunSnapshot {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            processor_name: name.into(),
            kind,
            status: ProcessorRunStatus::Queued,
            stage: None,
            progress: ProcessorRunProgress::default(),
            created_at: OffsetDateTime::now_utc(),
            started_at: None,
            finished_at: None,
            failure: None,
        };
        self.state
            .lock()
            .active
            .insert(run.id, Entry { snapshot: run.clone(), abort: None });
        if self.events.receiver_count() > 0 {
            self.publish(ProcessorRunEvent::Created { run: run.clone() });
        }
        run
    }

    fn start(&self, id: u64) {
        let started_at = OffsetDateTime::now_utc();
        {
            let mut state = self.state.lock();
            let Some(entry) = state.active.get_mut(&id) else { return };
            if entry.snapshot.status == ProcessorRunStatus::Running {
                return;
            }
            entry.snapshot.status = ProcessorRunStatus::Running;
            entry.snapshot.started_at = Some(started_at);
        }
        self.publish(ProcessorRunEvent::Started { run_id: id, started_at });
    }

    fn finish(&self, id: u64, status: ProcessorRunStatus, failure: Option<String>) {
        let finished_at = OffsetDateTime::now_utc();
        let failure = {
            let mut state = self.state.lock();
            let Some(entry) = state.active.remove(&id) else { return };
            failure.or(entry.snapshot.failure)
        };
        self.publish(ProcessorRunEvent::Finished {
            run_id: id,
            status,
            finished_at,
            failure,
        });
    }

    fn reporter(&self, id: u64) -> Arc<dyn ProcessorRunReporter> {
        Arc::new(ManagedRunReporter {
            manager: self.clone(),
            run_id: id,
            next_item_id: AtomicU64::new(1),
        })
    }

    fn attach_abort(&self, id: u64, handle: tokio::task::AbortHandle) {
        if let Some(entry) = self.state.lock().active.get_mut(&id) {
            entry.abort = Some(handle);
        }
    }

    pub fn submit<F>(
        &self,
        name: impl Into<String>,
        kind: ProcessorRunKind,
        task: F,
    ) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        let run = self.create(name, kind);
        let manager = self.clone();
        let id = run.id;
        let handle = tokio::spawn(async move {
            manager.start(id);
            let result =
                crate::processor_run_state::scope(manager.reporter(id), task).await;
            match result {
                Ok(()) => manager.finish(id, ProcessorRunStatus::Succeeded, None),
                Err(error) => manager.finish(id, ProcessorRunStatus::Failed, Some(error)),
            }
        });
        self.attach_abort(id, handle.abort_handle());
        run
    }

    pub fn submit_full<F>(&self, name: impl Into<String>, task: F) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(name, ProcessorRunKind::ManualFull, task)
    }

    pub fn submit_scheduled<F>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(name, ProcessorRunKind::ScheduledFull, task)
    }

    pub fn submit_automatic<F>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(name, ProcessorRunKind::Automatic, task)
    }

    pub fn submit_items<F>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(name, ProcessorRunKind::Items, task)
    }

    pub fn submit_rename<F>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(name, ProcessorRunKind::Rename, task)
    }

    pub fn submit_reprocess<F>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(name, ProcessorRunKind::Reprocess, task)
    }

    pub fn submit_dry_run_collected<F>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> (
        ProcessorRunSnapshot,
        oneshot::Receiver<Vec<crate::source_processor::DryRunEvent>>,
    )
    where
        F: Future<Output = Result<Vec<crate::source_processor::DryRunEvent>, String>>
            + Send
            + 'static,
    {
        let (sender, receiver) = oneshot::channel();
        let run = self.create(name, ProcessorRunKind::DryRunCollected);
        let manager = self.clone();
        let id = run.id;
        let handle = tokio::spawn(async move {
            manager.start(id);
            let result =
                crate::processor_run_state::scope(manager.reporter(id), task).await;
            match result {
                Ok(events) => {
                    let _ = sender.send(events);
                    manager.finish(id, ProcessorRunStatus::Succeeded, None);
                }
                Err(error) => manager.finish(id, ProcessorRunStatus::Failed, Some(error)),
            }
        });
        self.attach_abort(id, handle.abort_handle());
        (run, receiver)
    }

    pub fn submit_dry_run_streamed<F>(
        &self,
        name: impl Into<String>,
        task: F,
    ) -> (ProcessorRunSnapshot, mpsc::Receiver<crate::source_processor::DryRunEvent>)
    where
        F: Future<
                Output = Result<
                    mpsc::Receiver<crate::source_processor::DryRunEvent>,
                    String,
                >,
            > + Send
            + 'static,
    {
        let (output_sender, output_receiver) = mpsc::channel(32);
        let run = self.create(name, ProcessorRunKind::DryRunStreamed);
        let manager = self.clone();
        let id = run.id;
        let handle = tokio::spawn(async move {
            manager.start(id);
            let result =
                crate::processor_run_state::scope(manager.reporter(id), task).await;
            match result {
                Ok(mut input) => {
                    while let Some(event) = input.recv().await {
                        if output_sender.send(event).await.is_err() {
                            break;
                        }
                    }
                    manager.finish(id, ProcessorRunStatus::Succeeded, None);
                }
                Err(error) => manager.finish(id, ProcessorRunStatus::Failed, Some(error)),
            }
        });
        self.attach_abort(id, handle.abort_handle());
        (run, output_receiver)
    }

    pub fn managed_task(
        self: &Arc<Self>,
        processor: Arc<SourceProcessor>,
    ) -> Arc<dyn ProcessTask> {
        Arc::new(ManagedProcessTask {
            manager: self.clone(),
            processor,
            kind: ProcessorRunKind::ScheduledFull,
        })
    }

    pub fn start_auto_rename(&self, processor: Arc<SourceProcessor>) {
        let Some(interval) = processor.automatic_rename_interval() else { return };
        let name = processor.name.clone();
        self.stop_auto_rename(&name);
        let manager = self.clone();
        let task_name = name.clone();
        let handle = tokio::spawn(async move {
            loop {
                let processor = processor.clone();
                manager.submit_rename(task_name.clone(), async move {
                    processor
                        .run_rename()
                        .await
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                });
                tokio::time::sleep(interval).await;
            }
        });
        self.state.lock().automatic_rename_tasks.insert(name, handle.abort_handle());
    }

    pub fn stop_auto_rename(&self, processor_name: &str) {
        let (task, ids) = {
            let mut state = self.state.lock();
            let task = state.automatic_rename_tasks.remove(processor_name);
            let ids = state
                .active
                .values()
                .filter(|entry| entry.snapshot.processor_name == processor_name)
                .map(|entry| entry.snapshot.id)
                .collect::<Vec<_>>();
            (task, ids)
        };
        if let Some(task) = task {
            task.abort();
        }
        for id in ids {
            let _ = self.cancel(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessorRunEvent, ProcessorRunManager, ProcessorRunStatus};
    use crate::processor_run_state::{ProcessorItemStage, ProcessorRunStage};
    use std::sync::Arc;
    use tokio::sync::Notify;

    async fn wait_until_removed(manager: &ProcessorRunManager, id: u64) {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while manager.get(id).is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("completed run was not removed");
    }

    #[tokio::test]
    async fn reporter_emits_incremental_events_and_suppresses_unchanged_stages() {
        let manager = ProcessorRunManager::new();
        let mut events = manager.subscribe();
        let release = Arc::new(Notify::new());
        let task_release = release.clone();
        let run = manager.submit_full("processor", async move {
            crate::processor_run_state::set_run_stage(ProcessorRunStage::ProcessingItems);
            crate::processor_run_state::set_run_stage(ProcessorRunStage::ProcessingItems);
            crate::processor_run_state::set_total_items(1);
            let item = crate::processor_run_state::ProcessorRunItemGuard::new(
                "episode 1",
                ProcessorItemStage::FilteringItem,
            );
            item.set_stage(ProcessorItemStage::ResolvingFiles);
            item.set_stage(ProcessorItemStage::ResolvingFiles);
            task_release.notified().await;
            Ok(())
        });

        let mut run_stage_events = 0;
        let mut item_stage_events = 0;
        while item_stage_events == 0 {
            match events.recv().await.unwrap() {
                ProcessorRunEvent::RunStageChanged { run_id, .. } if run_id == run.id => {
                    run_stage_events += 1;
                }
                ProcessorRunEvent::ItemStageChanged { run_id, .. }
                    if run_id == run.id =>
                {
                    item_stage_events += 1;
                }
                _ => {}
            }
        }
        assert_eq!(run_stage_events, 1);
        assert_eq!(item_stage_events, 1);
        let snapshot = manager.get(run.id).unwrap();
        assert_eq!(snapshot.progress.total_items, Some(1));
        assert_eq!(
            snapshot.progress.active_items.values().next().unwrap().title,
            "episode 1"
        );
        release.notify_one();
        wait_until_removed(&manager, run.id).await;
    }

    #[tokio::test]
    async fn completed_run_emits_small_terminal_event_then_is_removed() {
        let manager = ProcessorRunManager::new();
        let mut events = manager.subscribe();
        let run = manager.submit_full("processor", async { Ok(()) });

        loop {
            if let ProcessorRunEvent::Finished { run_id, status, failure, .. } =
                events.recv().await.unwrap()
                && run_id == run.id
            {
                assert_eq!(status, ProcessorRunStatus::Succeeded);
                assert!(failure.is_none());
                break;
            }
        }
        assert!(manager.get(run.id).is_none());
    }

    #[tokio::test]
    async fn cancellation_aborts_and_removes_running_task() {
        let manager = ProcessorRunManager::new();
        let started = Arc::new(Notify::new());
        let task_started = started.clone();
        let run = manager.submit_reprocess("processor", async move {
            task_started.notify_one();
            std::future::pending().await
        });
        started.notified().await;

        assert!(manager.cancel(run.id));
        assert!(manager.get(run.id).is_none());
        assert!(!manager.cancel(run.id));
    }

    #[tokio::test]
    async fn automatic_and_dry_runs_are_removed_after_completion() {
        let manager = ProcessorRunManager::new();
        let automatic = manager.submit_automatic("processor", async { Ok(()) });
        let (collected, collected_events) =
            manager.submit_dry_run_collected("processor", async { Ok(Vec::new()) });
        let (streamed, mut streamed_events) =
            manager.submit_dry_run_streamed("processor", async {
                let (sender, receiver) = tokio::sync::mpsc::channel(1);
                drop(sender);
                Ok(receiver)
            });

        assert!(collected_events.await.unwrap().is_empty());
        assert!(streamed_events.recv().await.is_none());
        wait_until_removed(&manager, automatic.id).await;
        wait_until_removed(&manager, collected.id).await;
        wait_until_removed(&manager, streamed.id).await;
    }
}
