use sdk::ProcessingStorage;
use sdk::component::{
    Downloader, FileMover, ItemFileResolver, ProcessorTask, Source, VariableProvider,
};
use std::sync::Arc;

pub struct SourceProcessor {
    pub name: String,
    pub source_id: String,
    pub save_path: String,
    pub source: Arc<dyn Source>,
    // item_file_resolver: Arc<dyn ItemFileResolver>,
    // downloader: Arc<dyn Downloader>,
    // file_mover: Arc<dyn FileMover>,
    // processing_storage: Arc<dyn ProcessingStorage>,
    // options: ProcessorOptions,
}

struct ProcessorOptions {
    pub save_path_pattern: String,
    pub filename_pattern: String,
    pub variable_providers: Vec<Arc<dyn VariableProvider>>,
}

impl SourceProcessor {
    pub fn close(&self) {
        println!("close source processor")
    }
    pub fn safe_task(&self) -> Arc<ProcessorTask> {
        Arc::new(ProcessorTask {
            process_name: self.name.clone(),
            runnable: Box::new(move || {}),
            group: None,
        })
    }
}
