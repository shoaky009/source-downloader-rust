use crate::process::file::{PathPattern, RawFileContent, Renamer};
use crate::process::rule::{FileRule, ItemRule, ItemStrategy};
use crate::process::variable::VariableAggregation;
use async_trait::async_trait;
use backon::Retryable;
use backon::{BackoffBuilder, ExponentialBuilder};
use humantime::format_duration;
use source_downloader_sdk::component::{
    Downloader, FileContentFilter, ItemContentFilter, SourceFileFilter, SourceItemFilter,
};
use source_downloader_sdk::component::{FileContent, Source};
use source_downloader_sdk::component::{FileMover, ProcessingError};
use source_downloader_sdk::component::{FileTagger, ProcessTask, SourceFile};
use source_downloader_sdk::component::{ItemFileResolver, ItemPointer, SourcePointer};
use source_downloader_sdk::component::{PatternVariables, VariableProvider};
use source_downloader_sdk::storage::{
    ItemContentLite, ProcessingContent, ProcessingStatus, ProcessingStorage, ProcessorSourceState,
};
use source_downloader_sdk::time::OffsetDateTime;
use source_downloader_sdk::SourceItem;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

static INSTANCE_ID_GENERATOR: AtomicI64 = AtomicI64::new(0);
static PROCESS_ID_GENERATOR: AtomicI64 = AtomicI64::new(i64::MIN);
#[allow(dead_code, unused)]
pub struct SourceProcessor {
    pub name: String,
    pub source_id: String,
    save_path: Box<Path>,
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
    download_path: Box<Path>,
}

