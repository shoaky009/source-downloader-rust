#![allow(dead_code)]

use crate::SourceItem;
use crate::serde_json::{Map, Value};
use crate::storage::ProcessingStatus;
use async_trait::async_trait;
use http::Uri;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

pub const COMPONENT_REF_PAT: &str = ":";
pub type PatternVariables = HashMap<String, String>;

#[derive(Debug, PartialEq, Eq, Hash, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ComponentRootType {
    Trigger,
    Source,
    Downloader,
    ItemFileResolver,
    FileMover,
    VariableProvider,
    ProcessListener,
    SourceItemFilter,
    SourceFileFilter,
    ItemContentFilter,
    FileContentFilter,
    FileTagger,
    FileReplacementDecider,
    FileExistsDetector,
    VariableReplacer,
    Trimmer,
}

impl ComponentRootType {
    pub fn parse(str: &str) -> Result<Self, ComponentError> {
        match str {
            "trigger" => Ok(ComponentRootType::Trigger),
            "source" => Ok(ComponentRootType::Source),
            "downloader" => Ok(ComponentRootType::Downloader),
            "item-file-resolver" => Ok(ComponentRootType::ItemFileResolver),
            "file-mover" => Ok(ComponentRootType::FileMover),
            "variable-provider" => Ok(ComponentRootType::VariableProvider),
            "process-listener" => Ok(ComponentRootType::ProcessListener),
            "source-item-filter" => Ok(ComponentRootType::SourceItemFilter),
            "source-file-filter" => Ok(ComponentRootType::SourceFileFilter),
            "item-content-filter" => Ok(ComponentRootType::ItemContentFilter),
            "file-content-filter" => Ok(ComponentRootType::FileContentFilter),
            "file-tagger" => Ok(ComponentRootType::FileTagger),
            "file-replacement-decider" => Ok(ComponentRootType::FileReplacementDecider),
            "file-exists-detector" => Ok(ComponentRootType::FileExistsDetector),
            "variable-replacer" => Ok(ComponentRootType::VariableReplacer),
            "trimmer" => Ok(ComponentRootType::Trimmer),
            _ => Err(ComponentError::from("Invalid component root type")),
        }
    }
    pub fn name(&self) -> &str {
        match self {
            ComponentRootType::Trigger => "trigger",
            ComponentRootType::Source => "source",
            ComponentRootType::Downloader => "downloader",
            ComponentRootType::ItemFileResolver => "item-file-resolver",
            ComponentRootType::FileMover => "file-mover",
            ComponentRootType::VariableProvider => "variable-provider",
            ComponentRootType::ProcessListener => "process-listener",
            ComponentRootType::SourceItemFilter => "source-item-filter",
            ComponentRootType::SourceFileFilter => "source-file-filter",
            ComponentRootType::ItemContentFilter => "item-content-filter",
            ComponentRootType::FileContentFilter => "file-content-filter",
            ComponentRootType::FileTagger => "tagger",
            ComponentRootType::FileReplacementDecider => "file-replacement-decider",
            ComponentRootType::FileExistsDetector => "file-exists-detector",
            ComponentRootType::VariableReplacer => "variable-replacer",
            ComponentRootType::Trimmer => "trimmer",
        }
    }

    pub fn parse_component_id(&self, str: &str) -> ComponentId {
        let component_ref_pat = ":";
        let source_id = str.split(component_ref_pat).collect::<Vec<&str>>();
        let type_name = source_id.first().unwrap().to_string();
        let name = source_id.last().unwrap();
        ComponentId::new(
            ComponentType {
                root_type: self.to_owned(),
                name: type_name.to_owned(),
            },
            name.to_owned(),
        )
    }
}

impl Display for ComponentRootType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct ComponentType {
    pub root_type: ComponentRootType,
    pub name: String,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct ComponentId {
    pub component_type: ComponentType,
    pub name: String,
}

impl ComponentId {
    pub fn new(component_type: ComponentType, name: &str) -> Self {
        ComponentId {
            component_type,
            name: name.to_string(),
        }
    }

