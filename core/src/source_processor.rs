use crate::process::file::{RawFileContent, Renamer};
use crate::process::variable::VariableAggregation;
use async_trait::async_trait;
use backon::Retryable;
use backon::{BackoffBuilder, ExponentialBuilder};
use humantime::format_duration;
use sdk::SourceItem;
use sdk::component::{Downloader, SourceFileFilter, SourceItemFilter};
use sdk::component::{FileContent, Source};
use sdk::component::{FileMover, ProcessingError};
use sdk::component::{FileTagger, ProcessTask, SourceFile};
use sdk::component::{ItemFileResolver, ItemPointer, SourcePointer};
use sdk::component::{PatternVariables, VariableProvider};
use sdk::storage::{
    ItemContentLite, ProcessingContent, ProcessingStatus, ProcessingStorage, ProcessorSourceState,
};
use sdk::time::OffsetDateTime;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tracing::log::warn;
use tracing::{debug, info};
// 这些导入是为了让 async_trait 宏在文档测试中能够正确工作
#[doc(hidden)]
pub use std::future;
use std::io::Cursor;
#[doc(hidden)]
pub use std::marker;
use std::path::{Path, PathBuf};
#[doc(hidden)]
pub use std::pin;

static INSTANCE_ID_GENERATOR: AtomicI64 = AtomicI64::new(0);
static PROCESS_ID_GENERATOR: AtomicI64 = AtomicI64::new(i64::MIN);
#[allow(dead_code, unused)]
pub struct SourceProcessor {
    pub name: String,
    pub source_id: String,
    save_path: PathBuf,
    source: Arc<dyn Source>,
    item_file_resolver: Arc<dyn ItemFileResolver>,
    downloader: Arc<dyn Downloader>,
    file_mover: Arc<dyn FileMover>,
    processing_storage: Arc<dyn ProcessingStorage>,
    category: Option<String>,
    tags: HashSet<String>,
    options: ProcessorOptions,
    instance_id: i64,
    processing: AtomicBool,
    renamer: Renamer,
}

pub struct ProcessorOptions {
    pub save_path_pattern: String,
    pub filename_pattern: String,
    // ok
    pub variable_providers: Vec<Arc<dyn VariableProvider>>,
    // ok
    pub item_filters: Vec<Arc<dyn SourceItemFilter>>,
    pub source_file_filters: Vec<Arc<dyn SourceFileFilter>>,
    pub file_taggers: Vec<Arc<dyn FileTagger>>,
    pub variable_aggregation: VariableAggregation,
    // ok
    pub save_processing_content: bool,
    pub rename_task_interval: Duration,
    pub rename_times_threshold: u32,
    pub parallelism: u32,
    // ok
    pub task_group: Option<String>,
    // ok
    pub fetch_limit: u32,
    // ok
    pub item_error_continue: bool,
    // ok
    pub pointer_batch_mode: bool,
}

#[async_trait]
impl ProcessTask for SourceProcessor {
    async fn run(&self) -> Result<(), String> {
        let p = NormalProcess {};
        p.execute(self).await.map_err(|x| x.to_string())
    }

    fn name(&self) -> &str {
        self.name.as_str()
    }

    fn group(&self) -> Option<String> {
        self.source.group()
    }
}

#[allow(dead_code, unused)]
struct ProcessContext {
    pub trace_id: String,
    source_state: ProcessorSourceState,
    item_flag: HashSet<String>,
    processed_count: AtomicU32,
    filter_count: AtomicU32,
    process_start_at: Option<Instant>,
    process_end_at: Option<Instant>,
    fetch_start_at: Option<Instant>,
    fetch_end_at: Option<Instant>,
}

impl ProcessContext {
    fn filter_inc(&self) {
        self.filter_count.fetch_add(1, Ordering::Relaxed);
    }
    fn processed_inc(&self) {
        self.processed_count.fetch_add(1, Ordering::Relaxed);
    }
    fn summary(&self) -> String {
        format!(
            "处理了{}个 过滤了{}个; [total] took {}; [fetch-items] took {}; [process-items] took {}",
            self.processed_count.load(Ordering::Acquire),
            self.filter_count.load(Ordering::Acquire),
            match (self.process_start_at, self.process_end_at) {
                (Some(start), Some(end)) => Self::format_duration(end.duration_since(start)),
                _ => "N/A".to_string(),
            },
            match (self.fetch_start_at, self.fetch_end_at) {
                (Some(start), Some(end)) => Self::format_duration(end.duration_since(start)),
                _ => "N/A".to_string(),
            },
            match (self.fetch_end_at, self.process_end_at) {
                (Some(start), Some(end)) => Self::format_duration(end.duration_since(start)),
                _ => "N/A".to_string(),
            }
        )
    }

