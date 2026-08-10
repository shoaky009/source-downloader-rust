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
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub failure: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessorRunEvent {
    Created(ProcessorRunSnapshot),
    Updated(ProcessorRunSnapshot),
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
    pub fn subscribe(&self) -> broadcast::Receiver<ProcessorRunEvent> {
        self.events.subscribe()
    }
    pub fn list(&self) -> Vec<ProcessorRunSnapshot> {
        let s = self.state.lock();
        let mut runs =
            s.active.values().map(|entry| entry.snapshot.clone()).collect::<Vec<_>>();
        runs.sort_by_key(|run| run.id);
        runs
    }
    pub fn get(&self, id: u64) -> Option<ProcessorRunSnapshot> {
        self.state.lock().active.get(&id).map(|entry| entry.snapshot.clone())
    }
    pub fn cancel(&self, id: u64) -> bool {
        let a = { self.state.lock().active.get(&id).and_then(|e| e.abort.clone()) };
        if let Some(a) = a {
            a.abort();
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
        let r = ProcessorRunSnapshot {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            processor_name: name.into(),
            kind,
            status: ProcessorRunStatus::Queued,
            created_at: OffsetDateTime::now_utc(),
            started_at: None,
            finished_at: None,
            failure: None,
        };
        self.state.lock().active.insert(r.id, Entry { snapshot: r.clone(), abort: None });
        let _ = self.events.send(ProcessorRunEvent::Created(r.clone()));
        r
    }
    fn update(&self, id: u64, f: impl FnOnce(&mut ProcessorRunSnapshot)) {
        let r = {
            let mut s = self.state.lock();
            let Some(e) = s.active.get_mut(&id) else { return };
            f(&mut e.snapshot);
            e.snapshot.clone()
        };
        let _ = self.events.send(ProcessorRunEvent::Updated(r));
    }
    fn finish(&self, id: u64, status: ProcessorRunStatus, failure: Option<String>) {
        let run = {
            let mut state = self.state.lock();
            let Some(mut entry) = state.active.remove(&id) else { return };
            entry.snapshot.status = status;
            entry.snapshot.failure = failure;
            entry.snapshot.finished_at = Some(OffsetDateTime::now_utc());
            entry.snapshot
        };
        let _ = self.events.send(ProcessorRunEvent::Updated(run));
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
        let r = self.create(name, kind);
        let m = self.clone();
        let id = r.id;
        let j = tokio::spawn(async move {
            m.update(id, |s| {
                s.status = ProcessorRunStatus::Running;
                s.started_at = Some(OffsetDateTime::now_utc())
            });
            match task.await {
                Ok(()) => m.finish(id, ProcessorRunStatus::Succeeded, None),
                Err(e) => m.finish(id, ProcessorRunStatus::Failed, Some(e)),
            }
        });
        if let Some(e) = self.state.lock().active.get_mut(&id) {
            e.abort = Some(j.abort_handle())
        }
        r
    }
    pub fn submit_full<F>(&self, n: impl Into<String>, f: F) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(n, ProcessorRunKind::ManualFull, f)
    }
    pub fn submit_scheduled<F>(&self, n: impl Into<String>, f: F) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(n, ProcessorRunKind::ScheduledFull, f)
    }
    pub fn submit_automatic<F>(&self, n: impl Into<String>, f: F) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(n, ProcessorRunKind::Automatic, f)
    }
    pub fn submit_items<F>(&self, n: impl Into<String>, f: F) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(n, ProcessorRunKind::Items, f)
    }
    pub fn submit_rename<F>(&self, n: impl Into<String>, f: F) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(n, ProcessorRunKind::Rename, f)
    }
    pub fn submit_reprocess<F>(&self, n: impl Into<String>, f: F) -> ProcessorRunSnapshot
    where
        F: Future<Output = Result<(), String>> + Send + 'static,
    {
        self.submit(n, ProcessorRunKind::Reprocess, f)
    }
    pub fn submit_dry_run_collected<F>(
        &self,
        n: impl Into<String>,
        f: F,
    ) -> (
        ProcessorRunSnapshot,
        oneshot::Receiver<Vec<crate::source_processor::DryRunEvent>>,
    )
    where
        F: Future<Output = Result<Vec<crate::source_processor::DryRunEvent>, String>>
            + Send
            + 'static,
    {
        let (tx, rx) = oneshot::channel();
        let r = self.create(n, ProcessorRunKind::DryRunCollected);
        let m = self.clone();
        let id = r.id;
        let j = tokio::spawn(async move {
            m.update(id, |s| {
                s.status = ProcessorRunStatus::Running;
                s.started_at = Some(OffsetDateTime::now_utc())
            });
            match f.await {
                Ok(v) => {
                    let _ = tx.send(v);
                    m.finish(id, ProcessorRunStatus::Succeeded, None)
                }
                Err(e) => m.finish(id, ProcessorRunStatus::Failed, Some(e)),
            }
        });
        if let Some(e) = self.state.lock().active.get_mut(&id) {
            e.abort = Some(j.abort_handle())
        }
        (r, rx)
    }
    pub fn submit_dry_run_streamed<F>(
        &self,
        n: impl Into<String>,
        f: F,
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
        let (ot, or) = mpsc::channel(32);
        let r = self.create(n, ProcessorRunKind::DryRunStreamed);
        let m = self.clone();
        let id = r.id;
        let j = tokio::spawn(async move {
            m.update(id, |s| {
                s.status = ProcessorRunStatus::Running;
                s.started_at = Some(OffsetDateTime::now_utc())
            });
            match f.await {
                Ok(mut i) => {
                    while let Some(e) = i.recv().await {
                        if ot.send(e).await.is_err() {
                            break;
                        }
                    }
                    m.finish(id, ProcessorRunStatus::Succeeded, None)
                }
                Err(e) => m.finish(id, ProcessorRunStatus::Failed, Some(e)),
            }
        });
        if let Some(e) = self.state.lock().active.get_mut(&id) {
            e.abort = Some(j.abort_handle())
        }
        (r, or)
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
    use super::{
        ProcessorRunEvent, ProcessorRunKind, ProcessorRunManager, ProcessorRunStatus,
    };
    use std::sync::Arc;
    use tokio::sync::Notify;

    #[test]
    fn processor_run_snapshot_serializes_timestamps_as_rfc3339() {
        let snapshot = super::ProcessorRunSnapshot {
            id: 1,
            processor_name: "processor".to_owned(),
            kind: ProcessorRunKind::ManualFull,
            status: ProcessorRunStatus::Running,
            created_at: source_downloader_sdk::time::OffsetDateTime::UNIX_EPOCH,
            started_at: Some(source_downloader_sdk::time::OffsetDateTime::UNIX_EPOCH),
            finished_at: None,
            failure: None,
        };

        let value = source_downloader_sdk::serde_json::to_value(snapshot).unwrap();

        assert_eq!(value["createdAt"], "1970-01-01T00:00:00Z");
        assert_eq!(value["startedAt"], "1970-01-01T00:00:00Z");
        assert!(value["finishedAt"].is_null());
    }

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
    async fn completed_runs_emit_terminal_events_then_are_removed() {
        let manager = ProcessorRunManager::new();
        let mut events = manager.subscribe();
        let successful = manager.submit_full("processor", async { Ok(()) });
        let failed =
            manager.submit_items("processor", async { Err("failed".to_owned()) });

        let mut terminal_events = Vec::new();
        while terminal_events.len() < 2 {
            if let ProcessorRunEvent::Updated(run) = events.recv().await.unwrap()
                && matches!(
                    run.status,
                    ProcessorRunStatus::Succeeded | ProcessorRunStatus::Failed
                )
            {
                terminal_events.push(run);
            }
        }

        assert_eq!(terminal_events[0].kind, ProcessorRunKind::ManualFull);
        assert!(terminal_events.iter().all(|run| run.started_at.is_some()));
        assert!(terminal_events.iter().all(|run| run.finished_at.is_some()));
        assert_eq!(
            terminal_events
                .iter()
                .find(|run| run.id == failed.id)
                .and_then(|run| run.failure.as_deref()),
            Some("failed")
        );
        assert!(manager.get(successful.id).is_none());
        assert!(manager.get(failed.id).is_none());
        assert!(manager.list().is_empty());
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
    async fn list_contains_only_active_runs() {
        let manager = ProcessorRunManager::new();
        let release = Arc::new(Notify::new());
        let task_release = release.clone();
        let run = manager.submit_full("processor", async move {
            task_release.notified().await;
            Ok(())
        });

        assert_eq!(manager.list().iter().map(|run| run.id).collect::<Vec<_>>(), [run.id]);
        release.notify_one();
        wait_until_removed(&manager, run.id).await;
        assert!(manager.list().is_empty());
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

        assert_eq!(automatic.kind, ProcessorRunKind::Automatic);
        assert_eq!(collected.kind, ProcessorRunKind::DryRunCollected);
        assert_eq!(streamed.kind, ProcessorRunKind::DryRunStreamed);
        assert!(collected_events.await.unwrap().is_empty());
        assert!(streamed_events.recv().await.is_none());
        wait_until_removed(&manager, automatic.id).await;
        wait_until_removed(&manager, collected.id).await;
        wait_until_removed(&manager, streamed.id).await;
    }
}
