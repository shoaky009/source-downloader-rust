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
    FileConflict, Normal, TargetExists, Undetected, VariableError,
};
use source_downloader_sdk::component::{
    DownloadOptions, DownloadTask, Downloader, FileContentFilter, FileExistsDetector,
    InProcessingItem, ItemContent, ItemContentFilter, ProcessListener, SourceFileFilter,
    SourceFileRef, SourceItemFilter,
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
use std::any::{TypeId};
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

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

#[allow(dead_code, unused)]
struct ProcessRuntime {
    pub trace_id: String,
    pub mutex: Mutex<()>,
    source_state: ProcessorSourceState,
    source_pointer: Arc<dyn SourcePointer>,
    process_submitted_items: RwLock<HashSet<String>>,
    processed_count: AtomicU32,
    filter_count: AtomicU32,
    process_start_at: Option<Instant>,
    process_end_at: Option<Instant>,
    fetch_start_at: Option<Instant>,
    fetch_end_at: Option<Instant>,
    cancel_items: Vec<SourceItem>,
}

enum ItemAction {
    // Item被过滤(不存储Item信息), message为过滤原因
    Skip(String),
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
        if secs > 0 { format!("{}.{:03}s", secs, millis) } else { format!("{}ms", millis) }
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
                warn!("Retrying {} delay {} cause={} ", stage, format_duration(dur), err.message());
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
        ctx: &ProcessRuntime,
        pointer: Arc<dyn SourcePointer>,
    );

    async fn on_item_process_complete(
        &self,
        p: &SourceProcessor,
        processing_content: &ProcessingContent,
        files: &Vec<FileContent>,
    ) -> Result<(), ProcessingError>;

    async fn on_item_error<'a>(
        &self,
        _p: &SourceProcessor,
        _ctx: &ProcessRuntime,
        _in_processing: &InProcessingItem<'a>,
        _err: &ProcessingError,
    ) {
        // TODO invoke hooks
    }

    #[allow(unused)]
    async fn on_item_success(
        &self,
        p: &SourceProcessor,
        ctx: &ProcessRuntime,
        source_item: &SourceItem,
        item_pointer: &Arc<dyn ItemPointer>,
        source_pointer: &Arc<dyn SourcePointer>,
    ) {
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
        let source_pointer = p_rt.source_pointer.clone();
        debug!("Fetch with pointer: {}", p_rt.source_pointer.dump());
        p_rt.fetch_start_at = Some(Instant::now());
        let items = SourceProcessor::apply_retry(
            || async { p.source.fetch(source_pointer.clone(), p.options.fetch_limit).await },
            "fetch-source-items",
        )
        .await?;
        p_rt.fetch_end_at = Some(Instant::now());

        for item in items {
            let item_pointer = item.item_pointer;
            let source_item = item.source_item;
            let item_action = self.process_item(&source_item, &p_rt, p).await?;
            match &item_action {
                ItemAction::Skip(reason) => {
                    debug!("[item-skip] {} {:?} ", reason, source_item);
                    continue;
                }
                ItemAction::Error(err) => {
                    p_rt.processed_inc();
                    // todo invoke on_item_error hooks
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
                    self.on_item_success(p, &p_rt, &source_item, &item_pointer, &source_pointer)
                        .await;
                }
            }
        }
        self.on_process_complete(p, &p_rt, source_pointer.clone()).await;
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
    ) -> Result<Arc<dyn SourcePointer>, ProcessingError> {
        let source_pointer = p.source.parse_raw_pointer(source_state.last_pointer.to_owned());
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
            trace_id: PROCESS_ID_GENERATOR.fetch_add(i64::MIN, Ordering::Relaxed).to_string(),
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
        };
        Ok(p_ctx)
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
        let item_filters =
            item_strategy.map(|x| x.item_filters.as_ref()).flatten().unwrap_or(&opt.item_filters);
        for filter in item_filters {
            let filtered = !filter.filter(source_item).await;
            if filtered {
                debug!("[item-filtered] {}", source_item);
                rt.filter_inc();
                return Ok(ItemAction::Skip(format!("Filtered by: {}", filter)));
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
            .process_source_files(p, source_item, &item_variables, resolved_files, item_strategy)
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
        let (should_download, mut content_status, replace_files) = {
            let _guard = rt.mutex.lock().await;
            // preoccupiedTargetPath
            // identifyFilesToReplace
            let (should_download, content_status) =
                self.probe_content_status(p, rt, source_item, &file_contents, &vec![]);
            (should_download, content_status, vec![])
        };
        let mut rename_times = 0;
        if should_download {
            self.do_download(p, source_item, &file_contents, &replace_files).await?;
            let is_sync = !p.downloader.clone().as_async_downloader().is_ok();
            if is_sync {
                let movement_res =
                    self.do_movement(p, source_item, &file_contents, &replace_files).await;
                let replacement_res =
                    self.do_replacement(p, source_item, &file_contents, &replace_files).await;
                // 有点歧义后面重新定义
                if movement_res.is_ok() || replacement_res.is_ok() {
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
            item_content: ItemContentLite { source_item: source_item.clone(), item_variables },
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
        _p: &SourceProcessor,
        _source_item: &SourceItem,
        _file_contents: &Vec<FileContent>,
        _replace_files: &Vec<FileContent>,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    async fn do_replacement(
        &self,
        _p: &SourceProcessor,
        _source_item: &SourceItem,
        _file_contents: &Vec<FileContent>,
        _replace_files: &Vec<FileContent>,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    async fn do_download(
        &self,
        p: &SourceProcessor,
        source_item: &SourceItem,
        file_contents: &Vec<FileContent>,
        replace_files: &Vec<FileContent>,
    ) -> Result<(), ProcessingError> {
        let all_files: Vec<SourceFileRef> =
            file_contents.iter().chain(replace_files.iter()).map(Into::into).collect_vec();

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
        let headers: Option<HashMap<&String, &String>> = match (&options.headers, &source_headers) {
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

            for (idx, f) in file_contents.iter().enumerate().filter(|(_, f)| f.status == Undetected)
            {
                path_to_indices.entry(f.target_path()).or_default().push(idx);
            }

            path_to_indices.into_values().filter(|indices| indices.len() > 1).flatten().collect()
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
        if (*p.options.file_exists_detector).type_id() != TypeId::of::<SimpleFileExistsDetector>() {
            let detector_results = p.options.file_exists_detector.exists(
                p.file_mover.as_ref(),
                source_item,
                file_contents,
            );

            // 仅在此时建立一个局部反查表
            let path_to_local_idx: HashMap<&PathBuf, usize> = target_paths
                .iter()
                .enumerate()
                .map(|(i, &path)| (path, i))
                .collect();

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
        files: &Vec<FileContent>,
        replace_files: &Vec<FileContent>,
    ) -> (bool, ProcessingStatus) {
        if files.is_empty() {
            return (false, ProcessingStatus::NoFiles);
        };
        if !replace_files.is_empty() {
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

        let file_download_paths = files.iter().map(|f| &f.file_download_path).collect_vec();
        let all_exists = p.file_mover.exists(&file_download_paths).into_iter().all(|x| x);
        if all_exists {
            let is_async = p.downloader.clone().as_async_downloader().is_ok();
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
            let v = opt
                .variable_providers
                .get(idx)
                .expect("Failed to get variable provider by index, this should not happen");
            let vars = v.file_variables(source_item, item_variables, &relative_files).await;
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

        let item_var = p.renamer.item_rename_variables(source_item, item_variables.clone());

        let empty_vars = &PatternVariables::new();
        let file_count = relative_files.len();
        for (idx, x) in relative_files.into_iter().enumerate() {
            let var = file_vars.get(idx).unwrap_or_else(|| empty_vars);
            let file_rule = opt.file_rules.iter().find(|rule| rule.matcher.matches(&x, file_count));
            let file_strategy = file_rule.map(|r| &r.strategy);

            // Determine save_path_pattern and filename_pattern for this file
            let file_save_path_pattern = file_strategy
                .map(|s| s.save_path_pattern.clone())
                .flatten()
                .or_else(|| item_group_options.map(|s| s.save_path_pattern.clone()).flatten())
                .unwrap_or(opt.save_path_pattern.clone());
            let file_filename_pattern = file_strategy
                .map(|s| s.filename_pattern.clone())
                .flatten()
                .or_else(|| item_group_options.map(|s| s.filename_pattern.clone()).flatten())
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
    fn select_item_filter<'a>(&self, p: &'a SourceProcessor) -> &'a Vec<Arc<dyn SourceItemFilter>> {
        &p.options.item_filters
    }

    async fn on_process_complete(
        &self,
        p: &SourceProcessor,
        ctx: &ProcessRuntime,
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
            .inspect_err(|e| {
                error!("Failed to save source state: {:?}", e);
            })
            .unwrap();
        }
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
        // 事务?
        let content_id = p
            .processing_storage
            .save_processing_content(processing_content)
            .await
            .map_err(|x| {
                ProcessingError::non_retryable(format!("Failed to save item content {}", x.message))
            })?;

        let bytes = encode_files_and_compress(&files)?;
        p.processing_storage.save_file_contents(content_id, bytes).await.map_err(|x| {
            ProcessingError::non_retryable(format!("Failed to save file contents {}", x.message))
        })?;
        Ok(())
    }

    async fn on_item_success(
        &self,
        p: &SourceProcessor,
        ctx: &ProcessRuntime,
        source_item: &SourceItem,
        item_pointer: &Arc<dyn ItemPointer>,
        source_pointer: &Arc<dyn SourcePointer>,
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

impl NormalProcess {}

pub fn encode_files_and_compress(files: &Vec<FileContent>) -> Result<Vec<u8>, ProcessingError> {
    let bytes = if files.is_empty() {
        vec![]
    } else {
        let bytes = postcard::to_stdvec(&files).map_err(|x| {
            ProcessingError::non_retryable(format!("Failed to desc file content {}", x.to_string()))
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
pub fn decode_files_from_compressed(bytes: &[u8]) -> Result<Vec<FileContent>, ProcessingError> {
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
    use crate::config::ConfigOperator;
    use crate::processor_test_support::test_support::*;
    use jsonpath_rust::JsonPath;
    use source_downloader_sdk::component::ProcessTask;

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
            let root_path = V_PATH.join(format!("/{}", name)).expect("Failed to join path");
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
    // </editor-fold>

    // <editor-fold desc="Flow control tests">
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn flow_ctr_retry_then_ok() {
        let name = "flow_ctr_retry_then_ok";
        let cfg = cfg().get_processor_config(name).expect("Failed to get processor config");
        let pm = processor_manager().await;
        pm.create_processor(&cfg);
        let p = assert_processor(name, pm);
        let r = p.run().await;
        assert!(r.is_ok());
        assert!(logs_contain("Retrying fetch-source-items delay"));
    }
    // </editor-fold>
}
