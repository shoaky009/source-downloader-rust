use crate::config::ProcessorConfig;
use crate::source_processor::SourceProcessor;
use crate::{ComponentManager, ProcessorOptions};
use parking_lot::RwLock;
use sdk::ProcessingStorage;
use sdk::component::{ComponentError, ComponentType};
use std::collections::{HashMap, HashSet};
use std::ops::Not;
use std::sync::Arc;
use tracing::{error, info};

pub struct ProcessorManager {
    component_manager: Arc<RwLock<ComponentManager>>,
    _processing_storage: Arc<dyn ProcessingStorage>,
    processor_wrappers: RwLock<HashMap<String, Arc<ProcessorWrapper>>>,
}

impl ProcessorManager {
    pub fn new(
        component_manager: Arc<RwLock<ComponentManager>>,
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
            info!("Processor {} is disabled", config.name);
            return;
        }
        if let Err(err) = self.create(config) {
            error!("Processor {} create failed: {}", config.name, err);
            self.processor_wrappers.write().insert(
                config.name.to_owned(),
                Arc::new(ProcessorWrapper {
                    name: config.name.to_owned(),
                    processor: None,
                    error_message: Some(err.message),
                }),
            );
        }
    }

    fn create(&self, config: &ProcessorConfig) -> Result<Arc<ProcessorWrapper>, ComponentError> {
        let component_ref_pat = ":";
        let source_id = config
            .source
            .split(component_ref_pat)
            .collect::<Vec<&str>>();
        let source_type_name = source_id.first().unwrap().to_string();
        let source_name = source_id.last().unwrap();
        let source_type = &ComponentType::source(source_type_name);
        let source = self
            .component_manager
            .read()
            .get_component(source_type, source_name)?
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
            processor: Some(processor),
            error_message: None,
        });
        self.processor_wrappers
            .write()
            .insert(config.name.to_owned(), wrapper.clone());
        Ok(wrapper)
    }

    pub fn get_processor(&self, name: &str) -> Option<Arc<ProcessorWrapper>> {
        self.processor_wrappers.read().get(name).cloned()
    }

    pub fn processor_exists(&self, name: &str) -> bool {
        self.processor_wrappers.read().contains_key(name)
    }

    pub fn destroy_processor(&mut self, name: &str) {
        let removed = self.processor_wrappers.write().remove(name);
        info!("Processor:'{}' destroying", name);
        let Some(wrapper) = removed else { return };
        let Some(processor) = &wrapper.processor else {
            return;
        };
        let triggers = self.component_manager.read().get_all_trigger();
        for trigger in triggers {
            let task = processor.safe_task();
            trigger.remove_task(task);
        }
        processor.close();
    }

    pub fn get_all_processor_names(&self) -> HashSet<String> {
        self.processor_wrappers.read().keys().cloned().collect()
    }
}

pub struct ProcessorWrapper {
    pub name: String,
    pub processor: Option<SourceProcessor>,
    pub error_message: Option<String>,
}

#[cfg(test)]
mod test {
    use crate::components::system_file_source::SystemFileSourceSupplier;
    use crate::config::ProcessorConfig;
    use crate::processor_manager::ProcessorManager;
    use crate::{ComponentManager, YamlConfigOperator};
    use parking_lot::lock_api::RwLock;
    use std::sync::Arc;
    use storage_memory::MemoryProcessingStorage;

    #[test]
    fn normal_cases() {
        let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
        let mut component_manager = ComponentManager::new(Arc::new(YamlConfigOperator::new(
            "./tests/resources/config.yaml",
        )));
        let _ = component_manager.register_supplier(Arc::new(SystemFileSourceSupplier {}));
        let mut manager = ProcessorManager::new(
            Arc::new(RwLock::new(component_manager)),
            Arc::new(MemoryProcessingStorage::new()),
        );
        let name = "normal-case";
        manager.create_processor(&ProcessorConfig {
            name: name.to_string(),
            enabled: true,
            source: "system-file:test".to_string(),
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
            Arc::new(RwLock::new(component_manager)),
            Arc::new(MemoryProcessingStorage::new()),
        );

        let name = "normal-case";
        manager.create_processor(&ProcessorConfig {
            name: name.to_string(),
            enabled: true,
            source: "system-file:not-exists".to_string(),
            save_path: "./tests/resources/output".to_string(),
        });
        let processor_wp = manager.get_processor(name);
        assert!(processor_wp.is_some());
        assert!(processor_wp.unwrap().error_message.is_some());
    }
}
