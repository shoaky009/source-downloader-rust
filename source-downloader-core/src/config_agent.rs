//! Reviewed configuration plans. Validation never constructs runtime components.
use crate::application::CoreApplication;
use crate::config::{ComponentConfig, InstanceConfig, ProcessorConfig};
use crate::expression::{CompiledExpressionFactory, cel::FACTORY};
use crate::processor_manager::processor_component_ids;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use source_downloader_sdk::component::{ComponentId, ComponentRootType, ComponentType};
use std::collections::HashSet;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlannedInstance {
    pub name: String,
    pub factory_type: String,
    pub props: Map<String, Value>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlannedComponent {
    pub root_type: ComponentRootType,
    pub type_name: String,
    pub name: String,
    pub props: Map<String, Value>,
}

impl PlannedComponent {
    pub fn id(&self) -> ComponentId {
        ComponentId::new(
            ComponentType {
                root_type: self.root_type.clone(),
                name: self.type_name.clone(),
            },
            &self.name,
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigurationPlan {
    pub summary: String,
    pub instances: Vec<PlannedInstance>,
    pub components: Vec<PlannedComponent>,
    pub processors: Vec<Value>,
}

pub fn configuration_snapshot(core: &CoreApplication) -> Value {
    json!({
        "instances": core.config_operator.get_all_instance_config(),
        "components": core.config_operator.get_all_component_config(),
        "processors": core.config_operator.get_all_processor_config(),
    })
}

fn processor(value: &Value) -> Result<ProcessorConfig, String> {
    let text = value.to_string();
    let mut ignored = Vec::new();
    let mut deserializer = serde_json::Deserializer::from_str(&text);
    let config = serde_ignored::deserialize(&mut deserializer, |path| {
        ignored.push(path.to_string());
    })
    .map_err(|e| e.to_string())?;
    if !ignored.is_empty() {
        return Err(format!("Unknown processor fields: {}", ignored.join(", ")));
    }
    Ok(config)
}

pub fn validate_schema(schema: Option<&Value>, value: &Value) -> Result<(), String> {
    if let Some(schema) = schema {
        let mut schema = schema.clone();
        if let Some(object) = schema.as_object_mut()
            && object.contains_key("properties")
            && !object.contains_key("additionalProperties")
        {
            object.insert("additionalProperties".into(), json!(false));
        }
        let validator = jsonschema::validator_for(&schema).map_err(|e| e.to_string())?;
        let errors: Vec<_> = validator
            .iter_errors(value)
            .map(|e| format!("{}: {}", e.instance_path(), e))
            .collect();
        if !errors.is_empty() {
            return Err(errors.join("; "));
        }
    }
    Ok(())
}

fn valid_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() || name.contains(':') {
        return Err("Names must be nonempty and must not contain ':'".into());
    }
    Ok(())
}

impl ConfigurationPlan {
    pub fn normalize(&mut self) {
        for value in &mut self.processors {
            if let Some(object) = value.as_object_mut() {
                object.insert("enabled".into(), json!(true));
            }
        }
    }

    pub fn validate(&self, core: &CoreApplication) -> Result<(), String> {
        if self.processors.is_empty() {
            return Err("A plan must include at least one processor".into());
        }
        if self.instances.len() + self.components.len() + self.processors.len() > 100 {
            return Err("A plan may contain at most 100 resources".into());
        }
        let mut names = HashSet::new();
        for instance in &self.instances {
            valid_name(&instance.name)?;
            if !names.insert(instance.name.clone()) {
                return Err(format!("Duplicate instance: {}", instance.name));
            }
            let factory = core
                .instance_manager
                .get_factory_by_name(&instance.factory_type)
                .ok_or_else(|| {
                    format!("Unknown instance factory: {}", instance.factory_type)
                })?;
            let metadata = factory.get_metadata();
            validate_schema(
                metadata.as_ref().and_then(|m| m.props_json_schema.as_ref()),
                &json!(instance.props),
            )?;
        }
        names.clear();
        let mut supplied = HashSet::new();
        for component in &self.components {
            valid_name(&component.name)?;
            // YAML storage keys components by root type and name, not supplier type.
            if !names.insert(format!("{}:{}", component.root_type, component.name)) {
                return Err(format!("Duplicate component: {}", component.id().display()));
            }
            let supplier = core
                .component_manager
                .get_supplier(&component.id().component_type)
                .ok_or_else(|| {
                    format!("Unknown component: {}", component.id().display())
                })?;
            let metadata = supplier.get_metadata();
            validate_schema(
                metadata.as_ref().and_then(|m| m.props_json_schema.as_ref()),
                &json!(component.props),
            )?;
            for ty in supplier.supply_types() {
                if !supplied.insert(ComponentId::new(ty, &component.name)) {
                    return Err(format!("Component aliases overlap: {}", component.name));
                }
            }
        }
        names.clear();
        for value in &self.processors {
            let config = processor(value)?;
            valid_name(&config.name)?;
            if !config.enabled || config.save_path.trim().is_empty() {
                return Err(
                    "Processors must be enabled and have a destination path".into()
                );
            }
            if !names.insert(config.name.clone()) {
                return Err(format!("Duplicate processor: {}", config.name));
            }
            for id in processor_component_ids(&config) {
                let supplier =
                    core.component_manager.get_supplier(&id.component_type).ok_or_else(
                        || format!("Unknown component reference: {}", id.display()),
                    )?;
                let exists = supplier.supply_types().iter().any(|ty| {
                    core.config_operator.get_component_config(ty, &id.name).is_some()
                });
                if !supplied.contains(&id) && !exists && !supplier.is_support_no_props() {
                    return Err(format!(
                        "Missing component configuration: {}",
                        id.display()
                    ));
                }
            }
            let report = core.processor_manager.validate_compatibility(&config);
            if !report.valid {
                return Err(report
                    .violations
                    .iter()
                    .map(|v| v.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; "));
            }
            validate_expressions(value, "")?;
        }
        Ok(())
    }

    /// Reject overwrites before review. Previously completed steps are handled by execution.
    pub fn check_new_resources(&self, core: &CoreApplication) -> Result<(), String> {
        let existing_instances = core.config_operator.get_all_instance_config();
        let existing_components = core.config_operator.get_all_component_config();
        for instance in &self.instances {
            if existing_instances.iter().any(|v| v.name == instance.name) {
                return Err(format!(
                    "Instance {} already exists; reuse it",
                    instance.name
                ));
            }
        }
        for component in &self.components {
            if existing_components
                .get(component.root_type.name())
                .is_some_and(|configs| configs.iter().any(|v| v.name == component.name))
            {
                return Err(format!(
                    "Component {} already exists; reuse it",
                    component.id().display()
                ));
            }
            let supplier = core
                .component_manager
                .get_supplier(&component.id().component_type)
                .ok_or_else(|| {
                    format!("Unknown component: {}", component.id().display())
                })?;
            let loaded = core.component_manager.get_all_component();
            if loaded.iter().any(|wrapper| {
                wrapper.id.name == component.name
                    && supplier.supply_types().contains(&wrapper.id.component_type)
            }) {
                return Err(format!(
                    "Component {} is already loaded; reuse it or choose a new name",
                    component.name
                ));
            }
            for ty in supplier.supply_types() {
                if core
                    .config_operator
                    .get_component_config(&ty, &component.name)
                    .is_some()
                {
                    return Err(format!(
                        "Shared component {} already exists; reuse it",
                        component.name
                    ));
                }
            }
        }
        for value in &self.processors {
            let config = processor(value)?;
            if core.config_operator.get_processor_config(&config.name).is_some()
                || core.processor_manager.processor_exists(&config.name)
            {
                return Err(format!(
                    "Processor {} already exists; choose a new name",
                    config.name
                ));
            }
        }
        Ok(())
    }

    pub fn steps(&self) -> Vec<String> {
        self.instances
            .iter()
            .map(|v| format!("instance:{}", v.name))
            .chain(
                self.components.iter().map(|v| format!("component:{}", v.id().display())),
            )
            .chain(
                self.processors.iter().map(|v| {
                    format!("processor:{}", v["name"].as_str().unwrap_or_default())
                }),
            )
            .collect()
    }

    /// Execute one reviewed operation; identical saved resources can be resumed after failure.
    pub fn execute_step(
        &self,
        core: &CoreApplication,
        index: usize,
        resume: bool,
    ) -> Result<(), String> {
        if let Some(instance) = self.instances.get(index) {
            let existing = core
                .config_operator
                .get_all_instance_config()
                .into_iter()
                .find(|v| v.name == instance.name);
            if let Some(existing) = existing {
                if resume && existing.props == instance.props {
                    return Ok(());
                }
                return Err(format!(
                    "Instance {} changed or already exists",
                    instance.name
                ));
            }
            core.instance_manager
                .validate_instance(&instance.factory_type, &instance.props)
                .map_err(|e| e.message)?;
            return core
                .config_operator
                .save_instance(InstanceConfig {
                    name: instance.name.clone(),
                    props: instance.props.clone(),
                })
                .map_err(|e| e.message);
        }
        let index = index - self.instances.len();
        if let Some(component) = self.components.get(index) {
            let id = component.id();
            let existing = core
                .config_operator
                .get_all_component_config()
                .get(component.root_type.name())
                .and_then(|configs| configs.iter().find(|v| v.name == component.name))
                .cloned();
            if let Some(existing) = existing {
                if !resume
                    || existing.component_type != component.type_name
                    || existing.props != component.props
                {
                    return Err(format!(
                        "Component {} changed or already exists",
                        id.display()
                    ));
                }
            } else {
                core.config_operator
                    .save_component(
                        &component.root_type,
                        ComponentConfig {
                            name: component.name.clone(),
                            component_type: component.type_name.clone(),
                            props: component.props.clone(),
                        },
                    )
                    .map_err(|e| e.message)?;
            }
            // A failed construction is cached by ComponentManager; retry it explicitly.
            if resume {
                core.component_manager.destroy(&id);
            }
            core.component_manager
                .get_component(&id)
                .and_then(|v| v.require_component())
                .map_err(|e| e.message)?;
            return Ok(());
        }
        let index = index - self.components.len();
        let config =
            processor(self.processors.get(index).ok_or("Invalid operation index")?)?;
        if let Some(existing) = core.config_operator.get_processor_config(&config.name)
            && (!resume || json!(existing) != json!(config))
        {
            return Err(format!("Processor {} changed or already exists", config.name));
        }
        for id in processor_component_ids(&config) {
            core.component_manager
                .get_component(&id)
                .and_then(|wrapper| wrapper.require_component())
                .map_err(|e| e.message)?;
        }
        let prepared =
            core.processor_manager.prepare_processor(&config).map_err(|e| e.message)?;
        core.config_operator.save_processor(config).map_err(|e| e.message)?;
        core.activate_processor(prepared)
    }
}

fn validate_expressions(value: &Value, key: &str) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                validate_expressions(value, key)?;
            }
        }
        Value::Array(values) => {
            for value in values {
                validate_expressions(value, key)?;
            }
        }
        Value::String(pattern) if key.ends_with("-pattern") => {
            for capture in
                crate::process::file::VARIABLE_PATTERN_REGEX.captures_iter(pattern)
            {
                let expression = capture
                    .name("normal")
                    .or_else(|| capture.name("optional"))
                    .ok_or("Invalid path template")?;
                FACTORY.create::<String>(expression.as_str())?;
            }
        }
        Value::String(duration)
            if matches!(key, "rename-task-interval" | "retry-backoff") =>
        {
            iso8601_duration::Duration::parse(duration)
                .map_err(|e| format!("Invalid duration {duration}: {e:?}"))?
                .to_std()
                .ok_or("Duration cannot contain years or months")?;
        }
        Value::String(expression) if key.contains("expression") => {
            FACTORY.create::<bool>(expression)?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_processor_fields_are_rejected() {
        let value = json!({"name":"test","save-path":"/tmp/test","source":"uri", "downloader":"http", "item-file-resolver":"url", "file-mover":"general", "options":{"invented-option":true}});
        assert!(processor(&value).unwrap_err().contains("invented-option"));
    }

    #[test]
    fn invalid_nested_expression_is_rejected() {
        assert!(validate_expressions(&json!({"options":{"file-grouping":[{"expression-matching":"file. =="}]}}), "").is_err());
    }

    #[test]
    fn schema_reports_invalid_props() {
        let schema = json!({"type":"object", "required":["path"], "properties":{"path":{"type":"string"}}});
        assert!(validate_schema(Some(&schema), &json!({"path": 2})).is_err());
    }
}