    /// Legal format are `root_type:type_name:name` `root_type:type_name`
    pub fn parse(str: &str) -> Result<Self, ComponentError> {
        let split = str.split(COMPONENT_REF_PAT).collect::<Vec<&str>>();
        if split.len() > 3 || split.len() < 2 {
            return Err(ComponentError::from(
                "Invalid component id, should be in format of root_type:type_name:name or root_type:type_name",
            ));
        }
        let root_type_str = split.first().unwrap();
        let root_type = ComponentRootType::parse(root_type_str)?;
        Ok(ComponentId {
            component_type: ComponentType {
                root_type,
                name: split[1].to_string(),
            },
            name: split.last().unwrap().to_string(),
        })
    }

    pub fn display(&self) -> String {
        format!(
            "{}{}{}{}{}",
            self.component_type.root_type.name(),
            COMPONENT_REF_PAT,
            self.component_type.name,
            COMPONENT_REF_PAT,
            self.name
        )
    }
}

impl ComponentType {
    /// name不能包含:目前没做校验
    pub fn trigger(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::Trigger,
            name,
        }
    }
    pub fn source(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::Source,
            name,
        }
    }
    pub fn downloader(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::Downloader,
            name,
        }
    }
    pub fn file_mover(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::FileMover,
            name,
        }
    }
    pub fn variable_provider(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::VariableProvider,
            name,
        }
    }
    pub fn file_resolver(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::ItemFileResolver,
            name,
        }
    }
    pub fn item_filter(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::SourceItemFilter,
            name,
        }
    }
    pub fn item_content_filter(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::ItemContentFilter,
            name,
        }
    }
    pub fn listener(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::ProcessListener,
            name,
        }
    }
    pub fn source_file_filter(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::SourceFileFilter,
            name,
        }
    }
    pub fn file_content_filter(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::FileContentFilter,
            name,
        }
    }
    pub fn file_tagger(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::FileTagger,
            name,
        }
    }
    pub fn file_replacement_decider(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::FileReplacementDecider,
            name,
        }
    }
    pub fn item_exists_detector(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::FileExistsDetector,
            name,
        }
    }
    pub fn variable_replacer(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::VariableReplacer,
            name,
        }
    }
    pub fn trimmer(name: String) -> ComponentType {
        ComponentType {
            root_type: ComponentRootType::Trimmer,
            name,
        }
    }
}

impl Display for ComponentType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.root_type.name(), self.name)
    }
}

pub trait ComponentSupplier: Send + Sync {
    /// 组件的创建类型
    fn supply_types(&self) -> Vec<ComponentType>;

