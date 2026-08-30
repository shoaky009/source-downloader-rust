use serde::Serialize;
use source_downloader_sdk::time::OffsetDateTime;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessorRunStage {
    Initializing,
    FetchingItems,
    ScanningItems,
    ProcessingItems,
    Finalizing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessorItemStage {
    FilteringItem,
    ResolvingVariables,
    ResolvingFiles,
    FilteringContent,
    DecidingReplacements,
    SubmittingDownload,
    MovingFiles,
    SettlingItem,
}

impl ProcessorItemStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FilteringItem => "filter-item",
            Self::ResolvingVariables => "resolve-variables",
            Self::ResolvingFiles => "resolve-files",
            Self::FilteringContent => "filter-content",
            Self::DecidingReplacements => "decide-replacements",
            Self::SubmittingDownload => "submit-download",
            Self::MovingFiles => "move-files",
            Self::SettlingItem => "settle-item",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveProcessorItem {
    pub title: String,
    pub stage: ProcessorItemStage,
    #[serde(with = "source_downloader_sdk::time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessorRunProgress {
    pub total_items: Option<u32>,
    pub completed_items: u32,
    pub active_items: HashMap<u64, ActiveProcessorItem>,
}

pub trait ProcessorRunReporter: Send + Sync {
    fn set_run_stage(&self, stage: ProcessorRunStage);
    fn set_total_items(&self, total: u32);
    fn begin_item(&self, title: &str, stage: ProcessorItemStage) -> u64;
    fn set_item_stage(&self, item_id: u64, stage: ProcessorItemStage);
    fn complete_item(&self, item_id: u64);
}

tokio::task_local! {
    static REPORTER: RefCell<Option<Arc<dyn ProcessorRunReporter>>>;
}

pub async fn scope<F: Future>(
    reporter: Arc<dyn ProcessorRunReporter>,
    future: F,
) -> F::Output {
    REPORTER.scope(RefCell::new(Some(reporter)), future).await
}

pub fn current_reporter() -> Option<Arc<dyn ProcessorRunReporter>> {
    REPORTER.try_with(|slot| slot.borrow().clone()).ok().flatten()
}

pub fn set_run_stage(stage: ProcessorRunStage) {
    if let Some(reporter) = current_reporter() {
        reporter.set_run_stage(stage);
    }
}

pub fn set_total_items(total: usize) {
    if let Some(reporter) = current_reporter() {
        reporter.set_total_items(total as u32);
    }
}

pub struct ProcessorRunItemGuard {
    reporter: Option<Arc<dyn ProcessorRunReporter>>,
    id: u64,
}

impl ProcessorRunItemGuard {
    pub fn new(title: &str, stage: ProcessorItemStage) -> Self {
        let reporter = current_reporter();
        let id =
            reporter.as_ref().map_or(0, |reporter| reporter.begin_item(title, stage));
        Self { reporter, id }
    }

    pub fn set_stage(&self, stage: ProcessorItemStage) {
        if let Some(reporter) = &self.reporter {
            reporter.set_item_stage(self.id, stage);
        }
    }
}

impl Drop for ProcessorRunItemGuard {
    fn drop(&mut self) {
        if let Some(reporter) = &self.reporter {
            reporter.complete_item(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessorItemStage;

    #[test]
    fn item_stage_log_names_are_stable() {
        let stages = [
            (ProcessorItemStage::FilteringItem, "filter-item"),
            (ProcessorItemStage::ResolvingVariables, "resolve-variables"),
            (ProcessorItemStage::ResolvingFiles, "resolve-files"),
            (ProcessorItemStage::FilteringContent, "filter-content"),
            (ProcessorItemStage::DecidingReplacements, "decide-replacements"),
            (ProcessorItemStage::SubmittingDownload, "submit-download"),
            (ProcessorItemStage::MovingFiles, "move-files"),
            (ProcessorItemStage::SettlingItem, "settle-item"),
        ];

        for (stage, expected) in stages {
            assert_eq!(stage.as_str(), expected);
        }
    }
}
