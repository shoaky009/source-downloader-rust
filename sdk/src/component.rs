#![allow(dead_code)]

use crate::SourceItem;
use crate::{Map, Value};
use http::Uri;
use std::any::Any;
use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;
use std::io::Read;
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
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
    Tagger,
    FileReplacementDecider,
    FileExistsDetector,
    VariableReplacer,
    Trimmer,
}

impl ComponentRootType {
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
            ComponentRootType::Tagger => "tagger",
            ComponentRootType::FileReplacementDecider => "file-replacement-decider",
            ComponentRootType::FileExistsDetector => "file-exists-detector",
            ComponentRootType::VariableReplacer => "variable-replacer",
            ComponentRootType::Trimmer => "trimmer",
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct ComponentType {
    pub root_type: ComponentRootType,
    pub name: String,
}

impl ComponentType {
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
            root_type: ComponentRootType::Tagger,
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
    json_schema: Option<HashMap<String, Box<dyn Any>>>,
    ui_schema: Option<HashMap<String, Box<dyn Any>>>,
}

pub trait SdComponent: Any + Send + Sync + Debug {
    fn as_trigger(&self) -> Result<Arc<dyn Trigger>, ComponentError> {
        Err(ComponentError::from("Not a trigger component"))
    }
    fn as_source(self: Arc<Self>) -> Result<Arc<dyn Source>, ComponentError> {
        Err(ComponentError::from("Not a source component"))
    }
    fn as_downloader(&self) -> Result<Arc<dyn Downloader>, ComponentError> {
        Err(ComponentError::from("Not a downloader component"))
    }
    fn as_item_filter(&self) -> Result<Arc<dyn ItemFilter>, ComponentError> {
        Err(ComponentError::from("Not a item filter component"))
    }
    fn as_file_mover(&self) -> Result<Arc<dyn FileMover>, ComponentError> {
        Err(ComponentError::from("Not a file mover component"))
    }
    fn as_process_listener(&self) -> Result<Arc<dyn ProcessListener>, ComponentError> {
        Err(ComponentError::from("Not a process listener component"))
    }
    fn as_source_item_filter(&self) -> Result<Arc<dyn SourceItemFilter>, ComponentError> {
        Err(ComponentError::from("Not a source item filter component"))
    }
    fn as_source_file_filter(&self) -> Result<Arc<dyn SourceFileFilter>, ComponentError> {
        Err(ComponentError::from("Not a source file filter component"))
    }
    fn as_item_content_filter(&self) -> Result<Arc<dyn ItemContentFilter>, ComponentError> {
        Err(ComponentError::from("Not a item content filter component"))
    }
    fn as_file_content_filter(&self) -> Result<Arc<dyn FileContentFilter>, ComponentError> {
        Err(ComponentError::from("Not a file content filter component"))
    }
    fn as_file_tagger(&self) -> Result<Arc<dyn FileTagger>, ComponentError> {
        Err(ComponentError::from("Not a file tagger component"))
    }
    fn as_file_replacement_decider(
        &self,
    ) -> Result<Arc<dyn FileReplacementDecider>, ComponentError> {
        Err(ComponentError::from(
            "Not a file replacement decider component",
        ))
    }
    fn as_item_exists_detector(&self) -> Result<Arc<dyn ItemExistsDetector>, ComponentError> {
        Err(ComponentError::from("Not a item exists detector component"))
    }
    fn as_variable_replacer(&self) -> Result<Arc<dyn VariableReplacer>, ComponentError> {
        Err(ComponentError::from("Not a variable replacer component"))
    }
    fn as_trimmer(&self) -> Result<Arc<dyn Trimmer>, ComponentError> {
        Err(ComponentError::from("Not a trimmer component"))
    }
    fn as_async_downloader(&self) -> Result<Arc<dyn AsyncDownloader>, ComponentError> {
        Err(ComponentError::from("Not a async downloader component"))
    }
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
    fn add_task(&self, task: Arc<ProcessorTask>);
    fn remove_task(&self, task: Arc<ProcessorTask>);
}

pub trait Downloader: SdComponent {
    fn submit(&self, task: &DownloadTask) -> Result<(), ComponentError>;
    fn default_download_path(&self) -> &str;
    fn cancel(&self, item: &DownloadTask, files: &Vec<SourceFile>) -> Result<(), ComponentError>;
}

pub trait AsyncDownloader: Downloader {
    fn is_finished(&self, item: &SourceItem) -> Option<bool>;
}

pub trait Source: SdComponent {
    fn fetch(&self, source_pointer: &Map<String, Value>) -> Vec<PointedItem>;
    fn default_pointer(&self) -> Box<dyn ItemPointer>;
    fn headers(&self, _: &SourceItem) -> Option<HashMap<String, String>> {
        None
    }
    fn group(&self) -> Option<String> {
        None
    }
}

pub trait ItemFileResolver: SdComponent {
    fn resolve_files(&self, item: &SourceItem) -> Vec<SourceFile>;
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
        Err(ProcessingError {
            message: "Batch move is not supported".to_string(),
            skip: false,
        })
    }
}