    fn format_duration(dur: Duration) -> String {
        let secs = dur.as_secs();
        let millis = dur.subsec_millis();
        if secs > 0 {
            format!("{}.{:03}s", secs, millis)
        } else {
            format!("{}ms", millis)
        }
    }
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
        save_path: PathBuf,
        source: Arc<dyn Source>,
        item_file_resolver: Arc<dyn ItemFileResolver>,
        downloader: Arc<dyn Downloader>,
        file_mover: Arc<dyn FileMover>,
        processing_storage: Arc<dyn ProcessingStorage>,
        category: Option<String>,
        tags: HashSet<String>,
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
            category,
            tags,
            options,
            instance_id: INSTANCE_ID_GENERATOR.fetch_add(1, Ordering::Relaxed),
            processing: AtomicBool::new(false),
            renamer: Renamer::default(),
        }
    }

    pub fn instance_id(&self) -> i64 {
        self.instance_id
    }

    pub async fn dry_run(&self) {
        DryRunProcess {};
    }

    pub async fn reprocess(&self) {}

    async fn save_source_state(&self, state: &ProcessorSourceState) -> Result<(), String> {
        self.processing_storage
            .save_processor_source_state(state)
            .await
            .map_err(|x| x.message)
            .map(|_| ())
    }

    pub async fn apply_retry<T, Fut, F>(mut f: F, stage: &str) -> Result<T, ProcessingError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, ProcessingError>>,
    {
        (|| f())
            .retry(
                ExponentialBuilder::default()
                    .with_max_times(3)
                    .with_max_delay(Duration::from_secs(10))
                    .build(),
            )
            .when(|e| matches!(e, ProcessingError::Retryable { .. }))
            .notify(|err, dur| {
                warn!(
                    "Retrying {} delay {} cause={} ",
                    stage,
                    format_duration(dur),
                    err.message()
                );
            })
            .await
    }
}

impl Drop for SourceProcessor {
    fn drop(&mut self) {
        info!("Processor[dropped] {}({})", self.name, self.instance_id);
    }
}