pub struct ProcessorOptions {
    // ok
    pub save_path_pattern: Arc<PathPattern>,
    // ok
    pub filename_pattern: Arc<PathPattern>,
    // ok
    pub variable_providers: Vec<Arc<dyn VariableProvider>>,
    // ok
    pub item_filters: Vec<Arc<dyn SourceItemFilter>>,
    pub item_content_filters: Vec<Arc<dyn ItemContentFilter>>,
    // ok
    pub source_file_filters: Vec<Arc<dyn SourceFileFilter>>,
    // ok
    pub file_content_filters: Vec<Arc<dyn FileContentFilter>>,
    // ok
    pub file_taggers: Vec<Arc<dyn FileTagger>>,
    // ok
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
    // ok
    pub item_rules: Vec<ItemRule>,
    // ok
    pub file_rules: Vec<FileRule>,
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
        save_path: Box<Path>,
        source: Arc<dyn Source>,
        item_file_resolver: Arc<dyn ItemFileResolver>,
        downloader: Arc<dyn Downloader>,
        file_mover: Arc<dyn FileMover>,
        processing_storage: Arc<dyn ProcessingStorage>,
        category: Option<String>,
        tags: HashSet<String>,
        options: ProcessorOptions,
    ) -> Self {
        let download_path = Path::new(downloader.default_download_path()).into();
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
            download_path,
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
        let opt = &p.options;
        let item_rule = opt
            .item_rules
            .iter()
            .find(|x| x.matcher.matches(source_item));
        let item_strategy = item_rule.map(|x| &x.strategy);
        let item_filters = item_strategy
            .map(|x| x.item_filters.as_ref())
            .flatten()
            .unwrap_or(&opt.item_filters);
        for x in item_filters {
            let filtered = !x.filter(source_item).await;
            if filtered {
                debug!("[item-filtered] {}", source_item);
                ctx.filter_inc();
                return Ok(true);
            }
        }
        let mut item_raw_vars = vec![];

        let variable_providers = item_strategy
            .map(|x| x.variable_providers.as_ref())
            .flatten()
            .unwrap_or(&opt.variable_providers);
        for x in variable_providers {
            item_raw_vars.push((x.accuracy(), x.item_variables(source_item).await))
        }
        let item_variables = opt.variable_aggregation.merge(&item_raw_vars);

        let resolved_files = self.resolve_files(source_item, p).await?;
        let file_contents = self
            .process_source_files(
                p,
                source_item,
                &item_variables,
                resolved_files,
                item_strategy,
            )
            .await?;
        // opt.item_filters

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
            .filter(|x| p.options.source_file_filters.iter().all(|y| y.filter(x)))
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
        item_group_options: Option<&ItemStrategy>,
    ) -> Result<Vec<FileContent>, ProcessingError> {
        let mut relative_files: Vec<SourceFile> = vec![];
        let download_path = p.downloader.default_download_path();
        let opt = &p.options;
        for mut file in source_files.into_iter() {
            if let Ok(rel_path) = file.path.strip_prefix(download_path) {
                file.path = rel_path.to_path_buf();
            };
            relative_files.push(file);
        }

        let mut file_raw_vars = vec![];
        for idx in 0..opt.variable_providers.len() {
            let v = opt.variable_providers.get(idx).unwrap();
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
        let file_vars = opt.variable_aggregation.merge_files(&file_raw_vars);
        let mut result: Vec<FileContent> = vec![];

        let item_var = p
            .renamer
            .item_rename_variables(source_item, item_variables.clone());

        let empty_vars = &PatternVariables::new();
        let file_count = relative_files.len();
        for (idx, x) in relative_files.into_iter().enumerate() {
            let var = file_vars.get(idx).unwrap_or_else(|| empty_vars);
            let file_rule = opt
                .file_rules
                .iter()
                .find(|rule| rule.matcher.matches(&x, file_count));
            let file_strategy = file_rule.map(|r| &r.strategy);

            // Determine save_path_pattern and filename_pattern for this file
            let file_save_path_pattern = file_strategy
                .map(|s| s.save_path_pattern.clone())
                .flatten()
                .or_else(|| {
                    item_group_options
                        .map(|s| s.save_path_pattern.clone())
                        .flatten()
                })
                .unwrap_or(opt.save_path_pattern.clone());
            let file_filename_pattern = file_strategy
                .map(|s| s.filename_pattern.clone())
                .flatten()
                .or_else(|| {
                    item_group_options
                        .map(|s| s.filename_pattern.clone())
                        .flatten()
                })
                .unwrap_or(opt.filename_pattern.clone());

            let raw = RawFileContent {
                save_path: &p.save_path,
                download_path: &p.download_path,
                variables: var,
                save_path_pattern: &file_save_path_pattern,
                filename_pattern: &file_filename_pattern,
                source_file: &x,
            };
            let content = p.renamer.create_file_content(source_item, raw, &item_var);

            // Apply file_content_filters, use file_strategy's if present, otherwise use opt's
            let file_content_filters = file_strategy
                .map(|s| s.file_content_filters.as_ref())
                .flatten()
                .unwrap_or(&opt.file_content_filters);

            let mut should_include = true;
            for filter in file_content_filters {
                if !filter.filter(&content) {
                    debug!("[file-filtered] {}", content.target_filename);
                    should_include = false;
                    break;
                }
            }
            if !should_include {
                continue;
            }

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

    #[allow(dead_code)]
    fn decode_files_from_compressed(bytes: &[u8]) -> Result<Vec<FileContent>, ProcessingError> {
        if bytes.is_empty() {
            return Ok(vec![]);
        }
        let decompressed = zstd::decode_all(bytes).map_err(|x| {
            ProcessingError::non_retryable(format!(
                "Failed to decompress file content {}",
                x.to_string()
            ))
        })?;
        let files: Vec<FileContent> = postcard::from_bytes(&decompressed).map_err(|x| {
            ProcessingError::non_retryable(format!(
                "Failed to deserialize file content {}",
                x.to_string()
            ))
        })?;
        Ok(files)
    }
}

#[allow(dead_code)]
struct DryRunProcess {}
#[allow(dead_code)]
struct Reprocess {}
#[allow(dead_code)]
struct FixedItemProcess {}

#[cfg(test)]
mod test_support {
    use async_trait::async_trait;
    use source_downloader_sdk::component::{
        empty_item_pointer, ComponentError, ComponentSupplier, ComponentType, DownloadTask, Downloader,
        FileMover, ItemContent, ItemFileResolver, NullSourcePointer, PointedItem,
        ProcessingError, SdComponent, SdComponentMetadata, Source, SourceFile, SourcePointer,
    };
    use source_downloader_sdk::serde_json::{Map, Value};
    use source_downloader_sdk::time::OffsetDateTime;
    use source_downloader_sdk::{SdComponent, SourceItem};
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::Arc;
    use vfs::VfsPath;

    pub struct VfsFileSourceSupplier {
        pub root: Arc<VfsPath>,
    }

    impl ComponentSupplier for VfsFileSourceSupplier {
        fn supply_types(&self) -> Vec<ComponentType> {
            vec![
                ComponentType::source("vfs".to_string()),
                ComponentType::downloader("vfs".to_string()),
            ]
        }

        fn apply(
            &self,
            props: &Map<String, Value>,
        ) -> Result<Arc<dyn SdComponent>, ComponentError> {
            let path = props
                .get("path")
                .ok_or_else(|| ComponentError::from("Missing 'path' property"))?
                .as_str()
                .unwrap();
            let mode = props.get("mode").and_then(|v| v.as_i64()).unwrap_or(0) as i8;
            let path = self.root.join(path).unwrap();
            Ok(Arc::new(VfsFileSource { path, mode }))
        }

        fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
            None
        }
    }

    #[derive(Debug, SdComponent)]
    #[component(Source, Downloader)]
    struct VfsFileSource {
        path: VfsPath,
        mode: i8,
    }

    impl Downloader for VfsFileSource {
        fn submit(&self, _: &DownloadTask) -> Result<(), ComponentError> {
            Ok(())
        }

        fn default_download_path(&self) -> &str {
            self.path.as_str()
        }

        fn cancel(&self, _: &DownloadTask, _: &[SourceFile]) -> Result<(), ComponentError> {
            Ok(())
        }
    }

    #[async_trait]
    impl Source for VfsFileSource {
        async fn fetch(
            &self,
            _: Arc<dyn SourcePointer>,
            _: u32,
        ) -> Result<Vec<PointedItem>, ProcessingError> {
            match self.mode {
                0 => self.create_root_file_source_items(),
                1 => self.create_each_file_source_items(),
                _ => Err(ProcessingError::non_retryable(format!(
                    "Unknown mode: {}",
                    self.mode
                ))),
            }
        }

        fn default_pointer(&self) -> Arc<dyn SourcePointer> {
            Arc::new(NullSourcePointer {})
        }

        fn parse_raw_pointer(&self, _: Value) -> Arc<dyn SourcePointer> {
            Arc::new(NullSourcePointer {})
        }
    }

    impl VfsFileSource {
        // Mode 0: 对应 createRootFileSourceItems (Files.list)
        fn create_root_file_source_items(&self) -> Result<Vec<PointedItem>, ProcessingError> {
            self.path
                .read_dir()
                .map_err(|e| ProcessingError::non_retryable(e.to_string()))?
                .map(|p| Self::from_vfs_path(p))
                .collect()
        }

        // Mode 1: 对应 createEachFileSourceItems (path.walk)
        fn create_each_file_source_items(&self) -> Result<Vec<PointedItem>, ProcessingError> {
            self.path
                .walk_dir()
                .map_err(|e| ProcessingError::non_retryable(e.to_string()))?
                .map(|p| p.unwrap())
                .filter(|p| p.is_file().unwrap_or(false))
                .map(|p| Self::from_vfs_path(p))
                .collect()
        }

        fn from_vfs_path(path: VfsPath) -> Result<PointedItem, ProcessingError> {
            let file_name = path.filename();
            let is_dir = path.is_dir().unwrap();
            let file_type = if is_dir { "directory" } else { "file" };
            let file_size = path.metadata().unwrap().len;

            let mut attrs = Map::new();
            attrs.insert("size".to_string(), Value::from(file_size));

            let url = format!("file:/{}", path.as_str());
            let source_item = SourceItem {
                title: file_name,
                link: url.parse().unwrap(),
                datetime: OffsetDateTime::now_utc(),
                content_type: file_type.to_string(),
                download_uri: url.parse().unwrap(),
                attrs,
                tags: HashSet::new(),
                identity: None,
            };

            Ok(PointedItem {
                source_item,
                item_pointer: empty_item_pointer(),
            })
        }
    }

    pub struct VfsFileResolverSupplier;
    pub const VFS_RESOLVER_SUPPLIER: VfsFileResolverSupplier = VfsFileResolverSupplier {};

    impl ComponentSupplier for VfsFileResolverSupplier {
        fn supply_types(&self) -> Vec<ComponentType> {
            vec![ComponentType::file_resolver("vfs".to_owned())]
        }

        fn apply(&self, _: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
            Ok(Arc::new(VfsFileResolver {}))
        }

        fn is_support_no_props(&self) -> bool {
            true
        }

        fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
            None
        }
    }

    #[derive(Debug)]
    struct VfsFileResolver;

    impl SdComponent for VfsFileResolver {
        fn as_item_file_resolver(
            self: Arc<Self>,
        ) -> Result<Arc<dyn ItemFileResolver>, ComponentError> {
            Ok(self)
        }
    }

    #[async_trait]
    impl ItemFileResolver for VfsFileResolver {
        async fn resolve_files(&self, source_item: &SourceItem) -> Vec<SourceFile> {
            let path = PathBuf::from(
                source_item
                    .download_uri
                    .to_string()
                    .strip_prefix("file:/")
                    .unwrap(),
            );
            vec![SourceFile::new(path)]
        }
    }

    pub struct VfsMoverSupplier {
        pub root: Arc<VfsPath>,
    }

    impl ComponentSupplier for VfsMoverSupplier {
        fn supply_types(&self) -> Vec<ComponentType> {
            vec![ComponentType::file_mover("vfs".to_owned())]
        }

        fn apply(&self, _: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
            Ok(Arc::new(VfsMover {
                root: self.root.clone(),
            }))
        }

        fn is_support_no_props(&self) -> bool {
            true
        }

        fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
            todo!()
        }
    }

    #[derive(SdComponent, Debug)]
    #[component(FileMover)]
    struct VfsMover {
        root: Arc<VfsPath>,
    }

    #[allow(dead_code, unused)]
    impl FileMover for VfsMover {
        fn move_file(
            &self,
            source_file: &SourceFile,
            download_path: &str,
        ) -> Result<(), ProcessingError> {
            todo!()
        }

        fn exists(&self, path: Vec<&str>) -> Vec<bool> {
            path.iter()
                .map(|x| self.root.join(x).unwrap().exists().unwrap_or(false))
                .collect()
        }

        fn create_directories(&self, path: &str) -> Result<(), ProcessingError> {
            self.root.join(path).unwrap().create_dir().unwrap();
            Ok(())
        }

        fn replace(&self, item_content: &ItemContent) -> Result<(), ProcessingError> {
            todo!()
        }

        fn list_files(&self, path: &str) -> Vec<String> {
            self.root
                .join(path)
                .and_then(|p| p.read_dir())
                .unwrap()
                .map(|x| x.as_str().to_string())
                .collect()
        }

        fn path_metadata(&self, path: &str) -> SourceFile {
            todo!()
        }
    }
}