    /// 创建组件实例
    fn apply(&self, props: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError>;

    /// 如果是true即便没有在配置中定义也会调用[`ComponentSupplier::apply`]
    fn is_support_no_props(&self) -> bool {
        false
    }

    /// 声明组件的属性结构元信息提供给ui渲染表单
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>>;
}

pub struct SdComponentMetadata {
    description: String,
    props_json_schema: Option<Value>,
    props_ui_schema: Option<Value>,
    state_json_schema: Option<Value>,
    state_ui_schema: Option<Value>,
}

pub trait SdComponent: Any + Send + Sync + Debug + Display {
    fn as_trigger(self: Arc<Self>) -> Result<Arc<dyn Trigger>, ComponentError> {
        Err(ComponentError::from("Not a trigger component"))
    }
    fn as_source(self: Arc<Self>) -> Result<Arc<dyn Source>, ComponentError> {
        Err(ComponentError::from("Not a source component"))
    }
    fn as_item_file_resolver(self: Arc<Self>) -> Result<Arc<dyn ItemFileResolver>, ComponentError> {
        Err(ComponentError::from("Not a item file resolver component"))
    }
    fn as_downloader(self: Arc<Self>) -> Result<Arc<dyn Downloader>, ComponentError> {
        Err(ComponentError::from("Not a downloader component"))
    }
    fn as_file_mover(self: Arc<Self>) -> Result<Arc<dyn FileMover>, ComponentError> {
        Err(ComponentError::from("Not a file mover component"))
    }
    fn as_process_listener(self: Arc<Self>) -> Result<Arc<dyn ProcessListener>, ComponentError> {
        Err(ComponentError::from("Not a process listener component"))
    }
    fn as_source_item_filter(self: Arc<Self>) -> Result<Arc<dyn SourceItemFilter>, ComponentError> {
        Err(ComponentError::from("Not a source item filter component"))
    }
    fn as_source_file_filter(self: Arc<Self>) -> Result<Arc<dyn SourceFileFilter>, ComponentError> {
        Err(ComponentError::from("Not a source file filter component"))
    }
    fn as_item_content_filter(
        self: Arc<Self>,
    ) -> Result<Arc<dyn ItemContentFilter>, ComponentError> {
        Err(ComponentError::from("Not a item content filter component"))
    }
    fn as_file_content_filter(
        self: Arc<Self>,
    ) -> Result<Arc<dyn FileContentFilter>, ComponentError> {
        Err(ComponentError::from("Not a file content filter component"))
    }
    fn as_file_tagger(self: Arc<Self>) -> Result<Arc<dyn FileTagger>, ComponentError> {
        Err(ComponentError::from("Not a file tagger component"))
    }
    fn as_file_replacement_decider(
        self: Arc<Self>,
    ) -> Result<Arc<dyn FileReplacementDecider>, ComponentError> {
        Err(ComponentError::from(
            "Not a file replacement decider component",
        ))
    }
    fn as_item_exists_detector(
        self: Arc<Self>,
    ) -> Result<Arc<dyn ItemExistsDetector>, ComponentError> {
        Err(ComponentError::from("Not a item exists detector component"))
    }
    fn as_variable_provider(self: Arc<Self>) -> Result<Arc<dyn VariableProvider>, ComponentError> {
        Err(ComponentError::from("Not a variable provider component"))
    }
    fn as_variable_replacer(self: Arc<Self>) -> Result<Arc<dyn VariableReplacer>, ComponentError> {
        Err(ComponentError::from("Not a variable replacer component"))
    }
    fn as_trimmer(self: Arc<Self>) -> Result<Arc<dyn Trimmer>, ComponentError> {
        Err(ComponentError::from("Not a trimmer component"))
    }
    fn as_async_downloader(self: Arc<Self>) -> Result<Arc<dyn AsyncDownloader>, ComponentError> {
        Err(ComponentError::from("Not a async downloader component"))
    }
    fn as_stateful(self: Arc<Self>) -> Option<Arc<dyn Stateful>> {
        None
    }
}

pub trait Stateful: SdComponent {
    fn get_state_detail(&self) -> Option<Map<String, Value>> {
        None
    }
}

// <editor-fold desc="Component Trait">
pub trait Trigger: SdComponent {
    fn start(&self);
    fn stop(&self);
    fn restart(&self) {
        self.stop();
        self.start();
    }
    fn add_task(&self, task: Arc<dyn ProcessTask>);
    fn remove_task(&self, task: Arc<dyn ProcessTask>);
}

pub trait Downloader: SdComponent {
    fn submit(&self, task: &DownloadTask) -> Result<(), ComponentError>;
    fn default_download_path(&self) -> &str;
    fn cancel(&self, item: &DownloadTask, files: &[SourceFile]) -> Result<(), ComponentError>;
}

pub trait AsyncDownloader: Downloader {
    fn is_finished(&self, item: &SourceItem) -> Option<bool>;
}

#[async_trait]
pub trait Source: SdComponent {
    async fn fetch(
        &self,
        source_pointer: Arc<dyn SourcePointer>,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError>;
    fn default_pointer(&self) -> Arc<dyn SourcePointer>;
    fn parse_raw_pointer(&self, value: Value) -> Arc<dyn SourcePointer>;
    fn headers(&self, _: &SourceItem) -> Option<HashMap<String, String>> {
        None
    }
    fn group(&self) -> Option<String> {
        None
    }
}

#[async_trait]
pub trait ItemFileResolver: SdComponent {
    async fn resolve_files(&self, item: &SourceItem) -> Vec<SourceFile>;
}

pub trait FileMover: SdComponent {
    fn move_file(
        &self,
        source_file: &SourceFile,
        download_path: &str,
    ) -> Result<(), ProcessingError>;
    fn exists(&self, path: Vec<&str>) -> Vec<bool>;
    fn create_directories(&self, path: &str) -> Result<(), ProcessingError>;
    fn replace(&self, item_content: &ItemContent) -> Result<(), ProcessingError>;
    fn list_files(&self, path: &str) -> Vec<String>;
    fn path_metadata(&self, path: &str) -> SourceFile;
    fn is_supported_batch_move(&self) -> bool {
        false
    }
    fn batch_move(&self, _: &ItemContent) -> Result<(), ProcessingError> {
        Err(ProcessingError::non_retryable(
            "Batch move is not supported",
        ))
    }
}

pub trait ProcessListener: SdComponent {
    /// When item rename is successful
    fn on_item_success(&self, ctx: &dyn ProcessContext, item_content: &ItemContent);
    /// When item processing is failed
    fn on_item_error(&self, ctx: &dyn ProcessContext, item: &SourceItem, error: &ProcessingError);
    /// When processing is completed
    fn on_process_completed(&self, ctx: &dyn ProcessContext);
}

#[async_trait::async_trait]
pub trait SourceItemFilter: SdComponent {
    async fn filter(&self, item: &SourceItem) -> bool;
}

pub trait SourceFileFilter: SdComponent {
    fn filter(&self, file: &SourceFile) -> bool;
}

#[async_trait::async_trait]
pub trait ItemContentFilter: SdComponent {
    async fn filter(&self, item_content: &ItemContent) -> bool;
}

pub trait FileContentFilter: SdComponent {
    fn filter(&self, file_content: &FileContent) -> bool;
}

#[async_trait]
pub trait FileTagger: SdComponent {
    async fn tag(&self, source_file: &SourceFile) -> Option<String>;
}

pub trait FileReplacementDecider: SdComponent {
    fn should_replace(
        &self,
        current: &ItemContent,
        before: Option<&ItemContent>,
        existing_file: &SourceFile,
    ) -> bool;
}

pub trait ItemExistsDetector: SdComponent {
    fn exists(&self, file_mover: &dyn FileMover, item_content: &ItemContent) -> bool;
}

#[async_trait]
pub trait VariableProvider: SdComponent {
    fn accuracy(&self) -> i32 {
        1
    }
    async fn item_variables(&self, item: &SourceItem) -> HashMap<String, String>;
    async fn file_variables(
        &self,
        item: &SourceItem,
        item_variables: &PatternVariables,
        files: &[SourceFile],
    ) -> Vec<PatternVariables>;
    async fn extract_from(&self, item: &SourceItem, value: &str) -> Option<HashMap<String, Value>>;
    fn primary_variable_name(&self) -> Option<String>;
}

pub trait VariableReplacer: SdComponent {
    fn replace(&self, key: &str, value: String) -> String;
}

pub trait Trimmer: SdComponent {
    fn trim(&self, value: String, expect_size: usize) -> String;
}

// </editor-fold>

pub trait ItemPointer: Debug + Send + Sync + Any {
    fn as_any(&self) -> &dyn Any;
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EmptyPointer;

impl ItemPointer for EmptyPointer {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub static EMPTY_POINTER: LazyLock<Arc<EmptyPointer>> = LazyLock::new(|| Arc::new(EmptyPointer {}));

pub trait SourcePointer: Send + Sync {
    fn dump(&self) -> Value;
    fn update(&self, item: &SourceItem, item_pointer: &Arc<dyn ItemPointer>);
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
}

impl SourcePointer for EmptyPointer {
    fn dump(&self) -> Value {
        Value::Object(Map::new())
    }

