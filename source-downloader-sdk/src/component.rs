#![allow(dead_code)]

use crate::SourceItem;
use crate::serde_json::{Map, Value};
use crate::storage::ProcessingStatus;
use async_trait::async_trait;
use http::Uri;
use parking_lot::RwLock;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::any::{Any, TypeId};
use std::cmp::PartialEq;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::fs;
use std::hash::Hash;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, OnceLock};

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
            ComponentType { root_type: self.to_owned(), name: type_name.to_owned() },
            name.to_owned(),
        )
    }
}

impl Display for ComponentRootType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentType {
    pub root_type: ComponentRootType,
    #[serde(rename = "typeName")]
    pub name: String,
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentId {
    pub component_type: ComponentType,
    pub name: String,
}

impl ComponentId {
    pub fn new(component_type: ComponentType, name: &str) -> Self {
        ComponentId { component_type, name: name.to_string() }
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
            component_type: ComponentType { root_type, name: split[1].to_string() },
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
        ComponentType { root_type: ComponentRootType::Trigger, name }
    }
    pub fn source(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::Source, name }
    }
    pub fn downloader(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::Downloader, name }
    }
    pub fn file_mover(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::FileMover, name }
    }
    pub fn variable_provider(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::VariableProvider, name }
    }
    pub fn file_resolver(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::ItemFileResolver, name }
    }
    pub fn item_filter(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::SourceItemFilter, name }
    }
    pub fn item_content_filter(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::ItemContentFilter, name }
    }
    pub fn listener(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::ProcessListener, name }
    }
    pub fn source_file_filter(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::SourceFileFilter, name }
    }
    pub fn file_content_filter(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::FileContentFilter, name }
    }
    pub fn file_tagger(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::FileTagger, name }
    }
    pub fn file_replacement_decider(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::FileReplacementDecider, name }
    }
    pub fn file_exists_detector(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::FileExistsDetector, name }
    }
    pub fn variable_replacer(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::VariableReplacer, name }
    }
    pub fn trimmer(name: String) -> ComponentType {
        ComponentType { root_type: ComponentRootType::Trimmer, name }
    }
}

impl Display for ComponentType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.root_type.name(), self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentSelector {
    pub root_type: ComponentRootType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_names: Vec<String>,
}

