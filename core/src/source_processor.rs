use async_trait::async_trait;
use sdk::component::Downloader;
use sdk::component::FileMover;
use sdk::component::ItemFileResolver;
use sdk::component::ProcessTask;
use sdk::component::Source;
use sdk::component::VariableProvider;
use sdk::storage::{ProcessingStorage, ProcessorSourceState};
// 这些导入是为了让 async_trait 宏在文档测试中能够正确工作
use sdk::SourceItem;
#[doc(hidden)]
pub use std::future;
#[doc(hidden)]
pub use std::marker;
#[doc(hidden)]
pub use std::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time;
use tracing::info;

static INSTANCE_ID_GENERATOR: AtomicI64 = AtomicI64::new(i64::MIN);
static PROCESS_ID_GENERATOR: AtomicI64 = AtomicI64::new(i64::MIN);
#[allow(dead_code, unused)]
pub struct SourceProcessor {
    pub name: String,
    pub source_id: String,
    save_path: String,
    source: Arc<dyn Source>,
    item_file_resolver: Arc<dyn ItemFileResolver>,
    downloader: Arc<dyn Downloader>,
    file_mover: Arc<dyn FileMover>,
    processing_storage: Arc<dyn ProcessingStorage>,
    options: ProcessorOptions,
    instance_id: i64,
    processing: AtomicBool,
}

pub struct ProcessorOptions {
    pub save_path_pattern: String,
    pub filename_pattern: String,
    pub variable_providers: Vec<Arc<dyn VariableProvider>>,
}

#[async_trait]
impl ProcessTask for SourceProcessor {
    async fn run(&self) -> Result<(), String> {
        let _span1 = tracing::info_span!("", processor = self.name);
        let start_time = time::Instant::now();
        let _g1 = _span1.enter();
        info!("[run-start] {}({})", self.name, self.instance_id);
        if self.processing.swap(true, Ordering::AcqRel) {
            info!(
                "[run-reject] {}({}) Already processing",
                self.name, self.instance_id
            );
            return Err("Already processing".to_string());
        }
        let _processing_guard = ProcessingGuard::new(&self.processing);
        let p_ctx = ProcessContext {
            trace_id: PROCESS_ID_GENERATOR
                .fetch_add(i64::MIN, Ordering::SeqCst)
                .to_string(),
        };

        let source_state = self
            .processing_storage
            .find_processor_source_state(&self.name, &self.source_id)
            .await
            .map_err(|x| x.message)?
            .unwrap_or(ProcessorSourceState {
                id: None,
                processor_name: self.name.to_owned(),
                source_id: self.source_id.to_owned(),
                last_pointer: self.source.default_pointer().dump(),
            });

        tracing::debug!("Fetch with pointer: {}", source_state.last_pointer);
        let source_pointer = self
            .source
            .parse_raw_pointer(source_state.last_pointer.to_owned());
        let items = self
            .source
            .fetch(source_pointer.clone())
            .await
            .map_err(|x| x.message)?;
        for item in items {
            let item_pointer = item.item_pointer;
            let source_item = item.source_item;
            process_item(&source_item, &p_ctx);
            source_pointer.update(&source_item, item_pointer);
        }

        let new_pointer = source_pointer.dump();
        tracing::debug!("Process end pointer: {}", new_pointer);
        let _new_state = self
            .processing_storage
            .save_processor_source_state(&ProcessorSourceState {
                last_pointer: new_pointer,
                ..source_state
            })
            .await;
        info!("[run-done]: {} cost={:?}", self.name, start_time.elapsed());
        Ok(())
    }

    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn group(&self) -> Option<String> {
        self.source.group()
    }
}

fn process_item(source_item: &SourceItem, _ctx: &ProcessContext) {
    info!("[item-start] {:?}", source_item);
}

#[allow(dead_code, unused)]
struct ProcessContext {
    trace_id: String,
}

struct ProcessingGuard<'a> {
    running: &'a AtomicBool,
}

impl<'a> ProcessingGuard<'a> {
    fn new(running: &'a AtomicBool) -> Self {
        Self { running }
    }
}

impl Drop for ProcessingGuard<'_> {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
    }
}

impl SourceProcessor {
    pub fn new(
        name: String,
        source_id: String,
        save_path: String,
        source: Arc<dyn Source>,
        item_file_resolver: Arc<dyn ItemFileResolver>,
        downloader: Arc<dyn Downloader>,
        file_mover: Arc<dyn FileMover>,
        processing_storage: Arc<dyn ProcessingStorage>,
        options: ProcessorOptions,
    ) -> Self {
        Self {
            name,
            source_id,
            save_path,
            source,
            item_file_resolver,
            downloader,
            file_mover,
            processing_storage,
            options,
            instance_id: INSTANCE_ID_GENERATOR.fetch_add(1, Ordering::Relaxed),
            processing: AtomicBool::new(false),
        }
    }

    pub fn instance_id(&self) -> i64 {
        self.instance_id
    }

    pub async fn dry_run(&self) {}
}

impl Drop for SourceProcessor {
    fn drop(&mut self) {
        info!("Processor[dropped] {}({})", self.name, self.instance_id);
    }
}

#[allow(dead_code)]
trait Process {
    fn select_item_filter(&self);
    fn on_process_complete(&self);
    fn on_item_process_complete(&self);
}
#[allow(dead_code)]
struct DryRunProcess {}
#[allow(dead_code)]
struct NormalProcess {}
#[allow(dead_code)]
struct Reprocess {}
#[allow(dead_code)]
struct FixedItemProcess {}
