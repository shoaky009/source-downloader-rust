use crate::components::simple_file_exists_detector::SimpleFileExistsDetector;
use crate::process::file::{PathPattern, RawFileContent, Renamer};
use crate::process::rule::{FileRule, ItemRule, ItemStrategy};
use crate::process::variable::VariableAggregation;
use async_trait::async_trait;
use backon::Retryable;
use backon::{BackoffBuilder, ExponentialBuilder};
use humantime::format_duration;
use itertools::Itertools;
use parking_lot::RwLock;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::FileContentStatus::{
    Downloaded, FileConflict, Normal, ReadyReplace, TargetExists, Undetected,
    VariableError,
};
use source_downloader_sdk::component::{
    AsyncDownloader, DownloadOptions, DownloadTask, Downloader, FileContentFilter,
    FileExistsDetector, FileReplacementDecider, InProcessingItem, ItemContent,
    ItemContentFilter, ProcessContext, ProcessListener, ProcessorInfo, SourceFileFilter,
    SourceFileRef, SourceItemFilter,
};
use source_downloader_sdk::component::{FileContent, Source};
use source_downloader_sdk::component::{FileMover, ProcessingError};
use source_downloader_sdk::component::{FileTagger, ProcessTask, SourceFile};
use source_downloader_sdk::component::{ItemFileResolver, ItemPointer, SourcePointer};
use source_downloader_sdk::component::{PatternVariables, VariableProvider};
use source_downloader_sdk::storage::{
    ItemContentLite, ProcessingContent, ProcessingContentQuery, ProcessingStatus,
    ProcessingStorage, ProcessingTargetPath, ProcessorSourceState,
};
use source_downloader_sdk::time::OffsetDateTime;
use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

static INSTANCE_ID_GENERATOR: AtomicI64 = AtomicI64::new(0);
static PROCESS_ID_GENERATOR: AtomicI64 = AtomicI64::new(i64::MIN);
// static EMPTY_FILES: Vec<FileContent> = vec![];
// static EMPTY_PATTERN_VARIABLES: LazyLock<PatternVariables> = LazyLock::new(|| HashMap::new());

#[derive(Debug)]
pub struct ItemProcessResult {
    /// true 表示结束该 item 的流程处理（如被过滤）
    pub item_filtered: bool,
    pub file_contents: Vec<FileContent>,
    pub item_variables: PatternVariables,
    pub status: ProcessingStatus,
    pub message: Option<String>,
    pub finished_at: OffsetDateTime,
}
#[allow(dead_code, unused)]
pub struct SourceProcessor {
    pub name: String,
    pub source_id: String,
    save_path: Box<Path>,
    source: Arc<dyn Source>,
    item_file_resolver: Arc<dyn ItemFileResolver>,
    downloader: Arc<dyn Downloader>,
    async_downloader: Option<Arc<dyn AsyncDownloader>>,
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
    pub process_listeners: Vec<Arc<dyn ProcessListener>>,
    pub file_exists_detector: Arc<dyn FileExistsDetector>,
    pub file_replacement_decider: Arc<dyn FileReplacementDecider>,
    // ok
    pub download_options: DownloadOptions,
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

struct ListenerContext {
    processor_info: ProcessorInfo,
    processed_items: Vec<SourceItem>,
    contents: HashMap<String, (ProcessingContent, Vec<FileContent>)>,
    has_error: bool,
}

impl ListenerContext {
    fn new(processor: &SourceProcessor) -> Self {
        Self {
            processor_info: ProcessorInfo {
                name: processor.name.to_owned(),
                download_path: processor.download_path.to_string_lossy().into_owned(),
                source_save_path: processor.save_path.to_string_lossy().into_owned(),
                tags: processor.tags.to_owned(),
                category: processor.category.to_owned(),
            },
            processed_items: Vec::new(),
            contents: HashMap::new(),
            has_error: false,
        }
    }

    fn add(&mut self, content: ProcessingContent, files: Vec<FileContent>) {
        self.processed_items.push(content.item_content.source_item.clone());
        self.contents.insert(content.item_hash.to_owned(), (content, files));
    }
}

impl ProcessContext for ListenerContext {
    fn processor(&self) -> &ProcessorInfo {
        &self.processor_info
    }

    fn processed_items(&self) -> &Vec<SourceItem> {
        &self.processed_items
    }

    fn get_item_content(&self, item: &SourceItem) -> Option<InProcessingItem<'_>> {
        let (content, files) = self.contents.get(&item.hashing())?;
        Some(InProcessingItem {
            id: &content.id,
            processor_name: &content.processor_name,
            item_hash: &content.item_hash,
            item_identity: &content.item_identity,
            source_item: &content.item_content.source_item,
            item_variables: &content.item_content.item_variables,
            file_contents: files,
            rename_times: &content.rename_times,
            status: &content.status,
            failure_reason: content.failure_reason.as_deref(),
        })
    }

    fn has_error(&self) -> bool {
        self.has_error
    }
}

#[allow(dead_code, unused)]
struct ProcessRuntime {
    pub trace_id: String,
    pub mutex: Mutex<()>,
    source_state: ProcessorSourceState,
    source_pointer: Box<dyn SourcePointer>,
    process_submitted_items: RwLock<HashSet<String>>,
    processed_count: AtomicU32,
    filter_count: AtomicU32,
    process_start_at: Option<Instant>,
    process_end_at: Option<Instant>,
    fetch_start_at: Option<Instant>,
    fetch_end_at: Option<Instant>,
    cancel_items: Vec<SourceItem>,
    listener_context: ListenerContext,
}

enum ItemAction {
    // Source重复返回的Item
    Skip(String),
    // Item被过滤(不存储Item信息), message为过滤原因
    Filtered(String),
    // 处理成功
    Success {
        content: ProcessingContent,
        files: Vec<FileContent>,
    },
    // 处理失败
    #[allow(dead_code)]
    Error(ProcessingError),
}