#[allow(dead_code)]
trait Process {
    fn select_item_filter<'a>(&self, p: &'a SourceProcessor) -> &'a Vec<Arc<dyn SourceItemFilter>>;

    async fn on_process_complete(
        &self,
        p: &SourceProcessor,
        ctx: &ProcessContext,
        pointer: Arc<dyn SourcePointer>,
    );

    async fn on_item_process_complete(
        &self,
        p: &SourceProcessor,
        processing_content: &ProcessingContent,
        x1: &Vec<FileContent>,
    ) -> Result<(), ProcessingError>;

    async fn on_item_error(
        &self,
        _p: &SourceProcessor,
        _ctx: &ProcessContext,
        _err: &ProcessingError,
    ) {
        // TODO invoke hooks
    }

    #[allow(unused)]
    async fn on_item_success(
        &self,
        p: &SourceProcessor,
        ctx: &ProcessContext,
        item_pointer: &Box<dyn ItemPointer>,
        source_item: &SourceItem,
        source_pointer: Arc<dyn SourcePointer>,
    ) {
    }

    async fn execute(&self, p: &SourceProcessor) -> Result<(), ProcessingError> {
        let span_exec = tracing::info_span!("", processor = p.name);
        let start_time = Instant::now();
        let _span_exec_entered = span_exec.enter();
        info!("[run-start] {}({})", p.name, p.instance_id);
        if p.processing.swap(true, Ordering::AcqRel) {
            info!(
                "[run-reject] {}({}) Already processing",
                p.name, p.instance_id
            );
            return Err(ProcessingError::non_retryable("Already processing"));
        }
        let _processing_guard = ProcessingGuard::new(&p.processing);

        let source_state = p
            .processing_storage
            .find_processor_source_state(&p.name, &p.source_id)
            .await
            .map_err(|x| ProcessingError::non_retryable(x.message))?
            .unwrap_or(ProcessorSourceState {
                id: None,
                processor_name: p.name.to_owned(),
                source_id: p.source_id.to_owned(),
                last_pointer: p.source.default_pointer().dump(),
            });
        debug!("Fetch with pointer: {}", source_state.last_pointer);
        let mut p_ctx = ProcessContext {
            trace_id: PROCESS_ID_GENERATOR
                .fetch_add(i64::MIN, Ordering::Relaxed)
                .to_string(),
            source_state: source_state.to_owned(),
            item_flag: HashSet::new(),
            processed_count: AtomicU32::new(0),
            filter_count: AtomicU32::new(0),
            process_start_at: Some(start_time),
            process_end_at: None,
            fetch_start_at: None,
            fetch_end_at: None,
        };

        let source_pointer = p
            .source
            .parse_raw_pointer(source_state.last_pointer.to_owned());

        p_ctx.fetch_start_at = Some(Instant::now());
        let items = SourceProcessor::apply_retry(
            || async {
                p.source
                    .fetch(source_pointer.clone(), p.options.fetch_limit)
                    .await
            },
            "fetch-source-items",
        )
        .await?;
        p_ctx.fetch_end_at = Some(Instant::now());

        for item in items {
            let item_pointer = item.item_pointer;
            let source_item = item.source_item;
            let item_hash = source_item.hashing();
            if p_ctx.item_flag.contains(&item_hash) {
                p_ctx.filter_inc();
                info!("Source item duplicated: {:?} skipped", source_item);
                continue;
            }
            p_ctx.item_flag.insert(item_hash);

            let item_result = self.process_item(&source_item, &p_ctx, p).await;
            if item_result.is_err() {
                let err = item_result.unwrap_err();
                self.on_item_error(p, &p_ctx, &err).await;
                if matches!(err, ProcessingError::NonRetryable { skip: true, .. }) {
                    info!(
                        "[item-fail] 异常为可跳过类型 {} {:?}",
                        err.message(),
                        source_item
                    );
                    continue;
                }
                if !p.options.item_error_continue {
                    warn!(
                        "[item-fail] 退出本次触发处理, 如果未能解决该处理器将无法继续处理后续Item {:?} {}",
                        source_item,
                        err.message()
                    );
                    break;
                }
                continue;
            }
            if item_result? {
                continue;
            }

            p_ctx.processed_inc();
            self.on_item_success(
                p,
                &p_ctx,
                &item_pointer,
                &source_item,
                source_pointer.clone(),
            )
            .await;
        }
        self.on_process_complete(p, &p_ctx, source_pointer.clone())
            .await;

        p_ctx.process_end_at = Some(Instant::now());
        info!("[run-done] {} {}", p.name, p_ctx.summary());
        Ok(())
    }

    // 如果是true结束该item的流程处理
    async fn process_item(
        &self,
        source_item: &SourceItem,
        ctx: &ProcessContext,
        p: &SourceProcessor,
    ) -> Result<bool, ProcessingError> {
        info!("[item-start] {}", source_item);
        for x in &p.options.item_filters {
            let filtered = !x.filter(source_item).await;
            if filtered {
                debug!("[item-filtered] {}", source_item);
                ctx.filter_inc();
                return Ok(true);
            }
        }
        let mut item_raw_vars = vec![];
        for x in &p.options.variable_providers {
            item_raw_vars.push((x.accuracy(), x.item_variables(source_item).await))
        }
        let item_variables = p.options.variable_aggregation.merge(&item_raw_vars);

        let resolved_files = self.resolve_files(source_item, p).await?;
        let file_contents = self
            .process_source_files(p, source_item, &item_variables, resolved_files)
            .await?;
        let content = ProcessingContent {
            id: None,
            processor_name: p.name.clone(),
            item_hash: source_item.hashing(),
            item_identity: source_item.identity.clone(),
            item_content: ItemContentLite {
                source_item: source_item.clone(),
                item_variables,
            },
            rename_times: 0,
            status: ProcessingStatus::Renamed,
            failure_reason: None,
            created_at: OffsetDateTime::now_utc(),
            updated_at: None,
        };
        self.on_item_process_complete(p, &content, &file_contents)
            .await?;
        Ok(false)
    }

    async fn resolve_files(
        &self,
        source_item: &SourceItem,
        p: &SourceProcessor,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
        let original_files = p
            .item_file_resolver
            .resolve_files(source_item)
            .await
            .into_iter()
            .filter(|x| p.options.source_file_filters.iter().any(|y| !y.filter(x)))
            .collect::<Vec<_>>();
        let mut counts: HashMap<&Path, usize> = HashMap::new();
        for f in &original_files {
            let count = counts.entry(f.path.as_ref()).or_insert(0);
            *count += 1;
            if *count > 1 {
                return Err(ProcessingError::non_retryable(format!(
                    "resolved item:{} duplicated files:{}, It's likely that there's an issue with the component's implementation.",
                    source_item,
                    &f.path.to_str().unwrap_or_default()
                )));
            }
        }

        let mut resolved_files: Vec<SourceFile> = vec![];
        for f in original_files {
            let mut tags: Vec<String> = vec![];
            for x in &p.options.file_taggers {
                if let Some(tag) = x.tag(&f).await {
                    tags.push(tag);
                };
            }
            if tags.is_empty() {
                resolved_files.push(f);
            } else {
                tags.extend(p.tags.iter().cloned());
                resolved_files.push(SourceFile { tags, ..f });
            }
        }

        Ok(resolved_files)
    }

    async fn process_source_files(
        &self,
        p: &SourceProcessor,
        source_item: &SourceItem,
        item_variables: &PatternVariables,
        source_files: Vec<SourceFile>,
    ) -> Result<Vec<FileContent>, ProcessingError> {
        let mut relative_files: Vec<SourceFile> = vec![];
        let download_path = p.downloader.default_download_path();
        for mut x in source_files.into_iter() {
            if let Ok(rel_path) = x.path.strip_prefix(download_path) {
                x.path = rel_path.to_path_buf();
            };
            relative_files.push(x);
        }
        let mut file_raw_vars = vec![];
        for idx in 0..p.options.variable_providers.len() {
            let v = &p.options.variable_providers.get(idx).unwrap();
            let vars = v
                .file_variables(source_item, item_variables, &relative_files)
                .await;
            if vars.len() != relative_files.len() {
                return Err(ProcessingError::non_retryable(format!(
                    "Resolved files:{} and file variables:{} size not match, variable provider at {} implementation error",
                    relative_files.len(),
                    vars.len(),
                    idx
                )));
            }
            file_raw_vars.push((v.accuracy(), vars));
        }
        let file_vars = p.options.variable_aggregation.merge_files(&file_raw_vars);
        let mut result: Vec<FileContent> = vec![];

        let item_var = p
            .renamer
            .item_rename_variables(source_item, item_variables.clone());
        for (idx, x) in relative_files.into_iter().enumerate() {
            let var = file_vars.get(idx).unwrap();
            // 后面转引用
            let raw = RawFileContent {
                save_path: p.save_path.to_owned(),
                download_path: PathBuf::from(download_path),
                variables: var.to_owned(),
                save_path_pattern: p.options.save_path_pattern.to_owned(),
                filename_pattern: p.options.filename_pattern.to_owned(),
                source_file: x,
            };
            let content = p.renamer.create_file_content(source_item, raw, &item_var);
            result.push(content)
        }
        Ok(result)
    }
}

