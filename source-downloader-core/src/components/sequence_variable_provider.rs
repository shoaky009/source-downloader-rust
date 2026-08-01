use async_trait::async_trait;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct SequenceVariableProviderSupplier;
pub const SUPPLIER: SequenceVariableProviderSupplier = SequenceVariableProviderSupplier;

impl ComponentSupplier for SequenceVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("sequence".to_owned())]
    }

    fn apply(
        &self,
        _: &Map<String, Value>,
    ) -> Result<Arc<dyn source_downloader_sdk::component::SdComponent>, ComponentError>
    {
        Ok(Arc::new(SequenceVariableProvider))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableProvider)]
pub struct SequenceVariableProvider;

impl Display for SequenceVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("sequence")
    }
}

#[async_trait]
impl VariableProvider for SequenceVariableProvider {
    async fn item_variables(&self, _: &SourceItem) -> HashMap<String, String> {
        HashMap::new()
    }

    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        source_files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        let width = source_files.len().to_string().len();
        (0..source_files.len())
            .map(|index| {
                HashMap::from([(
                    String::from("sequence"),
                    format!("{:0width$}", index + 1, width = width),
                )])
            })
            .collect()
    }

    fn extract_from(&self, _: &SourceItem, _: &str) -> Option<HashMap<String, Value>> {
        None
    }

    fn primary_variable_name(&self) -> Option<String> {
        Some(String::from("sequence"))
    }
}
