use async_trait::async_trait;
use sdk::component::{ProcessTask, Source, VariableProvider};
use std::sync::Arc;
use tracing::info;

pub struct SourceProcessor {
    pub name: String,
    pub source_id: String,
    pub save_path: String,
    pub source: Arc<dyn Source>,
    // item_file_resolver: Arc<dyn ItemFileResolver>,
    // downloader: Arc<dyn Downloader>,
    // file_mover: Arc<dyn FileMover>,
    // processing_storage: Arc<dyn ProcessingStorage>,
    pub options: ProcessorOptions,
}

pub struct ProcessorOptions {
    pub save_path_pattern: String,
    pub filename_pattern: String,
    pub variable_providers: Vec<Arc<dyn VariableProvider>>,
}

#[async_trait]
impl ProcessTask for SourceProcessor {
    async fn run(&self) -> Result<(), String> {
        info!("Processor[run-start] {}", self.name);
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        info!("Processor[run-done]: {}", self.name);
        Ok(())
    }

    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn group(&self) -> Option<String> {
        None
    }
}

impl SourceProcessor {
    pub fn new(
        source_id: String,
        save_path: String,
        source: Arc<dyn Source>,
        options: ProcessorOptions,
    ) -> Self {
        Self {
            name: source_id.clone(),
            source_id,
            save_path,
            source,
            options,
        }
    }
}

impl Drop for SourceProcessor {
    fn drop(&mut self) {
        info!("Processor[dropped] {}", self.name);
    }
}