#[cfg(test)]
mod test {
    use crate::component_manager::ComponentManager;
    use crate::components::get_build_in_component_supplier;
    use crate::config::{ConfigOperator, YamlConfigOperator};
    use crate::processor_manager::ProcessorManager;
    use crate::source_processor::test_support::{
        VfsFileSourceSupplier, VfsMoverSupplier, VFS_RESOLVER_SUPPLIER,
    };
    use crate::source_processor::{NormalProcess, SourceProcessor};
    use indexmap::IndexMap;
    use jsonpath_rust::JsonPath;
    use serde::Deserialize;
    use serde_json::json;
    use source_downloader_sdk::component::ProcessTask;
    use source_downloader_sdk::storage::{ProcessingContentQuery, ProcessingStorage};
    use std::sync::{Arc, LazyLock, OnceLock};
    use storage_sqlite::SeaProcessingStorage;
    use vfs::MemoryFS;
    use vfs::VfsPath;

    static _CM: OnceLock<Arc<ComponentManager>> = OnceLock::new();
    static _PM: tokio::sync::OnceCell<ProcessorManager> = tokio::sync::OnceCell::const_new();
    static _S: tokio::sync::OnceCell<Arc<SeaProcessingStorage>> =
        tokio::sync::OnceCell::const_new();
    static _C: OnceLock<Arc<YamlConfigOperator>> = OnceLock::new();
    static V_PATH: LazyLock<Arc<VfsPath>> =
        LazyLock::new(|| Arc::new(VfsPath::new(MemoryFS::new())));
    static CASES: LazyLock<IndexMap<String, Case>> = LazyLock::new(|| {
        let file = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("processor_cases.yaml");
        let content = std::fs::read(file).unwrap();
        serde_yaml::from_slice(&content).unwrap()
    });
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
            m.register_supplier(Arc::new(VfsFileSourceSupplier {
                root: V_PATH.clone(),
            }))
            .unwrap();
            m.register_supplier(Arc::new(VFS_RESOLVER_SUPPLIER))
                .unwrap();

