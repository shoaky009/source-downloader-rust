use crate::components::simple_file_exists_detector::SimpleFileExistsDetector;
use crate::components::source_item_identity_filter::SourceItemIdentityFilter;
use crate::config::ListenerMode;
use crate::process::file::{PathPattern, RawFileContent, Renamer, VariableErrorStrategy};
use crate::process::rule::{FileRule, ItemRule, ItemStrategy};
use crate::process::variable::VariableAggregation;
use async_trait::async_trait;
use backon::Retryable;
use backon::{BackoffBuilder, ConstantBuilder};
use futures_util::future::{AbortHandle, Abortable};
use futures_util::stream::{FuturesOrdered, StreamExt};
use humantime::format_duration;
use itertools::Itertools;
use parking_lot::{Mutex as SyncMutex, RwLock};
use serde::Serialize;
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
use source_downloader_sdk::component::{
    EmptyPointer, ItemFileResolver, ItemPointer, PointedItem, SourcePointer,
};
use source_downloader_sdk::component::{FileContent, Source};
use source_downloader_sdk::component::{FileMover, ProcessingError};
use source_downloader_sdk::component::{FileTagger, ProcessTask, SourceFile};
use source_downloader_sdk::component::{PatternVariables, VariableProvider};
use source_downloader_sdk::serde_json::Value;
use source_downloader_sdk::storage::{
    ItemContentLite, ProcessingContent, ProcessingContentQuery, ProcessingStatus,
    ProcessingStorage, ProcessingTargetPath, ProcessorSourceState,
};
use source_downloader_sdk::time::OffsetDateTime;
use std::any::TypeId;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::fs::OpenOptions;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, info, warn};

static INSTANCE_ID_GENERATOR: AtomicI64 = AtomicI64::new(0);
static PROCESS_ID_GENERATOR: AtomicI64 = AtomicI64::new(i64::MIN);
// static EMPTY_FILES: Vec<FileContent> = vec![];
// static EMPTY_PATTERN_VARIABLES: LazyLock<PatternVariables> = LazyLock::new(|| HashMap::new());

/// 单个 item 处理后的文件、变量和状态；这里只承载结果，不负责执行处理、
/// 持久化或通知监听器。
#[derive(Debug)]
pub struct ItemProcessResult {
    /// true 表示结束该 item 的流程处理（如被过滤）
    pub item_filtered: bool,
    /// 处理过程中生成的文件内容。
    pub file_contents: Vec<FileContent>,
    /// 处理该 item 时解析得到的变量。
    pub item_variables: PatternVariables,
    /// 该 item 的最终处理状态。
    pub status: ProcessingStatus,
    /// 处理过程中的附加信息或错误说明。
    pub message: Option<String>,
    /// 该 item 完成处理的时间。
    pub finished_at: OffsetDateTime,
}

/// dry-run 的起始位置和过滤选项；不会改变正式处理器的持久化状态。
#[derive(Debug, Default)]
pub struct DryRunOptions {
    /// dry-run 使用的起始 source pointer；为空时使用处理器当前 pointer。
    pub pointer: Option<Value>,
    /// 是否应用已处理 item 过滤器。
    pub filter_processed: bool,
}

/// dry-run 生成的处理内容和文件结果；不代表正式处理已经持久化。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunResult {
    /// dry-run 生成的处理内容。
    pub processing_content: ProcessingContent,
    /// dry-run 解析出的文件内容。
    pub file_contents: Vec<FileContent>,
}
/// 处理器运行状态的只读快照；通过快照只能观察状态，不能修改处理器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorRuntimeSnapshot {
    /// 处理器实例创建时间。
    pub created_at: OffsetDateTime,
    /// 最近一次处理失败的错误信息。
    pub last_process_failed_message: Option<String>,
    /// 最近一次处理开始时间。
    pub last_start_process_time: Option<OffsetDateTime>,
    /// 最近一次处理结束时间。
    pub last_end_process_time: Option<OffsetDateTime>,
    /// 当前是否正在处理。
    pub processing: bool,
}
/// 处理器清理操作的结果统计；只报告删除数量，不执行删除或暴露存储细节。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessorContentDeletion {
    /// 删除的处理内容数量。
    pub processing_content: u64,
    /// 删除的目标路径数量。
    pub target_path: u64,
}

/// 处理器运行选项的对外摘要；只提供配置和统计信息，不持有运行时组件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorOptionInformation {
    /// 文件保存路径模式。
    pub save_path_pattern: String,
    /// 文件名生成模式。
    pub filename_pattern: String,
    /// 变量解析失败时采用的处理策略。
    pub variable_error_strategy: VariableErrorStrategy,
    /// 是否保存处理内容。
    pub save_processing_content: bool,
    /// 重命名任务的执行间隔。
    pub rename_task_interval: Duration,
    /// 触发重命名所需达到的次数阈值。
    pub rename_times_threshold: u32,
    /// 同时处理 item 的最大并发数。
    pub parallelism: u32,
    /// 单次操作允许的最大重试次数。
    pub retry_attempts: usize,
    /// 重试之间使用的等待时长。
    pub retry_backoff: Duration,
    /// 处理器加入的任务组。
    pub task_group: Option<String>,
    /// 每次从 source 获取的最大 item 数量。
    pub fetch_limit: u32,
    /// 单个 item 失败时是否继续处理后续 item。
    pub item_error_continue: bool,
    /// 是否以批量模式推进 source pointer。
    pub pointer_batch_mode: bool,
    /// 配置的 item 规则数量。
    pub item_rule_count: usize,
    /// 配置的文件规则数量。
    pub file_rule_count: usize,
    /// 下载内容所属的分类。
    pub download_category: Option<String>,
    /// 下载内容使用的标签。
    pub download_tags: Option<Vec<String>>,
    /// 下载请求使用的请求头。
    pub download_headers: Option<HashMap<String, String>>,
}

/// 变量处理链的输入、提供器和键筛选规则；只描述配置，不执行变量解析。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableProcessChainInformation {
    /// 变量处理链的输入变量或表达式。
    pub input: String,
    /// 处理链中使用的变量提供器名称。
    pub providers: Vec<String>,
    /// 处理结果中的变量键映射。
    pub key_mapping: HashMap<String, String>,
    /// 处理结果中排除的变量键。
    pub exclude_keys: HashSet<String>,
    /// 处理结果中包含的变量键。
    pub include_keys: HashSet<String>,
    /// 是否配置了条件表达式。
    pub conditional: bool,
}

/// 处理器的组件、路径、标签和选项摘要；作为查询结果对外提供，不暴露组件实例。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessorInformation {
    /// 处理器名称。
    pub name: String,
    /// source 组件的标识。
    pub source_id: String,
    /// source 组件名称。
    pub source: String,
    /// 变量提供器名称列表。
    pub variable_providers: Vec<String>,
    /// item 文件解析器名称。
    pub item_file_resolver: String,
    /// 下载器名称。
    pub downloader: String,
    /// 文件移动器名称。
    pub file_mover: String,
    /// item 过滤器名称列表。
    pub item_filters: Vec<String>,
    /// item 内容过滤器名称列表。
    pub item_content_filters: Vec<String>,
    /// source 文件过滤器名称列表。
    pub source_file_filters: Vec<String>,
    /// 文件内容过滤器名称列表。
    pub file_content_filters: Vec<String>,
    /// 文件标签器名称列表。
    pub file_taggers: Vec<String>,
    /// 按监听模式分组的处理监听器名称列表。
    pub process_listeners: HashMap<ListenerMode, Vec<String>>,
    /// 文件存在检测器名称。
    pub file_exists_detector: String,
    /// 文件替换决策器名称。
    pub file_replacement_decider: String,
    /// 变量替换器名称列表。
    pub variable_replacers: Vec<String>,
    /// 变量处理链的配置详情。
    pub variable_process_chains: Vec<VariableProcessChainInformation>,
    /// 按变量名分组的修剪器名称列表。
    pub trimming: HashMap<String, Vec<String>>,
    /// 下载器默认下载目录。
    pub download_path: String,
    /// 处理内容保存目录。
    pub source_save_path: String,
    /// 处理器所属分类。
    pub category: Option<String>,
    /// 处理器标签集合。
    pub tags: HashSet<String>,
    /// 处理器运行选项及其摘要信息。
    pub options: ProcessorOptionInformation,
}

/// 处理器最近一次运行的可变状态；只在内部运行时使用，不负责持久化或调度。
#[derive(Debug, Default)]
struct ProcessorRuntimeState {
    last_process_failed_message: Option<String>,
    last_start_process_time: Option<OffsetDateTime>,
    last_end_process_time: Option<OffsetDateTime>,
    processing: bool,
}

/// 处理器创建时间和受保护的运行状态；不负责启动、取消或执行处理流程。
#[derive(Debug)]
struct ProcessorRuntime {
    created_at: OffsetDateTime,
    state: RwLock<ProcessorRuntimeState>,
}

impl ProcessorRuntime {
    fn new() -> Self {
        Self {
            created_at: OffsetDateTime::now_utc(),
            state: RwLock::new(ProcessorRuntimeState::default()),
        }
    }
}

/// 处理器的运行入口，组合依赖并协调 source processing；具体抓取、解析、下载和存储由注入的组件完成。
#[allow(dead_code, unused)]
pub struct SourceProcessor {
    /// 处理器名称。
    pub name: String,
    /// source 组件的标识。
    pub source_id: String,
    /// 处理器状态或内容的保存目录。
    save_path: Box<Path>,
    /// 提供待处理 item 的 source 组件。
    source: Arc<dyn Source>,
    /// 将 item 解析为文件内容的组件。
    item_file_resolver: Arc<dyn ItemFileResolver>,
    /// 执行文件下载的组件。
    downloader: Arc<dyn Downloader>,
    /// 下载器提供的可选异步下载接口。
    async_downloader: Option<Arc<dyn AsyncDownloader>>,
    /// 执行文件移动的组件。
    file_mover: Arc<dyn FileMover>,
    /// 持久化处理内容和处理器状态的存储组件。
    processing_storage: Arc<dyn ProcessingStorage>,
    /// 处理器所属分类。
    category: Option<String>,
    /// 处理器标签集合。
    tags: HashSet<String>,
    /// 处理器运行配置。
    options: ProcessorOptions,
    /// 处理器实例的唯一运行时标识。
    instance_id: i64,
    /// 表示处理器当前是否正在处理的原子标志。
    processing: AtomicBool,
    /// 表示处理器是否已关闭的原子标志。
    closed: AtomicBool,
    /// 当前处理任务的取消句柄。
    active_process: SyncMutex<Option<AbortHandle>>,
    /// 处理器运行时间和最近处理状态。
    runtime: ProcessorRuntime,
    /// 负责生成文件目标路径和名称的重命名器。
    renamer: Renamer,
    /// 下载器默认下载目录的绝对路径。
    download_path: Box<Path>,
}

/// 一次处理运行所需的规则、组件和并发选项；只保存配置，不创建组件或执行流程。
pub struct ProcessorOptions {
    /// 文件保存路径模式。
    pub save_path_pattern: PathPattern,
    /// 文件名生成模式。
    pub filename_pattern: PathPattern,
    /// 变量提供器列表。
    pub variable_providers: Vec<Arc<dyn VariableProvider>>,
    /// item 过滤器列表。
    pub item_filters: Vec<Arc<dyn SourceItemFilter>>,
    /// item 内容过滤器列表。
    pub item_content_filters: Vec<Arc<dyn ItemContentFilter>>,
    /// source 文件过滤器列表。
    pub source_file_filters: Vec<Arc<dyn SourceFileFilter>>,
    /// 文件内容过滤器列表。
    pub file_content_filters: Vec<Arc<dyn FileContentFilter>>,
    /// 文件标签器列表。
    pub file_taggers: Vec<Arc<dyn FileTagger>>,
    /// 变量聚合方式。
    pub variable_aggregation: VariableAggregation,
    /// 是否保存处理内容。
    pub save_processing_content: bool,
    /// 重命名任务的执行间隔。
    pub rename_task_interval: Duration,
    /// 触发重命名所需达到的次数阈值。
    pub rename_times_threshold: u32,
    /// 同时处理 item 的最大并发数。
    pub parallelism: u32,
    /// 单次操作允许的最大重试次数。
    pub retry_attempts: usize,
    /// 重试之间使用的等待时长。
    pub retry_backoff: Duration,
    /// 处理器加入的任务组。
    pub task_group: Option<String>,
    /// 每次从 source 获取的最大 item 数量。
    pub fetch_limit: u32,
    /// 单个 item 失败时是否继续处理后续 item。
    pub item_error_continue: bool,
    /// 是否以批量模式推进 source pointer。
    pub pointer_batch_mode: bool,
    /// item 处理规则列表。
    pub item_rules: Vec<ItemRule>,
    /// 文件处理规则列表。
    pub file_rules: Vec<FileRule>,
    /// 按监听模式分组的处理监听器列表。
    pub process_listeners: HashMap<ListenerMode, Vec<Arc<dyn ProcessListener>>>,
    /// 检测目标文件是否已存在的组件。
    pub file_exists_detector: Arc<dyn FileExistsDetector>,
    /// 决定是否替换已有文件的组件。
    pub file_replacement_decider: Arc<dyn FileReplacementDecider>,
    /// 下载请求选项。
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
        self.options.task_group.clone()
    }
}

/// 给监听器提供一次运行的上下文、处理内容和错误状态；不负责处理 item 或推进 source pointer。
struct ListenerContext {
    processor_info: ProcessorInfo,
    contents: Vec<(ProcessingContent, Vec<FileContent>)>,
    content_indices: HashMap<String, usize>,
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
            contents: Vec::new(),
            content_indices: HashMap::new(),
            has_error: false,
        }
    }

    fn add(&mut self, content: ProcessingContent, files: Vec<FileContent>) {
        let index = self.contents.len();
        self.content_indices.insert(content.item_hash.to_owned(), index);
        self.contents.push((content, files));
    }

    fn get_item_content_by_hash(&self, item_hash: &str) -> Option<InProcessingItem<'_>> {
        let index = *self.content_indices.get(item_hash)?;
        let (content, files) = self.contents.get(index)?;
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
}

impl ProcessContext for ListenerContext {
    fn processor(&self) -> &ProcessorInfo {
        &self.processor_info
    }

    fn processed_items(&self) -> Box<dyn ExactSizeIterator<Item = &SourceItem> + '_> {
        Box::new(
            self.contents.iter().map(|(content, _)| &content.item_content.source_item),
        )
    }

    fn get_item_content(&self, item: &SourceItem) -> Option<InProcessingItem<'_>> {
        self.get_item_content_by_hash(&item.hashing())
    }

    fn has_error(&self) -> bool {
        self.has_error
    }
}

/// 一次处理运行的内存上下文，包含 trace、协调器、计时点和 item 状态；不跨运行复用或直接持久化。
#[allow(dead_code, unused)]
struct ProcessRuntime {
    trace_id: String,
    item: ItemProcessRuntime,
    coordinator: ProcessCoordinator,
    process_start_at: Option<Instant>,
    process_end_at: Option<Instant>,
    fetch_start_at: Option<Instant>,
    fetch_end_at: Option<Instant>,
}

/// 一次运行中 item 的计数、去重、取消和文件占用状态；只用于当前运行的并发协调。
struct ItemProcessRuntime {
    mutex: Mutex<()>,
    process_submitted_items: RwLock<HashSet<String>>,
    processed_count: AtomicU32,
    filter_count: AtomicU32,
    reserved_target_paths: RwLock<HashMap<PathBuf, String>>,
    in_flight_items: RwLock<HashMap<String, InFlightItem>>,
    cancelled_items: RwLock<HashSet<String>>,
}

/// 一个正在处理的 item 及其文件结果；只用于运行内跟踪，不是最终存储模型。
struct InFlightItem {
    content: ProcessingContent,
    files: Vec<FileContent>,
}

/// 一次运行的 source 状态、当前 pointer 和监听器上下文；负责连接这些状态，不执行 item 逻辑。
struct ProcessCoordinator {
    source_state: ProcessorSourceState,
    source_pointer: Box<dyn SourcePointer>,
    listener_context: ListenerContext,
}

enum ItemAction {
    // Source重复返回的Item
    Skip(String),
    // Item被过滤(不存储Item信息), message为过滤原因
    Filtered(String),
    // 处理成功
    Success {
        files: Vec<FileContent>,
        item_variables: PatternVariables,
        rename_times: u32,
        status: ProcessingStatus,
        failure_reason: Option<String>,
    },
    // 处理失败
    #[allow(dead_code)]
    Error(ProcessingError),
}