pub trait ProcessListener: SdComponent {
    fn on_item_success(&self, ctx: &dyn ProcessContext, item_content: &ItemContent);
    fn on_item_error(&self, ctx: &dyn ProcessContext, item: &SourceItem, error: &ProcessingError);
    fn on_process_completed(&self, ctx: &dyn ProcessContext);
}

pub trait SourceItemFilter: SdComponent {
    fn filter(&self, item: &SourceItem) -> bool;
}

pub trait SourceFileFilter: SdComponent {
    fn filter(&self, file: &SourceFile) -> bool;
}

pub trait ItemContentFilter: SdComponent {
    fn filter(&self, item_content: &ItemContent) -> bool;
}

pub trait FileContentFilter: SdComponent {
    fn filter(&self, file_content: &FileContent) -> bool;
}

pub trait FileTagger: SdComponent {
    fn tag(&self, source_file: &SourceFile) -> Option<String>;
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

pub trait VariableProvider: SdComponent {
    fn accuracy(&self) -> i32 {
        1
    }
    fn item_variables(&self, item: &SourceItem) -> Map<String, Value>;
    fn file_variables(
        &self,
        item: &SourceItem,
        item_variables: &Map<String, Value>,
        files: &[SourceFile],
    ) -> Map<String, Value>;
    fn extract_from(&self, item: &SourceItem, value: &str) -> Option<Map<String, Value>>;
    fn primary_variable_name(&self) -> Option<String>;
}

pub trait VariableReplacer: SdComponent {
    fn replace(&self, key: &str, value: String) -> String;
}

pub trait Trimmer: SdComponent {
    fn trim(&self, value: String, expect_size: &i32) -> String;
}

// </editor-fold>

pub trait ItemFilter: SdComponent {
    fn filter(&self, item: &PointedItem) -> bool;
}

pub trait ItemPointer: Debug + Send + Sync {
    fn clone_box(&self) -> Box<dyn ItemPointer>;
}

#[derive(Debug, Clone)]
struct EmptyPointer;

impl ItemPointer for EmptyPointer {
    fn clone_box(&self) -> Box<dyn ItemPointer> {
        Box::new(self.clone())
    }
}

const EMPTY_POINTER: EmptyPointer = EmptyPointer {};

pub fn empty_pointer() -> Box<dyn ItemPointer> {
    Box::new(EMPTY_POINTER)
}

#[derive(Debug)]
pub struct PointedItem {
    pub source_item: SourceItem,
    pub pointer: Box<dyn ItemPointer>,
}

impl Clone for PointedItem {
    fn clone(&self) -> Self {
        PointedItem {
            source_item: self.source_item.clone(),
            pointer: self.pointer.clone_box(),
        }
    }
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

#[derive(Debug)]
pub struct ProcessingError {
    pub message: String,
    pub skip: bool,
}

impl Error for ProcessingError {}

impl Display for ProcessingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

pub struct DownloadTask<'a> {
    pub item: &'a SourceItem,
    pub download_files: &'a Vec<SourceFile>,
    pub download_path: &'a String,
    pub download_options: &'a DownloadOptions,
}

pub struct SourceFile {
    pub path: String,
    pub attrs: Map<String, Value>,
    pub download_uri: Option<Uri>,
    pub tags: HashSet<String>,
    pub data: Option<Arc<dyn Read + Send + Sync>>,
}

pub struct DownloadOptions {
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub headers: Option<HashMap<String, String>>,
}

pub struct ProcessorTask {
    pub process_name: String,
    pub runnable: Box<dyn Fn() + Send + Sync + 'static>,
    pub group: Option<String>,
}

pub struct FileContent {
    pub download_path: String,
    pub file_download_path: String,
    pub pattern_variables: Map<String, Value>,
    pub tags: HashSet<String>,
    pub attrs: Map<String, Value>,
    pub file_uri: Option<Uri>,
    pub target_save_path: String,
    pub target_filename: String,
    pub exist_target_path: Option<String>,
}

pub struct ItemContent {
    pub source_item: SourceItem,
    pub file_contents: Vec<FileContent>,
    pub item_variables: Map<String, Value>,
}

pub trait ProcessContext {
    fn processor(&self) -> ProcessorInfo;
    fn processed_items(&self) -> Vec<SourceItem>;
    fn get_item_content(&self, item: &SourceItem) -> Option<ItemContent>;
    fn has_error(&self) -> bool;
}

pub struct ProcessorInfo {
    pub name: String,
    pub download_path: String,
    pub source_save_path: String,
    pub tags: HashSet<String>,
    pub category: Option<String>,
}
