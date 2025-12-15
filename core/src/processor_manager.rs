use crate::config::ProcessorConfig;
use crate::source_processor::{ProcessorOptions, SourceProcessor};
use parking_lot::RwLock;
use sdk::storage::ProcessingStorage;
use sdk::component::{ComponentError, ComponentRootType};
use std::collections::{HashMap, HashSet};
use std::ops::Not;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use crate::component_manager::ComponentManager;

pub struct ProcessorManager {
    component_manager: Arc<ComponentManager>,
    _processing_storage: Arc<dyn ProcessingStorage>,
    processor_wrappers: RwLock<HashMap<String, Arc<ProcessorWrapper>>>,
}

impl ProcessorManager {
    pub fn new(
        component_manager: Arc<ComponentManager>,
        processing_storage: Arc<dyn ProcessingStorage>,
    ) -> Self {
        Self {
            component_manager,
            _processing_storage: processing_storage,
            processor_wrappers: RwLock::new(HashMap::new()),
        }
    }

    pub fn create_processor(&self, config: &ProcessorConfig) {
        // TODO 补全所有
        if config.enabled.not() {
            info!("Processor[disabled] {}", config.name);
            return;
        }
        let processor_wrapper = match self.create_internal(config) {
            Ok(p) => p,
            Err(err) => {
                error!("Failed to create processor {}, cause: {}", config.name, err);
                self.processor_wrappers.write().insert(
                    config.name.to_owned(),
                    Arc::new(ProcessorWrapper {
                        name: config.name.to_owned(),
                        processor: None,
                        error_message: Some(err.message),
                    }),
                );
                return;
            }
        };
        self.register_task(config, processor_wrapper);
    }

    fn register_task(&self, config: &ProcessorConfig, processor_wrapper: Arc<ProcessorWrapper>) {
        let processor_task = processor_wrapper.processor.as_ref().unwrap();
        for component_ref in config.triggers.iter() {
            let id = &ComponentRootType::Trigger.parse_component_id(component_ref);
            let trigger_wrapper = match self.component_manager.get_component(id) {
                Ok(w) => w,
                Err(e) => {
                    warn!(
                        "Processor {} using a error trigger: {} will not add run task, cause: {}",
                        config.name, component_ref, e
                    );
                    continue;
                }
            };

            let component = match trigger_wrapper.get_and_mark_ref(&config.name) {
                None => {
                    error!(
                        "Trigger {} state not expected, it may be a bug",
                        component_ref
                    );
                    continue;
                }
                Some(p) => p,
            };
            match component.as_trigger() {
                Ok(x) => {
                    x.add_task(processor_task.clone());
                    info!("Processor[task-added] {} {}", config.name, component_ref);
                }
                Err(e) => {
                    error!("Trigger {} is not a trigger, cause: {}", component_ref, e);
                }
            }
        }
    }

    fn create_internal(
        &self,
        config: &ProcessorConfig,
    ) -> Result<Arc<ProcessorWrapper>, ComponentError> {
        let source_id = ComponentRootType::Source.parse_component_id(&config.source);
        let source = self
            .component_manager
            .get_component(&source_id)?
            .get_component()?
            .as_source()?;
        let processor = SourceProcessor {
            name: config.name.to_owned(),
            source_id: config.source.to_owned(),
            save_path: config.save_path.to_owned(),
            source: source.clone(),
            options: ProcessorOptions {
                save_path_pattern: "".to_owned(),
                filename_pattern: "".to_owned(),
                variable_providers: vec![],
            },
        };
        let wrapper = Arc::new(ProcessorWrapper {
            name: config.name.to_owned(),
            processor: Some(Arc::new(processor)),
            error_message: None,
        });
        self.processor_wrappers
            .write()
            .insert(config.name.to_owned(), wrapper.clone());
        info!("Processor[created] {}", config.name);
        Ok(wrapper)
    }

    pub fn get_processor(&self, name: &str) -> Option<Arc<ProcessorWrapper>> {
        self.processor_wrappers.read().get(name).cloned()
    }

    pub fn processor_exists(&self, name: &str) -> bool {
        self.processor_wrappers.read().contains_key(name)
    }

    pub fn destroy_processor(&self, name: &str) {
        let removed = self.processor_wrappers.write().remove(name);
        info!("Processor[destroying] {}", name);
        let Some(wrapper) = removed else { return };
        debug!(
            "ProcessorWp[on-destroy-arc] {}",
            Arc::strong_count(&wrapper)
        );
        let Some(processor) = &wrapper.processor else {
            return;
        };
        let triggers = self.component_manager.get_all_trigger();
        for trigger in triggers {
            let task = processor.clone();
            trigger.remove_task(task);
        }
        debug!("Processor[on-destroy-arc] {}", Arc::strong_count(processor));
    }

    pub fn get_all_processor_names(&self) -> HashSet<String> {
        self.processor_wrappers.read().keys().cloned().collect()
    }
}

pub struct ProcessorWrapper {
    pub name: String,
    pub processor: Option<Arc<SourceProcessor>>,
    pub error_message: Option<String>,
}

impl Drop for ProcessorWrapper {
    fn drop(&mut self) {
        debug!("ProcessorWp[dropped] {}", self.name);
    }
}

#[cfg(test)]
mod test {
    use crate::components::system_file_source::SUPPLIER;
    use crate::config::{ProcessorConfig, YamlConfigOperator};
    use crate::processor_manager::ProcessorManager;
    use std::sync::Arc;
    use storage_memory::MemoryProcessingStorage;
    use crate::component_manager::ComponentManager;

    #[test]
    fn normal_cases() {
        let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
        let component_manager = ComponentManager::new(Arc::new(YamlConfigOperator::new(
            "./tests/resources/config.yaml",
        )));
        let _ = component_manager.register_supplier(Arc::new(SUPPLIER));
        let manager = ProcessorManager::new(
            Arc::new(component_manager),
            Arc::new(MemoryProcessingStorage::new()),
        );
        let name = "normal-case";
        manager.create_processor(&ProcessorConfig {
            name: name.to_string(),
            enabled: true,
            source: "system-file:test".to_string(),
            triggers: vec![],
            save_path: "./tests/resources/output".to_string(),
        });
        assert!(manager.processor_exists(name));
        let processor_wp = manager.get_processor(name);
        assert!(processor_wp.is_some());
        assert!(processor_wp.as_ref().unwrap().error_message.is_none());
        assert!(processor_wp.as_ref().unwrap().processor.is_some());
        manager.destroy_processor(name);
        assert!(!manager.processor_exists(name));
    }

    #[test]
    fn create_processor_given_error_component() {
        let component_manager = ComponentManager::new(Arc::new(YamlConfigOperator::new(
            "./tests/resources/config.yaml",
        )));
        let manager = ProcessorManager::new(
            Arc::new(component_manager),
            Arc::new(MemoryProcessingStorage::new()),
        );

        let name = "normal-case";
        manager.create_processor(&ProcessorConfig {
            name: name.to_string(),
            enabled: true,
            triggers: vec![],
            source: "system-file:not-exists".to_string(),
            save_path: "./tests/resources/output".to_string(),
        });
        let processor_wp = manager.get_processor(name);
        assert!(processor_wp.is_some());
        assert!(processor_wp.unwrap().error_message.is_some());
    }
}