impl ItemProcessRuntime {
    fn filter_inc(&self) {
        self.filter_count.fetch_add(1, Ordering::Relaxed);
    }
    fn reserve_target_paths(&self, item_hash: &str, files: &mut [FileContent]) -> bool {
        let mut has_conflict = false;
        let mut reserved = self.reserved_target_paths.write();
        for file in files
            .iter_mut()
            .filter(|file| matches!(file.status, Normal | TargetExists | ReadyReplace))
        {
            let target_path = file.target_path();
            match reserved.get(target_path) {
                None => {
                    reserved.insert(target_path.to_path_buf(), item_hash.to_owned());
                }
                Some(owner) if owner == item_hash => {}
                Some(_) => {
                    file.status = FileConflict;
                    file.exist_target_path = None;
                    has_conflict = true;
                }
            }
        }
        has_conflict
    }

    fn release_target_paths(&self, item_hash: &str) {
        self.reserved_target_paths.write().retain(|_, owner| owner != item_hash);
    }

    fn register_in_flight(
        &self,
        processor: &SourceProcessor,
        source_item: &SourceItem,
        item_variables: &PatternVariables,
        files: &[FileContent],
    ) {
        let item_hash = source_item.hashing();
        self.in_flight_items.write().insert(
            item_hash.clone(),
            InFlightItem {
                content: ProcessingContent {
                    id: None,
                    processor_name: processor.name.clone(),
                    item_hash,
                    item_identity: source_item.identity.clone(),
                    item_content: ItemContentLite {
                        source_item: source_item.clone(),
                        item_variables: item_variables.clone(),
                    },
                    rename_times: 0,
                    status: ProcessingStatus::WaitingToRename,
                    failure_reason: None,
                    created_at: OffsetDateTime::now_utc(),
                    updated_at: None,
                },
                files: files.to_vec(),
            },
        );
    }

    fn begin_cancel(&self, item_hash: &str) -> bool {
        self.cancelled_items.write().insert(item_hash.to_owned())
    }

    fn undo_cancel(&self, item_hash: &str) {
        self.cancelled_items.write().remove(item_hash);
    }

    fn complete_cancel(&self, item_hash: &str) {
        self.release_target_paths(item_hash);
    }

    fn is_cancelled(&self, item_hash: &str) -> bool {
        self.cancelled_items.read().contains(item_hash)
    }

    fn processed_inc(&self) {
        self.processed_count.fetch_add(1, Ordering::Relaxed);
    }
}

impl ProcessRuntime {
    fn summary(&self) -> String {
        format!(
            "处理了{}个 过滤了{}个; [total] took {}; [fetch-items] took {}; [process-items] took {}",
            self.item.processed_count.load(Ordering::Acquire),
            self.item.filter_count.load(Ordering::Acquire),
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

/// 处理器运行状态的 RAII 记录器，负责记录开始、结束和失败；不拥有处理任务，也不改变结果。
struct ProcessingGuard<'a> {
    processor: &'a SourceProcessor,
}

impl<'a> ProcessingGuard<'a> {
    fn new(processor: &'a SourceProcessor) -> Self {
        let mut state = processor.runtime.state.write();
        state.last_start_process_time = Some(OffsetDateTime::now_utc());
        state.last_end_process_time = None;
        state.processing = true;
        drop(state);
        Self { processor }
    }

    fn record_result(&self, result: &Result<(), ProcessingError>) {
        self.processor.runtime.state.write().last_process_failed_message =
            result.as_ref().err().map(|error| error.message().to_owned());
    }
}

impl Drop for ProcessingGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.processor.runtime.state.write();
        state.last_end_process_time = Some(OffsetDateTime::now_utc());
        state.processing = false;
        drop(state);
        self.processor.active_process.lock().take();
        self.processor.processing.store(false, Ordering::Release);
    }
}

fn absolute_processor_path(path: &Path) -> Box<Path> {
    match std::path::absolute(path) {
        Ok(path) => path.into_boxed_path(),
        Err(error) => {
            warn!("Failed to make processor path absolute path={path:?}, error={error}");
            path.into()
        }
    }
}