impl ComponentSelector {
    pub fn matches(&self, component_type: &ComponentType) -> bool {
        self.root_type == component_type.root_type
            && (self.type_names.is_empty()
                || self.type_names.iter().any(|name| name == &component_type.name))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum ComponentCompatibilityRelation {
    InstanceNameEquals,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum ComponentCompatibilityConstraint {
    Requires {
        target: ComponentSelector,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        relations: Vec<ComponentCompatibilityRelation>,
    },
    Forbids {
        target: ComponentSelector,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        relations: Vec<ComponentCompatibilityRelation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentCompatibilityRule {
    pub code: String,
    pub owner: ComponentType,
    pub constraint: ComponentCompatibilityConstraint,
    pub message: String,
}

pub trait ComponentCreateContext: Send + Sync {
    fn get_instance(
        &self,
        name: &str,
        type_id: TypeId,
    ) -> Result<Arc<dyn Any + Send + Sync>, ComponentError>;
}
pub struct EmptyComponentCreateContext;

pub const EMPTY_COMPONENT_CREATE_CONTEXT: EmptyComponentCreateContext =
    EmptyComponentCreateContext;

impl ComponentCreateContext for EmptyComponentCreateContext {
    fn get_instance(
        &self,
        name: &str,
        _: TypeId,
    ) -> Result<Arc<dyn Any + Send + Sync>, ComponentError> {
        Err(ComponentError::new(format!(
            "Component instance '{name}' requires a creation context",
        )))
    }
}

pub trait ComponentSupplier: Send + Sync {
    /// 组件的创建类型
    fn supply_types(&self) -> Vec<ComponentType>;

    /// 声明创建 Processor 时可静态检查的组件兼容规则。
    fn compatibility_rules(&self) -> Vec<ComponentCompatibilityRule> {
        Vec::new()
    }

    /// 创建组件实例
    fn apply(
        &self,
        context: &dyn ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError>;

    /// 如果是true即便没有在配置中定义也会调用[`ComponentSupplier::apply`]
    fn is_support_no_props(&self) -> bool {
        false
    }

    /// 声明组件的属性结构元信息提供给ui渲染表单
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>>;
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SdComponentMetadata {
    pub description: String,
    pub props_json_schema: Option<Value>,
    pub props_ui_schema: Option<Value>,
    pub state_json_schema: Option<Value>,
    pub state_ui_schema: Option<Value>,
    pub source_pointer_json_schema: Option<Value>,
}

pub trait SdComponent: Any + Send + Sync + Debug + Display {
    fn as_trigger(self: Arc<Self>) -> Result<Arc<dyn Trigger>, ComponentError> {
        Err(ComponentError::from("Not a trigger component"))
    }
    fn as_source(self: Arc<Self>) -> Result<Arc<dyn Source>, ComponentError> {
        Err(ComponentError::from("Not a source component"))
    }
    fn as_item_file_resolver(
        self: Arc<Self>,
    ) -> Result<Arc<dyn ItemFileResolver>, ComponentError> {
        Err(ComponentError::from("Not a item file resolver component"))
    }
    fn as_downloader(self: Arc<Self>) -> Result<Arc<dyn Downloader>, ComponentError> {
        Err(ComponentError::from("Not a downloader component"))
    }
    fn as_file_mover(self: Arc<Self>) -> Result<Arc<dyn FileMover>, ComponentError> {
        Err(ComponentError::from("Not a file mover component"))
    }
    fn as_process_listener(
        self: Arc<Self>,
    ) -> Result<Arc<dyn ProcessListener>, ComponentError> {
        Err(ComponentError::from("Not a process listener component"))
    }
    fn as_source_item_filter(
        self: Arc<Self>,
    ) -> Result<Arc<dyn SourceItemFilter>, ComponentError> {
        Err(ComponentError::from("Not a source item filter component"))
    }
    fn as_source_file_filter(
        self: Arc<Self>,
    ) -> Result<Arc<dyn SourceFileFilter>, ComponentError> {
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
        Err(ComponentError::from("Not a file replacement decider component"))
    }
    fn as_file_exists_detector(
        self: Arc<Self>,
    ) -> Result<Arc<dyn FileExistsDetector>, ComponentError> {
        Err(ComponentError::from("Not a file exists detector component"))
    }
    fn as_variable_provider(
        self: Arc<Self>,
    ) -> Result<Arc<dyn VariableProvider>, ComponentError> {
        Err(ComponentError::from("Not a variable provider component"))
    }
    fn as_variable_replacer(
        self: Arc<Self>,
    ) -> Result<Arc<dyn VariableReplacer>, ComponentError> {
        Err(ComponentError::from("Not a variable replacer component"))
    }
    fn as_trimmer(self: Arc<Self>) -> Result<Arc<dyn Trimmer>, ComponentError> {
        Err(ComponentError::from("Not a trimmer component"))
    }
    fn as_async_downloader(
        self: Arc<Self>,
    ) -> Result<Arc<dyn AsyncDownloader>, ComponentError> {
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

#[async_trait]
pub trait Downloader: SdComponent {
    async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError>;
    fn default_download_path(&self) -> &str;
    async fn cancel(
        &self,
        item: &SourceItem,
        files: &[SourceFile],
    ) -> Result<(), ProcessingError>;
}

#[async_trait]
pub trait AsyncDownloader: Downloader {
    async fn is_finished(&self, item: &SourceItem) -> Option<bool>;
}

#[async_trait]
pub trait Source: SdComponent {
    async fn fetch<'pointer>(
        &self,
        source_pointer: &'pointer dyn SourcePointer,
        limit: u32,
    ) -> Result<Vec<PointedItem>, ProcessingError>;
    fn default_pointer(&self) -> Box<dyn SourcePointer>;
    fn parse_raw_pointer(&self, value: Value) -> Box<dyn SourcePointer>;
    fn headers(&self, _: &SourceItem) -> Option<HashMap<String, String>> {
        None
    }
    fn group(&self) -> Option<String> {
        None
    }
}
#[async_trait]
pub trait ItemFileResolver: SdComponent {
    async fn resolve_files(
        &self,
        item: &SourceItem,
    ) -> Result<Vec<SourceFile>, ProcessingError>;
}

#[async_trait]
pub trait FileMover: SdComponent {
    async fn move_file(
        &self,
        _source_item: &SourceItem,
        file: &FileContent,
    ) -> Result<(), ProcessingError> {
        fs::rename(&file.file_download_path, file.target_path()).map_err(Into::into)
    }

    async fn exists(&self, paths: &[&PathBuf]) -> Vec<bool> {
        paths.iter().map(|path| path.exists()).collect()
    }

    async fn create_directories(&self, path: &Path) -> Result<(), ProcessingError> {
        fs::create_dir_all(path).map_err(Into::into)
    }

    async fn replace(
        &self,
        _source_item: &SourceItem,
        files: &[&FileContent],
    ) -> Result<(), ProcessingError> {
        for file in files {
            let existing_path = file.exist_target_path.as_ref().ok_or_else(|| {
                ProcessingError::non_retryable("exist_target_path is missing")
            })?;
            let mut backup_name = existing_path.as_os_str().to_os_string();
            backup_name.push(".bak");
            let backup_path = PathBuf::from(backup_name);

            if existing_path.exists() {
                fs::rename(existing_path, &backup_path)?;
            }

            if let Err(error) = fs::rename(&file.file_download_path, file.target_path()) {
                if backup_path.exists() {
                    fs::rename(&backup_path, existing_path)?;
                }
                return Err(error.into());
            }
            if backup_path.exists() {
                fs::remove_file(&backup_path)?;
            }
        }
        Ok(())
    }

    async fn list_files(&self, path: &Path) -> Result<Vec<PathBuf>, ProcessingError> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        fs::read_dir(path)?
            .map(|entry| entry.map(|value| value.path()).map_err(Into::into))
            .collect()
    }

    async fn path_metadata(&self, path: &Path) -> Result<SourceFile, ProcessingError> {
        let metadata = fs::symlink_metadata(path)?;
        let mut file = SourceFile::new(path.to_path_buf());
        file.attrs.insert("size".to_owned(), metadata.len().into());
        file.attrs.insert(
            "lastModifiedTime".to_owned(),
            metadata
                .modified()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| Value::from(value.as_millis() as u64))
                .unwrap_or(Value::Null),
        );
        file.attrs.insert(
            "creationTime".to_owned(),
            metadata
                .created()
                .ok()
                .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|value| Value::from(value.as_millis() as u64))
                .unwrap_or(Value::Null),
        );
        file.attrs.insert(
            "isSymbolicLink".to_owned(),
            metadata.file_type().is_symlink().into(),
        );
        Ok(file)
    }

    async fn is_supported_batch_move(&self) -> bool {
        false
    }

    async fn batch_move(
        &self,
        _: &SourceItem,
        _: &[&FileContent],
    ) -> Result<(), ProcessingError> {
        Err(ProcessingError::non_retryable("Batch move is not supported"))
    }
}

/// Processors isolate listener errors: one failed listener does not prevent later
/// listeners from observing the same event.
///
/// Implementations must return operational failures as [`ProcessingError`] and
/// must not panic. Panics are programming errors and are not isolated.
pub trait ProcessListener: SdComponent {
    /// When item rename is successful
    fn on_item_success(
        &self,
        ctx: &dyn ProcessContext,
        item_content: &ItemContent,
    ) -> Result<(), ProcessingError>;
    /// When item processing is failed
    fn on_item_error(
        &self,
        ctx: &dyn ProcessContext,
        item: &SourceItem,
        error: &ProcessingError,
    ) -> Result<(), ProcessingError>;
    /// When processing is completed
    fn on_process_completed(
        &self,
        ctx: &dyn ProcessContext,
    ) -> Result<(), ProcessingError>;
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
        source_item: &SourceItem,
        current_file: &FileContent,
        before: Option<&InProcessingItem>,
        existing_file: &SourceFile,
    ) -> bool;
}

#[async_trait]
pub trait FileExistsDetector: SdComponent {
    async fn exists<'a>(
        &self,
        file_mover: &'a dyn FileMover,
        source_item: &'a SourceItem,
        file_contents: &'a [FileContent],
    ) -> HashMap<&'a PathBuf, Option<PathBuf>>;
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
    async fn extract_from(
        &self,
        item: &SourceItem,
        value: &str,
    ) -> Option<HashMap<String, Value>>;
    fn primary_variable_name(&self) -> Option<String>;
}

pub trait VariableReplacer: SdComponent {
    fn replace(&self, key: &str, value: String) -> String;
}

/// Reduces a configured variable before path rendering, typically using domain-specific
/// rules. Implementations do not guarantee that the final rendered path satisfies file
/// system length limits; the path overflow strategy enforces that separate constraint.
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

pub static EMPTY_POINTER: LazyLock<Arc<EmptyPointer>> =
    LazyLock::new(|| Arc::new(EmptyPointer {}));

pub trait SourcePointer: Any + Send + Sync {
    fn dump(&self) -> Value;
    fn update(&mut self, item: &SourceItem, item_pointer: &dyn ItemPointer);
    fn as_any(&self) -> &dyn Any;
}

impl SourcePointer for EmptyPointer {
    fn dump(&self) -> Value {
        Value::Object(Map::new())
    }

    fn update(&mut self, _: &SourceItem, _: &dyn ItemPointer) {}

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, Clone)]
pub struct PointedItem {
    pub source_item: SourceItem,
    pub item_pointer: Arc<dyn ItemPointer>,
}

pub fn deserialize_component_config<T>(
    props: &Map<String, Value>,
) -> Result<T, ComponentError>
where
    T: DeserializeOwned,
{
    serde_path_to_error::deserialize(Value::Object(props.clone())).map_err(|error| {
        let path = if error.path().iter().next().is_none() {
            "<root>".to_owned()
        } else {
            error.path().to_string()
        };
        ComponentError::new(format!(
            "Invalid configuration at '{path}': {}",
            error.inner()
        ))
    })
}
pub fn format_error_chain(error: &(dyn Error + 'static)) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[derive(Clone)]
pub struct ComponentError {
    pub message: String,
}

impl ComponentError {
    pub fn new<S: Into<String>>(message: S) -> Self {
        ComponentError { message: message.into() }
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
        Self::Retryable { message: message.into() }
    }

    pub fn non_retryable<S: Into<String>>(message: S) -> Self {
        Self::NonRetryable { message: message.into(), skip: false }
    }

    pub fn skip<S: Into<String>>(message: S) -> Self {
        Self::NonRetryable { message: message.into(), skip: true }
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

impl From<std::io::Error> for ProcessingError {
    fn from(err: std::io::Error) -> Self {
        ProcessingError::NonRetryable {
            message: format!("IO error: {}", err),
            skip: false,
        }
    }
}

pub struct DownloadTask<'a> {
    pub source_item: &'a SourceItem,
    pub download_files: &'a [SourceFileRef<'a>],
    pub download_path: &'a Path,
    pub category: &'a Option<String>,
    pub tags: Option<&'a [String]>,
    pub headers: Option<HashMap<&'a String, &'a String>>,
}

#[derive(Clone)]
pub struct SourceFile {
    pub path: PathBuf,
    pub attrs: Map<String, Value>,
    pub download_uri: Option<Uri>,
    pub tags: Vec<String>,
    pub data: Option<Arc<[u8]>>,
}

pub struct SourceFileRef<'a> {
    pub path: &'a Path,
    pub attrs: &'a Map<String, Value>,
    pub download_uri: Option<&'a Uri>,
    pub tags: &'a [String],
    pub data: Option<&'a Arc<[u8]>>,
}

impl<'a> From<&'a SourceFile> for SourceFileRef<'a> {
    fn from(value: &'a SourceFile) -> Self {
        SourceFileRef {
            path: &value.path,
            attrs: &value.attrs,
            download_uri: value.download_uri.as_ref(),
            tags: &value.tags,
            data: value.data.as_ref(),
        }
    }
}

impl<'a> From<&'a FileContent> for SourceFileRef<'a> {
    fn from(value: &'a FileContent) -> Self {
        SourceFileRef {
            path: &value.file_download_path,
            attrs: &value.attrs,
            download_uri: value.file_uri.as_ref(),
            tags: &value.tags,
            data: value.data.as_ref(),
        }
    }
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

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileContent {
    /// /mnt/downloads
    pub download_path: PathBuf,
    /// /mnt/downloads/1.txt
    pub file_download_path: PathBuf,
    pub source_save_path: PathBuf,
    pub pattern_variables: PatternVariables,
    pub file_save_path_pattern: String,
    pub filename_pattern: String,
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
    #[serde(skip, default)]
    pub target_path: OnceLock<PathBuf>,
    #[serde(skip, default)]
    pub data: Option<Arc<[u8]>>,
    pub processed_variables: Option<PatternVariables>,
}

impl Debug for FileContent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileContent")
            .field("file_download_path", &self.file_download_path)
            .field("file_uri", &self.file_uri)
            .field("target_save_path", &self.target_save_path)
            .field("target_filename", &self.target_filename)
            .field("exist_target_path", &self.exist_target_path)
            .field("errors", &self.errors)
            .field("status", &self.status)
            .finish()
    }
}

