use crate::component_manager::ComponentManager;
use crate::config::ProcessorConfig;
use crate::source_processor::{ProcessorOptions, SourceProcessor};
use parking_lot::RwLock;
use sdk::component::{ComponentError, ComponentRootType, VariableProvider};
use sdk::storage::ProcessingStorage;
use std::collections::{HashMap, HashSet};
use std::ops::Not;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub struct ProcessorManager {
    component_manager: Arc<ComponentManager>,
    processing_storage: Arc<dyn ProcessingStorage>,
    processor_wrappers: RwLock<HashMap<String, Arc<ProcessorWrapper>>>,
}

impl ProcessorManager {
    pub fn new(
        component_manager: Arc<ComponentManager>,
        processing_storage: Arc<dyn ProcessingStorage>,
    ) -> Self {
        Self {
            component_manager,
            processing_storage,
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

            let component = match trigger_wrapper.get_and_mark_ref(config.name.to_owned()) {
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
            .require_component()?
            .as_source()?;

        let mut variable_providers: Vec<Arc<dyn VariableProvider>> = vec![];
        for x in &config.options.variable_providers {
            let component_id = ComponentRootType::VariableProvider.parse_component_id(&x);
            variable_providers.push(
                self.component_manager
                    .get_component(&component_id)?
                    .require_component()?
                    .as_variable_provider()?
                    .clone(),
            );
        }

        let item_file_resolver = self
            .component_manager
            .get_component(
                &ComponentRootType::ItemFileResolver.parse_component_id(&config.item_file_resolver),
            )?
            .require_component()?
            .as_item_file_resolver()?;

        let downloader = self
            .component_manager
            .get_component(&ComponentRootType::Downloader.parse_component_id(&config.downloader))?
            .require_component()?
            .as_downloader()?;

        let file_mover = self
            .component_manager
            .get_component(&ComponentRootType::FileMover.parse_component_id(&config.file_mover))?
            .require_component()?
            .as_file_mover()?;

        let processor = SourceProcessor::new(
            config.name.to_owned(),
            config.source.to_owned(),
            config.save_path.to_owned(),
            source.clone(),
            item_file_resolver.clone(),
            downloader.clone(),
            file_mover.clone(),
            self.processing_storage.clone(),
            ProcessorOptions {
                save_path_pattern: config.options.save_path_pattern.to_owned(),
                filename_pattern: config.options.filename_pattern.to_owned(),
                variable_providers,
            },
        );
        let instance_id = processor.instance_id();
        let wrapper = Arc::new(ProcessorWrapper {
            name: config.name.to_owned(),
            processor: Some(Arc::new(processor)),
            error_message: None,
        });
        self.processor_wrappers
            .write()
            .insert(config.name.to_owned(), wrapper.clone());
        info!("Processor[created] {}({:?})", config.name, instance_id);
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
    use crate::component_manager::ComponentManager;
    use crate::components::get_build_in_component_supplier;
    use crate::config::{ProcessorConfig, ProcessorOptionConfig, YamlConfigOperator};
    use crate::processor_manager::ProcessorManager;
    use std::sync::Arc;
    use storage_memory::MemoryProcessingStorage;

    #[test]
    fn normal_cases() {
        let _ = tracing_subscriber::fmt().with_env_filter("info").try_init();
        let component_manager = ComponentManager::new(Arc::new(YamlConfigOperator::new(
            "./tests/resources/config.yaml",
        )));
        let _ = component_manager.register_suppliers(get_build_in_component_supplier());
        let manager = ProcessorManager::new(
            Arc::new(component_manager),
            Arc::new(MemoryProcessingStorage::new()),
        );
        let name = "normal-case";
        manager.create_processor(&ProcessorConfig {
            name: name.to_string(),
            enabled: true,
            triggers: vec![],
            source: "system-file:test".to_string(),
            item_file_resolver: "system-file:test".to_string(),
            downloader: "http".to_string(),
            file_mover: "system-file".to_string(),
            save_path: "./tests/resources/output".to_string(),
            options: ProcessorOptionConfig::default(),
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
            item_file_resolver: "system-file:test".to_string(),
            downloader: "http".to_string(),
            file_mover: "system-file".to_string(),
            save_path: "./tests/resources/output".to_string(),
            options: ProcessorOptionConfig::default(),
        });
        let processor_wp = manager.get_processor(name);
        assert!(processor_wp.is_some());
        assert!(processor_wp.unwrap().error_message.is_some());
    }
}