fn relative_path_from(base: &Path, target: &Path) -> Option<PathBuf> {
    let base_components = base.components().collect_vec();
    let target_components = target.components().collect_vec();
    let common_length = base_components
        .iter()
        .zip(&target_components)
        .take_while(|(base, target)| base == target)
        .count();
    if common_length == 0
        || base_components[common_length..]
            .iter()
            .chain(&target_components[common_length..])
            .any(|component| {
                matches!(component, Component::Prefix(_) | Component::RootDir)
            })
    {
        return None;
    }

    let mut relative = PathBuf::new();
    for component in &base_components[common_length..] {
        match component {
            Component::CurDir => {}
            Component::Normal(_) | Component::ParentDir => relative.push(".."),
            Component::Prefix(_) | Component::RootDir => return None,
        }
    }
    for component in &target_components[common_length..] {
        relative.push(component.as_os_str());
    }
    Some(relative)
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
        renamer: Renamer,
        options: ProcessorOptions,
    ) -> Self {
        let save_path = absolute_processor_path(&save_path);
        let download_path =
            absolute_processor_path(Path::new(downloader.default_download_path()));
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
            closed: AtomicBool::new(false),
            active_process: SyncMutex::new(None),
            runtime: ProcessorRuntime::new(),
            renamer,
            download_path,
        }
    }

    pub fn instance_id(&self) -> i64 {
        self.instance_id
    }

    fn process_listeners(&self, mode: ListenerMode) -> &[Arc<dyn ProcessListener>] {
        self.options.process_listeners.get(&mode).map(Vec::as_slice).unwrap_or_default()
    }

    fn notify_process_listeners(
        &self,
        mode: ListenerMode,
        event: &str,
        mut notify: impl FnMut(&dyn ProcessListener) -> Result<(), ProcessingError>,
    ) {
        for listener in self.process_listeners(mode) {
            if let Err(error) = notify(listener.as_ref()) {
                warn!(
                    "Processor[listener-error] {} listener={} event={} {}",
                    self.name,
                    listener,
                    event,
                    error.message()
                );
            }
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(active_process) = self.active_process.lock().as_ref() {
            active_process.abort();
        }
    }
    pub fn runtime_snapshot(&self) -> ProcessorRuntimeSnapshot {
        let state = self.runtime.state.read();
        ProcessorRuntimeSnapshot {
            created_at: self.runtime.created_at,
            last_process_failed_message: state.last_process_failed_message.clone(),
            last_start_process_time: state.last_start_process_time,
            last_end_process_time: state.last_end_process_time,
            processing: state.processing,
        }
    }

    pub fn information(&self) -> ProcessorInformation {
        ProcessorInformation {
            name: self.name.clone(),
            source_id: self.source_id.clone(),
            source: self.source.to_string(),
            variable_providers: self
                .options
                .variable_providers
                .iter()
                .map(ToString::to_string)
                .collect(),
            item_file_resolver: self.item_file_resolver.to_string(),
            downloader: self.downloader.to_string(),
            file_mover: self.file_mover.to_string(),
            item_filters: self
                .options
                .item_filters
                .iter()
                .map(ToString::to_string)
                .collect(),
            item_content_filters: self
                .options
                .item_content_filters
                .iter()
                .map(ToString::to_string)
                .collect(),
            source_file_filters: self
                .options
                .source_file_filters
                .iter()
                .map(ToString::to_string)
                .collect(),
            file_content_filters: self
                .options
                .file_content_filters
                .iter()
                .map(ToString::to_string)
                .collect(),
            file_taggers: self
                .options
                .file_taggers
                .iter()
                .map(ToString::to_string)
                .collect(),
            process_listeners: self
                .options
                .process_listeners
                .iter()
                .map(|(mode, listeners)| {
                    (*mode, listeners.iter().map(ToString::to_string).collect())
                })
                .collect(),
            file_exists_detector: self.options.file_exists_detector.to_string(),
            file_replacement_decider: self.options.file_replacement_decider.to_string(),
            variable_replacers: self
                .renamer
                .variable_replacers
                .iter()
                .map(ToString::to_string)
                .collect(),
            variable_process_chains: self
                .renamer
                .variable_process_chain
                .iter()
                .map(|chain| VariableProcessChainInformation {
                    input: chain.input.clone(),
                    providers: chain.chain.iter().map(ToString::to_string).collect(),
                    key_mapping: chain.output.key_mapping.clone(),
                    exclude_keys: chain.output.exclude_keys.clone(),
                    include_keys: chain.output.include_keys.clone(),
                    conditional: chain.condition.is_some(),
                })
                .collect(),
            trimming: self
                .renamer
                .trimming
                .iter()
                .map(|(variable, trimmers)| {
                    (variable.clone(), trimmers.iter().map(ToString::to_string).collect())
                })
                .collect(),
            download_path: self.download_path.to_string_lossy().into_owned(),
            source_save_path: self.save_path.to_string_lossy().into_owned(),
            category: self.category.clone(),
            tags: self.tags.clone(),
            options: ProcessorOptionInformation {
                save_path_pattern: self.options.save_path_pattern.pattern.clone(),
                filename_pattern: self.options.filename_pattern.pattern.clone(),
                variable_error_strategy: self.renamer.variable_error_strategy,
                save_processing_content: self.options.save_processing_content,
                rename_task_interval: self.options.rename_task_interval,
                rename_times_threshold: self.options.rename_times_threshold,
                parallelism: self.options.parallelism,
                retry_attempts: self.options.retry_attempts,
                retry_backoff: self.options.retry_backoff,
                task_group: self.options.task_group.clone(),
                fetch_limit: self.options.fetch_limit,
                item_error_continue: self.options.item_error_continue,
                pointer_batch_mode: self.options.pointer_batch_mode,
                item_rule_count: self.options.item_rules.len(),
                file_rule_count: self.options.file_rules.len(),
                download_category: self.options.download_options.category.clone(),
                download_tags: self.options.download_options.tags.clone(),
                download_headers: self.options.download_options.headers.clone(),
            },
        }
    }
    pub async fn dry_run(
        &self,
        options: DryRunOptions,
    ) -> Result<Vec<DryRunResult>, ProcessingError> {
        let process = DryRunProcess::collecting(self, options);
        process.execute(self).await?;
        Ok(process.into_results())
    }

    pub fn dry_run_stream(
        self: &Arc<Self>,
        options: DryRunOptions,
    ) -> impl futures_util::Stream<Item = Result<DryRunResult, ProcessingError>> + Send + 'static
    {
        let capacity = self.options.parallelism.max(1) as usize;
        let (sender, receiver) = mpsc::channel(capacity);
        let process = DryRunProcess::streaming(self, options, sender.clone());
        let processor = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = process.execute(&processor) => {
                    if let Err(error) = result {
                        let _ = sender.send(Err(error)).await;
                    }
                }
                _ = sender.closed() => {}
            }
        });
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|result| (result, receiver))
        })
    }

    pub async fn reprocess(
        &self,
        content: ProcessingContent,
    ) -> Result<(), ProcessingError> {
        if content.processor_name != self.name {
            return Err(ProcessingError::non_retryable(format!(
                "Content {:?} does not belong to processor {}",
                content.id, self.name
            )));
        }
        Reprocess::new(self, content).execute(self).await
    }

    pub async fn run_items(&self, items: Vec<SourceItem>) -> Result<(), ProcessingError> {
        FixedItemProcess { items }.execute(self).await
    }

    pub async fn source_state(&self) -> Result<ProcessorSourceState, ProcessingError> {
        Ok(self
            .processing_storage
            .find_processor_source_state(&self.name, &self.source_id)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?
            .unwrap_or(ProcessorSourceState {
                id: None,
                processor_name: self.name.clone(),
                source_id: self.source_id.clone(),
                last_pointer: self.source.default_pointer().dump(),
                last_active_time: None,
                retry_times: 0,
            }))
    }

    pub async fn update_source_pointer(
        &self,
        source_id: &str,
        pointer: Value,
    ) -> Result<Option<ProcessorSourceState>, ProcessingError> {
        let Some(mut state) = self
            .processing_storage
            .find_processor_source_state(&self.name, source_id)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?
        else {
            return Ok(None);
        };
        match (state.last_pointer.as_object_mut(), pointer) {
            (Some(current), Value::Object(updated)) => current.extend(updated),
            (_, updated) => state.last_pointer = updated,
        }
        self.processing_storage
            .save_processor_source_state(&state)
            .await
            .map(Some)
            .map_err(|error| ProcessingError::non_retryable(error.message))
    }

    pub async fn delete_contents(
        &self,
    ) -> Result<ProcessorContentDeletion, ProcessingError> {
        let processing_content = self
            .processing_storage
            .delete_processing_contents_by_processor(&self.name)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        let target_path = self
            .processing_storage
            .delete_paths_by_processor(&self.name)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        Ok(ProcessorContentDeletion { processing_content, target_path })
    }

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
                let Some(processor) = processor.upgrade() else {
                    break;
                };
                if processor.closed.load(Ordering::Acquire) {
                    break;
                }
                if let Err(error) = processor.run_rename().await {
                    warn!(
                        "Processor[rename-task-error] {} {}",
                        processor.name,
                        error.message()
                    );
                }
                drop(processor);
                tokio::time::sleep(interval).await;
            }
        });
    }

    pub async fn run_rename(&self) -> Result<usize, ProcessingError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ProcessingError::non_retryable("Processor is closed"));
        }
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
                }
                Some(false) => {}
                Some(true) => {
                    finished += 1;
                    match self.process_rename_content(&mut content).await {
                        Ok(files) => {
                            let renamed = content.status == ProcessingStatus::Renamed;
                            let item_hash = content.item_hash.to_owned();
                            listener_context.add(content, files);
                            let completed = listener_context
                                .get_item_content_by_hash(&item_hash)
                                .expect("renamed item content was just inserted");
                            let item_content = ItemContent {
                                source_item: completed.source_item,
                                file_contents: completed.file_contents,
                                item_variables: completed.item_variables,
                                status: *completed.status,
                            };
                            if renamed {
                                self.notify_process_listeners(
                                    ListenerMode::Each,
                                    "item-success",
                                    |listener| {
                                        listener.on_item_success(
                                            &listener_context,
                                            &item_content,
                                        )
                                    },
                                );
                            }
                        }
                        Err(error) => {
                            listener_context.has_error = true;
                            warn!(
                                "Processor[rename-item-error] {} item={} {}",
                                self.name,
                                content.item_content.source_item,
                                error.message()
                            );
                            let item_hash = content.item_hash.to_owned();
                            let files = match self.load_file_contents(&content).await {
                                Ok(files) => files,
                                Err(load_error) => {
                                    warn!(
                                        "Processor[rename-item-files-load-error] {} item={} {}",
                                        self.name,
                                        content.item_content.source_item,
                                        load_error.message()
                                    );
                                    Vec::new()
                                }
                            };
                            listener_context.add(content, files);
                            let failed = listener_context
                                .get_item_content_by_hash(&item_hash)
                                .expect("failed rename item content was just inserted");
                            self.notify_process_listeners(
                                ListenerMode::Each,
                                "item-error",
                                |listener| {
                                    listener.on_item_error(
                                        &listener_context,
                                        failed.source_item,
                                        &error,
                                    )
                                },
                            );
                        }
                    }
                }
            }
        }
        if finished > 0 {
            self.notify_process_listeners(
                ListenerMode::Batch,
                "process-completed",
                |listener| listener.on_process_completed(&listener_context),
            );
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
        ctx: &mut ProcessCoordinator,
        source_item: &SourceItem,
        item_pointer: &dyn ItemPointer,
    ) -> Result<(), ProcessingError> {
        ctx.source_pointer.update(source_item, item_pointer);
        ctx.source_state.last_pointer = ctx.source_pointer.dump();
        if !self.options.pointer_batch_mode {
            self.save_source_state(&ctx.source_state)
                .await
                .map_err(ProcessingError::non_retryable)?;
        }
        Ok(())
    }

    pub async fn apply_retry<T, Fut, F>(
        mut f: F,
        stage: &str,
        retry_attempts: usize,
        retry_backoff: Duration,
    ) -> Result<T, ProcessingError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, ProcessingError>>,
    {
        (|| f())
            .retry(
                ConstantBuilder::default()
                    .with_max_times(retry_attempts)
                    .with_delay(retry_backoff)
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
        &'a self,
        p: &'a SourceProcessor,
    ) -> &'a Vec<Arc<dyn SourceItemFilter>>;

    fn allows_in_flight_cancellation(&self) -> bool {
        true
    }

    async fn fetch_items(
        &self,
        processor: &SourceProcessor,
        source_pointer: &dyn SourcePointer,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        SourceProcessor::apply_retry(
            || async {
                processor
                    .source
                    .fetch(source_pointer, processor.options.fetch_limit)
                    .await
            },
            "fetch-source-items",
            processor.options.retry_attempts,
            processor.options.retry_backoff,
        )
        .await
    }

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
    ) -> Result<Option<i64>, ProcessingError>;

    async fn on_item_error(
        &self,
        _p: &SourceProcessor,
        _ctx: &mut ProcessCoordinator,
        _item: &SourceItem,
        _err: &ProcessingError,
    ) {
    }

    async fn persist_item_failure(
        &self,
        p: &SourceProcessor,
        source_item: &SourceItem,
        error: &ProcessingError,
        content_id: Option<i64>,
        created_at: Option<OffsetDateTime>,
    ) {
        let failed_content = ProcessingContent {
            id: content_id,
            processor_name: p.name.clone(),
            item_hash: source_item.hashing(),
            item_identity: source_item.identity.clone(),
            item_content: ItemContentLite {
                source_item: source_item.clone(),
                item_variables: HashMap::new(),
            },
            rename_times: 0,
            status: ProcessingStatus::Failure,
            failure_reason: Some(error.message().to_owned()),
            created_at: created_at.unwrap_or_else(OffsetDateTime::now_utc),
            updated_at: None,
        };
        let files = Vec::new();
        if let Err(save_error) =
            self.on_item_process_complete(p, &failed_content, &files).await
        {
            warn!("[item-failure-save-error] {} {}", save_error.message(), source_item);
        }
    }

    async fn on_item_filtered(
        &self,
        _p: &SourceProcessor,
        _ctx: &mut ProcessCoordinator,
        _source_item: &SourceItem,
        _item_pointer: &dyn ItemPointer,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    async fn on_item_success(
        &self,
        _p: &SourceProcessor,
        _advance_pointer: bool,
        _ctx: &mut ProcessCoordinator,
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
        if p.closed.load(Ordering::Acquire) {
            return Err(ProcessingError::non_retryable("Processor is closed"));
        }
        if p.processing.swap(true, Ordering::AcqRel) {
            info!("[run-reject] {}({}) Already processing", p.name, p.instance_id);
            return Err(ProcessingError::non_retryable("Already processing"));
        }
        let (abort_handle, abort_registration) = AbortHandle::new_pair();
        {
            let mut active_process = p.active_process.lock();
            if p.closed.load(Ordering::Acquire) {
                p.processing.store(false, Ordering::Release);
                return Err(ProcessingError::non_retryable("Processor is closed"));
            }
            *active_process = Some(abort_handle);
        }
        let processing_guard = ProcessingGuard::new(p);
        let result = async {
            let mut p_rt = self.init_process_context(p, start_time).await?;
            debug!("Fetch with pointer: {}", p_rt.coordinator.source_pointer.dump());
            p_rt.fetch_start_at = Some(Instant::now());
            let items =
                self.fetch_items(p, p_rt.coordinator.source_pointer.as_ref()).await?;
            p_rt.fetch_end_at = Some(Instant::now());
            let parallelism = p.options.parallelism.max(1) as usize;
            if p.options.parallelism == 0 {
                warn!("Processor parallelism=0 is invalid; using parallelism=1");
            }
            let process = self;
            let item_runtime = &p_rt.item;
            let processor = p;
            let make_item_future =
                move |item: source_downloader_sdk::component::PointedItem| async move {
                    let item_pointer = item.item_pointer;
                    let source_item = item.source_item;
                    let item_action = process
                        .process_item(&source_item, item_runtime, processor)
                        .await
                        .unwrap_or_else(ItemAction::Error);
                    (item_pointer, source_item, item_action)
                };
            let mut remaining_items = items.into_iter();
            let mut item_results = FuturesOrdered::new();
            for item in remaining_items.by_ref().take(parallelism) {
                item_results.push_back(make_item_future(item));
            }

            let mut stop_scheduling = false;
            while let Some((item_pointer, source_item, item_action)) =
                item_results.next().await
            {
                let advance_pointer = !stop_scheduling;
                let mut stop_after_item = false;
                match item_action {
                    ItemAction::Skip(reason) => {
                        debug!("[item-skip] {} {:?} ", reason, source_item);
                    }
                    ItemAction::Filtered(reason) => {
                        debug!("[item-filtered] {} {:?} ", reason, source_item);
                        if advance_pointer
                            && let Err(err) = self
                                .on_item_filtered(
                                    p,
                                    &mut p_rt.coordinator,
                                    &source_item,
                                    item_pointer.as_ref(),
                                )
                                .await
                        {
                            p_rt.coordinator.listener_context.has_error = true;
                            self.on_item_error(
                                p,
                                &mut p_rt.coordinator,
                                &source_item,
                                &err,
                            )
                            .await;
                            if p.options.item_error_continue {
                                self.persist_item_failure(
                                    p,
                                    &source_item,
                                    &err,
                                    None,
                                    None,
                                )
                                .await;
                                warn!(
                                    "[item-continue-on-error] {} {}",
                                    err.message(),
                                    source_item
                                );
                            } else {
                                stop_after_item = true;
                            }
                        }
                    }
                    ItemAction::Error(err) => {
                        item_runtime.processed_inc();
                        p_rt.coordinator.listener_context.has_error = true;
                        self.on_item_error(p, &mut p_rt.coordinator, &source_item, &err)
                            .await;
                        let skippable = matches!(
                            err,
                            ProcessingError::NonRetryable { skip: true, .. }
                        );
                        if skippable || p.options.item_error_continue {
                            self.persist_item_failure(p, &source_item, &err, None, None)
                                .await;
                            warn!(
                                "[item-continue-on-error] {} {}",
                                err.message(),
                                source_item
                            );
                        } else {
                            warn!(
                                "[item-stop-on-error] {}, 停止提交新 Item",
                                err.message()
                            );
                            stop_after_item = true;
                        }
                    }
                    ItemAction::Success {
                        files,
                        item_variables,
                        rename_times,
                        mut status,
                        failure_reason,
                    } => {
                        let item_hash = source_item.hashing();
                        if item_runtime.is_cancelled(&item_hash) {
                            status = ProcessingStatus::Cancelled;
                        }
                        let mut content = ProcessingContent {
                            id: None,
                            processor_name: p.name.clone(),
                            item_hash,
                            item_identity: source_item.identity.clone(),
                            item_content: ItemContentLite { source_item, item_variables },
                            rename_times,
                            status,
                            failure_reason,
                            created_at: OffsetDateTime::now_utc(),
                            updated_at: None,
                        };
                        match self.on_item_process_complete(p, &content, &files).await {
                            Ok(content_id) => {
                                content.id = content_id;
                                item_runtime.processed_inc();
                                let continued_failure =
                                    p.options.item_error_continue.then(|| {
                                        (
                                            content.id,
                                            content.created_at,
                                            content.item_content.source_item.clone(),
                                        )
                                    });
                                match self
                                    .on_item_success(
                                        p,
                                        advance_pointer,
                                        &mut p_rt.coordinator,
                                        item_pointer.as_ref(),
                                        content,
                                        files,
                                    )
                                    .await
                                {
                                    Ok(()) => {}
                                    Err(err) if p.options.item_error_continue => {
                                        let (content_id, created_at, source_item) =
                                            continued_failure.expect(
                                                "continued failure context is available",
                                            );
                                        self.persist_item_failure(
                                            p,
                                            &source_item,
                                            &err,
                                            content_id,
                                            Some(created_at),
                                        )
                                        .await;
                                        warn!(
                                            "[item-continue-on-error] {}",
                                            err.message()
                                        );
                                    }
                                    Err(_) => stop_after_item = true,
                                }
                            }
                            Err(err) => {
                                item_runtime.processed_inc();
                                p_rt.coordinator.listener_context.has_error = true;
                                let source_item = &content.item_content.source_item;
                                self.on_item_error(
                                    p,
                                    &mut p_rt.coordinator,
                                    source_item,
                                    &err,
                                )
                                .await;
                                let skippable = matches!(
                                    err,
                                    ProcessingError::NonRetryable { skip: true, .. }
                                );
                                if skippable || p.options.item_error_continue {
                                    warn!(
                                        "[item-continue-on-error] {} {}",
                                        err.message(),
                                        source_item
                                    );
                                } else {
                                    warn!(
                                        "[item-stop-on-error] {}, 停止提交新 Item",
                                        err.message()
                                    );
                                    stop_after_item = true;
                                }
                            }
                        }
                    }
                }

                if stop_after_item {
                    stop_scheduling = true;
                }
                if !stop_scheduling && let Some(item) = remaining_items.next() {
                    item_results.push_back(make_item_future(item));
                }
            }
            drop(item_results);
            for (content, _) in &mut p_rt.coordinator.listener_context.contents {
                if item_runtime.is_cancelled(&content.item_hash) {
                    content.status = ProcessingStatus::Cancelled;
                }
            }
            self.on_process_complete(p, &p_rt).await?;
            p_rt.process_end_at = Some(Instant::now());
            info!("[run-done] {} {}", p.name, p_rt.summary());
            Ok(())
        };
        let result = match Abortable::new(result, abort_registration).await {
            Ok(result) => result,
            Err(_) => Err(ProcessingError::non_retryable("Processing cancelled")),
        };
        processing_guard.record_result(&result);
        result
    }

    async fn get_source_state(
        &self,
        p: &SourceProcessor,
    ) -> Result<ProcessorSourceState, ProcessingError> {
        p.source_state().await
    }

    fn get_source_pointer(
        &self,
        p: &SourceProcessor,
        raw_pointer: Value,
    ) -> Box<dyn SourcePointer> {
        p.source.parse_raw_pointer(raw_pointer)
    }

    async fn init_process_context(
        &self,
        p: &SourceProcessor,
        start_time: Instant,
    ) -> Result<ProcessRuntime, ProcessingError> {
        let mut source_state = self.get_source_state(p).await?;
        let raw_pointer = std::mem::take(&mut source_state.last_pointer);
        let source_pointer = self.get_source_pointer(p, raw_pointer);
        source_state.last_pointer = source_pointer.dump();
        source_state.last_active_time = Some(OffsetDateTime::now_utc());
        let p_ctx = ProcessRuntime {
            trace_id: PROCESS_ID_GENERATOR
                .fetch_add(i64::MIN, Ordering::Relaxed)
                .to_string(),
            item: ItemProcessRuntime {
                mutex: Mutex::new(()),
                process_submitted_items: RwLock::new(HashSet::new()),
                processed_count: AtomicU32::new(0),
                filter_count: AtomicU32::new(0),
                reserved_target_paths: RwLock::new(HashMap::new()),
                in_flight_items: RwLock::new(HashMap::new()),
                cancelled_items: RwLock::new(HashSet::new()),
            },
            coordinator: ProcessCoordinator {
                source_state,
                source_pointer,
                listener_context: ListenerContext::new(p),
            },
            process_start_at: Some(start_time),
            process_end_at: None,
            fetch_start_at: None,
            fetch_end_at: None,
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
        let mut latest_contents = HashMap::new();
        for content in prior_contents {
            match latest_contents.entry(content.item_hash.clone()) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(content);
                }
                std::collections::hash_map::Entry::Occupied(mut entry)
                    if content.created_at > entry.get().created_at =>
                {
                    entry.insert(content);
                }
                std::collections::hash_map::Entry::Occupied(_) => {}
            }
        }
        let mut prior_by_hash = HashMap::with_capacity(latest_contents.len());
        for content in latest_contents.into_values() {
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

    async fn identify_in_flight_replacements(
        &self,
        processor: &SourceProcessor,
        runtime: &ItemProcessRuntime,
        source_item: &SourceItem,
        files: &mut [FileContent],
    ) -> Result<usize, ProcessingError> {
        let current_hash = source_item.hashing();
        let mut cancellations: HashMap<String, (SourceItem, Vec<SourceFile>)> =
            HashMap::new();
        let mut replacement_count = 0;
        {
            let reserved_paths = runtime.reserved_target_paths.read();
            let in_flight_items = runtime.in_flight_items.read();
            for file in files.iter_mut() {
                let target_path = file.target_path().clone();
                let Some(owner_hash) = reserved_paths.get(&target_path) else {
                    continue;
                };
                if owner_hash == &current_hash {
                    continue;
                }
                let Some(before) = in_flight_items.get(owner_hash) else {
                    continue;
                };
                let Some(existing_content) = before
                    .files
                    .iter()
                    .find(|candidate| candidate.target_path() == &target_path)
                else {
                    continue;
                };
                let existing_file = SourceFile {
                    path: existing_content.file_download_path.clone(),
                    attrs: existing_content.attrs.clone(),
                    download_uri: existing_content.file_uri.clone(),
                    tags: existing_content.tags.clone(),
                    data: existing_content.data.clone(),
                };
                let before_view = InProcessingItem {
                    id: &before.content.id,
                    processor_name: &before.content.processor_name,
                    item_hash: &before.content.item_hash,
                    item_identity: &before.content.item_identity,
                    source_item: &before.content.item_content.source_item,
                    item_variables: &before.content.item_content.item_variables,
                    file_contents: &before.files,
                    rename_times: &before.content.rename_times,
                    status: &before.content.status,
                    failure_reason: before.content.failure_reason.as_deref(),
                };
                if processor.options.file_replacement_decider.should_replace(
                    source_item,
                    file,
                    Some(&before_view),
                    &existing_file,
                ) {
                    file.status = ReadyReplace;
                    replacement_count += 1;
                    cancellations
                        .entry(owner_hash.clone())
                        .or_insert_with(|| {
                            (before.content.item_content.source_item.clone(), Vec::new())
                        })
                        .1
                        .push(existing_file);
                }
            }
        }

        for (item_hash, (item, files)) in cancellations {
            if !runtime.begin_cancel(&item_hash) {
                continue;
            }
            info!("[item-cancel-for-replacement] {}", item);
            if let Err(error) = processor.downloader.cancel(&item, &files).await {
                runtime.undo_cancel(&item_hash);
                return Err(error);
            }
            runtime.complete_cancel(&item_hash);
            if processor.options.save_processing_content {
                match processor
                    .processing_storage
                    .find_by_name_and_hash(&processor.name, &item_hash)
                    .await
                {
                    Ok(Some(mut content)) => {
                        content.status = ProcessingStatus::Cancelled;
                        content.updated_at = Some(OffsetDateTime::now_utc());
                        if let Err(error) = processor
                            .processing_storage
                            .save_processing_content(&content)
                            .await
                        {
                            warn!(
                                "[item-cancel-status-save-error] {} {}",
                                item, error.message
                            );
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(
                            "[item-cancel-status-load-error] {} {}",
                            item, error.message
                        );
                    }
                }
            }
        }
        Ok(replacement_count)
    }

    async fn process_item(
        &self,
        source_item: &SourceItem,
        rt: &ItemProcessRuntime,
        p: &SourceProcessor,
    ) -> Result<ItemAction, ProcessingError> {
        let item_hash = source_item.hashing();
        if !rt.process_submitted_items.write().insert(item_hash.clone()) {
            rt.filter_inc();
            debug!("Source item duplicated: {:?} skipped", source_item);
            return Ok(ItemAction::Skip("Source item duplicated".to_string()));
        }

        debug!("[item-start] {}", source_item);
        let opt = &p.options;
        let item_rule = opt.item_rules.iter().find(|x| x.matcher.matches(source_item));
        let item_strategy = item_rule.map(|x| &x.strategy);
        let item_filters = item_strategy
            .and_then(|strategy| strategy.item_filters.as_ref())
            .unwrap_or_else(|| self.select_item_filter(p));
        for filter in item_filters {
            let filtered = !filter.filter(source_item).await;
            if filtered {
                debug!("[item-filtered] {}", source_item);
                rt.filter_inc();
                return Ok(ItemAction::Filtered(format!("Filtered by: {}", filter)));
            }
        }
        let result = SourceProcessor::apply_retry(
            || async {
                self.process_item_attempt(source_item, &item_hash, rt, p, item_strategy)
                    .await
            },
            "process-item",
            p.options.retry_attempts,
            p.options.retry_backoff,
        )
        .await;
        if result.is_err() {
            rt.release_target_paths(&item_hash);
        }
        result
    }

    async fn process_item_attempt(
        &self,
        source_item: &SourceItem,
        item_hash: &str,
        rt: &ItemProcessRuntime,
        p: &SourceProcessor,
        item_strategy: Option<&ItemStrategy>,
    ) -> Result<ItemAction, ProcessingError> {
        let opt = &p.options;
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
                variable_providers,
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
        if content_status == ProcessingStatus::Filtered {
            return Ok(ItemAction::Success {
                files: file_contents,
                item_variables,
                rename_times: 0,
                status: content_status,
                failure_reason,
            });
        }
        let (should_download, mut content_status) = {
            let _guard = rt.mutex.lock().await;
            self.update_file_content_status(p, source_item, &mut file_contents).await;
            self.identify_files_to_replace(p, source_item, &mut file_contents).await?;
            if self.allows_in_flight_cancellation() && p.async_downloader.is_some() {
                self.identify_in_flight_replacements(
                    p,
                    rt,
                    source_item,
                    &mut file_contents,
                )
                .await?;
            }
            let has_reserved_target_conflict =
                rt.reserve_target_paths(item_hash, &mut file_contents);
            rt.register_in_flight(p, source_item, &item_variables, &file_contents);
            self.probe_content_status(
                p,
                rt,
                source_item,
                &file_contents,
                has_reserved_target_conflict,
            )
        };
        let mut rename_times = 0;
        if should_download && self.do_download(p, source_item, &file_contents).await? {
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

        if rt.is_cancelled(item_hash) {
            content_status = ProcessingStatus::Cancelled;
        }

        Ok(ItemAction::Success {
            files: file_contents,
            item_variables,
            rename_times,
            status: content_status,
            failure_reason,
        })
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
    ) -> Result<bool, ProcessingError> {
        let downloadable_files: Vec<&FileContent> = file_contents
            .iter()
            .filter(|file| file.status != TargetExists && file.status != Downloaded)
            .collect();
        let (direct_files, download_files): (Vec<SourceFileRef>, Vec<SourceFileRef>) =
            downloadable_files
                .iter()
                .copied()
                .map(SourceFileRef::from)
                .partition(|file| file.data.is_some());
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
            match (&source_headers, &options.headers) {
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
        Ok(true)
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
        rt: &ItemProcessRuntime,
        source_item: &SourceItem,
        files: &[FileContent],
        has_reserved_target_conflict: bool,
    ) -> (bool, ProcessingStatus) {
        if files.is_empty() {
            return (false, ProcessingStatus::NoFiles);
        };
        if has_reserved_target_conflict {
            return (false, ProcessingStatus::TargetAlreadyExists);
        }
        if files.iter().any(|file| file.status == ReadyReplace) {
            return (true, ProcessingStatus::WaitingToRename);
        };
        if rt.is_cancelled(&source_item.hashing()) {
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
                tags.extend(f.tags);
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
        variable_providers: &[Arc<dyn VariableProvider>],
        source_files: Vec<SourceFile>,
        item_group_options: Option<&ItemStrategy>,
    ) -> Result<Vec<FileContent>, ProcessingError> {
        let mut relative_files = Vec::with_capacity(source_files.len());
        let opt = &p.options;
        for mut file in source_files {
            if file.path.is_absolute() {
                file.path = relative_path_from(&p.download_path, &file.path).ok_or_else(|| {
                    ProcessingError::non_retryable(format!(
                        "Source file path {:?} cannot be relativized against download path {:?}",
                        file.path, p.download_path
                    ))
                })?;
            }
            relative_files.push(file);
        }

        // <editor-fold desc="Stage using VariableProviders for file">
        let mut file_raw_vars = vec![];
        for (idx, provider) in variable_providers.iter().enumerate() {
            let vars = provider
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
            file_raw_vars.push((provider.accuracy(), vars));
        }
        let file_vars = opt.variable_aggregation.merge_files(&file_raw_vars);
        // </editor-fold>
        let mut result: Vec<FileContent> = vec![];

        let item_var = p.renamer.item_rename_variables(source_item, item_variables);

        let empty_vars = &PatternVariables::new();
        let file_count = relative_files.len();
        for (idx, x) in relative_files.into_iter().enumerate() {
            let var = file_vars.get(idx).unwrap_or_else(|| empty_vars);
            let file_rule =
                opt.file_rules.iter().find(|rule| rule.matcher.matches(&x, file_count));
            let file_strategy = file_rule.map(|r| &r.strategy);

            // Determine save_path_pattern and filename_pattern for this file
            let file_save_path_pattern = file_strategy
                .and_then(|strategy| strategy.save_path_pattern.as_ref())
                .or_else(|| {
                    item_group_options
                        .and_then(|strategy| strategy.save_path_pattern.as_ref())
                })
                .unwrap_or(&opt.save_path_pattern);
            let file_filename_pattern = file_strategy
                .and_then(|strategy| strategy.filename_pattern.as_ref())
                .or_else(|| {
                    item_group_options
                        .and_then(|strategy| strategy.filename_pattern.as_ref())
                })
                .unwrap_or(&opt.filename_pattern);

            let raw = RawFileContent {
                save_path: &p.save_path,
                download_path: &p.download_path,
                variables: var,
                save_path_pattern: file_save_path_pattern,
                filename_pattern: file_filename_pattern,
                source_file: x,
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

/// 正式处理流程的 `Process` 实现，负责持久化、监听和 pointer 推进；不处理 dry-run 或重处理。
#[allow(dead_code)]
struct NormalProcess {}

impl Process for NormalProcess {
    fn select_item_filter<'a>(
        &'a self,
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
            || ctx.item.processed_count.load(Ordering::Acquire) == 0
        {
            p.save_source_state(&ctx.coordinator.source_state)
                .await
                .map_err(ProcessingError::non_retryable)?;
        }
        if !ctx.coordinator.listener_context.contents.is_empty()
            && p.async_downloader.is_none()
        {
            p.notify_process_listeners(
                ListenerMode::Batch,
                "process-completed",
                |listener| {
                    listener.on_process_completed(&ctx.coordinator.listener_context)
                },
            );
        }
        Ok(())
    }

    async fn on_item_process_complete(
        &self,
        p: &SourceProcessor,
        processing_content: &ProcessingContent,
        files: &Vec<FileContent>,
    ) -> Result<Option<i64>, ProcessingError> {
        debug!("[item-done] {:?}", &processing_content.item_content.source_item);
        if processing_content.status == ProcessingStatus::Filtered
            || !p.options.save_processing_content
        {
            return Ok(None);
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
            })?;
        Ok(Some(content_id))
    }

    async fn on_item_error(
        &self,
        p: &SourceProcessor,
        ctx: &mut ProcessCoordinator,
        item: &SourceItem,
        error: &ProcessingError,
    ) {
        p.notify_process_listeners(ListenerMode::Each, "item-error", |listener| {
            listener.on_item_error(&ctx.listener_context, item, error)
        });
    }

    async fn on_item_filtered(
        &self,
        p: &SourceProcessor,
        ctx: &mut ProcessCoordinator,
        source_item: &SourceItem,
        item_pointer: &dyn ItemPointer,
    ) -> Result<(), ProcessingError> {
        p.advance_source_pointer(ctx, source_item, item_pointer).await
    }

    async fn on_item_success(
        &self,
        p: &SourceProcessor,
        advance_pointer: bool,
        ctx: &mut ProcessCoordinator,
        item_pointer: &dyn ItemPointer,
        content: ProcessingContent,
        files: Vec<FileContent>,
    ) -> Result<(), ProcessingError> {
        let item_hash = content.item_hash.to_owned();
        ctx.listener_context.add(content, files);
        let completed = ctx
            .listener_context
            .get_item_content_by_hash(&item_hash)
            .expect("completed item was just inserted");
        let source_item = completed.source_item.clone();
        let item_content = ItemContent {
            source_item: &source_item,
            file_contents: completed.file_contents,
            item_variables: completed.item_variables,
            status: *completed.status,
        };
        if p.async_downloader.is_none() {
            p.notify_process_listeners(ListenerMode::Each, "item-success", |listener| {
                listener.on_item_success(&ctx.listener_context, &item_content)
            });
        }
        if advance_pointer
            && let Err(error) =
                p.advance_source_pointer(ctx, &source_item, item_pointer).await
        {
            ctx.listener_context.has_error = true;
            p.notify_process_listeners(ListenerMode::Each, "item-error", |listener| {
                listener.on_item_error(&ctx.listener_context, &source_item, &error)
            });
            return Err(error);
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

enum DryRunOutput {
    Collected(SyncMutex<Vec<DryRunResult>>),
    Streamed(mpsc::Sender<Result<DryRunResult, ProcessingError>>),
}

/// dry-run 的 `Process` 实现，只收集或流式输出预览结果，不保存内容或下载文件。
struct DryRunProcess {
    source_pointer: Option<Value>,
    item_filters: Vec<Arc<dyn SourceItemFilter>>,
    output: DryRunOutput,
}

impl DryRunProcess {
    fn collecting(processor: &SourceProcessor, options: DryRunOptions) -> Self {
        let item_filters = if options.filter_processed {
            processor.options.item_filters.clone()
        } else {
            processor
                .options
                .item_filters
                .iter()
                .filter(|filter| {
                    filter.as_ref().type_id() != TypeId::of::<SourceItemIdentityFilter>()
                })
                .cloned()
                .collect()
        };
        Self {
            source_pointer: options.pointer,
            item_filters,
            output: DryRunOutput::Collected(SyncMutex::new(Vec::new())),
        }
    }

    fn streaming(
        processor: &SourceProcessor,
        options: DryRunOptions,
        sender: mpsc::Sender<Result<DryRunResult, ProcessingError>>,
    ) -> Self {
        let mut process = Self::collecting(processor, options);
        process.output = DryRunOutput::Streamed(sender);
        process
    }

    fn into_results(self) -> Vec<DryRunResult> {
        match self.output {
            DryRunOutput::Collected(results) => results.into_inner(),
            DryRunOutput::Streamed(_) => {
                unreachable!("collecting dry-run must use collected output")
            }
        }
    }
}

impl Process for DryRunProcess {
    fn select_item_filter<'a>(
        &'a self,
        _: &'a SourceProcessor,
    ) -> &'a Vec<Arc<dyn SourceItemFilter>> {
        &self.item_filters
    }

    fn allows_in_flight_cancellation(&self) -> bool {
        false
    }

    async fn on_process_complete(
        &self,
        _: &SourceProcessor,
        _: &ProcessRuntime,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    async fn on_item_process_complete(
        &self,
        _: &SourceProcessor,
        _: &ProcessingContent,
        _: &Vec<FileContent>,
    ) -> Result<Option<i64>, ProcessingError> {
        Ok(None)
    }

    async fn on_item_success(
        &self,
        _: &SourceProcessor,
        _: bool,
        _: &mut ProcessCoordinator,
        _: &dyn ItemPointer,
        processing_content: ProcessingContent,
        file_contents: Vec<FileContent>,
    ) -> Result<(), ProcessingError> {
        let result = DryRunResult { processing_content, file_contents };
        match &self.output {
            DryRunOutput::Collected(results) => results.lock().push(result),
            DryRunOutput::Streamed(sender) => sender
                .send(Ok(result))
                .await
                .map_err(|_| ProcessingError::non_retryable("Dry-run stream closed"))?,
        }
        Ok(())
    }

    fn get_source_pointer(
        &self,
        processor: &SourceProcessor,
        raw_pointer: Value,
    ) -> Box<dyn SourcePointer> {
        processor
            .source
            .parse_raw_pointer(self.source_pointer.clone().unwrap_or(raw_pointer))
    }

    async fn do_download(
        &self,
        _: &SourceProcessor,
        _: &SourceItem,
        _: &[FileContent],
    ) -> Result<bool, ProcessingError> {
        Ok(false)
    }
}

/// 以已有处理内容为输入的重处理流程；不重新抓取 source。
struct Reprocess {
    content: ProcessingContent,
    item_filters: Vec<Arc<dyn SourceItemFilter>>,
}

impl Reprocess {
    fn new(processor: &SourceProcessor, content: ProcessingContent) -> Self {
        let item_filters = processor
            .options
            .item_filters
            .iter()
            .filter(|filter| {
                filter.as_ref().type_id() != TypeId::of::<SourceItemIdentityFilter>()
            })
            .cloned()
            .collect();
        Self { content, item_filters }
    }
}

impl Process for Reprocess {
    fn select_item_filter<'a>(
        &'a self,
        _: &'a SourceProcessor,
    ) -> &'a Vec<Arc<dyn SourceItemFilter>> {
        &self.item_filters
    }

    async fn fetch_items(
        &self,
        _: &SourceProcessor,
        _: &dyn SourcePointer,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        Ok(vec![PointedItem {
            source_item: self.content.item_content.source_item.clone(),
            item_pointer: Arc::new(EmptyPointer),
        }])
    }

    async fn on_process_complete(
        &self,
        _: &SourceProcessor,
        _: &ProcessRuntime,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    async fn on_item_process_complete(
        &self,
        processor: &SourceProcessor,
        processing_content: &ProcessingContent,
        files: &Vec<FileContent>,
    ) -> Result<Option<i64>, ProcessingError> {
        if !processor.options.save_processing_content {
            return Ok(None);
        }
        let mut content = processing_content.clone();
        content.id = self.content.id;
        content.processor_name = processor.name.clone();
        content.rename_times = 0;
        let content_id = processor
            .processing_storage
            .save_processing_content(&content)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        processor
            .processing_storage
            .save_file_contents(content_id, encode_files_and_compress(files)?)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        Ok(Some(content_id))
    }
}

/// 以固定 item 列表驱动处理流程，只替换 item 获取方式，其余行为沿用正常流程。
struct FixedItemProcess {
    items: Vec<SourceItem>,
}

impl Process for FixedItemProcess {
    fn select_item_filter<'a>(
        &'a self,
        processor: &'a SourceProcessor,
    ) -> &'a Vec<Arc<dyn SourceItemFilter>> {
        &processor.options.item_filters
    }

    async fn fetch_items(
        &self,
        _: &SourceProcessor,
        _: &dyn SourcePointer,
    ) -> Result<Vec<PointedItem>, ProcessingError> {
        Ok(self
            .items
            .iter()
            .cloned()
            .map(|source_item| PointedItem {
                source_item,
                item_pointer: Arc::new(EmptyPointer),
            })
            .collect())
    }

    async fn on_process_complete(
        &self,
        _: &SourceProcessor,
        _: &ProcessRuntime,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }

    async fn on_item_process_complete(
        &self,
        processor: &SourceProcessor,
        processing_content: &ProcessingContent,
        files: &Vec<FileContent>,
    ) -> Result<Option<i64>, ProcessingError> {
        if !processor.options.save_processing_content {
            return Ok(None);
        }
        let content_id = processor
            .processing_storage
            .save_processing_content(processing_content)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        processor
            .processing_storage
            .save_file_contents(content_id, encode_files_and_compress(files)?)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        Ok(Some(content_id))
    }
}

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

    /// 测试用的可比较 item pointer，只模拟 pointer 身份，不推进真实 source 状态。
    #[derive(Debug)]
    struct PointerItem(usize);

    impl ItemPointer for PointerItem {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    /// 测试用的可序列化 source pointer，只服务测试夹具，不代表生产实现。
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

    /// 记录测试流程中的并发峰值和启动顺序，只观测行为，不参与调度。
    #[derive(Debug)]
    struct ParallelismProbe {
        active: AtomicUsize,
        max_active: AtomicUsize,
        first_items_started: tokio::sync::Barrier,
        completed: ParkingMutex<Vec<usize>>,
    }

    impl ParallelismProbe {
        fn new(first_item_barrier_size: usize) -> Self {
            Self {
                active: AtomicUsize::new(0),
                max_active: AtomicUsize::new(0),
                first_items_started: tokio::sync::Barrier::new(first_item_barrier_size),
                completed: ParkingMutex::new(Vec::new()),
            }
        }

        async fn record(&self, sequence: usize) {
            let active = self.active.fetch_add(1, AtomicOrdering::AcqRel) + 1;
            self.max_active.fetch_max(active, AtomicOrdering::AcqRel);
            if sequence <= 2 {
                self.first_items_started.wait().await;
            }
            if sequence == 1 {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            self.completed.lock().push(sequence);
            self.active.fetch_sub(1, AtomicOrdering::AcqRel);
        }
    }

    /// 记录测试中的替换提交、取消和完成事件，只提供观测点，不执行文件替换。
    #[derive(Debug, Default)]
    struct ReplacementProbe {
        first_submitted: tokio::sync::Notify,
        first_cancelled: tokio::sync::Notify,
        cancelled_items: ParkingMutex<Vec<String>>,
    }

    /// 提供固定且可预测的测试文件标签，只验证标签注入流程。
    #[derive(Debug)]
    struct StaticFileTagger;

    impl Display for StaticFileTagger {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "static-file-tagger")
        }
    }

    impl source_downloader_sdk::component::SdComponent for StaticFileTagger {}

    #[async_trait]
    impl FileTagger for StaticFileTagger {
        async fn tag(&self, _: &SourceFile) -> Option<String> {
            Some("generated".to_owned())
        }
    }

    /// 提供可控的 source 测试组件，只生成测试 item 和故障，不执行真实抓取或持久化。
    #[derive(Debug)]
    struct PointerTestComponent {
        item_count: usize,
        duplicate_source_item: bool,
        probe: Option<Arc<ParallelismProbe>>,
        invalid_item: Option<usize>,
        resolved_file: Option<PathBuf>,
        submit_count: Option<Arc<AtomicUsize>>,
        unique_files: bool,
        skippable_download_item: Option<usize>,
        retryable_fetch_failures: Option<Arc<AtomicUsize>>,
        retryable_submit_failures: Option<Arc<AtomicUsize>>,
        submit_probe: Option<Arc<ParallelismProbe>>,
        replacement_probe: Option<Arc<ReplacementProbe>>,
        source_headers: Option<HashMap<String, String>>,
        submitted_headers: Option<Arc<ParkingMutex<Option<HashMap<String, String>>>>>,
        resolved_file_tags: Vec<String>,
        download_path: String,
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
            if let Some(failures) = &self.retryable_fetch_failures
                && failures
                    .fetch_update(
                        AtomicOrdering::AcqRel,
                        AtomicOrdering::Acquire,
                        |remaining| remaining.checked_sub(1),
                    )
                    .is_ok()
            {
                return Err(ProcessingError::retryable("retryable fetch error"));
            }
            Ok((1..=self.item_count)
                .map(|sequence| PointedItem {
                    source_item: SourceItem {
                        title: format!(
                            "item-{}",
                            if self.duplicate_source_item { 1 } else { sequence }
                        ),
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

        fn headers(&self, _: &SourceItem) -> Option<HashMap<String, String>> {
            self.source_headers.clone()
        }
    }

    #[async_trait]
    impl ItemFileResolver for PointerTestComponent {
        async fn resolve_files(&self, item: &SourceItem) -> Vec<SourceFile> {
            let sequence = item
                .title
                .strip_prefix("item-")
                .and_then(|value| value.parse::<usize>().ok())
                .expect("pointer test item sequence");
            if let Some(probe) = &self.probe {
                probe.record(sequence).await;
            }
            if sequence == 2
                && let Some(probe) = &self.replacement_probe
            {
                probe.first_submitted.notified().await;
            }
            if self.invalid_item == Some(sequence) {
                let path = PathBuf::from(format!("{sequence}.txt"));
                return vec![SourceFile::new(path.clone()), SourceFile::new(path)];
            }
            if let Some(path) = &self.resolved_file {
                return vec![SourceFile {
                    tags: self.resolved_file_tags.clone(),
                    ..SourceFile::new(path.clone())
                }];
            }
            if self.unique_files {
                return vec![SourceFile::new(PathBuf::from(format!("{sequence}.txt")))];
            }
            Vec::new()
        }
    }

    #[async_trait]
    impl Downloader for PointerTestComponent {
        async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError> {
            if let Some(submitted_headers) = &self.submitted_headers {
                *submitted_headers.lock() = task.headers.as_ref().map(|headers| {
                    headers
                        .iter()
                        .map(|(key, value)| ((*key).clone(), (*value).clone()))
                        .collect()
                });
            }
            if let Some(submit_count) = &self.submit_count {
                submit_count.fetch_add(1, AtomicOrdering::AcqRel);
            }
            if let Some(failures) = &self.retryable_submit_failures
                && failures
                    .fetch_update(
                        AtomicOrdering::AcqRel,
                        AtomicOrdering::Acquire,
                        |remaining| remaining.checked_sub(1),
                    )
                    .is_ok()
            {
                return Err(ProcessingError::retryable("retryable submit error"));
            }
            let sequence = task
                .source_item
                .title
                .strip_prefix("item-")
                .and_then(|value| value.parse::<usize>().ok())
                .expect("pointer test item sequence");
            if let Some(probe) = &self.submit_probe {
                probe.record(sequence).await;
            }
            if sequence == 1
                && let Some(probe) = &self.replacement_probe
            {
                probe.first_submitted.notify_one();
                probe.first_cancelled.notified().await;
            }
            if self.skippable_download_item == Some(sequence) {
                return Err(ProcessingError::skip("skippable test error"));
            }
            Ok(())
        }

        fn default_download_path(&self) -> &str {
            &self.download_path
        }

        async fn cancel(
            &self,
            item: &SourceItem,
            _: &[SourceFile],
        ) -> Result<(), ProcessingError> {
            if let Some(probe) = &self.replacement_probe {
                probe.cancelled_items.lock().push(item.title.clone());
                probe.first_cancelled.notify_one();
            }
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

    /// 测试用的 item 过滤器，拒绝所有 item，只验证 item 过滤分支。
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

    /// 测试用的内容过滤器，拒绝所有 item 内容，只验证内容过滤分支。
    #[derive(Debug)]
    struct RejectAllContent;

    impl Display for RejectAllContent {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "reject-all-content")
        }
    }

    impl source_downloader_sdk::component::SdComponent for RejectAllContent {}

    #[async_trait]
    impl ItemContentFilter for RejectAllContent {
        async fn filter(&self, _: &ItemContent) -> bool {
            false
        }
    }

    /// 构造测试用的 in-flight 文件替换场景，不实现生产替换策略。
    #[derive(Debug)]
    struct ReplaceInFlight;

    impl Display for ReplaceInFlight {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "replace-in-flight")
        }
    }

    impl source_downloader_sdk::component::SdComponent for ReplaceInFlight {}

    impl FileReplacementDecider for ReplaceInFlight {
        fn should_replace(
            &self,
            _: &SourceItem,
            _: &FileContent,
            before: Option<&InProcessingItem>,
            _: &SourceFile,
        ) -> bool {
            before.is_some()
        }
    }

    /// 提供固定变量值的测试变量提供器，不读取外部数据源。
    #[derive(Debug)]
    struct StaticVariableProvider(&'static str);

    impl Display for StaticVariableProvider {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "static-variable-provider")
        }
    }

    impl source_downloader_sdk::component::SdComponent for StaticVariableProvider {}

    #[async_trait]
    impl VariableProvider for StaticVariableProvider {
        async fn item_variables(&self, _: &SourceItem) -> HashMap<String, String> {
            HashMap::new()
        }

        async fn file_variables(
            &self,
            _: &SourceItem,
            _: &PatternVariables,
            files: &[SourceFile],
        ) -> Vec<PatternVariables> {
            files
                .iter()
                .map(|_| HashMap::from([("fileProvider".to_owned(), self.0.to_owned())]))
                .collect()
        }

        fn extract_from(
            &self,
            _: &SourceItem,
            _: &str,
        ) -> Option<HashMap<String, Value>> {
            None
        }

        fn primary_variable_name(&self) -> Option<String> {
            None
        }
    }

    /// 捕获测试期间生成的目标路径，只记录路径供断言。
    #[derive(Debug, Default)]
    struct PathCaptureProvider(ParkingMutex<Vec<PathBuf>>);

    impl Display for PathCaptureProvider {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "path-capture-provider")
        }
    }

    impl source_downloader_sdk::component::SdComponent for PathCaptureProvider {}

    #[async_trait]
    impl VariableProvider for PathCaptureProvider {
        async fn item_variables(&self, _: &SourceItem) -> HashMap<String, String> {
            HashMap::new()
        }

        async fn file_variables(
            &self,
            _: &SourceItem,
            _: &PatternVariables,
            files: &[SourceFile],
        ) -> Vec<PatternVariables> {
            *self.0.lock() = files.iter().map(|file| file.path.clone()).collect();
            vec![HashMap::new(); files.len()]
        }

        fn extract_from(
            &self,
            _: &SourceItem,
            _: &str,
        ) -> Option<HashMap<String, Value>> {
            None
        }

        fn primary_variable_name(&self) -> Option<String> {
            None
        }
    }

    /// 记录测试监听器收到的目标文件名，只验证监听通知内容。
    #[derive(Debug, Default)]
    struct TargetFilenameListener(ParkingMutex<Vec<String>>);

    impl Display for TargetFilenameListener {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "target-filename-listener")
        }
    }

    impl source_downloader_sdk::component::SdComponent for TargetFilenameListener {}

    impl ProcessListener for TargetFilenameListener {
        fn on_item_success(
            &self,
            _: &dyn ProcessContext,
            item_content: &ItemContent,
        ) -> Result<(), ProcessingError> {
            self.0.lock().extend(
                item_content
                    .file_contents
                    .iter()
                    .map(|file| file.target_filename.clone()),
            );
            Ok(())
        }

        fn on_item_error(
            &self,
            _: &dyn ProcessContext,
            _: &SourceItem,
            _: &ProcessingError,
        ) -> Result<(), ProcessingError> {
            Ok(())
        }

        fn on_process_completed(
            &self,
            _: &dyn ProcessContext,
        ) -> Result<(), ProcessingError> {
            Ok(())
        }
    }

    /// 可注入故障的内存 pointer storage 测试替身，不代表生产存储实现。
    #[derive(Default)]
    struct PointerStorage {
        states: ParkingMutex<Vec<ProcessorSourceState>>,
        initial_state: ParkingMutex<Option<ProcessorSourceState>>,
        saved_contents: ParkingMutex<Vec<ProcessingContent>>,
        fail_next_content_save: AtomicBool,
        next_content_id: AtomicUsize,
        content_exists: AtomicBool,
        fail_next_state_load: AtomicBool,
        fail_next_state_save: AtomicBool,
        query_count: AtomicUsize,
        query_results: ParkingMutex<Vec<ProcessingContent>>,
        found_paths: ParkingMutex<Vec<ProcessingTargetPath>>,
        stored_file_contents: ParkingMutex<HashMap<i64, Vec<u8>>>,
        fail_next_file_save: AtomicBool,
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
            content: &ProcessingContent,
        ) -> Result<i64, StorageError> {
            if self.fail_next_content_save.swap(false, Ordering::AcqRel) {
                return Err(StorageError {
                    message: "failed to save processing content".to_owned(),
                });
            }
            let id = content.id.unwrap_or_else(|| {
                self.next_content_id.fetch_add(1, AtomicOrdering::Relaxed) as i64
            });
            let mut saved = content.clone();
            saved.id = Some(id);
            let mut saved_contents = self.saved_contents.lock();
            if let Some(existing) =
                saved_contents.iter_mut().find(|existing| existing.id == Some(id))
            {
                *existing = saved;
            } else {
                saved_contents.push(saved);
            }
            Ok(id)
        }

        async fn processing_content_exists(
            &self,
            _: &str,
            _: &str,
        ) -> Result<bool, StorageError> {
            Ok(self.content_exists.load(Ordering::Acquire))
        }

        async fn delete_processing_content(&self, _: i64) -> Result<(), StorageError> {
            Ok(())
        }

        async fn delete_processing_contents_by_processor(
            &self,
            _: &str,
        ) -> Result<u64, StorageError> {
            Ok(0)
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
            self.query_count.fetch_add(1, AtomicOrdering::Release);
            Ok(self.query_results.lock().clone())
        }

        async fn save_file_contents(
            &self,
            content_id: i64,
            contents: Vec<u8>,
        ) -> Result<(), StorageError> {
            if self.fail_next_file_save.swap(false, Ordering::AcqRel) {
                return Err(StorageError {
                    message: "failed to save file contents".to_owned(),
                });
            }
            self.stored_file_contents.lock().insert(content_id, contents);
            Ok(())
        }

        async fn find_file_contents(
            &self,
            content_id: i64,
        ) -> Result<Option<Vec<u8>>, StorageError> {
            Ok(self.stored_file_contents.lock().get(&content_id).cloned())
        }

        async fn find_processor_source_state(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<ProcessorSourceState>, StorageError> {
            if self.fail_next_state_load.swap(false, Ordering::AcqRel) {
                return Err(StorageError {
                    message: "failed to load processor source state".to_owned(),
                });
            }
            Ok(self.initial_state.lock().clone())
        }

        async fn save_processor_source_state(
            &self,
            state: &ProcessorSourceState,
        ) -> Result<ProcessorSourceState, StorageError> {
            if self.fail_next_state_save.swap(false, Ordering::AcqRel) {
                return Err(StorageError {
                    message: "failed to save processor source state".to_owned(),
                });
            }
            self.states.lock().push(state.clone());
            Ok(state.clone())
        }

        async fn find_paths(
            &self,
            _: &[String],
        ) -> Result<Vec<ProcessingTargetPath>, StorageError> {
            Ok(self.found_paths.lock().clone())
        }

        async fn save_paths(
            &self,
            _: Vec<ProcessingTargetPath>,
        ) -> Result<(), StorageError> {
            Ok(())
        }

        async fn delete_paths_by_processor(&self, _: &str) -> Result<u64, StorageError> {
            Ok(0)
        }
    }
    /// 集中配置 pointer 流程测试的运行参数和故障注入，不作为处理器运行配置。
    struct PointerTestSettings {
        parallelism: u32,
        item_error_continue: bool,
        probe: Option<Arc<ParallelismProbe>>,
        duplicate_source_item: bool,
        invalid_item: Option<usize>,
        resolved_file: Option<PathBuf>,
        submit_count: Option<Arc<AtomicUsize>>,
        unique_files: bool,
        skippable_download_item: Option<usize>,
        retryable_submit_failures: Option<Arc<AtomicUsize>>,
        retryable_fetch_failures: Option<Arc<AtomicUsize>>,
        fail_next_state_save: bool,
        fail_next_file_save: bool,
        submit_probe: Option<Arc<ParallelismProbe>>,
        fail_next_content_save: bool,
        replacement_probe: Option<Arc<ReplacementProbe>>,
        source_headers: Option<HashMap<String, String>>,
        submitted_headers: Option<Arc<ParkingMutex<Option<HashMap<String, String>>>>>,
        resolved_file_tags: Vec<String>,
        download_path: String,
        save_path: PathBuf,
    }

    impl Default for PointerTestSettings {
        fn default() -> Self {
            Self {
                parallelism: 1,
                item_error_continue: false,
                probe: None,
                invalid_item: None,
                resolved_file: None,
                duplicate_source_item: false,
                submit_count: None,
                unique_files: false,
                skippable_download_item: None,
                retryable_submit_failures: None,
                fail_next_state_save: false,
                fail_next_file_save: false,
                retryable_fetch_failures: None,
                submit_probe: None,
                replacement_probe: None,
                source_headers: None,
                fail_next_content_save: false,
                submitted_headers: None,
                resolved_file_tags: Vec::new(),
                download_path: "/tmp/source-downloader-pointer-test".to_owned(),
                save_path: PathBuf::from("/tmp/source-downloader-pointer-test"),
            }
        }
    }

    fn pointer_test_processor(
        pointer_batch_mode: bool,
        item_count: usize,
        filter_items: bool,
    ) -> (SourceProcessor, Arc<PointerStorage>) {
        pointer_test_processor_with_settings(
            pointer_batch_mode,
            item_count,
            filter_items,
            PointerTestSettings::default(),
        )
    }

    fn pointer_test_processor_with_settings(
        pointer_batch_mode: bool,
        item_count: usize,
        filter_items: bool,
        settings: PointerTestSettings,
    ) -> (SourceProcessor, Arc<PointerStorage>) {
        let component = Arc::new(PointerTestComponent {
            item_count,
            probe: settings.probe,
            invalid_item: settings.invalid_item,
            resolved_file: settings.resolved_file,
            submit_count: settings.submit_count,
            unique_files: settings.unique_files,
            skippable_download_item: settings.skippable_download_item,
            duplicate_source_item: settings.duplicate_source_item,
            retryable_submit_failures: settings.retryable_submit_failures,
            submit_probe: settings.submit_probe,
            replacement_probe: settings.replacement_probe,
            source_headers: settings.source_headers,
            submitted_headers: settings.submitted_headers,
            resolved_file_tags: settings.resolved_file_tags,
            download_path: settings.download_path,
            retryable_fetch_failures: settings.retryable_fetch_failures,
        });
        let storage = Arc::new(PointerStorage {
            fail_next_content_save: AtomicBool::new(settings.fail_next_content_save),
            fail_next_state_save: AtomicBool::new(settings.fail_next_state_save),
            fail_next_file_save: AtomicBool::new(settings.fail_next_file_save),
            ..Default::default()
        });
        let item_filters: Vec<Arc<dyn SourceItemFilter>> =
            if filter_items { vec![Arc::new(RejectAllItems)] } else { Vec::new() };
        let processor = SourceProcessor::new(
            "pointer-test".to_string(),
            "pointer-test-source".to_string(),
            settings.save_path.into_boxed_path(),
            component.clone(),
            component.clone(),
            component.clone(),
            component,
            storage.clone(),
            None,
            HashSet::new(),
            Renamer::default(),
            ProcessorOptions {
                save_path_pattern: PathPattern::new_cel(String::new()),
                filename_pattern: PathPattern::new_cel(String::new()),
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
                retry_attempts: 3,
                retry_backoff: Duration::ZERO,
                rename_times_threshold: 3,
                parallelism: settings.parallelism,
                task_group: None,
                fetch_limit: 50,
                item_error_continue: settings.item_error_continue,
                pointer_batch_mode,
                item_rules: Vec::new(),
                file_rules: Vec::new(),
                process_listeners: HashMap::new(),
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
    async fn processor_download_headers_override_source_headers() {
        let submitted_headers = Arc::new(ParkingMutex::new(None));
        let (mut processor, _) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                unique_files: true,
                source_headers: Some(HashMap::from([
                    ("shared".to_owned(), "source".to_owned()),
                    ("source-only".to_owned(), "source".to_owned()),
                ])),
                submitted_headers: Some(submitted_headers.clone()),
                ..Default::default()
            },
        );
        processor.options.download_options.headers = Some(HashMap::from([
            ("shared".to_owned(), "processor".to_owned()),
            ("processor-only".to_owned(), "processor".to_owned()),
        ]));

        processor
            .run_items(vec![SourceItem {
                title: "item-1".to_owned(),
                ..Default::default()
            }])
            .await
            .unwrap();

        assert_eq!(
            *submitted_headers.lock(),
            Some(HashMap::from([
                ("shared".to_owned(), "processor".to_owned()),
                ("source-only".to_owned(), "source".to_owned()),
                ("processor-only".to_owned(), "processor".to_owned()),
            ]))
        );
    }

    #[test]
    fn process_task_uses_configured_task_group() {
        let (mut processor, _) = pointer_test_processor(false, 0, false);
        processor.options.task_group = Some("configured-group".to_owned());

        assert_eq!(processor.group().as_deref(), Some("configured-group"));
    }

    #[tokio::test]
    async fn reprocess_uses_original_item_and_content_id() {
        let submit_count = Arc::new(AtomicUsize::new(0));
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            3,
            false,
            PointerTestSettings {
                unique_files: true,
                submit_count: Some(submit_count.clone()),
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;
        let source_item = SourceItem { title: "item-7".to_owned(), ..Default::default() };
        let content = ProcessingContent {
            id: Some(42),
            processor_name: processor.name.clone(),
            item_hash: source_item.hashing(),
            item_identity: None,
            item_content: ItemContentLite { source_item, item_variables: HashMap::new() },
            rename_times: 5,
            status: ProcessingStatus::Failure,
            failure_reason: Some("previous failure".to_owned()),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: None,
        };

        processor.reprocess(content).await.unwrap();

        let saved_contents = storage.saved_contents.lock();
        assert_eq!(saved_contents.len(), 1);
        assert_eq!(saved_contents[0].id, Some(42));
        assert_eq!(saved_contents[0].item_content.source_item.title, "item-7");
        assert_eq!(saved_contents[0].rename_times, 0);
        assert_eq!(submit_count.load(AtomicOrdering::Acquire), 1);
        assert!(storage.saved_pointers().is_empty());
    }

    #[tokio::test]
    async fn fixed_item_process_uses_only_supplied_items() {
        let submit_count = Arc::new(AtomicUsize::new(0));
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            0,
            false,
            PointerTestSettings {
                unique_files: true,
                submit_count: Some(submit_count.clone()),
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;
        let items = vec![
            SourceItem { title: "item-8".to_owned(), ..Default::default() },
            SourceItem { title: "item-9".to_owned(), ..Default::default() },
        ];

        processor.run_items(items).await.unwrap();

        let saved_contents = storage.saved_contents.lock();
        assert_eq!(
            saved_contents
                .iter()
                .map(|content| content.item_content.source_item.title.as_str())
                .collect::<Vec<_>>(),
            vec!["item-8", "item-9"]
        );
        assert_eq!(submit_count.load(AtomicOrdering::Acquire), 2);
        assert!(storage.saved_pointers().is_empty());
    }

    #[tokio::test]
    async fn fixed_item_process_skips_duplicate_items() {
        let submit_count = Arc::new(AtomicUsize::new(0));
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            0,
            false,
            PointerTestSettings {
                unique_files: true,
                submit_count: Some(submit_count.clone()),
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;
        let item = SourceItem { title: "item-1".to_owned(), ..Default::default() };

        processor.run_items(vec![item.clone(), item]).await.unwrap();

        assert_eq!(storage.saved_contents.lock().len(), 1);
        assert_eq!(submit_count.load(AtomicOrdering::Acquire), 1);
        assert!(storage.saved_pointers().is_empty());
    }

    #[tokio::test]
    async fn source_duplicate_item_is_skipped_without_advancing_pointer() {
        let submit_count = Arc::new(AtomicUsize::new(0));
        let (processor, storage) = pointer_test_processor_with_settings(
            false,
            2,
            false,
            PointerTestSettings {
                duplicate_source_item: true,
                submit_count: Some(submit_count.clone()),
                unique_files: true,
                ..Default::default()
            },
        );
        processor.run().await.unwrap();

        assert_eq!(submit_count.load(AtomicOrdering::Acquire), 1);
        assert_eq!(storage.saved_pointers(), vec![json!(1)]);
    }

    #[tokio::test]
    async fn item_content_filtered_records_are_not_persisted() {
        let (mut processor, storage) = pointer_test_processor(false, 1, false);
        processor.options.save_processing_content = true;
        processor.options.item_content_filters = vec![Arc::new(RejectAllContent)];
        processor.async_downloader = None;
        let each_listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![each_listener.clone()]);
        let batch_listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![batch_listener.clone()]);

        processor.run().await.unwrap();

        assert!(storage.saved_contents.lock().is_empty());
        assert_eq!(storage.saved_pointers(), vec![json!(1)]);
        assert_eq!(each_listener.successes.load(AtomicOrdering::Relaxed), 1);
        assert!(each_listener.context_visible.load(AtomicOrdering::Relaxed));
        assert_eq!(batch_listener.completed_items.lock().as_slice(), ["item-1"]);
    }
    #[tokio::test]
    async fn file_taggers_preserve_source_tags_without_processor_tags() {
        let (mut processor, _) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                resolved_file: Some(PathBuf::from("tagged.txt")),
                resolved_file_tags: vec!["source".to_owned()],
                ..Default::default()
            },
        );
        processor.tags.insert("processor".to_owned());
        processor.options.file_taggers = vec![Arc::new(StaticFileTagger)];

        let results = processor.dry_run(DryRunOptions::default()).await.unwrap();

        assert_eq!(
            results[0].file_contents[0].tags,
            vec!["generated".to_owned(), "source".to_owned()]
        );
    }

    #[tokio::test]
    async fn processor_normalizes_roots_and_relativizes_absolute_source_files() {
        let current_dir = std::env::current_dir().unwrap();
        let relative_download_path = PathBuf::from("target/path-contract-download");
        let relative_save_path = PathBuf::from("target/path-contract-save");
        let expected_download_path = current_dir.join(&relative_download_path);
        let expected_save_path = current_dir.join(&relative_save_path);
        let source_file_path = expected_download_path.join("nested/file.txt");
        let provider = Arc::new(PathCaptureProvider::default());
        let (mut processor, _) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                resolved_file: Some(source_file_path.clone()),
                download_path: relative_download_path.to_string_lossy().into_owned(),
                save_path: relative_save_path,
                ..Default::default()
            },
        );
        processor.options.variable_providers = vec![provider.clone()];

        let results = processor.dry_run(DryRunOptions::default()).await.unwrap();
        let file = &results[0].file_contents[0];

        assert_eq!(
            (
                provider.0.lock().clone(),
                file.download_path.clone(),
                file.source_save_path.clone(),
                file.file_download_path.clone(),
            ),
            (
                vec![PathBuf::from("nested/file.txt")],
                expected_download_path,
                expected_save_path,
                source_file_path,
            )
        );
    }

    #[tokio::test]
    async fn processor_relativizes_absolute_source_files_outside_download_root() {
        let current_dir = std::env::current_dir().unwrap();
        let relative_download_path = PathBuf::from("target/path-contract-download");
        let source_file_path =
            current_dir.join("target/path-contract-outside/nested/file.txt");
        let provider = Arc::new(PathCaptureProvider::default());
        let (mut processor, _) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                resolved_file: Some(source_file_path),
                download_path: relative_download_path.to_string_lossy().into_owned(),
                ..Default::default()
            },
        );
        processor.options.variable_providers = vec![provider.clone()];

        processor.dry_run(DryRunOptions::default()).await.unwrap();

        assert_eq!(
            *provider.0.lock(),
            vec![PathBuf::from("../path-contract-outside/nested/file.txt")]
        );
    }

    #[tokio::test]
    async fn dry_run_returns_results_without_side_effects() {
        let submit_count = Arc::new(AtomicUsize::new(0));
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                unique_files: true,
                submit_count: Some(submit_count.clone()),
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;

        let results = processor.dry_run(DryRunOptions::default()).await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].processing_content.status,
            ProcessingStatus::WaitingToRename
        );
        assert_eq!(results[0].file_contents.len(), 1);
        assert_eq!(results[0].file_contents[0].target_filename, "1.txt");
        let serialized = serde_json::to_value(&results[0]).unwrap();
        assert!(serialized.get("processingContent").is_some());
        assert!(serialized.get("fileContents").is_some());
        assert_eq!(submit_count.load(AtomicOrdering::Acquire), 0);
        assert_eq!(storage.next_content_id.load(AtomicOrdering::Acquire), 0);
        assert!(storage.saved_pointers().is_empty());
    }

    #[tokio::test]
    async fn dry_run_stream_emits_each_result_without_side_effects() {
        let submit_count = Arc::new(AtomicUsize::new(0));
        let (processor, storage) = pointer_test_processor_with_settings(
            false,
            2,
            false,
            PointerTestSettings {
                unique_files: true,
                submit_count: Some(submit_count.clone()),
                ..Default::default()
            },
        );
        let processor = Arc::new(processor);

        let results =
            processor.dry_run_stream(DryRunOptions::default()).collect::<Vec<_>>().await;

        assert_eq!(results.len(), 2);
        assert_eq!(
            results
                .into_iter()
                .map(|result| result
                    .unwrap()
                    .processing_content
                    .item_content
                    .source_item
                    .title)
                .collect::<Vec<_>>(),
            ["item-1", "item-2"]
        );
        assert_eq!(submit_count.load(AtomicOrdering::Acquire), 0);
        assert_eq!(storage.next_content_id.load(AtomicOrdering::Acquire), 0);
        assert!(storage.saved_pointers().is_empty());
    }

    #[tokio::test]
    async fn dropping_dry_run_stream_cancels_processing() {
        let probe = Arc::new(ParallelismProbe::new(2));
        let (processor, _) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings { probe: Some(probe.clone()), ..Default::default() },
        );
        let processor = Arc::new(processor);
        let stream = processor.dry_run_stream(DryRunOptions::default());
        tokio::time::timeout(Duration::from_secs(1), async {
            while probe.active.load(AtomicOrdering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dry-run should reach the resolver");
        assert!(processor.runtime_snapshot().processing);

        drop(stream);

        tokio::time::timeout(Duration::from_millis(100), async {
            while processor.runtime_snapshot().processing {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropping the stream should cancel its dry-run");
    }

    #[tokio::test]
    async fn dry_run_can_ignore_processed_item_filter() {
        let (mut processor, storage) = pointer_test_processor(false, 1, false);
        storage.content_exists.store(true, Ordering::Release);
        processor.options.item_filters = vec![Arc::new(SourceItemIdentityFilter {
            processor_name: processor.name.clone(),
            storage: storage.clone(),
        })];

        let unfiltered = processor.dry_run(DryRunOptions::default()).await.unwrap();
        let filtered = processor
            .dry_run(DryRunOptions { filter_processed: true, ..Default::default() })
            .await
            .unwrap();

        assert_eq!(unfiltered.len(), 1);
        assert!(filtered.is_empty());
    }

    #[tokio::test]
    async fn item_group_provider_is_used_for_file_variables() {
        let (mut processor, _) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings { unique_files: true, ..Default::default() },
        );
        processor.async_downloader = None;
        processor.options.filename_pattern =
            PathPattern::new_cel("{fileProvider}.txt".to_owned());
        processor.options.variable_providers =
            vec![Arc::new(StaticVariableProvider("global"))];
        processor.options.item_rules = vec![ItemRule {
            matcher: Box::new(crate::process::rule::ExpressionAndTagMatcher::new(
                None,
                Some(HashSet::new()),
            )),
            strategy: ItemStrategy {
                save_path_pattern: None,
                filename_pattern: None,
                item_filters: None,
                variable_providers: Some(vec![Arc::new(StaticVariableProvider(
                    "item-group",
                ))]),
            },
        }];
        let listener = Arc::new(TargetFilenameListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![listener.clone()]);

        processor.run().await.unwrap();

        assert_eq!(*listener.0.lock(), vec!["item-group.txt".to_owned()]);
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

    #[tokio::test]
    async fn parallel_items_commit_pointers_in_fetch_order() {
        let probe = Arc::new(ParallelismProbe::new(2));
        let (processor, storage) = pointer_test_processor_with_settings(
            false,
            3,
            false,
            PointerTestSettings {
                parallelism: 2,
                probe: Some(probe.clone()),
                ..Default::default()
            },
        );

        tokio::time::timeout(Duration::from_secs(1), processor.run())
            .await
            .expect("parallel processing timed out")
            .unwrap();

        assert_eq!(probe.max_active.load(AtomicOrdering::Acquire), 2);
        assert_ne!(probe.completed.lock().first(), Some(&1));

        assert_eq!(storage.saved_pointers(), vec![json!(1), json!(2), json!(3)]);
    }
    #[tokio::test]
    async fn parallel_items_submit_downloads_concurrently() {
        let probe = Arc::new(ParallelismProbe::new(2));
        let (processor, _) = pointer_test_processor_with_settings(
            false,
            2,
            false,
            PointerTestSettings {
                parallelism: 2,
                unique_files: true,
                submit_probe: Some(probe.clone()),
                ..Default::default()
            },
        );

        tokio::time::timeout(Duration::from_secs(1), processor.run())
            .await
            .expect("parallel downloads timed out")
            .unwrap();

        assert_eq!(probe.max_active.load(AtomicOrdering::Acquire), 2);
    }
    #[tokio::test]
    async fn parallel_items_reserve_shared_target_once() {
        let submit_count = Arc::new(AtomicUsize::new(0));
        let (processor, storage) = pointer_test_processor_with_settings(
            false,
            2,
            false,
            PointerTestSettings {
                parallelism: 2,
                resolved_file: Some(PathBuf::from("shared.txt")),
                submit_count: Some(submit_count.clone()),
                ..Default::default()
            },
        );

        processor.run().await.unwrap();

        assert_eq!(submit_count.load(AtomicOrdering::Acquire), 1);
        assert_eq!(storage.saved_pointers(), vec![json!(1), json!(2)]);
    }

    #[tokio::test]
    async fn later_item_replaces_and_cancels_in_flight_download() {
        let submit_count = Arc::new(AtomicUsize::new(0));
        let replacement_probe = Arc::new(ReplacementProbe::default());
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            2,
            false,
            PointerTestSettings {
                parallelism: 2,
                resolved_file: Some(PathBuf::from("shared.txt")),
                submit_count: Some(submit_count.clone()),
                replacement_probe: Some(replacement_probe.clone()),
                ..Default::default()
            },
        );
        processor.options.file_replacement_decider = Arc::new(ReplaceInFlight);
        processor.options.save_processing_content = true;
        let listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![listener.clone()]);
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![listener.clone()]);

        tokio::time::timeout(Duration::from_secs(1), processor.run())
            .await
            .expect("in-flight replacement timed out")
            .unwrap();

        assert_eq!(*replacement_probe.cancelled_items.lock(), vec!["item-1".to_owned()]);
        assert_eq!(submit_count.load(AtomicOrdering::Acquire), 2);
        assert_eq!(storage.saved_pointers(), vec![json!(1), json!(2)]);
        let saved_contents = storage.saved_contents.lock();
        let first = saved_contents
            .iter()
            .find(|content| content.item_content.source_item.title == "item-1")
            .unwrap();
        let second = saved_contents
            .iter()
            .find(|content| content.item_content.source_item.title == "item-2")
            .unwrap();
        assert_eq!(first.status, ProcessingStatus::Cancelled);
        assert_eq!(second.status, ProcessingStatus::WaitingToRename);
        assert!(listener.successful_statuses.lock().is_empty());
        assert_eq!(listener.errors.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(listener.completions.load(AtomicOrdering::Relaxed), 0);
        assert!(listener.completed_items.lock().is_empty());
    }

    #[tokio::test]
    async fn zero_parallelism_runs_sequentially() {
        let probe = Arc::new(ParallelismProbe::new(1));
        let (processor, storage) = pointer_test_processor_with_settings(
            false,
            2,
            false,
            PointerTestSettings {
                parallelism: 0,
                probe: Some(probe.clone()),
                ..Default::default()
            },
        );

        processor.run().await.unwrap();

        assert_eq!(probe.max_active.load(AtomicOrdering::Acquire), 1);
        assert_eq!(storage.saved_pointers(), vec![json!(1), json!(2)]);
    }

    #[tokio::test]
    async fn process_item_retries_retryable_download_errors() {
        let submit_count = Arc::new(AtomicUsize::new(0));
        let retryable_failures = Arc::new(AtomicUsize::new(1));
        let (processor, storage) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                unique_files: true,
                submit_count: Some(submit_count.clone()),
                retryable_submit_failures: Some(retryable_failures.clone()),
                ..Default::default()
            },
        );

        processor.run().await.unwrap();

        assert_eq!(retryable_failures.load(AtomicOrdering::Acquire), 0);
        assert_eq!(submit_count.load(AtomicOrdering::Acquire), 2);
        assert_eq!(storage.saved_pointers(), vec![json!(1)]);
    }

    #[tokio::test]
    async fn fetch_retries_retryable_source_errors() {
        let fetch_failures = Arc::new(AtomicUsize::new(2));
        let (processor, storage) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                retryable_fetch_failures: Some(fetch_failures.clone()),
                ..Default::default()
            },
        );

        processor.run().await.unwrap();

        assert_eq!(fetch_failures.load(AtomicOrdering::Acquire), 0);
        assert_eq!(storage.saved_pointers(), vec![json!(1)]);
    }

    #[tokio::test]
    async fn exhausted_fetch_error_prevents_item_settlement() {
        let fetch_failures = Arc::new(AtomicUsize::new(usize::MAX));
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                retryable_fetch_failures: Some(fetch_failures),
                ..Default::default()
            },
        );
        processor.options.retry_attempts = 1;

        let error = processor.run().await.unwrap_err();

        assert!(error.contains("retryable fetch error"));
        assert!(storage.saved_pointers().is_empty());
    }

    #[tokio::test]
    async fn apply_retry_honors_configured_attempts() {
        let attempts = AtomicUsize::new(0);
        let result = SourceProcessor::apply_retry(
            || {
                attempts.fetch_add(1, AtomicOrdering::Relaxed);
                async { Err::<(), _>(ProcessingError::retryable("retry")) }
            },
            "test",
            2,
            Duration::ZERO,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(3, attempts.load(AtomicOrdering::Relaxed));
    }

    #[tokio::test]
    async fn rename_task_checks_immediately() {
        let (mut processor, storage) = pointer_test_processor(false, 0, false);
        processor.options.rename_task_interval = Duration::from_secs(3600);
        let processor = Arc::new(processor);

        processor.start_rename_task();

        tokio::time::timeout(Duration::from_millis(100), async {
            while storage.query_count.load(AtomicOrdering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("rename task should check before its configured interval");
    }

    #[tokio::test]
    async fn item_error_stops_new_work_and_drains_started_items() {
        let probe = Arc::new(ParallelismProbe::new(2));
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            3,
            false,
            PointerTestSettings {
                parallelism: 2,
                probe: Some(probe.clone()),
                invalid_item: Some(1),
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;

        processor.run().await.unwrap();

        assert_eq!(*probe.completed.lock(), vec![2, 1]);
        assert!(storage.saved_pointers().is_empty());
        assert!(
            storage
                .saved_contents
                .lock()
                .iter()
                .all(|content| content.status != ProcessingStatus::Failure)
        );
    }

    #[tokio::test]
    async fn item_persistence_error_stops_new_work_and_drains_started_items() {
        let probe = Arc::new(ParallelismProbe::new(2));
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            3,
            false,
            PointerTestSettings {
                parallelism: 2,
                probe: Some(probe.clone()),
                unique_files: true,
                fail_next_file_save: true,
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;

        processor.run().await.unwrap();

        assert_eq!(*probe.completed.lock(), vec![2, 1]);
        assert!(storage.saved_pointers().is_empty());
        let saved_contents = storage.saved_contents.lock();
        assert_eq!(saved_contents.len(), 2);
        assert_eq!(
            saved_contents
                .iter()
                .map(|content| content.item_content.source_item.title.as_str())
                .collect::<Vec<_>>(),
            vec!["item-1", "item-2"]
        );
        assert!(
            saved_contents
                .iter()
                .all(|content| content.status == ProcessingStatus::WaitingToRename)
        );
    }

    #[tokio::test]
    async fn item_content_persistence_error_stops_new_work_and_drains_started_items() {
        let probe = Arc::new(ParallelismProbe::new(2));
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            3,
            false,
            PointerTestSettings {
                parallelism: 2,
                probe: Some(probe.clone()),
                unique_files: true,
                fail_next_content_save: true,
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;

        processor.run().await.unwrap();

        assert_eq!(*probe.completed.lock(), vec![2, 1]);
        assert!(storage.saved_pointers().is_empty());
        let saved_contents = storage.saved_contents.lock();
        assert_eq!(saved_contents.len(), 1);
        assert_eq!(saved_contents[0].item_content.source_item.title, "item-2");
    }

    #[tokio::test]
    async fn item_error_continue_processes_remaining_items() {
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            3,
            false,
            PointerTestSettings {
                parallelism: 2,
                item_error_continue: true,
                invalid_item: Some(1),
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;

        processor.run().await.unwrap();

        assert_eq!(storage.saved_pointers(), vec![json!(2), json!(3)]);
        let saved_contents = storage.saved_contents.lock();
        let failed = saved_contents
            .iter()
            .find(|content| content.status == ProcessingStatus::Failure)
            .expect("continued item error should be persisted");
        assert_eq!(failed.item_content.source_item.title, "item-1");
        assert!(
            failed.failure_reason.as_deref().is_some_and(|reason| !reason.is_empty())
        );
    }

    #[tokio::test]
    async fn item_error_continue_recovers_from_pointer_save_error() {
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            3,
            false,
            PointerTestSettings {
                item_error_continue: true,
                fail_next_state_save: true,
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;

        processor.run().await.unwrap();

        assert_eq!(storage.saved_pointers(), vec![json!(2), json!(3)]);
        let saved_contents = storage.saved_contents.lock();
        let first_item = saved_contents
            .iter()
            .filter(|content| content.item_content.source_item.title == "item-1")
            .collect_vec();
        assert_eq!(first_item.len(), 1);
        assert_eq!(first_item[0].status, ProcessingStatus::Failure);
        assert_eq!(
            first_item[0].failure_reason.as_deref(),
            Some("failed to save processor source state")
        );
    }

    #[tokio::test]
    async fn item_error_continue_persists_filtered_pointer_save_error() {
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            3,
            true,
            PointerTestSettings {
                item_error_continue: true,
                fail_next_state_save: true,
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;

        processor.run().await.unwrap();

        assert_eq!(storage.saved_pointers().last(), Some(&json!(3)));
        let saved_contents = storage.saved_contents.lock();
        assert_eq!(saved_contents.len(), 1);
        assert_eq!(saved_contents[0].status, ProcessingStatus::Failure);
        assert_eq!(saved_contents[0].item_content.source_item.title, "item-1");
        assert_eq!(
            saved_contents[0].failure_reason.as_deref(),
            Some("failed to save processor source state")
        );
    }
    #[tokio::test]
    async fn skippable_item_error_continues_when_error_continue_is_disabled() {
        let (mut processor, storage) = pointer_test_processor_with_settings(
            false,
            3,
            false,
            PointerTestSettings {
                parallelism: 2,
                unique_files: true,
                skippable_download_item: Some(1),
                ..Default::default()
            },
        );
        processor.options.save_processing_content = true;

        processor.run().await.unwrap();

        assert_eq!(storage.saved_pointers(), vec![json!(2), json!(3)]);
        let saved_contents = storage.saved_contents.lock();
        let failed = saved_contents
            .iter()
            .find(|content| content.status == ProcessingStatus::Failure)
            .expect("skippable item error should be persisted");
        assert_eq!(failed.item_content.source_item.title, "item-1");
        assert_eq!(failed.failure_reason.as_deref(), Some("skippable test error"));
    }
    #[tokio::test]
    async fn existing_pointer_is_preserved_when_no_items_are_fetched() {
        let (processor, storage) = pointer_test_processor(true, 0, false);
        *storage.initial_state.lock() = Some(ProcessorSourceState {
            id: Some(7),
            processor_name: processor.name.to_owned(),
            source_id: processor.source_id.to_owned(),
            last_pointer: json!(41),
            last_active_time: None,
            retry_times: 0,
        });

        processor.run().await.unwrap();

        assert_eq!(storage.saved_pointers(), vec![json!(41)]);
    }

    #[tokio::test]
    async fn batch_pointer_state_save_error_is_reported_after_item_processing() {
        let (processor, storage) = pointer_test_processor_with_settings(
            true,
            1,
            false,
            PointerTestSettings { fail_next_state_save: true, ..Default::default() },
        );

        let error = processor.run().await.unwrap_err();

        assert_eq!(error, "failed to save processor source state");
        assert!(storage.saved_pointers().is_empty());
        assert_eq!(
            processor.runtime_snapshot().last_process_failed_message.as_deref(),
            Some("failed to save processor source state")
        );
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
    /// 记录测试期间收到的成功、错误和完成通知，只观测监听器事件。
    #[derive(Debug, Default)]
    struct RecordingListener {
        successes: AtomicUsize,
        errors: AtomicUsize,
        completions: AtomicUsize,
        context_visible: AtomicBool,
        completed_items: ParkingMutex<Vec<String>>,
        error_messages: ParkingMutex<Vec<String>>,
        successful_statuses: ParkingMutex<Vec<ProcessingStatus>>,
    }

    impl Display for RecordingListener {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "recording-listener")
        }
    }

    impl source_downloader_sdk::component::SdComponent for RecordingListener {}

    /// 模拟永不完成的测试下载操作，只验证取消和关闭行为。
    #[derive(Debug)]
    struct NeverFinishedDownloader;

    impl Display for NeverFinishedDownloader {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "never-finished")
        }
    }

    impl source_downloader_sdk::component::SdComponent for NeverFinishedDownloader {}

    #[async_trait]
    impl Downloader for NeverFinishedDownloader {
        async fn submit(&self, _: &DownloadTask) -> Result<(), ProcessingError> {
            Ok(())
        }

        fn default_download_path(&self) -> &str {
            "/tmp/source-downloader-never-finished"
        }

        async fn cancel(
            &self,
            _: &SourceItem,
            _: &[SourceFile],
        ) -> Result<(), ProcessingError> {
            Ok(())
        }
    }

    impl AsyncDownloader for NeverFinishedDownloader {
        fn is_finished(&self, _: &SourceItem) -> Option<bool> {
            Some(false)
        }
    }

    /// 模拟缺少下载状态的测试下载器，只验证相应错误处理。
    #[derive(Debug)]
    struct MissingDownloadStateDownloader;

    impl Display for MissingDownloadStateDownloader {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "missing-download-state")
        }
    }

    impl source_downloader_sdk::component::SdComponent for MissingDownloadStateDownloader {}

    #[async_trait]
    impl Downloader for MissingDownloadStateDownloader {
        async fn submit(&self, _: &DownloadTask) -> Result<(), ProcessingError> {
            Ok(())
        }

        fn default_download_path(&self) -> &str {
            "/tmp/source-downloader-missing-state"
        }

        async fn cancel(
            &self,
            _: &SourceItem,
            _: &[SourceFile],
        ) -> Result<(), ProcessingError> {
            Ok(())
        }
    }

    impl AsyncDownloader for MissingDownloadStateDownloader {
        fn is_finished(&self, _: &SourceItem) -> Option<bool> {
            None
        }
    }
    impl ProcessListener for RecordingListener {
        fn on_item_success(
            &self,
            ctx: &dyn ProcessContext,
            item_content: &ItemContent,
        ) -> Result<(), ProcessingError> {
            self.successes.fetch_add(1, AtomicOrdering::Relaxed);
            self.context_visible.store(
                ctx.get_item_content(item_content.source_item).is_some(),
                AtomicOrdering::Relaxed,
            );
            self.successful_statuses.lock().push(item_content.status);
            Ok(())
        }

        fn on_item_error(
            &self,
            _: &dyn ProcessContext,
            _: &SourceItem,
            error: &ProcessingError,
        ) -> Result<(), ProcessingError> {
            self.errors.fetch_add(1, AtomicOrdering::Relaxed);
            self.error_messages.lock().push(error.message().to_owned());
            Ok(())
        }

        fn on_process_completed(
            &self,
            ctx: &dyn ProcessContext,
        ) -> Result<(), ProcessingError> {
            self.completed_items
                .lock()
                .extend(ctx.processed_items().map(|item| item.title.to_owned()));
            self.completions.fetch_add(1, AtomicOrdering::Relaxed);
            Ok(())
        }
    }

    #[test]
    fn processor_information_exposes_resolved_configuration() {
        let (mut processor, _) = pointer_test_processor(false, 1, false);
        processor.category = Some("series".to_owned());
        processor.tags = HashSet::from(["tracked".to_owned()]);
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![Arc::new(RecordingListener::default())]);

        let information = processor.information();

        assert_eq!(information.name, "pointer-test");
        assert_eq!(information.source_id, "pointer-test-source");
        assert_eq!(information.source, "pointer-test");
        assert_eq!(information.item_file_resolver, "pointer-test");
        assert_eq!(information.downloader, "pointer-test");
        assert_eq!(information.file_mover, "pointer-test");
        assert_eq!(
            information.process_listeners.get(&ListenerMode::Each),
            Some(&vec!["recording-listener".to_owned()])
        );
        assert_eq!(information.download_path, processor.download_path.to_string_lossy());
        assert_eq!(information.source_save_path, processor.save_path.to_string_lossy());
        assert_eq!(information.category.as_deref(), Some("series"));
        assert_eq!(information.tags, HashSet::from(["tracked".to_owned()]));
        assert_eq!(information.options.parallelism, processor.options.parallelism);
        assert_eq!(
            information.options.variable_error_strategy,
            VariableErrorStrategy::Stay
        );
    }

    #[tokio::test]
    async fn source_state_uses_source_default_when_not_persisted() {
        let (processor, _) = pointer_test_processor(false, 1, false);

        let state = processor.source_state().await.unwrap();

        assert_eq!(state.id, None);
        assert_eq!(state.processor_name, "pointer-test");
        assert_eq!(state.source_id, "pointer-test-source");
        assert_eq!(state.last_pointer, json!(0));
    }

    #[tokio::test]
    async fn update_source_pointer_requires_state_and_merges_object_values() {
        let (processor, storage) = pointer_test_processor(false, 1, false);
        assert!(
            processor
                .update_source_pointer("pointer-test-source", json!({"page": 2}))
                .await
                .unwrap()
                .is_none()
        );
        *storage.initial_state.lock() = Some(ProcessorSourceState {
            id: Some(7),
            processor_name: processor.name.clone(),
            source_id: processor.source_id.clone(),
            last_pointer: json!({"page": 1, "retained": true}),
            last_active_time: None,
            retry_times: 0,
        });

        let updated = processor
            .update_source_pointer(
                "pointer-test-source",
                json!({"page": 2, "added": "value"}),
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(
            updated.last_pointer,
            json!({"page": 2, "retained": true, "added": "value"})
        );
        assert_eq!(storage.saved_pointers(), vec![updated.last_pointer]);
    }

    /// 始终返回监听器错误的测试替身，只验证监听器错误处理。
    #[derive(Debug)]
    struct FailingListener;
    #[tokio::test]
    async fn runtime_snapshot_tracks_completed_runs() {
        let created_before = OffsetDateTime::now_utc();
        let (mut processor, _) = pointer_test_processor(false, 1, false);
        processor.async_downloader = None;
        let created_after = OffsetDateTime::now_utc();

        let initial = processor.runtime_snapshot();
        assert!(initial.created_at >= created_before);
        assert!(initial.created_at <= created_after);
        assert_eq!(initial.last_process_failed_message, None);
        assert_eq!(initial.last_start_process_time, None);
        assert_eq!(initial.last_end_process_time, None);
        assert!(!initial.processing);

        processor.run().await.unwrap();

        let completed = processor.runtime_snapshot();
        assert_eq!(completed.last_process_failed_message, None);
        assert!(!completed.processing);
        assert!(
            completed.last_end_process_time.unwrap()
                >= completed.last_start_process_time.unwrap()
        );
    }

    #[tokio::test]
    async fn runtime_snapshot_records_run_failure() {
        let (mut processor, storage) = pointer_test_processor(false, 1, false);
        processor.async_downloader = None;
        storage.fail_next_state_load.store(true, Ordering::Release);

        assert!(processor.run().await.is_err());

        let snapshot = processor.runtime_snapshot();
        assert!(
            snapshot
                .last_process_failed_message
                .as_ref()
                .is_some_and(|message| !message.is_empty())
        );
        assert!(snapshot.last_start_process_time.is_some());
        assert!(snapshot.last_end_process_time.is_some());
        assert!(!snapshot.processing);
    }

    #[tokio::test]
    async fn runtime_snapshot_reports_active_processing() {
        let replacement_probe = Arc::new(ReplacementProbe::default());
        let (processor, _) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                resolved_file: Some(PathBuf::from("runtime-snapshot.txt")),
                replacement_probe: Some(replacement_probe.clone()),
                ..Default::default()
            },
        );
        let processor = Arc::new(processor);
        let running_processor = processor.clone();
        let run = tokio::spawn(async move { running_processor.run().await });
        replacement_probe.first_submitted.notified().await;

        let active = processor.runtime_snapshot();
        assert!(active.processing);
        assert!(active.last_start_process_time.is_some());
        assert_eq!(active.last_end_process_time, None);

        replacement_probe.first_cancelled.notify_one();
        run.await.unwrap().unwrap();
        let completed = processor.runtime_snapshot();
        assert!(!completed.processing);
        assert!(completed.last_end_process_time.is_some());
    }

    #[tokio::test]
    async fn close_cancels_active_processing_and_rejects_new_runs() {
        let replacement_probe = Arc::new(ReplacementProbe::default());
        let (processor, _) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                resolved_file: Some(PathBuf::from("cancelled-process.txt")),
                replacement_probe: Some(replacement_probe.clone()),
                ..Default::default()
            },
        );
        let processor = Arc::new(processor);
        let running_processor = processor.clone();
        let run = tokio::spawn(async move { running_processor.run().await });
        replacement_probe.first_submitted.notified().await;

        processor.close();

        let error = run.await.unwrap().unwrap_err();
        assert_eq!(error, "Processing cancelled");
        let snapshot = processor.runtime_snapshot();
        assert!(!snapshot.processing);
        assert_eq!(
            snapshot.last_process_failed_message.as_deref(),
            Some("Processing cancelled")
        );
        assert_eq!(processor.run().await.unwrap_err(), "Processor is closed");
    }

    impl Display for FailingListener {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "failing-listener")
        }
    }

    impl source_downloader_sdk::component::SdComponent for FailingListener {}

    impl ProcessListener for FailingListener {
        fn on_item_success(
            &self,
            _: &dyn ProcessContext,
            _: &ItemContent,
        ) -> Result<(), ProcessingError> {
            Err(ProcessingError::non_retryable("item success listener failure"))
        }

        fn on_item_error(
            &self,
            _: &dyn ProcessContext,
            _: &SourceItem,
            _: &ProcessingError,
        ) -> Result<(), ProcessingError> {
            Err(ProcessingError::non_retryable("item error listener failure"))
        }

        fn on_process_completed(
            &self,
            _: &dyn ProcessContext,
        ) -> Result<(), ProcessingError> {
            Err(ProcessingError::non_retryable("process completed listener failure"))
        }
    }

    #[tokio::test]
    async fn process_notifies_listeners_for_their_configured_mode() {
        let (mut processor, _) = pointer_test_processor(false, 1, false);
        processor.async_downloader = None;
        let each_listener = Arc::new(RecordingListener::default());
        let batch_listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![each_listener.clone()]);
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![batch_listener.clone()]);

        processor.run().await.unwrap();

        assert_eq!(each_listener.successes.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(each_listener.errors.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(each_listener.completions.load(AtomicOrdering::Relaxed), 0);
        assert!(each_listener.context_visible.load(AtomicOrdering::Relaxed));
        assert_eq!(batch_listener.successes.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(batch_listener.completions.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(*batch_listener.completed_items.lock(), vec!["item-1".to_owned()]);
    }

    #[tokio::test]
    async fn process_listener_failures_are_isolated() {
        let (mut success_processor, _) = pointer_test_processor(false, 1, false);
        success_processor.async_downloader = None;
        let success_listener = Arc::new(RecordingListener::default());
        success_processor.options.process_listeners.insert(
            ListenerMode::Each,
            vec![Arc::new(FailingListener), success_listener.clone()],
        );
        success_processor.options.process_listeners.insert(
            ListenerMode::Batch,
            vec![Arc::new(FailingListener), success_listener.clone()],
        );

        success_processor.run().await.unwrap();

        assert_eq!(success_listener.successes.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(success_listener.completions.load(AtomicOrdering::Relaxed), 1);

        let (mut error_processor, _) = pointer_test_processor_with_settings(
            false,
            1,
            false,
            PointerTestSettings {
                item_error_continue: true,
                invalid_item: Some(1),
                ..Default::default()
            },
        );
        error_processor.async_downloader = None;
        let error_listener = Arc::new(RecordingListener::default());
        error_processor.options.process_listeners.insert(
            ListenerMode::Each,
            vec![Arc::new(FailingListener), error_listener.clone()],
        );
        error_processor.options.process_listeners.insert(
            ListenerMode::Batch,
            vec![Arc::new(FailingListener), error_listener.clone()],
        );

        error_processor.run().await.unwrap();

        assert_eq!(error_listener.errors.load(AtomicOrdering::Relaxed), 1);
    }

    #[tokio::test]
    async fn async_process_defers_listeners_until_rename() {
        let (mut processor, _) = pointer_test_processor(false, 1, false);
        let each_listener = Arc::new(RecordingListener::default());
        let batch_listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![each_listener.clone()]);
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![batch_listener.clone()]);

        processor.run().await.unwrap();

        assert_eq!(each_listener.successes.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(batch_listener.completions.load(AtomicOrdering::Relaxed), 0);
    }

    #[tokio::test]
    async fn async_rename_does_not_complete_batch_while_downloads_are_unfinished() {
        let processor_name = "async-rename-unfinished";
        let storage = storage().await.clone();
        let source_item =
            SourceItem { title: "unfinished-item".to_owned(), ..Default::default() };
        storage
            .save_processing_content(&ProcessingContent {
                id: None,
                processor_name: processor_name.to_owned(),
                item_hash: source_item.hashing(),
                item_identity: None,
                item_content: ItemContentLite {
                    source_item,
                    item_variables: HashMap::new(),
                },
                rename_times: 0,
                status: ProcessingStatus::WaitingToRename,
                failure_reason: None,
                created_at: OffsetDateTime::now_utc(),
                updated_at: None,
            })
            .await
            .unwrap();
        let (mut processor, _) = pointer_test_processor(false, 0, false);
        processor.name = processor_name.to_owned();
        processor.processing_storage = storage;
        processor.async_downloader = Some(Arc::new(NeverFinishedDownloader));
        let batch_listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![batch_listener.clone()]);

        assert_eq!(processor.run_rename().await.unwrap(), 0);

        assert_eq!(batch_listener.completions.load(AtomicOrdering::Relaxed), 0);
    }
    #[tokio::test]
    async fn async_rename_missing_download_state_emits_no_listener_events() {
        use std::sync::OnceLock;

        let source_item =
            SourceItem { title: "missing-state".to_owned(), ..Default::default() };
        let content = ProcessingContent {
            id: Some(2),
            processor_name: "async-rename-missing-state".to_owned(),
            item_hash: source_item.hashing(),
            item_identity: None,
            item_content: ItemContentLite { source_item, item_variables: HashMap::new() },
            rename_times: 0,
            status: ProcessingStatus::WaitingToRename,
            failure_reason: None,
            created_at: OffsetDateTime::now_utc(),
            updated_at: None,
        };
        let file = FileContent {
            download_path: PathBuf::from("/download"),
            file_download_path: PathBuf::from("/download/file.txt"),
            source_save_path: PathBuf::from("/save"),
            pattern_variables: HashMap::new(),
            tags: Vec::new(),
            attrs: Default::default(),
            file_uri: None,
            target_save_path: PathBuf::from("/save"),
            target_filename: "file.txt".to_owned(),
            exist_target_path: None,
            errors: Vec::new(),
            status: Normal,
            target_path: OnceLock::new(),
            data: None,
        };
        let (mut processor, storage) = pointer_test_processor(false, 0, false);
        processor.name = content.processor_name.clone();
        processor.async_downloader = Some(Arc::new(MissingDownloadStateDownloader));
        *storage.query_results.lock() = vec![content];
        storage
            .stored_file_contents
            .lock()
            .insert(2, encode_files_and_compress(&vec![file]).unwrap());
        let each_listener = Arc::new(RecordingListener::default());
        let batch_listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![each_listener.clone()]);
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![batch_listener.clone()]);

        assert_eq!(processor.run_rename().await.unwrap(), 0);

        assert_eq!(each_listener.successes.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(each_listener.errors.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(batch_listener.completions.load(AtomicOrdering::Relaxed), 0);
    }

    #[tokio::test]
    async fn async_rename_target_already_exists_emits_only_batch_completion() {
        use std::fs;
        use std::sync::OnceLock;

        let root = std::env::temp_dir().join(format!(
            "source-downloader-rename-target-exists-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target_dir = root.join("target");
        fs::create_dir_all(&target_dir).unwrap();
        let target_file = target_dir.join("file.txt");
        fs::write(&target_file, b"existing").unwrap();
        let source_item =
            SourceItem { title: "target-exists".to_owned(), ..Default::default() };
        let content = ProcessingContent {
            id: Some(1),
            processor_name: "async-rename-target-exists".to_owned(),
            item_hash: source_item.hashing(),
            item_identity: None,
            item_content: ItemContentLite { source_item, item_variables: HashMap::new() },
            rename_times: 0,
            status: ProcessingStatus::WaitingToRename,
            failure_reason: None,
            created_at: OffsetDateTime::now_utc(),
            updated_at: None,
        };
        let file = FileContent {
            download_path: root.join("download"),
            file_download_path: root.join("download/file.txt"),
            source_save_path: target_dir.clone(),
            pattern_variables: HashMap::new(),
            tags: Vec::new(),
            attrs: Default::default(),
            file_uri: None,
            target_save_path: target_dir,
            target_filename: "file.txt".to_owned(),
            exist_target_path: None,
            errors: Vec::new(),
            status: Normal,
            target_path: OnceLock::new(),
            data: None,
        };
        let (mut processor, storage) = pointer_test_processor(false, 0, false);
        processor.file_mover = Arc::new(ReplacementFileMover);
        processor.name = content.processor_name.clone();
        *storage.query_results.lock() = vec![content];
        storage
            .stored_file_contents
            .lock()
            .insert(1, encode_files_and_compress(&vec![file]).unwrap());
        let each_listener = Arc::new(RecordingListener::default());
        let batch_listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![each_listener.clone()]);
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![batch_listener.clone()]);

        assert_eq!(processor.run_rename().await.unwrap(), 1);

        assert_eq!(each_listener.successes.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(*each_listener.error_messages.lock(), Vec::<String>::new());
        assert_eq!(batch_listener.completions.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(
            *batch_listener.completed_items.lock(),
            vec!["target-exists".to_owned()]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn async_rename_failure_is_visible_to_batch_listener() {
        use std::fs;
        use std::sync::OnceLock;

        let root = std::env::temp_dir()
            .join(format!("source-downloader-rename-failure-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let source_item =
            SourceItem { title: "rename-failure".to_owned(), ..Default::default() };
        let content = ProcessingContent {
            id: Some(3),
            processor_name: "async-rename-failure".to_owned(),
            item_hash: source_item.hashing(),
            item_identity: None,
            item_content: ItemContentLite { source_item, item_variables: HashMap::new() },
            rename_times: 0,
            status: ProcessingStatus::WaitingToRename,
            failure_reason: None,
            created_at: OffsetDateTime::now_utc(),
            updated_at: None,
        };
        let file = FileContent {
            download_path: root.join("download"),
            file_download_path: root.join("download/file.txt"),
            source_save_path: root.join("target"),
            pattern_variables: HashMap::new(),
            tags: Vec::new(),
            attrs: Default::default(),
            file_uri: None,
            target_save_path: root.join("target"),
            target_filename: "file.txt".to_owned(),
            exist_target_path: None,
            errors: Vec::new(),
            status: Normal,
            target_path: OnceLock::new(),
            data: None,
        };
        let (mut processor, storage) = pointer_test_processor(false, 0, false);
        processor.file_mover = Arc::new(FailingFileMover);
        processor.name = content.processor_name.clone();
        *storage.query_results.lock() = vec![content];
        storage
            .stored_file_contents
            .lock()
            .insert(3, encode_files_and_compress(&vec![file]).unwrap());
        let each_listener = Arc::new(RecordingListener::default());
        let batch_listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![each_listener.clone()]);
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![batch_listener.clone()]);

        assert_eq!(processor.run_rename().await.unwrap(), 1);

        assert_eq!(each_listener.successes.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(each_listener.errors.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(batch_listener.completions.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(
            *batch_listener.completed_items.lock(),
            vec!["rename-failure".to_owned()]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn process_context_preserves_completed_item_order() {
        let probe = Arc::new(ParallelismProbe::new(2));
        let (mut processor, _) = pointer_test_processor_with_settings(
            false,
            3,
            false,
            PointerTestSettings {
                parallelism: 2,
                probe: Some(probe.clone()),
                ..Default::default()
            },
        );
        processor.async_downloader = None;
        let listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![listener.clone()]);

        processor.run().await.unwrap();
        assert_ne!(probe.completed.lock().first(), Some(&1));

        assert_eq!(
            *listener.completed_items.lock(),
            vec!["item-1".to_owned(), "item-2".to_owned(), "item-3".to_owned()]
        );
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
        let each_listener = Arc::new(RecordingListener::default());
        let batch_listener = Arc::new(RecordingListener::default());
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Each, vec![each_listener.clone()]);
        processor
            .options
            .process_listeners
            .insert(ListenerMode::Batch, vec![batch_listener.clone()]);

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
        assert_eq!(each_listener.successes.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(each_listener.completions.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(batch_listener.successes.load(AtomicOrdering::Relaxed), 0);
        assert_eq!(batch_listener.completions.load(AtomicOrdering::Relaxed), 1);
        fs::remove_dir_all(root).unwrap();
    }

    /// 模拟替换文件的测试移动器，只验证替换路径，不执行真实移动。
    #[derive(Debug)]
    struct ReplacementFileMover;

    impl Display for ReplacementFileMover {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "replacement-test-mover")
        }
    }

    impl source_downloader_sdk::component::SdComponent for ReplacementFileMover {}
    impl FileMover for ReplacementFileMover {}

    /// 始终返回移动失败的测试替身，只验证移动失败处理。
    #[derive(Debug)]
    struct FailingFileMover;

    impl Display for FailingFileMover {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "failing-file-mover")
        }
    }

    impl source_downloader_sdk::component::SdComponent for FailingFileMover {}

    impl FileMover for FailingFileMover {
        fn move_file(
            &self,
            _: &SourceItem,
            _: &FileContent,
        ) -> Result<(), ProcessingError> {
            Err(ProcessingError::non_retryable("rename test failure"))
        }
    }

    /// 记录是否观察到前置 item 的测试决策器，只验证替换决策输入。
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

    /// 记录最近一次 item 标题的测试决策器，只提供替换断言数据。
    #[derive(Debug, Default)]
    struct PriorTitleDecider(ParkingMutex<Option<String>>);

    impl Display for PriorTitleDecider {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "prior-title")
        }
    }

    impl source_downloader_sdk::component::SdComponent for PriorTitleDecider {}

    impl FileReplacementDecider for PriorTitleDecider {
        fn should_replace(
            &self,
            _: &SourceItem,
            _: &FileContent,
            before: Option<&InProcessingItem>,
            _: &SourceFile,
        ) -> bool {
            *self.0.lock() = before.map(|item| item.source_item.title.clone());
            false
        }
    }

    #[tokio::test]
    async fn replacement_decider_receives_latest_prior_content_regardless_of_query_order()
    {
        use std::sync::OnceLock;

        let prior_hash = "prior-hash".to_owned();
        let prior_content =
            |id: i64, title: &str, created_at: OffsetDateTime| ProcessingContent {
                id: Some(id),
                processor_name: "replacement-history-test".to_owned(),
                item_hash: prior_hash.clone(),
                item_identity: None,
                item_content: ItemContentLite {
                    source_item: SourceItem {
                        title: title.to_owned(),
                        ..Default::default()
                    },
                    item_variables: HashMap::new(),
                },
                rename_times: 1,
                status: ProcessingStatus::Renamed,
                failure_reason: None,
                created_at,
                updated_at: None,
            };
        let older = prior_content(1, "older", OffsetDateTime::UNIX_EPOCH);
        let newer =
            prior_content(2, "newer", OffsetDateTime::from_unix_timestamp(1).unwrap());
        let target_path = PathBuf::from("replacement-history-target.txt");
        let storage = Arc::new(PointerStorage {
            query_results: ParkingMutex::new(vec![older, newer]),
            found_paths: ParkingMutex::new(vec![ProcessingTargetPath {
                path: target_path.to_string_lossy().into_owned(),
                processor_name: "replacement-history-test".to_owned(),
                item_hash: prior_hash,
            }]),
            stored_file_contents: ParkingMutex::new(HashMap::from([
                (1, encode_files_and_compress(&Vec::new()).unwrap()),
                (2, encode_files_and_compress(&Vec::new()).unwrap()),
            ])),
            ..Default::default()
        });
        let mut file = FileContent {
            download_path: PathBuf::new(),
            file_download_path: PathBuf::from("download.txt"),
            source_save_path: PathBuf::new(),
            pattern_variables: HashMap::new(),
            tags: Vec::new(),
            attrs: Default::default(),
            file_uri: None,
            target_save_path: PathBuf::new(),
            target_filename: "replacement-history-target.txt".to_owned(),
            exist_target_path: Some(target_path.clone()),
            errors: Vec::new(),
            status: TargetExists,
            target_path: OnceLock::new(),
            data: None,
        };
        file.target_path.set(target_path).unwrap();
        let (mut processor, _) = pointer_test_processor(false, 0, false);
        processor.processing_storage = storage;
        let decider = Arc::new(PriorTitleDecider::default());
        processor.options.file_replacement_decider = decider.clone();

        NormalProcess {}
            .identify_files_to_replace(
                &processor,
                &SourceItem { title: "current".to_owned(), ..Default::default() },
                &mut [file],
            )
            .await
            .unwrap();

        assert_eq!(decider.0.lock().as_deref(), Some("newer"));
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
