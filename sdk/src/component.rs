#![allow(dead_code)]

use std::any::Any;
use std::collections::HashMap;

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

struct ComponentType {
    root_type: ComponentRootType,
    name: String,
}

pub trait ComponentSupplier {

    fn supply_types(&self) -> Vec<&ComponentType>;

    fn apply(&self, props: Option<HashMap<&String, &dyn Any>>) -> Box<dyn SdComponent>;

    fn is_support_no_props(&self) -> bool {
        false
    }

    // fn get_metadata() -> Option<Box<SdComponentMetadata>>;
}

pub trait SdComponent {}

pub struct SdComponentMetadata {
    description: String,
    ui_schema: Option<HashMap<String, Box<dyn Any>>>,
}