    fn update(&self, _: &SourceItem, _: &Arc<dyn ItemPointer>) {}

    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }
}

#[derive(Debug, Clone)]
pub struct PointedItem {
    pub source_item: SourceItem,
    pub item_pointer: Arc<dyn ItemPointer>,
}

#[derive(Clone)]
pub struct ComponentError {
    pub message: String,
}

impl ComponentError {
    pub fn new<S: Into<String>>(message: S) -> Self {
        ComponentError {
            message: message.into(),
        }
    }
}

impl Display for ComponentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl Debug for ComponentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "ComponentError: {}", self.message)
    }
}

impl Error for ComponentError {}

impl From<&str> for ComponentError {
    fn from(s: &str) -> Self {
        ComponentError::new(s)
    }
}

impl From<String> for ComponentError {
    fn from(s: String) -> Self {
        ComponentError::new(s)
    }
}

#[derive(Debug, Clone)]
pub enum ProcessingError {
    Retryable { message: String },
    NonRetryable { message: String, skip: bool },
}

impl ProcessingError {
    pub fn retryable<S: Into<String>>(message: S) -> Self {
        Self::Retryable {
            message: message.into(),
        }
    }

    pub fn non_retryable<S: Into<String>>(message: S) -> Self {
        Self::NonRetryable {
            message: message.into(),
            skip: false,
        }
    }

