use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, PatternVariables, SdComponent,
    SdComponentMetadata, SourceFile, VariableProvider, deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct RegexVariableProviderSupplier;
pub const SUPPLIER: RegexVariableProviderSupplier = RegexVariableProviderSupplier;

#[derive(Deserialize)]
struct RegexVariableProviderConfig {
    regexes: Vec<RegexVariableConfig>,
    primary: Option<String>,
}

#[derive(Deserialize)]
struct RegexVariableConfig {
    name: String,
    regex: String,
    #[serde(default = "default_field")]
    field: String,
}

fn default_field() -> String {
    String::from("title")
}

impl ComponentSupplier for RegexVariableProviderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::variable_provider("regex".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config: RegexVariableProviderConfig = deserialize_component_config(props)?;
        let mut regexes = Vec::with_capacity(config.regexes.len());
        for (index, regex) in config.regexes.into_iter().enumerate() {
            let compiled = Regex::new(&regex.regex).map_err(|error| {
                ComponentError::new(format!(
                    "Invalid configuration at 'regexes[{index}].regex': Invalid regex for '{}': {error}",
                    regex.name
                ))
            })?;
            regexes.push(RegexVariable {
                name: regex.name,
                regex: compiled,
                field: regex.field,
            });
        }
        Ok(Arc::new(RegexVariableProvider { regexes, primary: config.primary }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Extracts variables from source item fields using configured regular expressions.".to_owned(),
            props_json_schema: Some(json!({"type":"object","properties":{"regexes":{"type":"array","items":{"type":"object","properties":{"name":{"type":"string"},"regex":{"type":"string"},"field":{"type":"string","default":"title"}},"required":["name","regex"]}},"primary":{"type":"string"}},"required":["regexes"]})),
            props_ui_schema: None, state_json_schema: None, state_ui_schema: None, source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableProvider)]
pub struct RegexVariableProvider {
    regexes: Vec<RegexVariable>,
    primary: Option<String>,
}

#[derive(Debug)]
pub struct RegexVariable {
    pub name: String,
    pub regex: Regex,
    pub field: String,
}

impl Display for RegexVariableProvider {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("regex")
    }
}

#[async_trait]
impl VariableProvider for RegexVariableProvider {
    async fn item_variables(&self, source_item: &SourceItem) -> HashMap<String, String> {
        self.regexes
            .iter()
            .filter_map(|variable| {
                let value = resolve_field(source_item, &variable.field)?;
                variable
                    .regex
                    .find(value.as_ref())
                    .map(|matched| (variable.name.clone(), matched.as_str().to_owned()))
            })
            .collect()
    }

    async fn file_variables(
        &self,
        _: &SourceItem,
        _: &PatternVariables,
        source_files: &[SourceFile],
    ) -> Vec<PatternVariables> {
        vec![HashMap::new(); source_files.len()]
    }

    async fn extract_from(
        &self,
        _: &SourceItem,
        text: &str,
    ) -> Option<HashMap<String, Value>> {
        let variables: HashMap<String, Value> = self
            .regexes
            .iter()
            .filter_map(|variable| {
                variable.regex.find(text).map(|matched| {
                    (variable.name.clone(), Value::String(matched.as_str().to_owned()))
                })
            })
            .collect();
        (!variables.is_empty()).then_some(variables)
    }

    fn primary_variable_name(&self) -> Option<String> {
        self.primary.clone()
    }
}

fn resolve_field<'a>(
    source_item: &'a SourceItem,
    field: &str,
) -> Option<std::borrow::Cow<'a, str>> {
    match field {
        "title" => Some(source_item.title.as_str().into()),
        "link" => Some(source_item.link.to_string().into()),
        "downloadUri" => Some(source_item.download_uri.to_string().into()),
        "contentType" => Some(source_item.content_type.as_str().into()),
        "datetime" => Some(source_item.datetime.to_string().into()),
        unknown => {
            tracing::error!(field = unknown, "Unknown regex variable provider field");
            None
        }
    }
}