impl FileContent {
    pub fn target_path(&self) -> &PathBuf {
        self.target_path.get_or_init(|| self.target_save_path.join(&self.target_filename))
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

pub struct ItemContent<'a> {
    pub source_item: &'a SourceItem,
    pub file_contents: &'a Vec<FileContent>,
    pub item_variables: &'a PatternVariables,
    pub status: ProcessingStatus,
}

pub struct InProcessingItem<'a> {
    pub id: &'a Option<i64>,
    pub processor_name: &'a str,
    pub item_hash: &'a str,
    pub item_identity: &'a Option<String>,
    pub source_item: &'a SourceItem,
    pub item_variables: &'a PatternVariables,
    pub file_contents: &'a Vec<FileContent>,
    pub rename_times: &'a u32,
    pub status: &'a ProcessingStatus,
    pub failure_reason: Option<&'a str>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Eq, PartialEq)]
pub enum FileContentStatus {
    Undetected,

    /**
     * 正常没有任何文件冲突
     */
    Normal,

    /**
     * 已下载
     */
    Downloaded,

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
    Replaced,

    /**
     * 该文件是替换的
     */
    Replace,
}

pub trait ProcessContext {
    fn processor(&self) -> &ProcessorInfo;
    fn processed_items(&self) -> Box<dyn ExactSizeIterator<Item = &SourceItem> + '_>;
    fn get_item_content(&self, item: &SourceItem) -> Option<InProcessingItem<'_>>;
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
        TaskRegistry { tasks: Arc::new(RwLock::new(vec![])) }
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
    use crate::component::{
        ComponentId, ComponentRootType, FileContent, FileContentStatus,
        deserialize_component_config,
    };
    use crate::serde_json::json;
    use serde::Deserialize;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    #[derive(Debug, Deserialize)]
    #[serde(rename_all = "kebab-case")]
    struct FlatConfig {
        api_hash: String,
    }