impl ProcessRuntime {
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
                (Some(start), Some(end)) =>
                    Self::format_duration(end.duration_since(start)),
                _ => "N/A".to_string(),
            },
            match (self.fetch_start_at, self.fetch_end_at) {
                (Some(start), Some(end)) =>
                    Self::format_duration(end.duration_since(start)),
                _ => "N/A".to_string(),
            },
            match (self.fetch_end_at, self.process_end_at) {
                (Some(start), Some(end)) =>
                    Self::format_duration(end.duration_since(start)),
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
        let async_downloader = downloader.clone().as_async_downloader().ok();
        Self {
            name,
            source_id,
            save_path,
            source,
            item_file_resolver,
            downloader,
            async_downloader,
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

    pub fn start_rename_task(self: &Arc<Self>) {
        if self.async_downloader.is_none() {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            warn!("Processor[rename-task-not-started] {} no Tokio runtime", self.name);
            return;
        };
        let interval = self.options.rename_task_interval;
        let processor = Arc::downgrade(self);
        runtime.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let Some(processor) = processor.upgrade() else {
                    break;
                };
                if let Err(error) = processor.run_rename().await {
                    warn!(
                        "Processor[rename-task-error] {} {}",
                        processor.name,
                        error.message()
                    );
                }
            }
        });
    }

    pub async fn run_rename(&self) -> Result<usize, ProcessingError> {
        let Some(async_downloader) = self.async_downloader.as_ref() else {
            warn!("Processor[rename-skip] {} downloader is synchronous", self.name);
            return Ok(0);
        };
        let contents = self
            .processing_storage
            .query_processing_content(&ProcessingContentQuery {
                processor_name: Some(vec![self.name.clone()]),
                rename_times_threshold: Some(self.options.rename_times_threshold),
                status: Some(vec![ProcessingStatus::WaitingToRename]),
                ..Default::default()
            })
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        let had_items = !contents.is_empty();
        let mut listener_context = ListenerContext::new(self);
        let mut finished = 0;

        for mut content in contents {
            match async_downloader.is_finished(&content.item_content.source_item) {
                None => {
                    content.status = ProcessingStatus::DownloadFailed;
                    content.updated_at = Some(OffsetDateTime::now_utc());
                    self.processing_storage
                        .save_processing_content(&content)
                        .await
                        .map_err(|error| ProcessingError::non_retryable(error.message))?;
                    let paths = self.load_content_paths(&content).await?;
                    self.processing_storage
                        .delete_paths(&paths, Some(&content.item_hash))
                        .await
                        .map_err(|error| ProcessingError::non_retryable(error.message))?;
                    listener_context.has_error = true;
                    let error =
                        ProcessingError::non_retryable("Asynchronous download failed");
                    for listener in &self.options.process_listeners {
                        listener.on_item_error(
                            &listener_context,
                            &content.item_content.source_item,
                            &error,
                        );
                    }
                }
                Some(false) => {}
                Some(true) => {
                    finished += 1;
                    match self.process_rename_content(&mut content).await {
                        Ok(files) => {
                            listener_context.add(content, files);
                            let item = listener_context
                                .processed_items
                                .last()
                                .expect("renamed item was just inserted");
                            let completed = listener_context
                                .get_item_content(item)
                                .expect("renamed item content was just inserted");
                            let item_content = ItemContent {
                                source_item: completed.source_item,
                                file_contents: completed.file_contents,
                                item_variables: completed.item_variables,
                                status: *completed.status,
                            };
                            for listener in &self.options.process_listeners {
                                listener
                                    .on_item_success(&listener_context, &item_content);
                            }
                        }
                        Err(error) => {
                            listener_context.has_error = true;
                            for listener in &self.options.process_listeners {
                                listener.on_item_error(
                                    &listener_context,
                                    &content.item_content.source_item,
                                    &error,
                                );
                            }
                            warn!(
                                "Processor[rename-item-error] {} item={} {}",
                                self.name,
                                content.item_content.source_item,
                                error.message()
                            );
                        }
                    }
                }
            }
        }
        if had_items {
            for listener in &self.options.process_listeners {
                listener.on_process_completed(&listener_context);
            }
        }
        Ok(finished)
    }

    async fn load_file_contents(
        &self,
        content: &ProcessingContent,
    ) -> Result<Vec<FileContent>, ProcessingError> {
        let content_id = content.id.ok_or_else(|| {
            ProcessingError::non_retryable("Persisted processing content has no id")
        })?;
        let bytes = self
            .processing_storage
            .find_file_contents(content_id)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?
            .ok_or_else(|| {
                ProcessingError::non_retryable(format!(
                    "File contents not found for processing content {}",
                    content_id
                ))
            })?;
        decode_files_from_compressed(&bytes)
    }

    async fn load_content_paths(
        &self,
        content: &ProcessingContent,
    ) -> Result<Vec<String>, ProcessingError> {
        Ok(self
            .load_file_contents(content)
            .await?
            .iter()
            .map(|file| file.target_path().to_string_lossy().into_owned())
            .collect())
    }

    async fn process_rename_content(
        &self,
        content: &mut ProcessingContent,
    ) -> Result<Vec<FileContent>, ProcessingError> {
        let mut files = self.load_file_contents(content).await?;
        let target_paths = files.iter().map(FileContent::target_path).collect_vec();
        let mut rename_result = None;
        if files.iter().all(|file| file.status != ReadyReplace)
            && self.file_mover.exists(&target_paths).into_iter().all(|exists| exists)
        {
            content.rename_times += 1;
            content.status = ProcessingStatus::TargetAlreadyExists;
            content.updated_at = Some(OffsetDateTime::now_utc());
        } else {
            let process = NormalProcess {};
            process
                .update_file_content_status(
                    self,
                    &content.item_content.source_item,
                    &mut files,
                )
                .await;
            let movement_result = process
                .do_movement(self, &content.item_content.source_item, &files)
                .await;
            let replacement_result = process
                .do_replacement(self, &content.item_content.source_item, &files)
                .await;
            let result = movement_result.and(replacement_result);
            let renamed = result.is_ok();
            rename_result = Some(result);
            content.rename_times += 1;
            content.status = if renamed {
                ProcessingStatus::Renamed
            } else {
                ProcessingStatus::WaitingToRename
            };
            content.updated_at = Some(OffsetDateTime::now_utc());
        }

        self.processing_storage
            .save_processing_content(content)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        if let Some(content_id) = content.id {
            self.processing_storage
                .save_file_contents(content_id, encode_files_and_compress(&files)?)
                .await
                .map_err(|error| ProcessingError::non_retryable(error.message))?;
        }
        if let Some(Err(error)) = rename_result {
            return Err(error);
        }
        let paths = files
            .iter()
            .map(|file| file.target_path().to_string_lossy().into_owned())
            .collect_vec();
        self.processing_storage
            .delete_paths(&paths, None)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        Ok(files)
    }

    async fn save_source_state(
        &self,
        state: &ProcessorSourceState,
    ) -> Result<(), String> {
        self.processing_storage
            .save_processor_source_state(state)
            .await
            .map_err(|x| x.message)
            .map(|_| ())
    }

    async fn advance_source_pointer(
        &self,
        ctx: &mut ProcessRuntime,
        source_item: &SourceItem,
        item_pointer: &dyn ItemPointer,
    ) -> Result<(), ProcessingError> {
        ctx.source_pointer.update(source_item, item_pointer);
        if !self.options.pointer_batch_mode {
            self.save_source_state(&ProcessorSourceState {
                id: ctx.source_state.id,
                processor_name: ctx.source_state.processor_name.to_owned(),
                source_id: ctx.source_state.source_id.to_owned(),
                last_pointer: ctx.source_pointer.dump(),
            })
            .await
            .map_err(ProcessingError::non_retryable)?;
        }
        Ok(())
    }

    pub async fn apply_retry<T, Fut, F>(
        mut f: F,
        stage: &str,
    ) -> Result<T, ProcessingError>
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
    fn select_item_filter<'a>(
        &self,
        p: &'a SourceProcessor,
    ) -> &'a Vec<Arc<dyn SourceItemFilter>>;

    async fn on_process_complete(
        &self,
        p: &SourceProcessor,
        ctx: &ProcessRuntime,
    ) -> Result<(), ProcessingError>;

    async fn on_item_process_complete(
        &self,
        p: &SourceProcessor,
        processing_content: &ProcessingContent,
        files: &Vec<FileContent>,
    ) -> Result<(), ProcessingError>;

    async fn on_item_error(
        &self,
        _p: &SourceProcessor,
        _ctx: &mut ProcessRuntime,
        _item: &SourceItem,
        _err: &ProcessingError,
    ) {
    }

    async fn on_item_filtered(
        &self,
        _p: &SourceProcessor,
        _ctx: &mut ProcessRuntime,
        _source_item: &SourceItem,
        _item_pointer: &dyn ItemPointer,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    async fn on_item_success(
        &self,
        _p: &SourceProcessor,
        _ctx: &mut ProcessRuntime,
        _source_item: &SourceItem,
        _item_pointer: &dyn ItemPointer,
        _content: ProcessingContent,
        _files: Vec<FileContent>,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    async fn execute(&self, p: &SourceProcessor) -> Result<(), ProcessingError> {
        let span_exec = tracing::info_span!("", processor = p.name);
        let start_time = Instant::now();
        let _span_exec_entered = span_exec.enter();
        info!("[run-start] {}({})", p.name, p.instance_id);
        if p.processing.swap(true, Ordering::AcqRel) {
            info!("[run-reject] {}({}) Already processing", p.name, p.instance_id);
            return Err(ProcessingError::non_retryable("Already processing"));
        }
        let _processing_guard = ProcessingGuard::new(&p.processing);
        let mut p_rt = self.init_process_context(p, start_time).await?;
        debug!("Fetch with pointer: {}", p_rt.source_pointer.dump());
        p_rt.fetch_start_at = Some(Instant::now());
        let items = SourceProcessor::apply_retry(
            || async {
                p.source.fetch(p_rt.source_pointer.as_ref(), p.options.fetch_limit).await
            },
            "fetch-source-items",
        )
        .await?;
        p_rt.fetch_end_at = Some(Instant::now());

        for item in items {
            let item_pointer = item.item_pointer;
            let source_item = item.source_item;
            let item_action = match self.process_item(&source_item, &p_rt, p).await {
                Ok(action) => action,
                Err(error) => ItemAction::Error(error),
            };
            match item_action {
                ItemAction::Skip(reason) => {
                    debug!("[item-skip] {} {:?} ", reason, source_item);
                    continue;
                }
                ItemAction::Filtered(reason) => {
                    debug!("[item-filtered] {} {:?} ", reason, source_item);
                    self.on_item_filtered(
                        p,
                        &mut p_rt,
                        &source_item,
                        item_pointer.as_ref(),
                    )
                    .await?;
                    continue;
                }
                ItemAction::Error(err) => {
                    p_rt.processed_inc();
                    p_rt.listener_context.has_error = true;
                    self.on_item_error(p, &mut p_rt, &source_item, &err).await;
                    if matches!(err, ProcessingError::NonRetryable { skip: true, .. }) {
                        warn!(
                            "[item-skip-on-error] 异常为可跳过类型 {} {}",
                            err.message(),
                            source_item
                        );
                        continue;
                    }
                    warn!(
                        "[item-non-retryable-error] 异常为不可跳过类型 {}, 退出本次触发处理",
                        err.message()
                    );
                    break;
                }
                ItemAction::Success { content, files } => {
                    p_rt.processed_inc();
                    self.on_item_success(
                        p,
                        &mut p_rt,
                        &source_item,
                        item_pointer.as_ref(),
                        content,
                        files,
                    )
                    .await?;
                }
            }
        }
        self.on_process_complete(p, &p_rt).await?;
        p_rt.process_end_at = Some(Instant::now());
        info!("[run-done] {} {}", p.name, p_rt.summary());
        Ok(())
    }

    async fn get_source_state(
        &self,
        p: &SourceProcessor,
    ) -> Result<ProcessorSourceState, ProcessingError> {
        Ok(p.processing_storage
            .find_processor_source_state(&p.name, &p.source_id)
            .await
            .map_err(|x| ProcessingError::non_retryable(x.message))?
            .unwrap_or(ProcessorSourceState {
                id: None,
                processor_name: p.name.to_owned(),
                source_id: p.source_id.to_owned(),
                last_pointer: p.source.default_pointer().dump(),
            }))
    }

    async fn get_source_pointer(
        &self,
        p: &SourceProcessor,
        source_state: &ProcessorSourceState,
    ) -> Result<Box<dyn SourcePointer>, ProcessingError> {
        let source_pointer =
            p.source.parse_raw_pointer(source_state.last_pointer.to_owned());
        Ok(source_pointer)
    }

    async fn init_process_context(
        &self,
        p: &SourceProcessor,
        start_time: Instant,
    ) -> Result<ProcessRuntime, ProcessingError> {
        let source_state = self.get_source_state(p).await?;
        let source_pointer = self.get_source_pointer(p, &source_state).await?;
        let p_ctx = ProcessRuntime {
            trace_id: PROCESS_ID_GENERATOR
                .fetch_add(i64::MIN, Ordering::Relaxed)
                .to_string(),
            mutex: Mutex::new(()),
            source_state,
            source_pointer,
            process_submitted_items: RwLock::new(HashSet::new()),
            processed_count: AtomicU32::new(0),
            filter_count: AtomicU32::new(0),
            process_start_at: Some(start_time),
            process_end_at: None,
            fetch_start_at: None,
            fetch_end_at: None,
            cancel_items: vec![],
            listener_context: ListenerContext::new(p),
        };
        Ok(p_ctx)
    }

    async fn identify_files_to_replace(
        &self,
        p: &SourceProcessor,
        source_item: &SourceItem,
        files: &mut [FileContent],
    ) -> Result<usize, ProcessingError> {
        let candidate_indices = files
            .iter()
            .enumerate()
            .filter_map(|(index, file)| {
                (file.status == TargetExists && file.exist_target_path.is_some())
                    .then_some(index)
            })
            .collect_vec();
        if candidate_indices.is_empty() {
            return Ok(0);
        }
        let existing_paths = candidate_indices
            .iter()
            .map(|index| {
                files[*index]
                    .exist_target_path
                    .as_ref()
                    .expect("replacement candidate has an existing target path")
            })
            .collect_vec();
        let physical_exists = p.file_mover.exists(&existing_paths);
        let path_strings = existing_paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect_vec();
        let current_hash = source_item.hashing();
        let relations = p
            .processing_storage
            .find_paths(&path_strings)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?
            .into_iter()
            .filter(|relation| relation.item_hash != current_hash)
            .map(|relation| (relation.path, relation.item_hash))
            .collect::<HashMap<_, _>>();
        let item_hashes = relations.values().cloned().unique().collect_vec();
        let prior_contents = if item_hashes.is_empty() {
            Vec::new()
        } else {
            p.processing_storage
                .query_processing_content(&ProcessingContentQuery {
                    item_hash: Some(item_hashes),
                    status: Some(vec![ProcessingStatus::Renamed]),
                    ..Default::default()
                })
                .await
                .map_err(|error| ProcessingError::non_retryable(error.message))?
        };
        let mut prior_by_hash = HashMap::new();
        for content in prior_contents {
            if prior_by_hash.contains_key(&content.item_hash) {
                continue;
            }
            let content_id = content.id.ok_or_else(|| {
                ProcessingError::non_retryable("Persisted replacement content has no id")
            })?;
            let encoded_files = p
                .processing_storage
                .find_file_contents(content_id)
                .await
                .map_err(|error| ProcessingError::non_retryable(error.message))?
                .ok_or_else(|| {
                    ProcessingError::non_retryable(format!(
                        "File contents not found for replacement content {}",
                        content_id
                    ))
                })?;
            let key = content.item_hash.to_owned();
            prior_by_hash
                .insert(key, (content, decode_files_from_compressed(&encoded_files)?));
        }

        let mut replacement_count = 0;
        for ((index, physical_exists), path) in
            candidate_indices.into_iter().zip(physical_exists).zip(path_strings)
        {
            let file = &mut files[index];
            let existing_path = file
                .exist_target_path
                .as_ref()
                .expect("replacement candidate has an existing target path");
            let existing_file = if physical_exists {
                p.file_mover.path_metadata(existing_path)?
            } else {
                SourceFile::new(existing_path.to_path_buf())
            };
            let before =
                relations.get(&path).and_then(|item_hash| prior_by_hash.get(item_hash));
            let before_view = before.map(|(content, files)| InProcessingItem {
                id: &content.id,
                processor_name: &content.processor_name,
                item_hash: &content.item_hash,
                item_identity: &content.item_identity,
                source_item: &content.item_content.source_item,
                item_variables: &content.item_content.item_variables,
                file_contents: files,
                rename_times: &content.rename_times,
                status: &content.status,
                failure_reason: content.failure_reason.as_deref(),
            });
            if p.options.file_replacement_decider.should_replace(
                source_item,
                file,
                before_view.as_ref(),
                &existing_file,
            ) {
                file.status = ReadyReplace;
                replacement_count += 1;
            }
        }
        Ok(replacement_count)
    }

    async fn process_item(
        &self,
        source_item: &SourceItem,
        rt: &ProcessRuntime,
        p: &SourceProcessor,
    ) -> Result<ItemAction, ProcessingError> {
        let item_hash = source_item.hashing();
        if rt.process_submitted_items.read().contains(&item_hash) {
            rt.filter_inc();
            debug!("Source item duplicated: {:?} skipped", source_item);
            return Ok(ItemAction::Skip("Source item duplicated".to_string()));
        }
        rt.process_submitted_items.write().insert(item_hash);

        debug!("[item-start] {}", source_item);
        let opt = &p.options;
        let item_rule = opt.item_rules.iter().find(|x| x.matcher.matches(source_item));
        let item_strategy = item_rule.map(|x| &x.strategy);
        let item_filters = item_strategy
            .map(|x| x.item_filters.as_ref())
            .flatten()
            .unwrap_or(&opt.item_filters);
        for filter in item_filters {
            let filtered = !filter.filter(source_item).await;
            if filtered {
                debug!("[item-filtered] {}", source_item);
                rt.filter_inc();
                return Ok(ItemAction::Filtered(format!("Filtered by: {}", filter)));
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
        let mut file_contents = self
            .process_source_files(
                p,
                source_item,
                &item_variables,
                resolved_files,
                item_strategy,
            )
            .await?;

        let mut content_status = ProcessingStatus::WaitingToRename;
        let mut failure_reason: Option<String> = None;
        let item_content = ItemContent {
            source_item,
            file_contents: &file_contents,
            item_variables: &item_variables,
            status: content_status,
        };
        for x in &opt.item_content_filters {
            let filtered = !x.filter(&item_content).await;
            if filtered {
                debug!("[item-content-filtered] {}", source_item);
                rt.filter_inc();
                content_status = ProcessingStatus::Filtered;
                failure_reason = Some(format!("Filtered by: {}", x));
                break;
            }
        }
        //  ==== 数据准备阶段结束, 开始决定是否下载
        if content_status != ProcessingStatus::Filtered {
            // 1. 根据目标文件路径更新file_content状态
            self.update_file_content_status(p, source_item, &mut file_contents).await;
        }
        let (should_download, mut content_status) = {
            let _guard = rt.mutex.lock().await;
            self.identify_files_to_replace(p, source_item, &mut file_contents).await?;
            self.probe_content_status(p, rt, source_item, &file_contents)
        };
        let mut rename_times = 0;
        if should_download {
            self.do_download(p, source_item, &file_contents).await?;
            let is_sync = p.async_downloader.is_none();
            if is_sync {
                let movement_res = self.do_movement(p, source_item, &file_contents).await;
                let replacement_res =
                    self.do_replacement(p, source_item, &file_contents).await;
                let has_replacements =
                    file_contents.iter().any(|file| file.status == ReadyReplace);
                let postprocessing_succeeded = if has_replacements {
                    movement_res.is_ok() && replacement_res.is_ok()
                } else {
                    movement_res.is_ok() || replacement_res.is_ok()
                };
                if postprocessing_succeeded {
                    content_status = ProcessingStatus::Renamed;
                    rename_times = 1;
                } else {
                    content_status = ProcessingStatus::Failure;
                }
            }
        }

        let content = ProcessingContent {
            id: None,
            processor_name: p.name.clone(),
            item_hash: source_item.hashing(),
            item_identity: source_item.identity.clone(),
            item_content: ItemContentLite {
                source_item: source_item.clone(),
                item_variables,
            },
            rename_times,
            status: content_status,
            failure_reason,
            created_at: OffsetDateTime::now_utc(),
            updated_at: None,
        };

        self.on_item_process_complete(p, &content, &file_contents).await?;

        Ok(ItemAction::Success { files: file_contents, content })
    }

    async fn do_movement(
        &self,
        p: &SourceProcessor,
        source_item: &SourceItem,
        file_contents: &[FileContent],
    ) -> Result<(), ProcessingError> {
        let movable_files: Vec<&FileContent> = file_contents
            .iter()
            .filter(|file| {
                file.status == Normal && file.file_download_path != *file.target_path()
            })
            .collect();
        if movable_files.is_empty() {
            return Ok(());
        }

        let mut directories = HashSet::new();
        for file in &movable_files {
            if directories.insert(file.target_save_path.as_path()) {
                p.file_mover.create_directories(&file.target_save_path)?;
            }
        }

        if p.file_mover.is_supported_batch_move() {
            return p.file_mover.batch_move(source_item, &movable_files);
        }
        for file in movable_files {
            p.file_mover.move_file(source_item, file)?;
        }
        Ok(())
    }

    async fn do_replacement(
        &self,
        p: &SourceProcessor,
        source_item: &SourceItem,
        file_contents: &[FileContent],
    ) -> Result<(), ProcessingError> {
        let replacement_files =
            file_contents.iter().filter(|file| file.status == ReadyReplace).collect_vec();
        p.file_mover.replace(source_item, &replacement_files)
    }

    async fn do_download(
        &self,
        p: &SourceProcessor,
        source_item: &SourceItem,
        file_contents: &[FileContent],
    ) -> Result<(), ProcessingError> {
        let downloadable_files: Vec<&FileContent> = file_contents
            .iter()
            .filter(|file| file.status != TargetExists && file.status != Downloaded)
            .collect();
        let all_files: Vec<SourceFileRef> =
            downloadable_files.iter().map(|file| (*file).into()).collect();

        let (direct_files, download_files): (Vec<_>, Vec<_>) =
            all_files.into_iter().partition(|f| f.data.is_some());
        for direct_file in direct_files {
            if let Some(data) = direct_file.data {
                if let Some(parent) = direct_file.path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                let mut f = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&direct_file.path)
                    .await?;
                f.write_all(data).await?;
                f.flush().await?;
            }
        }

        let source_headers = p.source.headers(source_item);
        let options = &p.options.download_options;
        let headers: Option<HashMap<&String, &String>> =
            match (&options.headers, &source_headers) {
                (None, None) => None,
                (h1, h2) => {
                    let mut merged = HashMap::new();
                    if let Some(map1) = h1 {
                        for (k, v) in map1 {
                            merged.insert(k, v);
                        }
                    }
                    if let Some(map2) = h2 {
                        for (k, v) in map2 {
                            merged.insert(k, v);
                        }
                    }
                    Some(merged)
                }
            };

        let opt = DownloadTask {
            source_item,
            download_files: &download_files,
            download_path: p.download_path.as_ref(),
            category: &options.category,
            tags: options.tags.as_deref(),
            headers,
        };
        p.downloader.submit(&opt).await?;
        if p.options.save_processing_content {
            let item_hash = source_item.hashing();
            let paths = downloadable_files
                .into_iter()
                .map(|file| ProcessingTargetPath {
                    path: file.target_path().to_string_lossy().into_owned(),
                    processor_name: p.name.clone(),
                    item_hash: item_hash.clone(),
                })
                .collect();
            p.processing_storage.save_paths(paths).await.map_err(|error| {
                ProcessingError::non_retryable(format!(
                    "Failed to save target paths: {}",
                    error.message
                ))
            })?;
        }
        Ok(())
    }

    async fn update_file_content_status(
        &self,
        p: &SourceProcessor,
        source_item: &SourceItem,
        file_contents: &mut Vec<FileContent>,
    ) {
        let conflict_indices: HashSet<usize> = {
            let mut path_to_indices: HashMap<&Path, Vec<usize>> = HashMap::new();

            for (idx, f) in
                file_contents.iter().enumerate().filter(|(_, f)| f.status == Undetected)
            {
                path_to_indices.entry(f.target_path()).or_default().push(idx);
            }

            path_to_indices
                .into_values()
                .filter(|indices| indices.len() > 1)
                .flatten()
                .collect()
        };

        for (idx, x) in file_contents.iter_mut().enumerate() {
            if x.status != Undetected {
                continue;
            }
            if !x.errors.is_empty() {
                x.status = VariableError;
                continue;
            }
            if conflict_indices.contains(&idx) {
                x.status = FileConflict;
                continue;
            }
        }

        let updates = self.build_exists_updates(p, source_item, file_contents).await;

        for (idx, exists_path_opt) in updates {
            let x = &mut file_contents[idx];
            if x.status != Undetected {
                continue;
            }

            if let Some(exists_path) = exists_path_opt {
                x.status = TargetExists;
                x.exist_target_path = Some(exists_path);
            } else {
                x.status = Normal;
            }
        }
    }

    /// 核心优化点：将原来返回 HashMap<&PathBuf, ...> 改造为返回具体的更新指令 (索引, Option<路径>)
    async fn build_exists_updates(
        &self,
        p: &SourceProcessor,
        source_item: &SourceItem,
        file_contents: &[FileContent],
    ) -> Vec<(usize, Option<PathBuf>)> {
        let mut target_paths = Vec::new();
        let mut indices = Vec::new();

        // 收集待检查的路径和它们对应的索引
        for (idx, f) in file_contents.iter().enumerate() {
            if f.status == Undetected {
                target_paths.push(f.target_path());
                indices.push(idx);
            }
        }

        if target_paths.is_empty() {
            return Vec::new();
        }

        let exists_results = p.file_mover.exists(&target_paths);

        // 性能优化：使用两个并行数组暂存结果，而不是昂贵的 HashMap
        let mut exists_out: Vec<Option<&PathBuf>> = target_paths
            .iter()
            .zip(exists_results)
            .map(|(&path, exists)| if exists { Some(path) } else { None })
            .collect();

        // 如果开启了高级检测器，再进行覆写合并
        if (*p.options.file_exists_detector).type_id()
            != TypeId::of::<SimpleFileExistsDetector>()
        {
            let detector_results = p.options.file_exists_detector.exists(
                p.file_mover.as_ref(),
                source_item,
                file_contents,
            );

            // 仅在此时建立一个局部反查表
            let path_to_local_idx: HashMap<&PathBuf, usize> =
                target_paths.iter().enumerate().map(|(i, &path)| (path, i)).collect();

            for (path, exists_path) in detector_results {
                if let Some(&local_idx) = path_to_local_idx.get(path) {
                    // 如果 file_mover 认为已存在，detector 不能覆盖
                    if exists_out[local_idx].is_none() {
                        exists_out[local_idx] = exists_path;
                    }
                }
            }
        }

        // 将并行数组打包返回，并在真正需要时才做 PathBuf 的克隆分配
        indices
            .into_iter()
            .zip(exists_out)
            .map(|(idx, path_opt)| (idx, path_opt.map(|p| p.to_path_buf())))
            .collect()
    }

    fn probe_content_status(
        &self,
        p: &SourceProcessor,
        rt: &ProcessRuntime,
        source_item: &SourceItem,
        files: &[FileContent],
    ) -> (bool, ProcessingStatus) {
        if files.is_empty() {
            return (false, ProcessingStatus::NoFiles);
        };
        if files.iter().any(|file| file.status == ReadyReplace) {
            return (true, ProcessingStatus::WaitingToRename);
        };
        if rt.cancel_items.contains(source_item) {
            return (false, ProcessingStatus::Cancelled);
        }
        // 预防这一批次的Item有相同的目标，并且是AsyncDownloader的情况下会重复下载
        if files.iter().all(|x| x.status == TargetExists) {
            warn!(
                "Item files already exists:{}, files:{:?}",
                source_item,
                files.iter().map(|f| f.target_path.get()).collect_vec()
            );
            return (false, ProcessingStatus::TargetAlreadyExists);
        }

        let file_download_paths =
            files.iter().map(|f| &f.file_download_path).collect_vec();
        let all_exists = p.file_mover.exists(&file_download_paths).into_iter().all(|x| x);
        if all_exists {
            let is_async = p.async_downloader.is_some();
            return (is_async, ProcessingStatus::WaitingToRename);
        }
        (true, ProcessingStatus::WaitingToRename)
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

        // <editor-fold desc="Stage using VariableProviders for file">
        let mut file_raw_vars = vec![];
        for idx in 0..opt.variable_providers.len() {
            let v = opt.variable_providers.get(idx).expect(
                "Failed to get variable provider by index, this should not happen",
            );
            let vars =
                v.file_variables(source_item, item_variables, &relative_files).await;
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
        // </editor-fold>
        let mut result: Vec<FileContent> = vec![];

        let item_var =
            p.renamer.item_rename_variables(source_item, item_variables);

        let empty_vars = &PatternVariables::new();
        let file_count = relative_files.len();
        for (idx, x) in relative_files.into_iter().enumerate() {
            let var = file_vars.get(idx).unwrap_or_else(|| empty_vars);
            let file_rule =
                opt.file_rules.iter().find(|rule| rule.matcher.matches(&x, file_count));
            let file_strategy = file_rule.map(|r| &r.strategy);

            // Determine save_path_pattern and filename_pattern for this file
            let file_save_path_pattern = file_strategy
                .map(|s| s.save_path_pattern.clone())
                .flatten()
                .or_else(|| {
                    item_group_options.map(|s| s.save_path_pattern.clone()).flatten()
                })
                .unwrap_or(opt.save_path_pattern.clone());
            let file_filename_pattern = file_strategy
                .map(|s| s.filename_pattern.clone())
                .flatten()
                .or_else(|| {
                    item_group_options.map(|s| s.filename_pattern.clone()).flatten()
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

            // <editor-fold desc="Stage using FileContentFilter">
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
            // </editor-fold>
            result.push(content)
        }
        Ok(result)
    }
}

#[allow(dead_code)]
struct NormalProcess {}

impl Process for NormalProcess {
    fn select_item_filter<'a>(
        &self,
        p: &'a SourceProcessor,
    ) -> &'a Vec<Arc<dyn SourceItemFilter>> {
        &p.options.item_filters
    }

    async fn on_process_complete(
        &self,
        p: &SourceProcessor,
        ctx: &ProcessRuntime,
    ) -> Result<(), ProcessingError> {
        if p.options.pointer_batch_mode
            || ctx.processed_count.load(Ordering::Acquire) == 0
        {
            p.save_source_state(&ProcessorSourceState {
                id: ctx.source_state.id,
                processor_name: ctx.source_state.processor_name.to_owned(),
                source_id: ctx.source_state.source_id.to_owned(),
                last_pointer: ctx.source_pointer.dump(),
            })
            .await
            .map_err(ProcessingError::non_retryable)?;
        }
        for listener in &p.options.process_listeners {
            listener.on_process_completed(&ctx.listener_context);
        }
        Ok(())
    }

    async fn on_item_process_complete(
        &self,
        p: &SourceProcessor,
        processing_content: &ProcessingContent,
        files: &Vec<FileContent>,
    ) -> Result<(), ProcessingError> {
        debug!("[item-done] {:?}", &processing_content.item_content.source_item);
        if !p.options.save_processing_content {
            return Ok(());
        }
        let content_id = p
            .processing_storage
            .save_processing_content(processing_content)
            .await
            .map_err(|error| {
                ProcessingError::non_retryable(format!(
                    "Failed to save item content {}",
                    error.message
                ))
            })?;
        p.processing_storage
            .save_file_contents(content_id, encode_files_and_compress(files)?)
            .await
            .map_err(|error| {
                ProcessingError::non_retryable(format!(
                    "Failed to save file contents {}",
                    error.message
                ))
            })
    }

    async fn on_item_error(
        &self,
        p: &SourceProcessor,
        ctx: &mut ProcessRuntime,
        item: &SourceItem,
        error: &ProcessingError,
    ) {
        for listener in &p.options.process_listeners {
            listener.on_item_error(&ctx.listener_context, item, error);
        }
    }

    async fn on_item_filtered(
        &self,
        p: &SourceProcessor,
        ctx: &mut ProcessRuntime,
        source_item: &SourceItem,
        item_pointer: &dyn ItemPointer,
    ) -> Result<(), ProcessingError> {
        p.advance_source_pointer(ctx, source_item, item_pointer).await
    }

    async fn on_item_success(
        &self,
        p: &SourceProcessor,
        ctx: &mut ProcessRuntime,
        source_item: &SourceItem,
        item_pointer: &dyn ItemPointer,
        content: ProcessingContent,
        files: Vec<FileContent>,
    ) -> Result<(), ProcessingError> {
        p.advance_source_pointer(ctx, source_item, item_pointer).await?;
        ctx.listener_context.add(content, files);
        let completed = ctx
            .listener_context
            .get_item_content(source_item)
            .expect("completed item was just inserted");
        let item_content = ItemContent {
            source_item: completed.source_item,
            file_contents: completed.file_contents,
            item_variables: completed.item_variables,
            status: *completed.status,
        };
        for listener in &p.options.process_listeners {
            listener.on_item_success(&ctx.listener_context, &item_content);
        }
        Ok(())
    }
}

impl NormalProcess {}

pub fn encode_files_and_compress(
    files: &Vec<FileContent>,
) -> Result<Vec<u8>, ProcessingError> {
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
pub fn decode_files_from_compressed(
    bytes: &[u8],
) -> Result<Vec<FileContent>, ProcessingError> {
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

#[allow(dead_code)]
struct DryRunProcess {}
#[allow(dead_code)]
struct Reprocess {}
#[allow(dead_code)]
struct FixedItemProcess {}

#[cfg(test)]
mod test {
    use super::*;
    use crate::config::ConfigOperator;
    use crate::process::variable::SmartStrategy;
    use crate::processor_test_support::test_support::*;
    use jsonpath_rust::JsonPath;
    use parking_lot::Mutex as ParkingMutex;
    use source_downloader_sdk::component::PointedItem;
    use source_downloader_sdk::http::Uri;
    use source_downloader_sdk::serde_json::{Value, json};
    use source_downloader_sdk::storage::{
        Error as StorageError, ProcessingContentQuery, ProcessingTargetPath,
    };
    use std::any::Any;
    use std::fmt::{Display, Formatter};
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    #[derive(Debug)]
    struct PointerItem(usize);

    impl ItemPointer for PointerItem {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[derive(Default)]
    struct TestSourcePointer(usize);

    impl SourcePointer for TestSourcePointer {
        fn dump(&self) -> Value {
            json!(self.0)
        }

        fn update(&mut self, _: &SourceItem, item_pointer: &dyn ItemPointer) {
            let item_pointer = item_pointer
                .as_any()
                .downcast_ref::<PointerItem>()
                .expect("pointer test item type");
            self.0 = item_pointer.0;
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[derive(Debug)]
    struct PointerTestComponent {
        item_count: usize,
    }

    impl Display for PointerTestComponent {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "pointer-test")
        }
    }

    impl source_downloader_sdk::component::SdComponent for PointerTestComponent {
        fn as_async_downloader(
            self: Arc<Self>,
        ) -> Result<
            Arc<dyn AsyncDownloader>,
            source_downloader_sdk::component::ComponentError,
        > {
            Ok(self)
        }
    }

    #[async_trait]
    impl Source for PointerTestComponent {
        async fn fetch<'pointer>(
            &self,
            _: &'pointer dyn SourcePointer,
            _: u32,
        ) -> Result<Vec<PointedItem>, ProcessingError> {
            Ok((1..=self.item_count)
                .map(|sequence| PointedItem {
                    source_item: SourceItem {
                        title: format!("item-{sequence}"),
                        link: Uri::from_static("http://localhost/item"),
                        datetime: OffsetDateTime::UNIX_EPOCH,
                        content_type: "test".to_string(),
                        download_uri: Uri::from_static("http://localhost/download"),
                        attrs: Default::default(),
                        tags: Vec::new(),
                        identity: None,
                    },
                    item_pointer: Arc::new(PointerItem(sequence)),
                })
                .collect())
        }

        fn default_pointer(&self) -> Box<dyn SourcePointer> {
            Box::new(TestSourcePointer::default())
        }

        fn parse_raw_pointer(&self, value: Value) -> Box<dyn SourcePointer> {
            Box::new(TestSourcePointer(value.as_u64().unwrap_or_default() as usize))
        }
    }

    #[async_trait]
    impl ItemFileResolver for PointerTestComponent {
        async fn resolve_files(&self, _: &SourceItem) -> Vec<SourceFile> {
            Vec::new()
        }
    }

    #[async_trait]
    impl Downloader for PointerTestComponent {
        async fn submit(&self, _: &DownloadTask) -> Result<(), ProcessingError> {
            Ok(())
        }

        fn default_download_path(&self) -> &str {
            "/tmp/source-downloader-pointer-test"
        }

        async fn cancel(
            &self,
            _: &SourceItem,
            _: &[SourceFile],
        ) -> Result<(), ProcessingError> {
            Ok(())
        }
    }

    impl AsyncDownloader for PointerTestComponent {
        fn is_finished(&self, _: &SourceItem) -> Option<bool> {
            Some(true)
        }
    }

    impl FileMover for PointerTestComponent {
        fn exists(&self, paths: &[&PathBuf]) -> Vec<bool> {
            vec![false; paths.len()]
        }
    }

    #[derive(Debug)]
    struct RejectAllItems;

    impl Display for RejectAllItems {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "reject-all-items")
        }
    }

    impl source_downloader_sdk::component::SdComponent for RejectAllItems {}

    #[async_trait]
    impl SourceItemFilter for RejectAllItems {
        async fn filter(&self, _: &SourceItem) -> bool {
            false
        }
    }

    #[derive(Default)]
    struct PointerStorage {
        states: ParkingMutex<Vec<ProcessorSourceState>>,
        next_content_id: AtomicUsize,
    }

    impl PointerStorage {
        fn saved_pointers(&self) -> Vec<Value> {
            self.states.lock().iter().map(|state| state.last_pointer.clone()).collect()
        }
    }

    #[async_trait]
    impl ProcessingStorage for PointerStorage {
        async fn save_processing_content(
            &self,
            _: &ProcessingContent,
        ) -> Result<i64, StorageError> {
            Ok(self.next_content_id.fetch_add(1, AtomicOrdering::Relaxed) as i64)
        }

        async fn processing_content_exists(
            &self,
            _: &str,
            _: &str,
        ) -> Result<bool, StorageError> {
            Ok(false)
        }

        async fn delete_processing_content(&self, _: i64) -> Result<(), StorageError> {
            Ok(())
        }

        async fn find_by_name_and_hash(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<ProcessingContent>, StorageError> {
            Ok(None)
        }

        async fn find_content_by_id(
            &self,
            _: i64,
        ) -> Result<Option<ProcessingContent>, StorageError> {
            Ok(None)
        }

        async fn query_processing_content(
            &self,
            _: &ProcessingContentQuery,
        ) -> Result<Vec<ProcessingContent>, StorageError> {
            Ok(Vec::new())
        }

        async fn save_file_contents(
            &self,
            _: i64,
            _: Vec<u8>,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn find_file_contents(
            &self,
            _: i64,
        ) -> Result<Option<Vec<u8>>, StorageError> {
            Ok(None)
        }

        async fn find_processor_source_state(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<ProcessorSourceState>, StorageError> {
            Ok(None)
        }

        async fn save_processor_source_state(
            &self,
            state: &ProcessorSourceState,
        ) -> Result<ProcessorSourceState, StorageError> {
            self.states.lock().push(state.clone());
            Ok(state.clone())
        }

        async fn save_paths(
            &self,
            _: Vec<ProcessingTargetPath>,
        ) -> Result<(), StorageError> {
            Ok(())
        }
    }

    fn pointer_test_processor(
        pointer_batch_mode: bool,
        item_count: usize,
        filter_items: bool,
    ) -> (SourceProcessor, Arc<PointerStorage>) {
        let component = Arc::new(PointerTestComponent { item_count });
        let storage = Arc::new(PointerStorage::default());
        let item_filters: Vec<Arc<dyn SourceItemFilter>> =
            if filter_items { vec![Arc::new(RejectAllItems)] } else { Vec::new() };
        let processor = SourceProcessor::new(
            "pointer-test".to_string(),
            "pointer-test-source".to_string(),
            PathBuf::from("/tmp/source-downloader-pointer-test").into_boxed_path(),
            component.clone(),
            component.clone(),
            component.clone(),
            component,
            storage.clone(),
            None,
            HashSet::new(),
            ProcessorOptions {
                save_path_pattern: Arc::new(PathPattern::new_cel(String::new())),
                filename_pattern: Arc::new(PathPattern::new_cel(String::new())),
                variable_providers: Vec::new(),
                item_filters,
                item_content_filters: Vec::new(),
                source_file_filters: Vec::new(),
                file_content_filters: Vec::new(),
                file_taggers: Vec::new(),
                variable_aggregation: VariableAggregation::new(
                    Box::new(SmartStrategy),
                    HashMap::new(),
                ),
                save_processing_content: false,
                rename_task_interval: Duration::from_secs(300),
                rename_times_threshold: 3,
                parallelism: 1,
                task_group: None,
                fetch_limit: 50,
                item_error_continue: false,
                pointer_batch_mode,
                item_rules: Vec::new(),
                file_rules: Vec::new(),
                process_listeners: Vec::new(),
                file_exists_detector: Arc::new(SimpleFileExistsDetector {}),
                file_replacement_decider: Arc::new(
                    crate::components::never_replace_decider::NeverReplaceDecider,
                ),
                download_options: DownloadOptions {
                    category: None,
                    tags: None,
                    headers: None,
                },
            },
        );
        (processor, storage)
    }

    #[tokio::test]
    async fn pointer_batch_mode_saves_once_after_fetch() {
        let (processor, storage) = pointer_test_processor(true, 2, false);

        processor.run().await.unwrap();

        assert_eq!(storage.saved_pointers(), vec![json!(2)]);
    }

    #[tokio::test]
    async fn non_batch_pointer_mode_saves_after_each_item() {
        let (processor, storage) = pointer_test_processor(false, 2, false);

        processor.run().await.unwrap();

        assert_eq!(storage.saved_pointers(), vec![json!(1), json!(2)]);
    }

    #[tokio::test]
    async fn filtered_item_advances_pointer() {
        let (processor, storage) = pointer_test_processor(false, 1, true);

        processor.run().await.unwrap();

        assert_eq!(storage.saved_pointers(), vec![json!(1), json!(1)]);
    }

    // <editor-fold desc="Sync item content tests">
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn sync_downloader_case() {
        let cfg = cfg();
        let pm = processor_manager().await;
        let storage = storage().await;
        for (name, case) in CASES.iter() {
            pm.create_processor(
                &cfg.get_processor_config(name).expect("Failed to get processor config"),
            );
            let p = assert_processor(name, pm);
            let root_path =
                V_PATH.join(format!("/{}", name)).expect("Failed to join path");
            apply_case_files(&root_path, &case.files);

            let result = p.run().await;
            assert!(result.is_ok());

            let content = build_result_json(storage, name).await;
            for (assert_idx, assertion) in case.assertions.iter().enumerate() {
                let selection = content.query(&assertion.select).unwrap_or_default();
                if !assertion.allow_empty && selection.is_empty() {
                    let err =
                        AssertionError::new("Selection result is empty".to_string())
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
    // </editor-fold>
    #[derive(Debug, Default)]
    struct RecordingListener {
        successes: AtomicUsize,
        errors: AtomicUsize,
        completions: AtomicUsize,
        context_visible: AtomicBool,
    }

    impl Display for RecordingListener {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "recording-listener")
        }
    }

    impl source_downloader_sdk::component::SdComponent for RecordingListener {}

    impl ProcessListener for RecordingListener {
        fn on_item_success(&self, ctx: &dyn ProcessContext, item_content: &ItemContent) {
            self.successes.fetch_add(1, AtomicOrdering::Relaxed);
            self.context_visible.store(
                ctx.get_item_content(item_content.source_item).is_some(),
                AtomicOrdering::Relaxed,
            );
        }

        fn on_item_error(
            &self,
            _: &dyn ProcessContext,
            _: &SourceItem,
            _: &ProcessingError,
        ) {
            self.errors.fetch_add(1, AtomicOrdering::Relaxed);
        }

        fn on_process_completed(&self, ctx: &dyn ProcessContext) {
            assert_eq!(ctx.processed_items().len(), 1);
            self.completions.fetch_add(1, AtomicOrdering::Relaxed);
        }
    }

    #[tokio::test]
    async fn process_notifies_item_and_completion_listeners() {
        let (mut processor, _) = pointer_test_processor(false, 1, false);
        let listener = Arc::new(RecordingListener::default());
        processor.options.process_listeners = vec![listener.clone()];

        processor.run().await.unwrap();

        assert_eq!(listener.successes.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(listener.errors.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(listener.completions.load(AtomicOrdering::Relaxed), 1);
        assert!(listener.context_visible.load(AtomicOrdering::Relaxed));
    }

    // <editor-fold desc="Flow control tests">
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn flow_ctr_retry_then_ok() {
        let name = "flow_ctr_retry_then_ok";
        let cfg =
            cfg().get_processor_config(name).expect("Failed to get processor config");
        let pm = processor_manager().await;
        pm.create_processor(&cfg);
        let p = assert_processor(name, pm);
        let r = p.run().await;
        assert!(r.is_ok());
        assert!(logs_contain("Retrying fetch-source-items delay"));
    }
    // </editor-fold>
    #[tokio::test]
    async fn async_rename_moves_download_and_completes_record() {
        use std::fs;
        use std::sync::OnceLock;

        let root = std::env::temp_dir()
            .join(format!("source-downloader-async-rename-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let download_dir = root.join("download");
        let target_dir = root.join("target");
        fs::create_dir_all(&download_dir).unwrap();
        fs::create_dir_all(&target_dir).unwrap();
        let download_file = download_dir.join("file.txt");
        fs::write(&download_file, b"content").unwrap();
        let replacement_download_file = download_dir.join("replacement.txt");
        let replacement_target_file = target_dir.join("replacement.txt");
        fs::write(&replacement_download_file, b"new-content").unwrap();
        fs::write(&replacement_target_file, b"old-content").unwrap();

        let source_item = SourceItem {
            title: "async-item".to_owned(),
            link: Uri::from_static("https://example.com"),
            datetime: OffsetDateTime::now_utc(),
            content_type: "text/plain".to_owned(),
            download_uri: Uri::from_static("https://example.com/file"),
            attrs: Default::default(),
            tags: Vec::new(),
            identity: None,
        };
        let file = FileContent {
            download_path: download_dir.clone(),
            file_download_path: download_file.clone(),
            source_save_path: target_dir.clone(),
            pattern_variables: HashMap::new(),
            tags: Vec::new(),
            attrs: Default::default(),
            file_uri: None,
            target_save_path: target_dir.clone(),
            target_filename: "renamed.txt".to_owned(),
            exist_target_path: None,
            errors: Vec::new(),
            status: Normal,
            target_path: OnceLock::new(),
            data: None,
        };
        let replacement_file = FileContent {
            download_path: download_dir,
            file_download_path: replacement_download_file.clone(),
            source_save_path: target_dir.clone(),
            pattern_variables: HashMap::new(),
            tags: Vec::new(),
            attrs: Default::default(),
            file_uri: None,
            target_save_path: target_dir.clone(),
            target_filename: "replacement.txt".to_owned(),
            exist_target_path: Some(replacement_target_file.clone()),
            errors: Vec::new(),
            status: ReadyReplace,
            target_path: OnceLock::new(),
            data: None,
        };
        let processor_name = "async-rename-test";
        let storage = storage().await.clone();
        let mut content = ProcessingContent {
            id: None,
            processor_name: processor_name.to_owned(),
            item_hash: source_item.hashing(),
            item_identity: None,
            item_content: ItemContentLite { source_item, item_variables: HashMap::new() },
            rename_times: 0,
            status: ProcessingStatus::WaitingToRename,
            failure_reason: None,
            created_at: OffsetDateTime::now_utc(),
            updated_at: None,
        };
        let content_id = storage.save_processing_content(&content).await.unwrap();
        content.id = Some(content_id);
        storage
            .save_file_contents(
                content_id,
                encode_files_and_compress(&vec![file, replacement_file]).unwrap(),
            )
            .await
            .unwrap();
        storage
            .save_paths(vec![ProcessingTargetPath {
                path: target_dir.join("renamed.txt").to_string_lossy().into_owned(),
                processor_name: processor_name.to_owned(),
                item_hash: content.item_hash.clone(),
            }])
            .await
            .unwrap();

        let (mut processor, _) = pointer_test_processor(false, 0, false);
        processor.name = processor_name.to_owned();
        processor.processing_storage = storage.clone();
        let listener = Arc::new(RecordingListener::default());
        processor.options.process_listeners = vec![listener.clone()];

        assert_eq!(processor.run_rename().await.unwrap(), 1);
        let saved = storage.find_content_by_id(content_id).await.unwrap().unwrap();
        assert_eq!(saved.status, ProcessingStatus::Renamed);
        assert_eq!(saved.rename_times, 1);
        assert!(!download_file.exists());
        let target_file = target_dir.join("renamed.txt");
        assert_eq!(fs::read(&target_file).unwrap(), b"content");
        assert_eq!(fs::read(&replacement_target_file).unwrap(), b"new-content");
        assert!(!replacement_download_file.exists());
        assert!(
            storage
                .find_paths(&[target_file.to_string_lossy().into_owned()])
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(listener.successes.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(listener.completions.load(AtomicOrdering::Relaxed), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[derive(Debug)]
    struct ReplacementFileMover;

    impl Display for ReplacementFileMover {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "replacement-test-mover")
        }
    }

    impl source_downloader_sdk::component::SdComponent for ReplacementFileMover {}
    impl FileMover for ReplacementFileMover {}

    #[derive(Debug)]
    struct AlwaysReplaceDecider {
        saw_prior_item: AtomicBool,
    }

    impl Display for AlwaysReplaceDecider {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "always-replace")
        }
    }

    impl source_downloader_sdk::component::SdComponent for AlwaysReplaceDecider {}

    impl FileReplacementDecider for AlwaysReplaceDecider {
        fn should_replace(
            &self,
            _: &SourceItem,
            _: &FileContent,
            before: Option<&InProcessingItem>,
            _: &SourceFile,
        ) -> bool {
            self.saw_prior_item.store(before.is_some(), AtomicOrdering::Relaxed);
            true
        }
    }

    #[tokio::test]
    async fn replacement_decider_receives_prior_item_and_replaces_target() {
        use std::fs;
        use std::sync::OnceLock;

        let root = std::env::temp_dir()
            .join(format!("source-downloader-replacement-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let target_file = root.join("target.txt");
        let download_file = root.join("download.txt");
        fs::write(&target_file, b"old").unwrap();
        fs::write(&download_file, b"new").unwrap();

        let previous_item = SourceItem {
            title: "previous-item".to_owned(),
            link: Uri::from_static("https://example.com/previous"),
            datetime: OffsetDateTime::UNIX_EPOCH,
            content_type: "text/plain".to_owned(),
            download_uri: Uri::from_static("https://example.com/previous/file"),
            attrs: Default::default(),
            tags: Vec::new(),
            identity: None,
        };
        let current_item = SourceItem {
            title: "current-item".to_owned(),
            link: Uri::from_static("https://example.com/current"),
            datetime: OffsetDateTime::now_utc(),
            content_type: "text/plain".to_owned(),
            download_uri: Uri::from_static("https://example.com/current/file"),
            attrs: Default::default(),
            tags: Vec::new(),
            identity: None,
        };
        let storage = storage().await.clone();
        let processor_name = "replacement-test";
        let mut previous_content = ProcessingContent {
            id: None,
            processor_name: processor_name.to_owned(),
            item_hash: previous_item.hashing(),
            item_identity: None,
            item_content: ItemContentLite {
                source_item: previous_item,
                item_variables: HashMap::new(),
            },
            rename_times: 1,
            status: ProcessingStatus::Renamed,
            failure_reason: None,
            created_at: OffsetDateTime::now_utc(),
            updated_at: None,
        };
        let previous_id =
            storage.save_processing_content(&previous_content).await.unwrap();
        previous_content.id = Some(previous_id);
        storage
            .save_file_contents(
                previous_id,
                encode_files_and_compress(&Vec::new()).unwrap(),
            )
            .await
            .unwrap();
        storage
            .save_paths(vec![ProcessingTargetPath {
                path: target_file.to_string_lossy().into_owned(),
                processor_name: processor_name.to_owned(),
                item_hash: previous_content.item_hash.clone(),
            }])
            .await
            .unwrap();

        let mut file = FileContent {
            download_path: root.clone(),
            file_download_path: download_file.clone(),
            source_save_path: root.clone(),
            pattern_variables: HashMap::new(),
            tags: Vec::new(),
            attrs: Default::default(),
            file_uri: None,
            target_save_path: root.clone(),
            target_filename: "target.txt".to_owned(),
            exist_target_path: Some(target_file.clone()),
            errors: Vec::new(),
            status: TargetExists,
            target_path: OnceLock::new(),
            data: None,
        };
        file.target_path.set(target_file.clone()).unwrap();
        let mut files = vec![file];
        let (mut processor, _) = pointer_test_processor(false, 0, false);
        processor.processing_storage = storage;
        processor.file_mover = Arc::new(ReplacementFileMover);
        let decider =
            Arc::new(AlwaysReplaceDecider { saw_prior_item: AtomicBool::new(false) });
        processor.options.file_replacement_decider = decider.clone();
        let process = NormalProcess {};

        assert_eq!(
            process
                .identify_files_to_replace(&processor, &current_item, &mut files)
                .await
                .unwrap(),
            1
        );
        assert_eq!(files[0].status, ReadyReplace);
        assert!(decider.saw_prior_item.load(AtomicOrdering::Relaxed));
        process.do_replacement(&processor, &current_item, &files).await.unwrap();
        assert_eq!(fs::read(&target_file).unwrap(), b"new");
        assert!(!download_file.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
