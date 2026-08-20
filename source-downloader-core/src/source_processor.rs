use crate::components::simple_file_exists_detector::SimpleFileExistsDetector;
use crate::components::source_item_identity_filter::SourceItemIdentityFilter;
use crate::config::ListenerMode;
use crate::process::file::{PathPattern, RawFileContent, Renamer, VariableErrorStrategy};
use crate::process::rule::{FileRule, ItemRule, ItemStrategy};
use crate::process::variable::VariableAggregation;
use crate::processor_run_state::{ProcessorItemStage, ProcessorRunItemGuard};
use async_trait::async_trait;
use backon::Retryable;
use backon::{BackoffBuilder, ConstantBuilder};
use futures_util::future::{AbortHandle, Abortable, FutureExt};
use futures_util::stream::{FuturesOrdered, FuturesUnordered, StreamExt};
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
use std::collections::{BTreeMap, HashMap, HashSet};
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
#[cfg(test)]
type SubmittedHeaders = Arc<parking_lot::Mutex<Option<HashMap<String, String>>>>;

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

/// dry-run 对外暴露的错误分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DryRunErrorKind {
    /// 重试耗尽后仍然失败。
    Retryable,
    /// 重试不能解决的错误。
    NonRetryable,
}

/// dry-run 错误的稳定客户端表示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunError {
    /// 可直接展示的错误原因。
    pub message: String,
    /// 错误是否属于可重试分类。
    pub kind: DryRunErrorKind,
    /// 正式处理流程是否允许跳过该错误。
    pub skippable: bool,
}

impl From<&ProcessingError> for DryRunError {
    fn from(error: &ProcessingError) -> Self {
        match error {
            ProcessingError::Retryable { message } => Self {
                message: message.clone(),
                kind: DryRunErrorKind::Retryable,
                skippable: false,
            },
            ProcessingError::NonRetryable { message, skip } => Self {
                message: message.clone(),
                kind: DryRunErrorKind::NonRetryable,
                skippable: *skip,
            },
        }
    }
}

/// item 失败后本次 dry-run 实际采用的调度动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DryRunItemErrorAction {
    /// 继续提交后续 item。
    Continue,
    /// 停止提交后续 item。
    Stop,
}

/// dry-run 终结事件中的结果统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRunSummary {
    /// 成功产出预览结果的 item 数。
    pub succeeded: u32,
    /// 处理失败的 item 数。
    pub failed: u32,
    /// 是否因 item 错误停止提交后续 item。
    pub stopped: bool,
}

