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
    submitted_headers: Option<SubmittedHeaders>,
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
    ) -> Result<Arc<dyn AsyncDownloader>, source_downloader_sdk::component::ComponentError>
    {
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
    async fn resolve_files(
        &self,
        item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError> {
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
            return Ok(vec![SourceFile::new(path.clone()), SourceFile::new(path)]);
        }
        if let Some(path) = &self.resolved_file {
            return Ok(vec![SourceFile {
                tags: self.resolved_file_tags.clone(),
                ..SourceFile::new(path.clone())
            }]);
        }
        if self.unique_files {
            return Ok(vec![SourceFile::new(PathBuf::from(format!("{sequence}.txt")))]);
        }
        Ok(Vec::new())
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

#[async_trait]
impl AsyncDownloader for PointerTestComponent {
    async fn is_finished(&self, _: &SourceItem) -> Option<bool> {
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

    async fn extract_from(
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

    async fn extract_from(
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
            item_content.file_contents.iter().map(|file| file.target_filename.clone()),
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

    async fn save_paths(&self, _: Vec<ProcessingTargetPath>) -> Result<(), StorageError> {
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
    submitted_headers: Option<SubmittedHeaders>,
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
        .run_items(vec![SourceItem { title: "item-1".to_owned(), ..Default::default() }])
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
fn dry_run_results(events: Vec<DryRunEvent>) -> Vec<DryRunResult> {
    events
        .into_iter()
        .filter_map(|event| match event {
            DryRunEvent::Item { result } => Some(result),
            _ => None,
        })
        .collect()
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

    let results = dry_run_results(processor.dry_run(DryRunOptions::default()).await);

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

    let results = dry_run_results(processor.dry_run(DryRunOptions::default()).await);
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

    processor.dry_run(DryRunOptions::default()).await;

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

    let results = dry_run_results(processor.dry_run(DryRunOptions::default()).await);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].processing_content.status, ProcessingStatus::WaitingToRename);
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
async fn dry_run_reports_item_error_and_stop_action() {
    let (processor, _) = pointer_test_processor_with_settings(
        false,
        3,
        false,
        PointerTestSettings { invalid_item: Some(2), ..Default::default() },
    );

    let events = processor.dry_run(DryRunOptions::default()).await;

    assert!(matches!(&events[0], DryRunEvent::Item { .. }));
    assert!(matches!(
        &events[1],
        DryRunEvent::ItemError {
            item,
            error: DryRunError {
                kind: DryRunErrorKind::NonRetryable,
                skippable: false,
                ..
            },
            action: DryRunItemErrorAction::Stop,
            ..
        } if item.title == "item-2"
    ));
    assert!(matches!(
        &events[2],
        DryRunEvent::Complete {
            summary: DryRunSummary { succeeded: 1, failed: 1, stopped: true },
        }
    ));
}

#[tokio::test]
async fn dry_run_reports_item_error_and_continue_action() {
    let (processor, _) = pointer_test_processor_with_settings(
        false,
        3,
        false,
        PointerTestSettings {
            item_error_continue: true,
            invalid_item: Some(2),
            ..Default::default()
        },
    );

    let events = processor.dry_run(DryRunOptions::default()).await;

    assert!(matches!(
        events.as_slice(),
        [
            DryRunEvent::Item { .. },
            DryRunEvent::ItemError {
                item,
                action: DryRunItemErrorAction::Continue,
                ..
            },
            DryRunEvent::Item { .. },
            DryRunEvent::Complete {
                summary: DryRunSummary {
                    succeeded: 2,
                    failed: 1,
                    stopped: false,
                },
            },
        ] if item.title == "item-2"
    ));
}

#[tokio::test]
async fn dry_run_reports_run_error_before_item_processing() {
    let (processor, _) = pointer_test_processor(false, 1, false);
    processor.close();

    let events = processor.dry_run(DryRunOptions::default()).await;

    assert!(matches!(
        events.as_slice(),
        [DryRunEvent::RunError {
            error: DryRunError {
                message,
                kind: DryRunErrorKind::NonRetryable,
                skippable: false,
            },
        }] if message == "Processor is closed"
    ));
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

    let events =
        processor.dry_run_stream(DryRunOptions::default()).collect::<Vec<_>>().await;

    assert!(matches!(
        events.as_slice(),
        [
            DryRunEvent::Item { result: first },
            DryRunEvent::Item { result: second },
            DryRunEvent::Complete {
                summary: DryRunSummary {
                    succeeded: 2,
                    failed: 0,
                    stopped: false,
                },
            },
        ] if first.processing_content.item_content.source_item.title == "item-1"
            && second.processing_content.item_content.source_item.title == "item-2"
    ));
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

    let unfiltered = dry_run_results(processor.dry_run(DryRunOptions::default()).await);
    let filtered = dry_run_results(
        processor
            .dry_run(DryRunOptions { filter_processed: true, ..Default::default() })
            .await,
    );

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
    assert!(failed.failure_reason.as_deref().is_some_and(|reason| !reason.is_empty()));
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

// <editor-fold desc="Data-driven processor cases">
#[tokio::test]
#[tracing_test::traced_test]
async fn processor_cases() {
    let cfg = cfg();
    let pm = processor_manager().await;
    let storage = storage().await;
    for (name, case) in CASES.iter() {
        pm.create_processor(
            &cfg.get_processor_config(name).expect("Failed to get processor config"),
        );
        let processor = assert_processor(name, pm);
        let root_path = V_PATH.join(format!("/{name}")).expect("Failed to join path");
        apply_case_files(&root_path, &case.files);

        let result = processor.run().await;
        match &case.expected_run_error {
            Some(expected) => assert_eq!(result.unwrap_err(), *expected, "case: {name}"),
            None => result.unwrap_or_else(|error| panic!("case {name}: {error}")),
        }

        let content = build_result_json(storage, name).await;
        for (assert_idx, assertion) in case.assertions.iter().enumerate() {
            let selection = content.query(&assertion.select).unwrap_or_default();
            if let Some(expected) = assertion.exact_count
                && selection.len() != expected
            {
                panic!(
                    "{}",
                    AssertionError::new(format!(
                        "selection count failed, expected {expected}, got {}",
                        selection.len()
                    ))
                    .with_context(format!("case: {name}"))
                    .with_context(format!("assertion #{assert_idx}"))
                    .with_context(format!("select: {}", assertion.select))
                );
            }
            if assertion.exact_count.is_none()
                && !assertion.allow_empty
                && selection.is_empty()
            {
                panic!(
                    "{}",
                    AssertionError::new("Selection result is empty")
                        .with_context(format!("case: {name}"))
                        .with_context(format!("assertion #{assert_idx}"))
                        .with_context(format!("select: {}", assertion.select))
                );
            }
            for (node_idx, node) in selection.iter().enumerate() {
                if let Err(err) = apply_assertion(node, &assertion.asserts) {
                    let err = err
                        .with_context(format!("case: {name}"))
                        .with_context(format!("assertion #{assert_idx}"))
                        .with_context(format!("select: {}", assertion.select))
                        .with_context(format!("node index: {node_idx}"))
                        .with_context(format!("content #{node}"));
                    panic!("{err}");
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

#[async_trait]
impl AsyncDownloader for NeverFinishedDownloader {
    async fn is_finished(&self, _: &SourceItem) -> Option<bool> {
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

#[async_trait]
impl AsyncDownloader for MissingDownloadStateDownloader {
    async fn is_finished(&self, _: &SourceItem) -> Option<bool> {
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
    assert_eq!(information.options.variable_error_strategy, VariableErrorStrategy::Stay);
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
            item_content: ItemContentLite { source_item, item_variables: HashMap::new() },
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
        file_save_path_pattern: String::new(),
        filename_pattern: String::new(),
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
        processed_variables: None,
    };
    let (mut processor, storage) = pointer_test_processor(false, 0, false);
    processor.name = content.processor_name.clone();
    processor.async_downloader = Some(Arc::new(MissingDownloadStateDownloader));
    *storage.query_results.lock() = vec![content];
    storage
        .stored_file_contents
        .lock()
        .insert(2, encode_files_and_compress(&[file]).unwrap());
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

    let root = std::env::temp_dir()
        .join(format!("source-downloader-rename-target-exists-{}", std::process::id()));
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
        file_save_path_pattern: String::new(),
        filename_pattern: String::new(),
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
        processed_variables: None,
    };
    let (mut processor, storage) = pointer_test_processor(false, 0, false);
    processor.file_mover = Arc::new(ReplacementFileMover);
    processor.name = content.processor_name.clone();
    *storage.query_results.lock() = vec![content];
    storage
        .stored_file_contents
        .lock()
        .insert(1, encode_files_and_compress(&[file]).unwrap());
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
    assert_eq!(*batch_listener.completed_items.lock(), vec!["target-exists".to_owned()]);
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
        file_save_path_pattern: String::new(),
        filename_pattern: String::new(),
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
        processed_variables: None,
    };
    let (mut processor, storage) = pointer_test_processor(false, 0, false);
    processor.file_mover = Arc::new(FailingFileMover);
    processor.name = content.processor_name.clone();
    *storage.query_results.lock() = vec![content];
    storage
        .stored_file_contents
        .lock()
        .insert(3, encode_files_and_compress(&[file]).unwrap());
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
    assert_eq!(*batch_listener.completed_items.lock(), vec!["rename-failure".to_owned()]);
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
        file_save_path_pattern: String::new(),
        filename_pattern: String::new(),
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
        processed_variables: None,
    };
    let replacement_file = FileContent {
        download_path: download_dir,
        file_download_path: replacement_download_file.clone(),
        source_save_path: target_dir.clone(),
        pattern_variables: HashMap::new(),
        file_save_path_pattern: String::new(),
        filename_pattern: String::new(),
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
        processed_variables: None,
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
            encode_files_and_compress(&[file, replacement_file]).unwrap(),
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
    fn move_file(&self, _: &SourceItem, _: &FileContent) -> Result<(), ProcessingError> {
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
async fn replacement_decider_receives_latest_prior_content_regardless_of_query_order() {
    use std::sync::OnceLock;

    let prior_hash = "prior-hash".to_owned();
    let prior_content =
        |id: i64, title: &str, created_at: OffsetDateTime| ProcessingContent {
            id: Some(id),
            processor_name: "replacement-history-test".to_owned(),
            item_hash: prior_hash.clone(),
            item_identity: None,
            item_content: ItemContentLite {
                source_item: SourceItem { title: title.to_owned(), ..Default::default() },
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
    let file = FileContent {
        download_path: PathBuf::new(),
        file_download_path: PathBuf::from("download.txt"),
        source_save_path: PathBuf::new(),
        pattern_variables: HashMap::new(),
        file_save_path_pattern: String::new(),
        filename_pattern: String::new(),
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
        processed_variables: None,
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
            "current-hash",
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
    let previous_id = storage.save_processing_content(&previous_content).await.unwrap();
    previous_content.id = Some(previous_id);
    storage
        .save_file_contents(previous_id, encode_files_and_compress(&Vec::new()).unwrap())
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

    let file = FileContent {
        download_path: root.clone(),
        file_download_path: download_file.clone(),
        source_save_path: root.clone(),
        pattern_variables: HashMap::new(),
        file_save_path_pattern: String::new(),
        filename_pattern: String::new(),
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
        processed_variables: None,
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

    let current_hash = current_item.hashing();
    assert_eq!(
        process
            .identify_files_to_replace(
                &processor,
                &current_item,
                &current_hash,
                &mut files,
            )
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
