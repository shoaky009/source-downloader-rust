use async_trait::async_trait;
use sdk::component::{ProcessTask, Source, VariableProvider};
use std::sync::Arc;
use tracing::info;
// 这些导入是为了让 async_trait 宏在文档测试中能够正确工作
#[doc(hidden)]
pub use std::future;
#[doc(hidden)]
pub use std::marker;
#[doc(hidden)]
pub use std::pin;
use std::sync::atomic::AtomicI64;

static GLOBAL_ID: AtomicI64 = AtomicI64::new(i64::MIN);

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
    instance_id: i64,
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
        name: String,
        source_id: String,
        save_path: String,
        source: Arc<dyn Source>,
        options: ProcessorOptions,
    ) -> Self {
        Self {
            name,
            source_id,
            save_path,
            source,
            options,
            instance_id: GLOBAL_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        }
    }

    pub fn instance_id(&self) -> i64 {
        self.instance_id
    }
}

impl Drop for SourceProcessor {
    fn drop(&mut self) {
        info!("Processor[dropped] {}({})", self.name, self.instance_id);
    }
}
