use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileContentStatus, ItemContent,
    ProcessContext, ProcessListener, ProcessingError, SdComponent, SdComponentMetadata,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::Arc;

pub struct RunCommandSupplier;
pub const SUPPLIER: RunCommandSupplier = RunCommandSupplier;

impl ComponentSupplier for RunCommandSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::listener("command".to_owned())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let raw_command = props
            .get("command")
            .ok_or_else(|| ComponentError::from("Missing 'command' property"))?;
        let command = match raw_command {
            Value::Array(values) => values.iter().map(value_to_string).collect(),
            Value::Object(values) => values.values().map(value_to_string).collect(),
            _ => {
                return Err(ComponentError::from("'command' must be an array or object"));
            }
        };
        let with_subject_summary = match props.get("withSubjectSummary") {
            None => false,
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(ComponentError::from(
                    "'withSubjectSummary' must be a boolean",
                ));
            }
        };
        Ok(Arc::new(RunCommand { command, with_subject_summary }))
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
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

        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &props,
                )
                .is_err()
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