    pub fn skip<S: Into<String>>(message: S) -> Self {
        Self::NonRetryable {
            message: message.into(),
            skip: true,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            ProcessingError::Retryable { message } => message,
            ProcessingError::NonRetryable { message, .. } => message,
        }
    }
}

impl Error for ProcessingError {}

impl Display for ProcessingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

pub struct DownloadTask<'a> {
    pub item: &'a SourceItem,
    pub download_files: &'a Vec<SourceFile>,
    pub download_path: &'a String,
    pub download_options: &'a DownloadOptions,
}

#[derive(Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub attrs: Map<String, Value>,
    pub download_uri: Option<Uri>,
    pub tags: Vec<String>,
    pub data: Option<Arc<dyn Read + Send + Sync>>,
}

impl SourceFile {
    pub fn new(path: PathBuf) -> Self {
        SourceFile {
            path,
            attrs: Map::new(),
            download_uri: None,
            tags: vec![],
            data: None,
        }
    }
}

pub struct DownloadOptions {
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub headers: Option<HashMap<String, String>>,
}

#[async_trait]
pub trait ProcessTask: Send + Sync {
    async fn run(&self) -> Result<(), String>;
    fn name(&self) -> &str;
    fn group(&self) -> Option<String>;
}

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
pub struct FileContent {
    /// /mnt/downloads
    pub download_path: PathBuf,
    /// /mnt/downloads/1.txt
    pub file_download_path: PathBuf,
    pub source_save_path: PathBuf,
    pub pattern_variables: PatternVariables,
    pub tags: Vec<String>,
    pub attrs: Map<String, Value>,
    #[serde(with = "http_serde::option::uri")]
    pub file_uri: Option<Uri>,
    /// /mnt/target
    pub target_save_path: PathBuf,
    /// 1.txt
    pub target_filename: String,
    /// /mnt/target/1.txt
    pub exist_target_path: Option<PathBuf>,
    pub errors: Vec<String>,
    pub status: FileContentStatus,
}

impl FileContent {
    pub fn target_path(&self) -> PathBuf {
        self.target_save_path.join(&self.target_filename)
    }
    pub fn file_save_root_dir(&self) -> Option<PathBuf> {
        if self.source_save_path == self.target_save_path {
            return None;
        }
        if let Ok(relative) = self.target_save_path.strip_prefix(&self.source_save_path) {
            // 3. 获取相对路径的第一级目录 (对应 Kotlin 的 Path(prefix).firstOrNull())
            // components().next() 获取第一项
            if let Some(first_component) = relative.components().next() {
                // 4. 将第一级目录拼接到 source_save_path (对应 Kotlin 的 resolve)
                let resolve = self.source_save_path.join(first_component);
                // 5. 判断结果是否与源码路径不同 (对应 Kotlin 的 takeIf)
                if resolve != self.source_save_path {
                    return Some(resolve);
                }
            }
        }
        None
    }
}

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
pub enum FileContentStatus {
    UNDETECTED,

