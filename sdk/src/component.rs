#![allow(dead_code)]

use crate::SourceItem;
use serde_json::{Map, Value};
use std::any::Any;
use std::cmp::PartialEq;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{Debug, Display, Formatter};
use std::hash::Hash;

#[derive(Debug, PartialEq, Eq, Hash)]
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
    pub fn name(&self) -> &'static str {
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

#[derive(Debug, PartialEq, Eq, Hash)]
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
}

pub trait ComponentSupplier {
    fn supply_types(&self) -> Vec<ComponentType>;

    fn apply(&self, props: Map<String, Value>) -> Result<Box<dyn SdComponent>, ComponentError>;

    fn is_support_no_props(&self) -> bool {
        false
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>>;
}

pub struct SdComponentMetadata {
    description: String,
    json_schema: Option<HashMap<String, Box<dyn Any>>>,
    ui_schema: Option<HashMap<String, Box<dyn Any>>>,
}

pub trait SdComponent {}

pub trait Source: SdComponent {
    fn fetch(&self) -> Vec<PointedItem>;
}

pub trait ItemPointer {}
struct EmptyPointer;

impl ItemPointer for EmptyPointer {}

const EMPTY_POINTER: EmptyPointer = EmptyPointer {};

pub fn empty_pointer() -> Box<dyn ItemPointer> {
    Box::new(EMPTY_POINTER)
}

pub struct PointedItem {
    pub source_item: SourceItem,
    pub pointer: Box<dyn ItemPointer>,
}

pub struct ComponentError {
    message: String,
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