            m.register_supplier(Arc::new(VfsMoverSupplier {
                root: V_PATH.clone(),
            }))
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

    #[derive(Deserialize)]
    struct Case {
        pub files: Vec<CaseFile>,
        pub assertions: Vec<Assertion>,
    }
    #[derive(Deserialize)]
    struct CaseFile {
        pub path: String,
        pub content: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    struct Assertion {
        // JSON path
        pub select: String,
        #[serde(default)]
        pub allow_empty: bool,
        pub asserts: Vec<AssertExpr>,
    }
    #[derive(Deserialize)]
    struct AssertExpr {
        // JSON path
        pub path: Option<String>,
        pub pointer: Option<String>,
        pub equals: Option<serde_json::Value>,
        pub length: Option<usize>,
        pub exists: Option<bool>,
    }

    #[derive(Debug)]
    pub struct AssertionError {
        pub message: String,
        pub context: Vec<String>,
    }

    impl AssertionError {
        pub fn new(msg: impl Into<String>) -> Self {
            Self {
                message: msg.into(),
                context: Vec::new(),
            }
        }

        pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
            self.context.push(ctx.into());
            self
        }
    }

    impl std::fmt::Display for AssertionError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            for ctx in self.context.iter().rev() {
                writeln!(f, "  at {}", ctx)?;
            }
            write!(f, "Assertion failed: {}", self.message)
        }
    }

    fn apply_case_files(root_path: &VfsPath, files: &[CaseFile]) {
        root_path.create_dir_all().unwrap();
        for file in files.into_iter() {
            let path = root_path.join(&file.path).unwrap();
            let parent = path.parent();
            if !parent.exists().unwrap() {
                parent.create_dir_all().unwrap();
            }
            let mut f = path.create_file().unwrap();
            if let Some(content) = &file.content {
                f.write_all(content.as_bytes()).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn sync_downloader_case() {
        let cfg = cfg();
        let pm = processor_manager().await;
        let storage = storage().await;
        for (name, case) in CASES.iter() {
            pm.create_processor(&cfg.get_processor_config(name).unwrap());
            let p = assert_processor(name, pm);
            let root_path = V_PATH.join(format!("/{}", name)).unwrap();
            apply_case_files(&root_path, &case.files);

            let result = p.run().await;
            assert!(result.is_ok());

            let content = build_result_json(storage, name).await;
            for (assert_idx, assertion) in case.assertions.iter().enumerate() {
                let selection = content.query(&assertion.select).unwrap_or_default();
                if !assertion.allow_empty && selection.is_empty() {
                    let err = AssertionError::new("Selection result is empty".to_string())
                        .with_context(format!("case: {}", name))
                        .with_context(format!("assertion #{}", assert_idx))
                        .with_context(format!("select: {}", assertion.select));
                    panic!("{}", err)
                }
                for (node_idx, node) in selection.iter().enumerate() {
                    if let Err(err) = apply_assertion(node, &assertion.asserts) {
                        let err = err
                            .with_context(format!("case: {}", name))
                            .with_context(format!("assertion #{}", assert_idx))
                            .with_context(format!("select: {}", assertion.select))
                            .with_context(format!("node index: {}", node_idx))
                            .with_context(format!("content #{}", node));
                        panic!("{}", err);
                    }
                }
            }
        }
    }

    fn apply_assertion(
        node: &serde_json::Value,
        asserts: &Vec<AssertExpr>,
    ) -> Result<(), AssertionError> {
        for assert in asserts {
            let target = if let Some(pointer) = &assert.pointer {
                node.pointer(pointer).ok_or_else(|| {
                    AssertionError::new(format!("JSONPointer not found: {}", pointer))
                })?
            } else if let Some(path) = &assert.path {
                let mut cur = node;
                for seg in path.trim_start_matches('.').split('.') {
                    cur = cur.get(seg).ok_or_else(|| {
                        AssertionError::new(format!(
                            "Path segment '{}' not found in '{}'",
                            seg, path
                        ))
                    })?;
                }
                cur
            } else {
                node
            };

            if let Some(expected) = &assert.equals {
                if target != expected {
                    return Err(AssertionError::new(format!(
                        "equals failed, expected {}, got {}",
                        expected, target
                    )));
                }
            }

            if let Some(len) = assert.length {
                let arr = target
                    .as_array()
                    .ok_or_else(|| AssertionError::new("target is not an array"))?;

                if arr.len() != len {
                    return Err(AssertionError::new(format!(
                        "length failed, expected {}, got {}",
                        len,
                        arr.len()
                    )));
                }
            }

            if let Some(true) = assert.exists {
                if target.is_null() {
                    return Err(AssertionError::new("expected value to exist"));
                }
            }
        }
        Ok(())
    }

    async fn build_result_json(
        storage: &Arc<SeaProcessingStorage>,
        name: &str,
    ) -> serde_json::Value {
        let contents = storage
            .query_processing_content(&ProcessingContentQuery {
                processor_name: Some(vec![name.to_string()]),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut res = Vec::new();
        for content in contents {
            let files = storage
                .find_file_contents(content.id.unwrap())
                .await
                .unwrap()
                .map(|bytes| NormalProcess::decode_files_from_compressed(&bytes).unwrap())
                .unwrap_or_default();

            let mut value = serde_json::to_value(content).unwrap();
            value["files"] = serde_json::to_value(files).unwrap();
            res.push(value);
        }
        json!(res)
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