    /**
     * 正常没有任何文件冲突
     */
    NORMAL,

    /**
     * 已下载
     */
    DOWNLOADED,

    /**
     * 路径模板变量不存在
     */
    VariableError,

    /**
     * 目标文件已经存在
     */
    TargetExists,

    /**
     * SourceItem中的目标文件冲突
     */
    FileConflict,

    /**
     * 准备替换
     */
    ReadyReplace,

    /**
     * 该文件是被替换了的
     */
    REPLACED,

    /**
     * 该文件是替换的
     */
    REPLACE,
}

pub struct ItemContent<'a> {
    pub source_item: &'a SourceItem,
    pub file_contents: &'a Vec<FileContent>,
    pub item_variables: &'a PatternVariables,
    pub status: ProcessingStatus,
}

pub trait ProcessContext {
    fn processor(&self) -> &ProcessorInfo;
    fn processed_items(&self) -> &Vec<SourceItem>;
    fn get_item_content(&self, item: &SourceItem) -> Option<ItemContent<'_>>;
    fn has_error(&self) -> bool;
}

pub struct ProcessorInfo {
    pub name: String,
    pub download_path: String,
    pub source_save_path: String,
    pub tags: HashSet<String>,
    pub category: Option<String>,
}

/// Help trigger to hold tasks
#[derive(Clone, Default)]
pub struct TaskRegistry {
    pub tasks: Arc<RwLock<Vec<Arc<dyn ProcessTask>>>>,
}

impl TaskRegistry {
    pub fn new() -> Self {
        TaskRegistry {
            tasks: Arc::new(RwLock::new(vec![])),
        }
    }

    pub fn add(&self, task: Arc<dyn ProcessTask>) {
        self.tasks.write().push(task);
    }

    pub fn remove(&self, task: Arc<dyn ProcessTask>) {
        self.tasks.write().retain(|t| !Arc::ptr_eq(t, &task));
    }
}

#[cfg(test)]
mod test {
    use crate::component::{ComponentId, ComponentRootType, FileContent, FileContentStatus};
    use std::path::PathBuf;

    #[test]
    fn parse_component_id_given_raw_string() {
        let component_id = ComponentId::parse("source:test").unwrap();
        assert_eq!(
            ComponentRootType::Source,
            component_id.component_type.root_type
        );
        assert_eq!("test", component_id.component_type.name);
        assert_eq!("test", component_id.name);

        let component_id = ComponentId::parse("source:system:test").unwrap();
        assert_eq!(
            ComponentRootType::Source,
            component_id.component_type.root_type
        );
        assert_eq!("system", component_id.component_type.name);
        assert_eq!("test", component_id.name);

        let component_id = ComponentId::parse("source");
        assert!(component_id.is_err());

        let component_id = ComponentId::parse("source:aa:ss:dd");
        assert!(component_id.is_err());
    }

    #[test]
    fn test_file_save_root_dir() {
        // 2 depth
        let mut f = FileContent {
            file_download_path: PathBuf::from("src/test/resources/downloads/1.txt"),
            source_save_path: PathBuf::from("src/test/resources/target"),
            download_path: PathBuf::from("src/test/resources/downloads"),
            pattern_variables: Default::default(),
            tags: vec![],
            attrs: Default::default(),
            file_uri: None,
            target_save_path: PathBuf::from("src/test/resources/target/test/S01"),
            target_filename: "1.txt".to_string(),
            exist_target_path: None,
            errors: vec![],
            status: FileContentStatus::UNDETECTED,
        };
        assert_eq!(
            PathBuf::from("src/test/resources/target/test"),
            f.file_save_root_dir().unwrap()
        );

        // 1 depth
        f.target_save_path = PathBuf::from("src/test/resources/target/test");
        assert_eq!(
            PathBuf::from("src/test/resources/target/test"),
            f.file_save_root_dir().unwrap()
        );

        // 0 depth
        f.target_save_path = PathBuf::from("src/test/resources/target");
        assert_eq!(None, f.file_save_root_dir());
    }
}