    #[derive(Debug, Deserialize)]
    struct NestedConfig {
        rules: Vec<RuleConfig>,
    }

    #[derive(Debug, Deserialize)]
    struct RuleConfig {
        matcher: MatcherConfig,
    }

    #[derive(Debug, Deserialize)]
    struct MatcherConfig {
        tags: Vec<String>,
    }

    #[test]
    fn deserialize_component_config_reports_top_level_field() {
        let props = json!({ "api-hash": 1 }).as_object().unwrap().clone();

        let error = deserialize_component_config::<FlatConfig>(&props).unwrap_err();

        assert_eq!(
            error.message,
            "Invalid configuration at 'api-hash': invalid type: integer `1`, expected a string"
        );
    }

    #[test]
    fn deserialize_component_config_reports_missing_field() {
        let props = json!({}).as_object().unwrap().clone();

        let error = deserialize_component_config::<FlatConfig>(&props).unwrap_err();

        assert_eq!(
            error.message,
            "Invalid configuration at '<root>': missing field `api-hash`"
        );
    }

    #[test]
    fn deserialize_component_config_reports_nested_collection_path() {
        let props = json!({
            "rules": [{
                "matcher": {
                    "tags": "anime"
                }
            }]
        })
        .as_object()
        .unwrap()
        .clone();

        let error = deserialize_component_config::<NestedConfig>(&props).unwrap_err();

        assert_eq!(
            error.message,
            "Invalid configuration at 'rules[0].matcher.tags': invalid type: string \"anime\", expected a sequence"
        );
    }

    #[test]
    fn parse_component_id_given_raw_string() {
        let component_id = ComponentId::parse("source:test").unwrap();
        assert_eq!(ComponentRootType::Source, component_id.component_type.root_type);
        assert_eq!("test", component_id.component_type.name);
        assert_eq!("test", component_id.name);

        let component_id = ComponentId::parse("source:system:test").unwrap();
        assert_eq!(ComponentRootType::Source, component_id.component_type.root_type);
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
            file_save_path_pattern: String::new(),
            filename_pattern: String::new(),
            tags: vec![],
            attrs: Default::default(),
            file_uri: None,
            target_save_path: PathBuf::from("src/test/resources/target/test/S01"),
            target_filename: "1.txt".to_string(),
            exist_target_path: None,
            errors: vec![],
            status: FileContentStatus::Undetected,
            target_path: OnceLock::new(),
            data: None,
            processed_variables: None,
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