#[allow(dead_code)]
struct NormalProcess {}

impl Process for NormalProcess {
    fn select_item_filter<'a>(&self, p: &'a SourceProcessor) -> &'a Vec<Arc<dyn SourceItemFilter>> {
        &p.options.item_filters
    }

    async fn on_process_complete(
        &self,
        p: &SourceProcessor,
        ctx: &ProcessContext,
        pointer: Arc<dyn SourcePointer>,
    ) {
        // TODO invoke hooks
        // 第二个条件待定
        if p.options.pointer_batch_mode || ctx.processed_count.load(Ordering::Acquire) == 0 {
            p.save_source_state(&ProcessorSourceState {
                last_pointer: pointer.dump(),
                ..ctx.source_state.clone()
            })
            .await
            .unwrap();
        }
    }

    async fn on_item_process_complete(
        &self,
        p: &SourceProcessor,
        processing_content: &ProcessingContent,
        files: &Vec<FileContent>,
    ) -> Result<(), ProcessingError> {
        info!(
            "[item-done] {:?}",
            &processing_content.item_content.source_item
        );
        if !p.options.save_processing_content {
            return Ok(());
        }
        // 事务?
        let content_id = p
            .processing_storage
            .save_processing_content(processing_content)
            .await
            .map_err(|x| {
                ProcessingError::non_retryable(format!("Failed to save item content {}", x.message))
            })?;

        let bytes = Self::encode_files_and_compress(&files)?;
        p.processing_storage
            .save_file_contents(content_id, bytes)
            .await
            .map_err(|x| {
                ProcessingError::non_retryable(format!(
                    "Failed to save file contents {}",
                    x.message
                ))
            })?;
        Ok(())
    }

    async fn on_item_success(
        &self,
        p: &SourceProcessor,
        ctx: &ProcessContext,
        item_pointer: &Box<dyn ItemPointer>,
        source_item: &SourceItem,
        source_pointer: Arc<dyn SourcePointer>,
    ) {
        // TODO invoke hooks
        source_pointer.update(source_item, item_pointer);
        if !p.options.pointer_batch_mode {
            let new_pointer = source_pointer.dump();
            p.save_source_state(&ProcessorSourceState {
                last_pointer: new_pointer,
                ..ctx.source_state.clone()
            })
            .await
            .unwrap()
        }
    }
}

