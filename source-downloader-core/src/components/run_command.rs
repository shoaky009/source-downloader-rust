use serde::de::Visitor;
use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};
use serde::{Deserialize, Deserializer};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContentStatus, ItemContent,
    ProcessContext, ProcessListener, ProcessingError, SdComponent, SdComponentMetadata,
    deserialize_component_config,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::fmt::{Display, Formatter};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;

pub struct RunCommandSupplier;
pub const SUPPLIER: RunCommandSupplier = RunCommandSupplier;

struct CommandValues(Vec<String>);

impl<'de> Deserialize<'de> for CommandValues {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CommandValuesVisitor;

        impl<'de> Visitor<'de> for CommandValuesVisitor {
            type Value = CommandValues;

            fn expecting(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array or object")
            }

            fn visit_seq<A>(self, sequence: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let sequence = SeqAccessDeserializer::new(sequence);
                let values = Vec::<Value>::deserialize(sequence)?;
                Ok(CommandValues(values.iter().map(value_to_string).collect()))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let map = MapAccessDeserializer::new(map);
                let values = Map::<String, Value>::deserialize(map)?;
                Ok(CommandValues(values.values().map(value_to_string).collect()))
            }
        }

        deserializer.deserialize_any(CommandValuesVisitor)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RunCommandConfig {
    command: CommandValues,
    #[serde(default)]
    with_subject_summary: bool,
}

impl ComponentSupplier for RunCommandSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::listener("command".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<RunCommandConfig>(props)?;
        let CommandValues(command) = config.command;
        Ok(Arc::new(RunCommand {
            command,
            with_subject_summary: config.with_subject_summary,
        }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        Some(Box::new(SdComponentMetadata {
            description: "Runs a configured command when processing events.".to_owned(),
            props_json_schema: Some(
                json!({"type":"object","properties":{"command":{"oneOf":[{"type":"array","items":{}},{"type":"object"}]},"withSubjectSummary":{"type":"boolean","default":false}},"required":["command"]}),
            ),
            props_ui_schema: None,
            state_json_schema: None,
            state_ui_schema: None,
            source_pointer_json_schema: None,
        }))
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(ProcessListener)]
pub struct RunCommand {
    command: Vec<String>,
    with_subject_summary: bool,
}

impl Display for RunCommand {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("command")
    }
}

impl RunCommand {
    pub fn run(
        &self,
        item_content: &ItemContent,
    ) -> Result<std::process::Output, ProcessingError> {
        let mut commands = self.command.clone();
        if self.with_subject_summary {
            commands.push(summary_content(item_content));
        }
        if commands.is_empty() {
            return Err(ProcessingError::non_retryable("Command is empty"));
        }
        tracing::debug!(command = %commands.join(" "), "Running command");
        let mut command = Command::new(&commands[0]);
        command.args(&commands[1..]).stderr(Stdio::inherit());
        command
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| ProcessingError::non_retryable(error.to_string()))?
            .wait_with_output()
            .map_err(|error| ProcessingError::non_retryable(error.to_string()))
    }
}

impl ProcessListener for RunCommand {
    fn on_item_success(
        &self,
        _: &dyn ProcessContext,
        item_content: &ItemContent,
    ) -> Result<(), ProcessingError> {
        let output = self.run(item_content)?;
        let mut stdout = output.stdout.as_slice();
        if !output.status.success() {
            let mut result = Vec::new();
            stdout.read_to_end(&mut result)?;
            tracing::warn!(
                exit_code = ?output.status.code(),
                result = %String::from_utf8_lossy(&result),
                "Command completed with a non-zero exit code"
            );
        }
        if tracing::enabled!(tracing::Level::DEBUG) {
            let mut result = Vec::new();
            stdout.read_to_end(&mut result)?;
            tracing::debug!(result = %String::from_utf8_lossy(&result), "Command result");
        }
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

fn value_to_string(value: &Value) -> String {
    value.as_str().map(str::to_owned).unwrap_or_else(|| value.to_string())
}

fn summary_content(item_content: &ItemContent) -> String {
    if item_content.file_contents.len() == 1
        && matches!(
            item_content.file_contents[0].status,
            FileContentStatus::Normal
                | FileContentStatus::Replaced
                | FileContentStatus::Replace
        )
        && let Some(name) = item_content.file_contents[0].target_path().file_name()
    {
        return format!("{} 处理完成", name.to_string_lossy());
    }

    let has_warning = item_content.file_contents.iter().any(|file| {
        matches!(
            file.status,
            FileContentStatus::VariableError
                | FileContentStatus::TargetExists
                | FileContentStatus::FileConflict
        )
    });
    if has_warning {
        let mut groups: Vec<(FileContentStatus, usize)> = Vec::new();
        for file in item_content.file_contents {
            let status = file.status.clone();
            if let Some((_, count)) =
                groups.iter_mut().find(|(known, _)| *known == status)
            {
                *count += 1;
            } else {
                groups.push((status, 1));
            }
        }
        let status_summary = groups
            .iter()
            .map(|(status, count)| format!("{}:{}个", status_name(status), count))
            .collect::<Vec<_>>()
            .join(",");
        return format!(
            "{}内的{}个文件处理完成 {}",
            item_content.source_item.title,
            item_content.file_contents.len(),
            status_summary
        );
    }

    format!(
        "{}内的{}个文件处理完成",
        item_content.source_item.title,
        item_content.file_contents.len()
    )
}

fn status_name(status: &FileContentStatus) -> &'static str {
    match status {
        FileContentStatus::Undetected => "UNDETECTED",
        FileContentStatus::Normal => "NORMAL",
        FileContentStatus::Downloaded => "DOWNLOADED",
        FileContentStatus::VariableError => "VARIABLE_ERROR",
        FileContentStatus::TargetExists => "TARGET_EXISTS",
        FileContentStatus::FileConflict => "FILE_CONFLICT",
        FileContentStatus::ReadyReplace => "READY_REPLACE",
        FileContentStatus::Replaced => "REPLACED",
        FileContentStatus::Replace => "REPLACE",
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use source_downloader_sdk::component::FileContent;
    use source_downloader_sdk::storage::ProcessingStatus;
    use std::collections::HashMap;

    #[test]
    fn supplier_rejects_scalar_commands() {
        let props = serde_json::json!({"command": "echo"}).as_object().unwrap().clone();

        let error = SUPPLIER
            .apply(
                &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                &props,
            )
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "Invalid configuration at 'command': invalid type: string \"echo\", expected an array or object"
        );
    }

    #[test]
    fn summary_reports_a_single_completed_file() {
        let source_item =
            SourceItem { title: String::from("episode"), ..SourceItem::default() };
        let file = FileContent {
            status: FileContentStatus::Normal,
            target_filename: String::from("episode.txt"),
            ..Default::default()
        };
        let files = vec![file];
        let variables = HashMap::new();
        let item_content = ItemContent {
            source_item: &source_item,
            file_contents: &files,
            item_variables: &variables,
            status: ProcessingStatus::Renamed,
        };

        assert_eq!(summary_content(&item_content), "episode.txt 处理完成");
    }
}