/// dry-run 的统一结果事件；收集和流式接口共享相同的事件顺序与终结语义。
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum DryRunEvent {
    /// item 成功完成 dry-run。
    Item {
        /// item 的处理内容和文件预览。
        result: DryRunResult,
    },
    /// item 处理失败。
    ItemError {
        /// 失败 item 的稳定哈希。
        item_hash: String,
        /// 失败 item 的原始内容。
        item: SourceItem,
        /// 结构化错误原因。
        error: DryRunError,
        /// 失败后实际采用的调度动作。
        action: DryRunItemErrorAction,
    },
    /// dry-run 正常结束，包括因 item 错误停止调度的情况。
    Complete {
        /// 本次 dry-run 的结果统计。
        summary: DryRunSummary,
    },
    /// item 调度之外的执行阶段失败；该事件是终结事件。
    RunError {
        /// 结构化错误原因。
        error: DryRunError,
    },
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
    /// 表示处理器当前是否正在执行重命名扫描。
    renaming: AtomicBool,
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

impl Default for ProcessorOptions {
    fn default() -> Self {
        Self {
            save_path_pattern: PathPattern::new_cel(String::new()),
            filename_pattern: PathPattern::new_cel(String::new()),
            variable_providers: Vec::new(),
            item_filters: Vec::new(),
            item_content_filters: Vec::new(),
            source_file_filters: Vec::new(),
            file_content_filters: Vec::new(),
            file_taggers: Vec::new(),
            variable_aggregation: VariableAggregation::new(
                Box::new(crate::process::variable::SmartStrategy),
                HashMap::new(),
            ),
            save_processing_content: false,
            rename_task_interval: Duration::from_secs(300),
            rename_times_threshold: 3,
            parallelism: 1,
            retry_attempts: 3,
            retry_backoff: Duration::from_secs(5),
            task_group: None,
            fetch_limit: 50,
            item_error_continue: false,
            pointer_batch_mode: true,
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
        }
    }
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
    reserved_target_paths: RwLock<ReservedTargetPaths>,
    in_flight_items: RwLock<HashMap<String, InFlightItem>>,
    cancelled_items: RwLock<HashSet<String>>,
}

/// 一个正在处理的 item 及其文件结果；只用于运行内跟踪，不是最终存储模型。
struct InFlightItem {
    content: ProcessingContent,
    files: Vec<FileContent>,
}

#[derive(Default)]
struct ReservedTargetPaths {
    owners_by_path: HashMap<PathBuf, String>,
    paths_by_owner: HashMap<String, Vec<PathBuf>>,
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

struct CompletedItem {
    item_pointer: Arc<dyn ItemPointer>,
    source_item: SourceItem,
    item_hash: String,
    action: ItemAction,
    progress: crate::processor_run_state::ProcessorRunItemGuard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduleDecision {
    Continue,
    Stop,
}

impl ScheduleDecision {
    fn should_stop(self) -> bool {
        matches!(self, Self::Stop)
    }
}

impl From<ScheduleDecision> for DryRunItemErrorAction {
    fn from(decision: ScheduleDecision) -> Self {
        match decision {
            ScheduleDecision::Continue => Self::Continue,
            ScheduleDecision::Stop => Self::Stop,
        }
    }
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
            match reserved.owners_by_path.get(target_path) {
                None => {
                    let target_path = target_path.to_path_buf();
                    reserved
                        .owners_by_path
                        .insert(target_path.clone(), item_hash.to_owned());
                    reserved
                        .paths_by_owner
                        .entry(item_hash.to_owned())
                        .or_default()
                        .push(target_path);
                }
                Some(owner) if owner == item_hash => {}
                Some(_) => {
                    if file.status != TargetExists {
                        file.status = FileConflict;
                        file.exist_target_path = None;
                    }
                    has_conflict = true;
                }
            }
        }
        has_conflict
    }

    fn release_target_paths(&self, item_hash: &str) {
        let mut reserved = self.reserved_target_paths.write();
        let Some(paths) = reserved.paths_by_owner.remove(item_hash) else {
            return;
        };
        for path in paths {
            reserved.owners_by_path.remove(&path);
        }
    }

    fn register_in_flight(
        &self,
        processor: &SourceProcessor,
        item_hash: &str,
        source_item: &SourceItem,
        item_variables: &PatternVariables,
        files: &[FileContent],
    ) {
        self.in_flight_items.write().insert(
            item_hash.to_owned(),
            InFlightItem {
                content: ProcessingContent {
                    id: None,
                    processor_name: processor.name.clone(),
                    item_hash: item_hash.to_owned(),
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

/// 重命名运行状态的 RAII 记录器，保证取消和错误路径均释放 CAS 标志。
struct RenamingGuard<'a> {
    processor: &'a SourceProcessor,
}

impl Drop for RenamingGuard<'_> {
    fn drop(&mut self) {
        self.processor.renaming.store(false, Ordering::Release);
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
    #[allow(clippy::too_many_arguments)]
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
            renaming: AtomicBool::new(false),
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
    pub async fn dry_run(&self, options: DryRunOptions) -> Vec<DryRunEvent> {
        let process = DryRunProcess::collecting(self, options);
        if let Err(error) = process.execute(self).await {
            process.emit_run_error(&error).await;
        }
        process.into_events()
    }

    pub fn dry_run_stream(
        self: &Arc<Self>,
        options: DryRunOptions,
    ) -> impl futures_util::Stream<Item = DryRunEvent> + Send + 'static {
        let capacity = self.options.parallelism.max(1) as usize;
        let (sender, receiver) = mpsc::channel(capacity);
        let process = DryRunProcess::streaming(self, options, sender.clone());
        let processor = self.clone();
        tokio::spawn(async move {
            tokio::select! {
                result = process.execute(&processor) => {
                    if let Err(error) = result {
                        process.emit_run_error(&error).await;
                    }
                }
                _ = sender.closed() => {}
            }
        });
        futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver.recv().await.map(|event| (event, receiver))
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

    pub(crate) fn automatic_rename_interval(&self) -> Option<Duration> {
        self.async_downloader.as_ref().map(|_| self.options.rename_task_interval)
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
        if self
            .renaming
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            warn!("Processor[rename-reject] {} already renaming", self.name);
            return Err(ProcessingError::non_retryable("Already renaming"));
        }
        let _renaming_guard = RenamingGuard { processor: self };
        crate::processor_run_state::set_run_stage(
            crate::processor_run_state::ProcessorRunStage::Initializing,
        );
        if self.closed.load(Ordering::Acquire) {
            return Err(ProcessingError::non_retryable("Processor is closed"));
        }
        let Some(async_downloader) = self.async_downloader.as_ref() else {
            warn!("Processor[rename-skip] {} downloader is synchronous", self.name);
            return Ok(0);
        };
        crate::processor_run_state::set_run_stage(
            crate::processor_run_state::ProcessorRunStage::ScanningItems,
        );
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
        let mut movable = Vec::new();

        for mut content in contents {
            match async_downloader.is_finished(&content.item_content.source_item).await {
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
                Some(true) => movable.push(content),
            }
        }
        crate::processor_run_state::set_total_items(movable.len());
        crate::processor_run_state::set_run_stage(
            crate::processor_run_state::ProcessorRunStage::ProcessingItems,
        );
        let finished = movable.len();
        for mut content in movable {
            let progress = ProcessorRunItemGuard::new(
                &content.item_content.source_item.title,
                ProcessorItemStage::CheckingFiles,
            );
            match self.process_rename_content(&mut content, &progress).await {
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
                    progress.set_stage(ProcessorItemStage::Notifying);
                    if renamed {
                        self.notify_process_listeners(
                            ListenerMode::Each,
                            "item-success",
                            |listener| {
                                listener.on_item_success(&listener_context, &item_content)
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
                    progress.set_stage(ProcessorItemStage::Notifying);
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
        crate::processor_run_state::set_run_stage(
            crate::processor_run_state::ProcessorRunStage::Finalizing,
        );
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
        progress: &ProcessorRunItemGuard,
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
            progress.set_stage(ProcessorItemStage::MovingFiles);
            let movement_result = process
                .do_movement(self, &content.item_content.source_item, &files)
                .await;
            progress.set_stage(ProcessorItemStage::ReplacingFiles);
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

        progress.set_stage(ProcessorItemStage::Persisting);
        self.processing_storage
            .save_processing_content(content)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        if let Some(content_id) = content.id {
            self.processing_storage
                .save_file_contents(
                    content_id,
                    encode_files_and_compress_async(&files).await?,
                )
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
        files: &[FileContent],
    ) -> Result<Option<i64>, ProcessingError>;

    async fn on_item_error(
        &self,
        _p: &SourceProcessor,
        _ctx: &mut ProcessCoordinator,
        _item: &SourceItem,
        _err: &ProcessingError,
    ) {
    }

    async fn on_item_error_settled(
        &self,
        _source_item: &SourceItem,
        _error: &ProcessingError,
        _decision: ScheduleDecision,
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

    fn settle_skipped_item(
        &self,
        source_item: &SourceItem,
        reason: String,
    ) -> ScheduleDecision {
        debug!("[item-skip] {} {:?} ", reason, source_item);
        ScheduleDecision::Continue
    }

    async fn settle_filtered_item(
        &self,
        p: &SourceProcessor,
        coordinator: &mut ProcessCoordinator,
        item_pointer: Arc<dyn ItemPointer>,
        source_item: &SourceItem,
        reason: String,
        advance_pointer: bool,
    ) -> ScheduleDecision {
        debug!("[item-filtered] {} {:?} ", reason, source_item);
        if !advance_pointer {
            return ScheduleDecision::Continue;
        }
        if let Err(err) = self
            .on_item_filtered(p, coordinator, source_item, item_pointer.as_ref())
            .await
        {
            coordinator.listener_context.has_error = true;
            self.on_item_error(p, coordinator, source_item, &err).await;
            let decision = if p.options.item_error_continue {
                self.persist_item_failure(p, source_item, &err, None, None).await;
                warn!("[item-continue-on-error] {} {}", err.message(), source_item);
                ScheduleDecision::Continue
            } else {
                ScheduleDecision::Stop
            };
            self.on_item_error_settled(source_item, &err, decision).await;
            return decision;
        }
        ScheduleDecision::Continue
    }

    async fn settle_error_item(
        &self,
        p: &SourceProcessor,
        item_runtime: &ItemProcessRuntime,
        coordinator: &mut ProcessCoordinator,
        source_item: &SourceItem,
        err: ProcessingError,
    ) -> ScheduleDecision {
        item_runtime.processed_inc();
        coordinator.listener_context.has_error = true;
        self.on_item_error(p, coordinator, source_item, &err).await;
        let skippable = matches!(&err, ProcessingError::NonRetryable { skip: true, .. });
        let decision = if skippable || p.options.item_error_continue {
            self.persist_item_failure(p, source_item, &err, None, None).await;
            warn!("[item-continue-on-error] {} {}", err.message(), source_item);
            ScheduleDecision::Continue
        } else {
            warn!("[item-stop-on-error] {}, 停止提交新 Item", err.message());
            ScheduleDecision::Stop
        };
        self.on_item_error_settled(source_item, &err, decision).await;
        decision
    }

    #[allow(clippy::too_many_arguments)]
    async fn settle_success_item(
        &self,
        p: &SourceProcessor,
        item_runtime: &ItemProcessRuntime,
        coordinator: &mut ProcessCoordinator,
        item_pointer: Arc<dyn ItemPointer>,
        item_hash: String,
        source_item: SourceItem,
        files: Vec<FileContent>,
        item_variables: PatternVariables,
        rename_times: u32,
        mut status: ProcessingStatus,
        failure_reason: Option<String>,
        advance_pointer: bool,
    ) -> ScheduleDecision {
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
                let continued_failure = p.options.item_error_continue.then(|| {
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
                        coordinator,
                        item_pointer.as_ref(),
                        content,
                        files,
                    )
                    .await
                {
                    Ok(()) => ScheduleDecision::Continue,
                    Err(err) => {
                        if let Some((content_id, created_at, source_item)) =
                            continued_failure
                        {
                            self.persist_item_failure(
                                p,
                                &source_item,
                                &err,
                                content_id,
                                Some(created_at),
                            )
                            .await;
                            warn!("[item-continue-on-error] {}", err.message());
                            ScheduleDecision::Continue
                        } else {
                            ScheduleDecision::Stop
                        }
                    }
                }
            }
            Err(err) => {
                item_runtime.processed_inc();
                coordinator.listener_context.has_error = true;
                let source_item = &content.item_content.source_item;
                self.on_item_error(p, coordinator, source_item, &err).await;
                let skippable =
                    matches!(&err, ProcessingError::NonRetryable { skip: true, .. });
                if skippable || p.options.item_error_continue {
                    warn!("[item-continue-on-error] {} {}", err.message(), source_item);
                    ScheduleDecision::Continue
                } else {
                    warn!("[item-stop-on-error] {}, 停止提交新 Item", err.message());
                    ScheduleDecision::Stop
                }
            }
        }
    }

    async fn settle_completed_item(
        &self,
        p: &SourceProcessor,
        item_runtime: &ItemProcessRuntime,
        coordinator: &mut ProcessCoordinator,
        completed: CompletedItem,
        advance_pointer: bool,
    ) -> ScheduleDecision {
        let CompletedItem { item_pointer, source_item, item_hash, action, progress } =
            completed;
        progress.set_stage(ProcessorItemStage::Persisting);
        match action {
            ItemAction::Skip(reason) => self.settle_skipped_item(&source_item, reason),
            ItemAction::Filtered(reason) => {
                self.settle_filtered_item(
                    p,
                    coordinator,
                    item_pointer,
                    &source_item,
                    reason,
                    advance_pointer,
                )
                .await
            }
            ItemAction::Error(err) => {
                self.settle_error_item(p, item_runtime, coordinator, &source_item, err)
                    .await
            }
            ItemAction::Success {
                files,
                item_variables,
                rename_times,
                status,
                failure_reason,
            } => {
                self.settle_success_item(
                    p,
                    item_runtime,
                    coordinator,
                    item_pointer,
                    item_hash,
                    source_item,
                    files,
                    item_variables,
                    rename_times,
                    status,
                    failure_reason,
                    advance_pointer,
                )
                .await
            }
        }
    }

    async fn execute(&self, p: &SourceProcessor) -> Result<(), ProcessingError> {
        crate::processor_run_state::set_run_stage(
            crate::processor_run_state::ProcessorRunStage::Initializing,
        );
        let span_exec = tracing::info_span!("", processor = p.name);
        let start_time = Instant::now();
        let _span_exec_entered = span_exec.enter();
        info!("[run-start] {}({})", p.name, p.instance_id);
        if p.closed.load(Ordering::Acquire) {
            return Err(ProcessingError::non_retryable("Processor is closed"));
        }
        if p.processing.swap(true, Ordering::AcqRel) {
            debug!("[run-reject] {}({}) Already processing", p.name, p.instance_id);
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
            crate::processor_run_state::set_run_stage(
                crate::processor_run_state::ProcessorRunStage::FetchingItems,
            );
            let items =
                self.fetch_items(p, p_rt.coordinator.source_pointer.as_ref()).await?;
            crate::processor_run_state::set_total_items(items.len());
            crate::processor_run_state::set_run_stage(
                crate::processor_run_state::ProcessorRunStage::ProcessingItems,
            );
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
                    let item_hash = source_item.hashing();
                    let progress = crate::processor_run_state::ProcessorRunItemGuard::new(
                        &source_item.title,
                        crate::processor_run_state::ProcessorItemStage::FilteringItem,
                    );
                    let action = process
                        .process_item(
                            &source_item,
                            &item_hash,
                            item_runtime,
                            processor,
                            &progress,
                        )
                        .await
                        .unwrap_or_else(ItemAction::Error);
                    progress.set_stage(crate::processor_run_state::ProcessorItemStage::AwaitingSettlement);
                    CompletedItem {
                        item_pointer,
                        source_item,
                        item_hash,
                        action,
                        progress,
                    }
                };
            let make_sequenced_future = |sequence, item| {
                make_item_future(item).map(move |completed| (sequence, completed))
            };
            let mut remaining_items = items.into_iter();
            let mut stop_scheduling = false;
            if p.options.item_error_continue {
                let mut item_results = FuturesUnordered::new();
                for (sequence, item) in
                    remaining_items.by_ref().enumerate().take(parallelism)
                {
                    item_results.push(make_sequenced_future(sequence, item));
                }
                let mut next_sequence = 0;
                let mut next_submit_sequence = item_results.len();
                let mut completed_by_sequence = BTreeMap::new();
                while let Some((sequence, completed)) = item_results.next().await {
                    completed_by_sequence.insert(sequence, completed);
                    while let Some(completed) =
                        completed_by_sequence.remove(&next_sequence)
                    {
                        if self
                            .settle_completed_item(
                                p,
                                item_runtime,
                                &mut p_rt.coordinator,
                                completed,
                                true,
                            )
                            .await
                            .should_stop()
                        {
                            stop_scheduling = true;
                        }
                        next_sequence += 1;
                    }
                    if !stop_scheduling && let Some(item) = remaining_items.next() {
                        let sequence = next_submit_sequence;
                        next_submit_sequence += 1;
                        item_results.push(make_sequenced_future(sequence, item));
                    }
                }
            } else {
                let mut item_results = FuturesOrdered::new();
                for item in remaining_items.by_ref().take(parallelism) {
                    item_results.push_back(make_item_future(item));
                }
                while let Some(completed) = item_results.next().await {
                    let advance_pointer = !stop_scheduling;
                    if self
                        .settle_completed_item(
                            p,
                            item_runtime,
                            &mut p_rt.coordinator,
                            completed,
                            advance_pointer,
                        )
                        .await
                        .should_stop()
                    {
                        stop_scheduling = true;
                    }
                    if !stop_scheduling && let Some(item) = remaining_items.next() {
                        item_results.push_back(make_item_future(item));
                    }
                }
            }
            for (content, _) in &mut p_rt.coordinator.listener_context.contents {
                if item_runtime.is_cancelled(&content.item_hash) {
                    content.status = ProcessingStatus::Cancelled;
                }
            }
            crate::processor_run_state::set_run_stage(
                crate::processor_run_state::ProcessorRunStage::Finalizing,
            );
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
                reserved_target_paths: RwLock::new(ReservedTargetPaths::default()),
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
        current_hash: &str,
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
        let latest_contents = latest_contents
            .into_values()
            .map(|content| {
                content
                    .id
                    .ok_or_else(|| {
                        ProcessingError::non_retryable(
                            "Persisted replacement content has no id",
                        )
                    })
                    .map(|content_id| (content_id, content))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let content_ids =
            latest_contents.iter().map(|(content_id, _)| *content_id).collect_vec();
        let mut encoded_by_id = p
            .processing_storage
            .find_file_contents_by_ids(&content_ids)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        let mut prior_by_hash = HashMap::with_capacity(latest_contents.len());
        for (content_id, content) in latest_contents {
            let encoded_files = encoded_by_id.remove(&content_id).ok_or_else(|| {
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
        current_hash: &str,
        files: &mut [FileContent],
    ) -> Result<usize, ProcessingError> {
        let mut cancellations: HashMap<String, (SourceItem, Vec<SourceFile>)> =
            HashMap::new();
        let mut replacement_count = 0;
        {
            let reserved_paths = runtime.reserved_target_paths.read();
            let in_flight_items = runtime.in_flight_items.read();
            for file in files.iter_mut() {
                let target_path = file.target_path().clone();
                let Some(owner_hash) = reserved_paths.owners_by_path.get(&target_path)
                else {
                    continue;
                };
                if owner_hash == current_hash {
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
            debug!("[item-cancel-for-replacement] {}", item);
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
        item_hash: &str,
        rt: &ItemProcessRuntime,
        p: &SourceProcessor,
        progress: &ProcessorRunItemGuard,
    ) -> Result<ItemAction, ProcessingError> {
        if !rt.process_submitted_items.write().insert(item_hash.to_owned()) {
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
                self.process_item_attempt(
                    source_item,
                    item_hash,
                    rt,
                    p,
                    item_strategy,
                    progress,
                )
                .await
            },
            "process-item",
            p.options.retry_attempts,
            p.options.retry_backoff,
        )
        .await;
        if result.is_err() {
            rt.release_target_paths(item_hash);
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
        progress: &ProcessorRunItemGuard,
    ) -> Result<ItemAction, ProcessingError> {
        progress.set_stage(ProcessorItemStage::ResolvingVariables);
        let opt = &p.options;
        let mut item_raw_vars = vec![];
        let variable_providers = item_strategy
            .and_then(|x| x.variable_providers.as_ref())
            .unwrap_or(&opt.variable_providers);
        for x in variable_providers {
            item_raw_vars.push((x.accuracy(), x.item_variables(source_item).await))
        }
        let item_variables = opt.variable_aggregation.merge(&item_raw_vars);

        let resolved_files = self.resolve_files(source_item, p).await?;
        progress.set_stage(ProcessorItemStage::ResolvingFiles);
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
        progress.set_stage(ProcessorItemStage::CheckingFiles);
        let (should_download, mut content_status) = {
            let _guard = rt.mutex.lock().await;
            self.update_file_content_status(p, source_item, &mut file_contents).await;
            progress.set_stage(ProcessorItemStage::DecidingReplacements);
            self.identify_files_to_replace(p, source_item, item_hash, &mut file_contents)
                .await?;
            if self.allows_in_flight_cancellation() && p.async_downloader.is_some() {
                self.identify_in_flight_replacements(
                    p,
                    rt,
                    source_item,
                    item_hash,
                    &mut file_contents,
                )
                .await?;
            }
            let has_reserved_target_conflict =
                rt.reserve_target_paths(item_hash, &mut file_contents);
            rt.register_in_flight(
                p,
                item_hash,
                source_item,
                &item_variables,
                &file_contents,
            );
            self.probe_content_status(
                p,
                rt,
                source_item,
                item_hash,
                &file_contents,
                has_reserved_target_conflict,
            )
        };
        progress.set_stage(ProcessorItemStage::SubmittingDownload);
        let mut rename_times = 0;
        if should_download
            && self.do_download(p, source_item, item_hash, &file_contents).await?
        {
            let is_sync = p.async_downloader.is_none();
            if is_sync {
                progress.set_stage(ProcessorItemStage::MovingFiles);
                let movement_res = self.do_movement(p, source_item, &file_contents).await;
                progress.set_stage(ProcessorItemStage::ReplacingFiles);
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
        item_hash: &str,
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
            let paths = downloadable_files
                .into_iter()
                .map(|file| ProcessingTargetPath {
                    path: file.target_path().to_string_lossy().into_owned(),
                    processor_name: p.name.clone(),
                    item_hash: item_hash.to_owned(),
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
        let mut exists_out: Vec<Option<PathBuf>> = target_paths
            .iter()
            .zip(exists_results)
            .map(|(&path, exists)| exists.then(|| path.clone()))
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
        indices.into_iter().zip(exists_out).collect()
    }

    fn probe_content_status(
        &self,
        p: &SourceProcessor,
        rt: &ItemProcessRuntime,
        source_item: &SourceItem,
        item_hash: &str,
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
        if rt.is_cancelled(item_hash) {
            return (false, ProcessingStatus::Cancelled);
        }
        // 预防这一批次的Item有相同的目标，并且是AsyncDownloader的情况下会重复下载
        if files.iter().all(|x| x.status == TargetExists) {
            debug!(
                "Item files already exist: {}, files: {:?}",
                source_item,
                files.iter().map(|file| file.target_path.get()).collect_vec()
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
            .await?
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
                    f.path.to_str().unwrap_or_default()
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

        let item_var = p.renamer.item_rename_variables(source_item, item_variables).await;

        let empty_vars = &PatternVariables::new();
        let file_count = relative_files.len();
        for (idx, x) in relative_files.into_iter().enumerate() {
            let var = file_vars.get(idx).unwrap_or(empty_vars);
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
            let content =
                p.renamer.create_file_content(source_item, raw, &item_var).await?;

            // <editor-fold desc="Stage using FileContentFilter">
            let file_content_filters = file_strategy
                .and_then(|s| s.file_content_filters.as_ref())
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
        files: &[FileContent],
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
            .save_file_contents(content_id, encode_files_and_compress_async(files).await?)
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

async fn encode_files_and_compress_async(
    files: &[FileContent],
) -> Result<Vec<u8>, ProcessingError> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let bytes = serialize_files(files)?;
    tokio::task::spawn_blocking(move || compress_files(bytes)).await.map_err(|error| {
        ProcessingError::non_retryable(format!(
            "File content compression task failed: {error}"
        ))
    })?
}

pub fn encode_files_and_compress(
    files: &[FileContent],
) -> Result<Vec<u8>, ProcessingError> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    compress_files(serialize_files(files)?)
}

fn serialize_files(files: &[FileContent]) -> Result<Vec<u8>, ProcessingError> {
    serde_json::to_vec(files).map_err(|error| {
        ProcessingError::non_retryable(format!(
            "Failed to serialize file content: {error}"
        ))
    })
}

fn compress_files(bytes: Vec<u8>) -> Result<Vec<u8>, ProcessingError> {
    zstd::encode_all(Cursor::new(bytes), 6).map_err(|error| {
        ProcessingError::non_retryable(format!(
            "Failed to compress file content: {error}"
        ))
    })
}

#[allow(dead_code)]
pub fn decode_files_from_compressed(
    bytes: &[u8],
) -> Result<Vec<FileContent>, ProcessingError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }

    let decompressed = zstd::decode_all(bytes).map_err(|error| {
        ProcessingError::non_retryable(format!(
            "Failed to decompress file content: {error}"
        ))
    })?;
    serde_json::from_slice(&decompressed).map_err(|error| {
        ProcessingError::non_retryable(format!(
            "Failed to deserialize file content: {error}"
        ))
    })
}

enum DryRunOutput {
    Collected(SyncMutex<Vec<DryRunEvent>>),
    Streamed(mpsc::Sender<DryRunEvent>),
}

/// dry-run 的 `Process` 实现，只收集或流式输出预览结果，不保存内容或下载文件。
struct DryRunProcess {
    source_pointer: Option<Value>,
    item_filters: Vec<Arc<dyn SourceItemFilter>>,
    output: DryRunOutput,
    succeeded: AtomicU32,
    failed: AtomicU32,
    stopped: AtomicBool,
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
            succeeded: AtomicU32::new(0),
            failed: AtomicU32::new(0),
            stopped: AtomicBool::new(false),
        }
    }

    fn streaming(
        processor: &SourceProcessor,
        options: DryRunOptions,
        sender: mpsc::Sender<DryRunEvent>,
    ) -> Self {
        let mut process = Self::collecting(processor, options);
        process.output = DryRunOutput::Streamed(sender);
        process
    }

    async fn emit(&self, event: DryRunEvent) {
        match &self.output {
            DryRunOutput::Collected(events) => events.lock().push(event),
            DryRunOutput::Streamed(sender) => {
                let _ = sender.send(event).await;
            }
        }
    }

    fn summary(&self) -> DryRunSummary {
        DryRunSummary {
            succeeded: self.succeeded.load(Ordering::Acquire),
            failed: self.failed.load(Ordering::Acquire),
            stopped: self.stopped.load(Ordering::Acquire),
        }
    }

    async fn emit_run_error(&self, error: &ProcessingError) {
        self.emit(DryRunEvent::RunError { error: error.into() }).await;
    }

    fn into_events(self) -> Vec<DryRunEvent> {
        match self.output {
            DryRunOutput::Collected(events) => events.into_inner(),
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
        self.emit(DryRunEvent::Complete { summary: self.summary() }).await;
        Ok(())
    }

    async fn on_item_error_settled(
        &self,
        source_item: &SourceItem,
        error: &ProcessingError,
        decision: ScheduleDecision,
    ) {
        self.failed.fetch_add(1, Ordering::AcqRel);
        self.stopped.fetch_or(decision.should_stop(), Ordering::AcqRel);
        self.emit(DryRunEvent::ItemError {
            item_hash: source_item.hashing(),
            item: source_item.clone(),
            error: error.into(),
            action: decision.into(),
        })
        .await;
    }

    async fn on_item_process_complete(
        &self,
        _: &SourceProcessor,
        _: &ProcessingContent,
        _: &[FileContent],
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
        self.succeeded.fetch_add(1, Ordering::AcqRel);
        self.emit(DryRunEvent::Item {
            result: DryRunResult { processing_content, file_contents },
        })
        .await;
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
        _: &str,
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
        files: &[FileContent],
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
            .save_file_contents(content_id, encode_files_and_compress_async(files).await?)
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
        files: &[FileContent],
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
            .save_file_contents(content_id, encode_files_and_compress_async(files).await?)
            .await
            .map_err(|error| ProcessingError::non_retryable(error.message))?;
        Ok(Some(content_id))
    }
}

#[cfg(test)]
#[path = "source_processor/tests.rs"]
mod test;