impl NormalProcess {
    fn encode_files_and_compress(files: &Vec<FileContent>) -> Result<Vec<u8>, ProcessingError> {
        let bytes = if files.is_empty() {
            vec![]
        } else {
            let bytes = postcard::to_stdvec(&files).map_err(|x| {
                ProcessingError::non_retryable(format!(
                    "Failed to desc file content {}",
                    x.to_string()
                ))
            })?;
            // 压缩比待定
            let level = 6;
            zstd::encode_all(Cursor::new(bytes), level).map_err(|x| {
                ProcessingError::non_retryable(format!(
                    "Failed to compress file content {}",
                    x.to_string()
                ))
            })?
        };
        Ok(bytes)
    }
}

#[allow(dead_code)]
struct DryRunProcess {}
#[allow(dead_code)]
struct Reprocess {}
#[allow(dead_code)]
struct FixedItemProcess {}

#[cfg(test)]
mod test {
    use crate::component_manager::ComponentManager;
    use crate::components::get_build_in_component_supplier;
    use crate::config::{ConfigOperator, YamlConfigOperator};
    use crate::processor_manager::ProcessorManager;
    use crate::source_processor::SourceProcessor;
    use sdk::component::ProcessTask;
    use std::sync::{Arc, OnceLock};
    use storage_sqlite::SeaProcessingStorage;

    static _CM: OnceLock<Arc<ComponentManager>> = OnceLock::new();
    static _PM: tokio::sync::OnceCell<ProcessorManager> = tokio::sync::OnceCell::const_new();
    static _S: tokio::sync::OnceCell<Arc<SeaProcessingStorage>> =
        tokio::sync::OnceCell::const_new();
    static _C: OnceLock<Arc<YamlConfigOperator>> = OnceLock::new();

    fn cfg() -> &'static Arc<YamlConfigOperator> {
        _C.get_or_init(|| Arc::new(YamlConfigOperator::new("./tests/resources/config.yaml")))
    }
    async fn storage() -> &'static Arc<SeaProcessingStorage> {
        _S.get_or_init(|| async {
            Arc::new(SeaProcessingStorage::new("sqlite::memory:").await.unwrap())
        })
        .await
    }
    fn component_manager() -> &'static Arc<ComponentManager> {
        _CM.get_or_init(|| {
            let m = Arc::new(ComponentManager::new(cfg().clone()));
            m.register_suppliers(get_build_in_component_supplier())
                .unwrap();
            m
        })
    }

    async fn processor_manager() -> &'static ProcessorManager {
        _PM.get_or_init(|| async {
            ProcessorManager::new(component_manager().clone(), storage().await.clone())
        })
        .await
    }

    #[tokio::test]
    async fn sync_downloader_case() {
        let name = "sync_downloader_case";
        let pm = processor_manager().await;
        pm.create_processor(&cfg().get_processor_config(name).unwrap());
        let p = assert_processor(name, pm);
        let result = p.run().await;
        assert!(result.is_ok())
    }

    fn assert_processor(name: &str, pm: &ProcessorManager) -> Arc<SourceProcessor> {
        let w = pm.get_processor(name).expect("Processor wrapper not found");
        let p = w.processor.clone();
        match p {
            None => {
                panic!(
                    "Processor instance not found cause {}",
                    w.error_message.as_ref().unwrap().to_string()
                )
            }
            Some(p) => p,
        }
    }
}
