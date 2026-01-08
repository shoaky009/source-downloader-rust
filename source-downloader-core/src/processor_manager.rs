use crate::component_manager::ComponentManager;
use crate::components::expression_file_content_filter::ExpressionFileContentFilter;
use crate::components::expression_item_content_filter::ExpressionItemContentFilter;
use crate::components::expression_item_filter::ExpressionItemFilter;
use crate::components::source_item_identity_filter::SourceItemIdentityFilter;
use crate::config::{ProcessorConfig, ProcessorOptionConfig};
use crate::expression::cel::FACTORY;
use crate::expression::CompiledExpressionFactory;
use crate::process::file::PathPattern;
use crate::process::rule::{
    ExpressionAndTagMatcher, FileRule, FileStrategy, ItemRule, ItemStrategy,
};
use crate::process::variable::{AnyStrategy, SmartStrategy, VariableAggregation, VoteStrategy};
use crate::source_processor::{ProcessorOptions, SourceProcessor};
use parking_lot::RwLock;
use source_downloader_sdk::component::{
    ComponentError, ComponentRootType, FileContentFilter, FileTagger, ItemContentFilter,
    SourceFileFilter, SourceItemFilter, VariableProvider,
};
use source_downloader_sdk::storage::ProcessingStorage;
use std::collections::{HashMap, HashSet};
use std::ops::Not;
use std::path::Path;
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

        let b = config
            .options
            .task_group
            .clone()
            .or(source.group())
            .unwrap_or_else(|| source_id.component_type.name);
        let processor = SourceProcessor::new(
            config.name.to_owned(),
            config.source.to_owned(),
            Path::new(&config.save_path).into(),
            source.to_owned(),
            item_file_resolver.to_owned(),
            downloader.to_owned(),
            file_mover.to_owned(),
            self.processing_storage.to_owned(),
            config.category.to_owned(),
            config.tags.to_owned(),
            self.create_options(&config, b)?,
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

    fn create_options(
        &self,
        config: &ProcessorConfig,
        group: String,
    ) -> Result<ProcessorOptions, ComponentError> {
        let opt = &config.options;
        let mut item_filters: Vec<Arc<dyn SourceItemFilter>> = vec![];
        if !opt.item_expression_exclusions.is_empty() || !opt.item_expression_inclusions.is_empty()
        {
            let filter = Self::apply_item_expression(
                &opt.item_expression_exclusions,
                &opt.item_expression_inclusions,
            )?;
            item_filters.push(Arc::new(filter));
        }

        for x in &opt.item_filters {
            let component_id = ComponentRootType::SourceItemFilter.parse_component_id(&x);
            item_filters.push(
                self.component_manager
                    .get_component(&component_id)?
                    .require_component()?
                    .as_source_item_filter()?
                    .clone(),
            );
        }

        // ===
        let mut source_file_filters: Vec<Arc<dyn SourceFileFilter>> = vec![];
        for x in &opt.source_file_filters {
            let component_id = ComponentRootType::SourceFileFilter.parse_component_id(&x);
            source_file_filters.push(
                self.component_manager
                    .get_component(&component_id)?
                    .require_component()?
                    .as_source_file_filter()?
                    .clone(),
            );
        }

        // ===
        let mut variable_providers: Vec<Arc<dyn VariableProvider>> = vec![];
        for x in &opt.variable_providers {
            let component_id = ComponentRootType::VariableProvider.parse_component_id(&x);
            variable_providers.push(
                self.component_manager
                    .get_component(&component_id)?
                    .require_component()?
                    .as_variable_provider()?
                    .clone(),
            );
        }

        let identity_filter = Arc::new(SourceItemIdentityFilter {
            processor_name: config.name.clone(),
            storage: self.processing_storage.clone(),
        });
        if opt.save_processing_content {
            item_filters.push(identity_filter.clone())
        }

        // ===
        let mut file_taggers: Vec<Arc<dyn FileTagger>> = vec![];
        for x in &opt.file_taggers {
            let component_id = ComponentRootType::FileTagger.parse_component_id(&x);
            file_taggers.push(
                self.component_manager
                    .get_component(&component_id)?
                    .require_component()?
                    .as_file_tagger()?
                    .clone(),
            );
        }

        // ===
        let mut file_content_filters: Vec<Arc<dyn FileContentFilter>> = vec![];
        if !opt.file_content_expression_inclusions.is_empty()
            || !opt.file_content_expression_inclusions.is_empty()
        {
            let filter = Self::apply_file_content_expression(
                &opt.file_content_expression_inclusions,
                &opt.file_content_expression_inclusions,
            )?;
            file_content_filters.push(Arc::new(filter));
        }

        for x in &opt.file_content_filters {
            let component_id = ComponentRootType::FileContentFilter.parse_component_id(&x);
            file_content_filters.push(
                self.component_manager
                    .get_component(&component_id)?
                    .require_component()?
                    .as_file_content_filter()?
                    .clone(),
            );
        }
        // ===

        let mut item_content_filters: Vec<Arc<dyn ItemContentFilter>> = vec![];
        if !opt.item_content_expression_inclusions.is_empty()
            || !opt.item_content_expression_inclusions.is_empty()
        {
            let filter = Self::apply_item_content_expression(
                &opt.item_content_expression_inclusions,
                &opt.item_content_expression_inclusions,
            )?;
            item_content_filters.push(Arc::new(filter));
        }

        for x in &opt.file_content_filters {
            let component_id = ComponentRootType::ItemContentFilter.parse_component_id(&x);
            item_content_filters.push(
                self.component_manager
                    .get_component(&component_id)?
                    .require_component()?
                    .as_item_content_filter()?
                    .clone(),
            );
        }

        Ok(ProcessorOptions {
            save_path_pattern: Arc::new(PathPattern::new_cel(
                config.options.save_path_pattern.to_owned(),
            )),
            filename_pattern: Arc::new(PathPattern::new_cel(
                config.options.filename_pattern.to_owned(),
            )),
            variable_providers,
            item_filters,
            item_content_filters,
            file_content_filters,
            source_file_filters,
            file_taggers,
            variable_aggregation: VariableAggregation::new(
                match &opt.variable_conflict_strategy {
                    None => Box::new(SmartStrategy),
                    Some(s) => match s.as_str() {
                        "ANY" => Box::new(AnyStrategy),
                        "VOTE" => Box::new(VoteStrategy),
                        _ => Box::new(SmartStrategy),
                    },
                },
                opt.variable_name_replace.to_owned(),
            ),
            save_processing_content: config.options.save_processing_content.to_owned(),
            rename_task_interval: humantime::parse_duration(&config.options.rename_task_interval)
                .map_err(|e| e.to_string())?,
            rename_times_threshold: config.options.rename_times_threshold.to_owned(),
            parallelism: config.options.parallelism.to_owned(),
            task_group: Some(group),
            fetch_limit: config.options.fetch_limit.to_owned(),
            item_error_continue: config.options.item_error_continue,
            pointer_batch_mode: config.options.pointer_batch_mode,
            item_rules: self.apply_item_grouping(config, opt, identity_filter)?,
            file_rules: self.apply_file_grouping(config, opt)?,
        })
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

    fn apply_item_expression(
        exclusions: &[String],
        inclusions: &[String],
    ) -> Result<ExpressionItemFilter, ComponentError> {
        let exclusions = exclusions
            .iter()
            .map(|x| FACTORY.create(x))
            .collect::<Result<Vec<_>, _>>()?;
        let inclusions = inclusions
            .iter()
            .map(|x| FACTORY.create(x))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ExpressionItemFilter::new(exclusions, inclusions))
    }

    fn apply_item_content_expression(
        exclusions: &[String],
        inclusions: &[String],
    ) -> Result<ExpressionItemContentFilter, ComponentError> {
        let exclusions = exclusions
            .iter()
            .map(|x| FACTORY.create(x))
            .collect::<Result<Vec<_>, _>>()?;
        let inclusions = inclusions
            .iter()
            .map(|x| FACTORY.create(x))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ExpressionItemContentFilter::new(exclusions, inclusions))
    }

    fn apply_file_content_expression(
        exclusions: &[String],
        inclusions: &[String],
    ) -> Result<ExpressionFileContentFilter, ComponentError> {
        let exclusions = exclusions
            .iter()
            .map(|x| FACTORY.create(x))
            .collect::<Result<Vec<_>, _>>()?;
        let inclusions = inclusions
            .iter()
            .map(|x| FACTORY.create(x))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ExpressionFileContentFilter::new(exclusions, inclusions))
    }

    fn apply_item_grouping(
        &self,
        cfg: &ProcessorConfig,
        opt: &ProcessorOptionConfig,
        identity_filter: Arc<SourceItemIdentityFilter>,
    ) -> Result<Vec<ItemRule>, ComponentError> {
        let mut result = vec![];
        for item_opt_cfg in opt.item_grouping.iter() {
            // ====
            let expression_filters = if item_opt_cfg.item_expression_inclusions.is_some()
                || item_opt_cfg.item_expression_exclusions.is_some()
            {
                let exclusions = item_opt_cfg
                    .item_expression_exclusions
                    .as_deref()
                    .unwrap_or_default();
                let inclusions = item_opt_cfg
                    .item_expression_inclusions
                    .as_deref()
                    .unwrap_or_default();
                let filter = Self::apply_item_expression(exclusions, inclusions)?;
                Some(vec![Arc::new(filter) as Arc<dyn SourceItemFilter>])
            } else {
                None
            };

            // ===
            let source_item_filters =
                if let Some(ref filter_names) = item_opt_cfg.source_item_filters {
                    let mut filters = Vec::new();
                    for name in filter_names {
                        let cid = ComponentRootType::SourceItemFilter.parse_component_id(name);
                        let wp = self.component_manager.get_component(&cid)?;

                        let filter = wp.require_component()?.as_source_item_filter()?;
                        filters.push(filter);
                        wp.get_and_mark_ref(cfg.name.to_owned());
                    }
                    Some(filters)
                } else {
                    None
                };

            let mut item_filters = if expression_filters.is_some() || source_item_filters.is_some()
            {
                let mut filters = Vec::new();
                filters.extend(expression_filters.unwrap_or_default());
                filters.extend(source_item_filters.unwrap_or_default());
                Some(filters)
            } else {
                None
            };
            // ===

            let providers = if let Some(ref provider_names) = item_opt_cfg.variable_providers {
                let mut providers = Vec::new();
                for name in provider_names {
                    let cid = ComponentRootType::VariableProvider.parse_component_id(name);
                    let wp = self.component_manager.get_component(&cid)?;
                    let provider = wp.require_component()?.as_variable_provider()?;
                    providers.push(provider);
                    wp.get_and_mark_ref(cfg.name.to_owned());
                }
                Some(providers)
            } else {
                None
            };

            if opt.save_processing_content {
                if let Some(filters) = item_filters.as_mut() {
                    filters.push(identity_filter.clone());
                }
            }
            // ===
            let expression_matching = item_opt_cfg
                .expression_matching
                .as_ref()
                .map(|x| FACTORY.create(&x))
                .transpose()?;
            let matcher =
                ExpressionAndTagMatcher::new(expression_matching, item_opt_cfg.tags.to_owned());

            let strategy = ItemStrategy {
                save_path_pattern: item_opt_cfg
                    .save_path_pattern
                    .as_ref()
                    .map(|x| Arc::new(PathPattern::new_cel(x.clone()))),
                filename_pattern: item_opt_cfg
                    .filename_pattern
                    .as_ref()
                    .map(|x| Arc::new(PathPattern::new_cel(x.clone()))),
                item_filters,
                variable_providers: providers,
            };
            result.push(ItemRule {
                matcher: Box::new(matcher),
                strategy,
            })
        }
        Ok(result)
    }

    fn apply_file_grouping(
        &self,
        cfg: &ProcessorConfig,
        opt: &ProcessorOptionConfig,
    ) -> Result<Vec<FileRule>, ComponentError> {
        let mut result = vec![];
        for file_opt_cfg in opt.file_grouping.iter() {
            // ====
            let expression_filters = if file_opt_cfg.file_content_expression_inclusions.is_some()
                || file_opt_cfg.file_content_expression_exclusions.is_some()
            {
                let exclusions = file_opt_cfg
                    .file_content_expression_exclusions
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|x| FACTORY.create(x))
                    .collect::<Result<Vec<_>, _>>()?;

                let inclusions = file_opt_cfg
                    .file_content_expression_inclusions
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|x| FACTORY.create(x))
                    .collect::<Result<Vec<_>, _>>()?;
                let filter = ExpressionFileContentFilter::new(exclusions, inclusions);
                Some(vec![Arc::new(filter) as Arc<dyn FileContentFilter>])
            } else {
                None
            };

            // ===
            let file_content_filters =
                if let Some(ref filter_names) = file_opt_cfg.file_content_filters {
                    let mut filters = Vec::new();
                    for name in filter_names {
                        let cid = ComponentRootType::FileContentFilter.parse_component_id(name);
                        let wp = self.component_manager.get_component(&cid)?;

                        let filter = wp.require_component()?.as_file_content_filter()?;
                        filters.push(filter);
                        wp.get_and_mark_ref(cfg.name.to_owned());
                    }
                    Some(filters)
                } else {
                    None
                };

            let file_content_filters =
                if expression_filters.is_some() || file_content_filters.is_some() {
                    let mut filters = Vec::new();
                    filters.extend(expression_filters.unwrap_or_default());
                    filters.extend(file_content_filters.unwrap_or_default());
                    Some(filters)
                } else {
                    None
                };
            // ===
            let expression_matching = file_opt_cfg
                .expression_matching
                .as_ref()
                .map(|x| FACTORY.create(&x))
                .transpose()?;
            let matcher =
                ExpressionAndTagMatcher::new(expression_matching, file_opt_cfg.tags.to_owned());

            let strategy = FileStrategy {
                save_path_pattern: None,
                filename_pattern: None,
                file_content_filters,
            };
            result.push(FileRule {
                matcher: Box::new(matcher),
                strategy,
            })
        }
        Ok(result)
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
    use std::collections::HashSet;
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
            category: None,
            tags: HashSet::new(),
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
            category: None,
            tags: HashSet::new(),
        });
        let processor_wp = manager.get_processor(name);
        assert!(processor_wp.is_some());
        assert!(processor_wp.unwrap().error_message.is_some());
    }
}
