use crate::component_manager::ComponentManager;
use crate::components::expression_file_content_filter::ExpressionFileContentFilter;
use crate::components::expression_item_content_filter::ExpressionItemContentFilter;
use crate::components::expression_item_filter::ExpressionItemFilter;
use crate::components::source_item_identity_filter::SourceItemIdentityFilter;
use crate::config::{ListenerMode, ProcessorConfig, ProcessorOptionConfig};
use crate::expression::CompiledExpressionFactory;
use crate::expression::cel::FACTORY;
use crate::process::file::{
    PathPattern, Renamer, VariableProcessChain, VariableProcessOutput,
};
use crate::process::rule::{
    ExpressionAndTagMatcher, FileRule, FileStrategy, ItemRule, ItemStrategy,
};
use crate::process::variable::{
    AnyStrategy, SmartStrategy, VariableAggregation, VoteStrategy,
};
use crate::source_processor::{ProcessorOptions, SourceProcessor};
use parking_lot::RwLock;
use source_downloader_sdk::component::{
    ComponentError, ComponentId, ComponentRootType, FileContentFilter, FileTagger,
    ItemContentFilter, ProcessListener, SdComponent, SourceFileFilter, SourceItemFilter,
    Trimmer, VariableProvider, VariableReplacer,
};
use source_downloader_sdk::storage::ProcessingStorage;
use std::collections::{HashMap, HashSet};
use std::fmt::{Display, Formatter};
use std::ops::Not;
use std::path::Path;
use std::string::ToString;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub struct ProcessorManager {
    component_manager: Arc<ComponentManager>,
    processing_storage: Arc<dyn ProcessingStorage>,
    processor_wrappers: RwLock<HashMap<String, Arc<ProcessorWrapper>>>,
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(VariableReplacer)]
struct KeyFilterVariableReplacer {
    replacer: Arc<dyn VariableReplacer>,
    keys: Option<HashSet<String>>,
}

impl VariableReplacer for KeyFilterVariableReplacer {
    fn replace(&self, key: &str, value: String) -> String {
        if self.keys.as_ref().is_none_or(|keys| keys.contains(key)) {
            self.replacer.replace(key, value)
        } else {
            value
        }
    }
}

impl Display for KeyFilterVariableReplacer {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "key-filter({})", self.replacer)
    }
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

    fn get_component_for_processor(
        &self,
        component_id: &ComponentId,
        processor_name: &str,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        self.component_manager
            .get_component(component_id)?
            .require_and_mark_ref(processor_name)
    }

    pub fn create_processor(&self, config: &ProcessorConfig) {
        if config.enabled.not() {
            info!("Processor[disabled] {}", config.name);
            return;
        }
        let processor_wrapper = match self.create_internal(config) {
            Ok(p) => p,
            Err(err) => {
                self.component_manager.remove_processor_refs(&config.name);
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

    fn register_task(
        &self,
        config: &ProcessorConfig,
        processor_wrapper: Arc<ProcessorWrapper>,
    ) {
        let processor_task = processor_wrapper.processor.as_ref().unwrap();
        for component_ref in config.triggers.iter() {
            let id = &ComponentRootType::Trigger.parse_component_id(component_ref);
            let component = match self.get_component_for_processor(id, &config.name) {
                Ok(component) => component,
                Err(error) => {
                    warn!(
                        "Processor {} using an invalid trigger: {} cause: {}",
                        config.name, component_ref, error
                    );
                    continue;
                }
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
        let source =
            self.get_component_for_processor(&source_id, &config.name)?.as_source()?;

        let item_file_resolver_id = ComponentRootType::ItemFileResolver
            .parse_component_id(&config.item_file_resolver);
        let item_file_resolver = self
            .get_component_for_processor(&item_file_resolver_id, &config.name)?
            .as_item_file_resolver()?;

        let downloader_id =
            ComponentRootType::Downloader.parse_component_id(&config.downloader);
        let downloader = self
            .get_component_for_processor(&downloader_id, &config.name)?
            .as_downloader()?;

        let file_mover_id =
            ComponentRootType::FileMover.parse_component_id(&config.file_mover);
        let file_mover = self
            .get_component_for_processor(&file_mover_id, &config.name)?
            .as_file_mover()?;

        let task_group = config
            .options
            .task_group
            .clone()
            .or(source.group())
            .unwrap_or(source_id.component_type.name);
        let processor = Arc::new(SourceProcessor::new(
            config.name.to_owned(),
            config.source.to_owned(),
            Path::new(&config.save_path).into(),
            source,
            item_file_resolver,
            downloader,
            file_mover,
            self.processing_storage.to_owned(),
            config.category.to_owned(),
            config.tags.to_owned(),
            self.create_renamer(config)?,
            self.create_options(config, task_group)?,
        ));
        let instance_id = processor.instance_id();
        processor.start_rename_task();
        let wrapper = Arc::new(ProcessorWrapper {
            name: config.name.to_owned(),
            processor: Some(processor),
            error_message: None,
        });
        self.processor_wrappers.write().insert(config.name.to_owned(), wrapper.clone());
        info!("Processor[created] {}({:?})", config.name, instance_id);
        Ok(wrapper)
    }

    fn create_renamer(
        &self,
        config: &ProcessorConfig,
    ) -> Result<Renamer, ComponentError> {
        let mut variable_replacers: Vec<Arc<dyn VariableReplacer>> =
            Vec::with_capacity(config.options.variable_replacers.len());
        for replacer_config in &config.options.variable_replacers {
            let component_id = ComponentRootType::VariableReplacer
                .parse_component_id(&replacer_config.id);
            let replacer = self
                .get_component_for_processor(&component_id, &config.name)?
                .as_variable_replacer()?;
            variable_replacers.push(Arc::new(KeyFilterVariableReplacer {
                replacer,
                keys: replacer_config.keys.clone(),
            }));
        }

        let mut trimming = HashMap::with_capacity(config.options.trimming.len());
        for trimming_config in &config.options.trimming {
            let mut trimmers: Vec<Arc<dyn Trimmer>> =
                Vec::with_capacity(trimming_config.trimmers.len());
            for trimmer_id in &trimming_config.trimmers {
                let component_id =
                    ComponentRootType::Trimmer.parse_component_id(trimmer_id);
                trimmers.push(
                    self.get_component_for_processor(&component_id, &config.name)?
                        .as_trimmer()?,
                );
            }
            trimming.insert(trimming_config.variable_name.clone(), trimmers);
        }

        let mut variable_process_chain =
            Vec::with_capacity(config.options.variable_process.len());
        for process_config in &config.options.variable_process {
            let mut chain = Vec::with_capacity(process_config.chain.len());
            for provider_id in &process_config.chain {
                let component_id =
                    ComponentRootType::VariableProvider.parse_component_id(provider_id);
                chain.push(
                    self.get_component_for_processor(&component_id, &config.name)?
                        .as_variable_provider()?,
                );
            }
            let condition = process_config
                .condition_expression
                .as_deref()
                .map(|expression| FACTORY.create::<bool>(expression))
                .transpose()?
                .map(Arc::from);
            variable_process_chain.push(VariableProcessChain {
                input: process_config.input.clone(),
                chain,
                output: VariableProcessOutput {
                    key_mapping: process_config.output.key_mapping.clone(),
                    exclude_keys: process_config.output.exclude_keys.clone(),
                    include_keys: process_config.output.include_keys.clone(),
                },
                condition,
            });
        }

        Ok(Renamer {
            variable_error_strategy: config.options.variable_error_strategy,
            variable_replacers,
            trimming,
            path_name_length_limit: config.options.path_name_length_limit,
            variable_process_chain,
        })
    }

    fn create_options(
        &self,
        config: &ProcessorConfig,
        group: String,
    ) -> Result<ProcessorOptions, ComponentError> {
        let opt = &config.options;
        let mut item_filters: Vec<Arc<dyn SourceItemFilter>> = vec![];
        if !opt.item_expression_exclusions.is_empty()
            || !opt.item_expression_inclusions.is_empty()
        {
            let filter = Self::apply_item_expression(
                &opt.item_expression_exclusions,
                &opt.item_expression_inclusions,
            )?;
            item_filters.push(Arc::new(filter));
        }

        for x in &opt.item_filters {
            let component_id = ComponentRootType::SourceItemFilter.parse_component_id(x);
            item_filters.push(
                self.get_component_for_processor(&component_id, &config.name)?
                    .as_source_item_filter()?,
            );
        }

        // ===
        let mut source_file_filters: Vec<Arc<dyn SourceFileFilter>> = vec![];
        for x in &opt.source_file_filters {
            let component_id = ComponentRootType::SourceFileFilter.parse_component_id(x);
            source_file_filters.push(
                self.get_component_for_processor(&component_id, &config.name)?
                    .as_source_file_filter()?,
            );
        }

        // ===
        let mut variable_providers: Vec<Arc<dyn VariableProvider>> = vec![];
        for x in &opt.variable_providers {
            let component_id = ComponentRootType::VariableProvider.parse_component_id(x);
            variable_providers.push(
                self.get_component_for_processor(&component_id, &config.name)?
                    .as_variable_provider()?,
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
            let component_id = ComponentRootType::FileTagger.parse_component_id(x);
            file_taggers.push(
                self.get_component_for_processor(&component_id, &config.name)?
                    .as_file_tagger()?,
            );
        }

        // ===
        let mut file_content_filters: Vec<Arc<dyn FileContentFilter>> = vec![];
        if !opt.file_content_expression_exclusions.is_empty()
            || !opt.file_content_expression_inclusions.is_empty()
        {
            let filter = Self::apply_file_content_expression(
                &opt.file_content_expression_exclusions,
                &opt.file_content_expression_inclusions,
            )?;
            file_content_filters.push(Arc::new(filter));
        }

        for x in &opt.file_content_filters {
            let component_id = ComponentRootType::FileContentFilter.parse_component_id(x);
            file_content_filters.push(
                self.get_component_for_processor(&component_id, &config.name)?
                    .as_file_content_filter()?,
            );
        }
        // ===

        let mut item_content_filters: Vec<Arc<dyn ItemContentFilter>> = vec![];
        if !opt.item_content_expression_exclusions.is_empty()
            || !opt.item_content_expression_inclusions.is_empty()
        {
            let filter = Self::apply_item_content_expression(
                &opt.item_content_expression_exclusions,
                &opt.item_content_expression_inclusions,
            )?;
            item_content_filters.push(Arc::new(filter));
        }

        for x in &opt.item_content_filters {
            let component_id = ComponentRootType::ItemContentFilter.parse_component_id(x);
            item_content_filters.push(
                self.get_component_for_processor(&component_id, &config.name)?
                    .as_item_content_filter()?,
            );
        }
        // ===
        let mut process_listeners: HashMap<ListenerMode, Vec<Arc<dyn ProcessListener>>> =
            HashMap::new();
        for listener_config in &config.options.process_listeners {
            let component_id = ComponentRootType::ProcessListener
                .parse_component_id(&listener_config.id);
            let listener = self
                .get_component_for_processor(&component_id, &config.name)?
                .as_process_listener()?;
            process_listeners.entry(listener_config.mode).or_default().push(listener);
        }

        // ==
        let file_exists_detector_id = ComponentRootType::FileExistsDetector
            .parse_component_id(opt.file_exists_detector.as_deref().unwrap_or("simple"));
        let file_exists_detector = self
            .get_component_for_processor(&file_exists_detector_id, &config.name)?
            .as_file_exists_detector()?;
        let file_replacement_decider_id = ComponentRootType::FileReplacementDecider
            .parse_component_id(
                opt.file_replacement_decider.as_deref().unwrap_or("never"),
            );
        let file_replacement_decider = self
            .get_component_for_processor(&file_replacement_decider_id, &config.name)?
            .as_file_replacement_decider()?;

        Ok(ProcessorOptions {
            save_path_pattern: PathPattern::new_cel(
                config.options.save_path_pattern.to_owned(),
            ),
            filename_pattern: PathPattern::new_cel(
                config.options.filename_pattern.to_owned(),
            ),
            variable_providers,
            item_filters,
            item_content_filters,
            file_content_filters,
            source_file_filters,
            file_taggers,
            process_listeners,
            file_exists_detector,
            file_replacement_decider,
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
            save_processing_content: config.options.save_processing_content,
            rename_task_interval: humantime::parse_duration(
                &config.options.rename_task_interval,
            )
            .map_err(|e| e.to_string())?,
            rename_times_threshold: config.options.rename_times_threshold,
            parallelism: config.options.parallelism,
            retry_attempts: config.options.retry_attempts,
            retry_backoff: humantime::parse_duration(&config.options.retry_backoff)
                .map_err(|error| error.to_string())?,
            task_group: Some(group),
            fetch_limit: config.options.fetch_limit,
            item_error_continue: config.options.item_error_continue,
            pointer_batch_mode: config.options.pointer_batch_mode,
            item_rules: self.apply_item_grouping(config, opt, identity_filter)?,
            file_rules: self.apply_file_grouping(config, opt)?,
            download_options: config.options.download_options.clone().into(),
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
        debug!("ProcessorWp[on-destroy-arc] {}", Arc::strong_count(&wrapper));
        self.component_manager.remove_processor_refs(name);
        let Some(processor) = &wrapper.processor else {
            return;
        };
        let triggers = self.component_manager.get_all_trigger();
        for trigger in triggers {
            let task = processor.clone();
            trigger.remove_task(task);
        }
        processor.close();
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
                        let cid =
                            ComponentRootType::SourceItemFilter.parse_component_id(name);
                        let filter = self
                            .get_component_for_processor(&cid, &cfg.name)?
                            .as_source_item_filter()?;
                        filters.push(filter);
                    }
                    Some(filters)
                } else {
                    None
                };

            let mut item_filters =
                if expression_filters.is_some() || source_item_filters.is_some() {
                    let mut filters = Vec::new();
                    filters.extend(expression_filters.unwrap_or_default());
                    filters.extend(source_item_filters.unwrap_or_default());
                    Some(filters)
                } else {
                    None
                };
            // ===

            let providers =
                if let Some(ref provider_names) = item_opt_cfg.variable_providers {
                    let mut providers = Vec::new();
                    for name in provider_names {
                        let cid =
                            ComponentRootType::VariableProvider.parse_component_id(name);
                        let provider = self
                            .get_component_for_processor(&cid, &cfg.name)?
                            .as_variable_provider()?;
                        providers.push(provider);
                    }
                    Some(providers)
                } else {
                    None
                };

            if opt.save_processing_content
                && let Some(filters) = item_filters.as_mut()
            {
                filters.push(identity_filter.clone());
            }
            // ===
            let expression_matching = item_opt_cfg
                .expression_matching
                .as_ref()
                .map(|x| FACTORY.create(x))
                .transpose()?;
            let matcher = ExpressionAndTagMatcher::new(
                expression_matching,
                item_opt_cfg.tags.to_owned(),
            );

            let strategy = ItemStrategy {
                save_path_pattern: item_opt_cfg
                    .save_path_pattern
                    .as_ref()
                    .map(|x| PathPattern::new_cel(x.clone())),
                filename_pattern: item_opt_cfg
                    .filename_pattern
                    .as_ref()
                    .map(|x| PathPattern::new_cel(x.clone())),
                item_filters,
                variable_providers: providers,
            };
            result.push(ItemRule { matcher: Box::new(matcher), strategy })
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
            let expression_filters =
                if file_opt_cfg.file_content_expression_inclusions.is_some()
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
                        let cid =
                            ComponentRootType::FileContentFilter.parse_component_id(name);
                        let filter = self
                            .get_component_for_processor(&cid, &cfg.name)?
                            .as_file_content_filter()?;
                        filters.push(filter);
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
                .map(|x| FACTORY.create(x))
                .transpose()?;
            let matcher = ExpressionAndTagMatcher::new(
                expression_matching,
                file_opt_cfg.tags.to_owned(),
            );

            let strategy = FileStrategy {
                save_path_pattern: file_opt_cfg
                    .save_path_pattern
                    .as_ref()
                    .map(|pattern| PathPattern::new_cel(pattern.clone())),
                filename_pattern: file_opt_cfg
                    .filename_pattern
                    .as_ref()
                    .map(|pattern| PathPattern::new_cel(pattern.clone())),
                file_content_filters,
            };
            result.push(FileRule { matcher: Box::new(matcher), strategy })
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
    use crate::config::{
        FileRuleConfig, ProcessorConfig, ProcessorOptionConfig, TrimmingConfig,
        VariableReplacerConfig, YamlConfigOperator,
    };
    use crate::processor_manager::ProcessorManager;
    use source_downloader_sdk::component::ComponentRootType;
    use std::collections::HashSet;
    use std::sync::Arc;
    use storage_memory::MemoryProcessingStorage;

    #[tokio::test]
    async fn normal_cases() {
        let component_manager = Arc::new(ComponentManager::new(Arc::new(
            YamlConfigOperator::new("./tests/resources/config.yaml"),
        )));
        let _ = component_manager
            .register_suppliers(get_build_in_component_supplier(&component_manager));
        let manager = ProcessorManager::new(
            component_manager.clone(),
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
        let processor =
            processor_wp.as_ref().unwrap().processor.as_ref().unwrap().clone();
        let referenced_ids = [
            ComponentRootType::Source.parse_component_id("system-file:test"),
            ComponentRootType::ItemFileResolver.parse_component_id("system-file:test"),
            ComponentRootType::Downloader.parse_component_id("http"),
            ComponentRootType::FileMover.parse_component_id("system-file"),
            ComponentRootType::FileExistsDetector.parse_component_id("simple"),
        ];
        for id in &referenced_ids {
            assert!(
                component_manager.get_component(id).unwrap().get_refs().contains(name)
            );
        }
        let source_component = component_manager
            .get_component(&referenced_ids[0])
            .unwrap()
            .require_component()
            .unwrap();
        let source_refs_while_running = Arc::strong_count(&source_component);
        drop(processor_wp);
        manager.destroy_processor(name);
        assert!(!manager.processor_exists(name));
        assert_eq!(
            source_downloader_sdk::component::ProcessTask::run(processor.as_ref())
                .await
                .unwrap_err(),
            "Processor is closed"
        );
        drop(processor);
        assert!(Arc::strong_count(&source_component) < source_refs_while_running);
        for id in &referenced_ids {
            assert!(component_manager.get_component(id).unwrap().get_refs().is_empty());
        }
    }

    #[test]
    fn create_processor_given_error_component() {
        let component_manager = Arc::new(ComponentManager::new(Arc::new(
            YamlConfigOperator::new("./tests/resources/config.yaml"),
        )));
        component_manager
            .register_suppliers(get_build_in_component_supplier(&component_manager))
            .unwrap();
        let manager = ProcessorManager::new(
            component_manager.clone(),
            Arc::new(MemoryProcessingStorage::new()),
        );

        let name = "normal-case";
        manager.create_processor(&ProcessorConfig {
            name: name.to_string(),
            enabled: true,
            triggers: vec![],
            source: "system-file:test".to_string(),
            item_file_resolver: "system-file:test".to_string(),
            downloader: "not-exists".to_string(),
            file_mover: "system-file".to_string(),
            save_path: "./tests/resources/output".to_string(),
            options: ProcessorOptionConfig::default(),
            category: None,
            tags: HashSet::new(),
        });
        let processor_wp = manager.get_processor(name);
        assert!(processor_wp.is_some());
        assert!(processor_wp.unwrap().error_message.is_some());
        let source_id = ComponentRootType::Source.parse_component_id("system-file:test");
        assert!(
            component_manager.get_component(&source_id).unwrap().get_refs().is_empty()
        );
        let resolver_id =
            ComponentRootType::ItemFileResolver.parse_component_id("system-file:test");
        assert!(
            component_manager.get_component(&resolver_id).unwrap().get_refs().is_empty()
        );
    }
    #[tokio::test]
    async fn content_filter_options_keep_exclusions_and_item_filter_references() {
        use source_downloader_sdk::SourceItem;
        use source_downloader_sdk::component::{FileContent, ItemContent};
        use source_downloader_sdk::storage::ProcessingStatus;
        use std::collections::HashMap;
        use std::path::PathBuf;

        let component_manager = Arc::new(ComponentManager::new(Arc::new(
            YamlConfigOperator::new("./tests/resources/config.yaml"),
        )));
        component_manager
            .register_suppliers(get_build_in_component_supplier(&component_manager))
            .unwrap();
        let manager = ProcessorManager::new(
            component_manager,
            Arc::new(MemoryProcessingStorage::new()),
        );
        let options = ProcessorOptionConfig {
            file_content_expression_exclusions: vec![
                "file.name == 'blocked.txt'".to_owned(),
            ],
            item_content_expression_exclusions: vec![
                "item.title == 'blocked'".to_owned(),
            ],
            ..Default::default()
        };
        let config = ProcessorConfig {
            name: "filter-options".to_owned(),
            enabled: true,
            triggers: Vec::new(),
            source: "system-file:test".to_owned(),
            item_file_resolver: "system-file:test".to_owned(),
            downloader: "http".to_owned(),
            file_mover: "system-file".to_owned(),
            save_path: "./tests/resources/output".to_owned(),
            options,
            category: None,
            tags: HashSet::new(),
        };

        let options =
            manager.create_options(&config, "filter-options".to_owned()).unwrap();

        assert_eq!(options.file_content_filters.len(), 1);
        assert_eq!(options.item_content_filters.len(), 1);
        let file = FileContent {
            file_download_path: PathBuf::from("blocked.txt"),
            ..Default::default()
        };
        assert!(!options.file_content_filters[0].filter(&file));
        let source_item =
            SourceItem { title: "blocked".to_owned(), ..Default::default() };
        let files = Vec::new();
        let variables = HashMap::new();
        let item_content = ItemContent {
            source_item: &source_item,
            file_contents: &files,
            item_variables: &variables,
            status: ProcessingStatus::WaitingToRename,
        };
        assert!(!options.item_content_filters[0].filter(&item_content).await);
    }

    #[test]
    fn file_group_patterns_are_compiled_into_strategy() {
        let manager = ProcessorManager::new(
            Arc::new(ComponentManager::new(Arc::new(YamlConfigOperator::new(
                "./tests/resources/config.yaml",
            )))),
            Arc::new(MemoryProcessingStorage::new()),
        );
        let options = ProcessorOptionConfig {
            file_grouping: vec![FileRuleConfig {
                save_path_pattern: Some("{series}/Season {season}".to_owned()),
                filename_pattern: Some("{title} - {episode}".to_owned()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let config = ProcessorConfig {
            name: "file-group-patterns".to_owned(),
            enabled: true,
            save_path: String::new(),
            triggers: Vec::new(),
            source: String::new(),
            item_file_resolver: String::new(),
            downloader: String::new(),
            file_mover: String::new(),
            options: options.clone(),
            category: None,
            tags: HashSet::new(),
        };

        let rules = manager.apply_file_grouping(&config, &options).unwrap();
        let strategy = &rules[0].strategy;

        assert_eq!(
            strategy.save_path_pattern.as_ref().map(|pattern| pattern.pattern.as_str()),
            Some("{series}/Season {season}")
        );
        assert_eq!(
            strategy.filename_pattern.as_ref().map(|pattern| pattern.pattern.as_str()),
            Some("{title} - {episode}")
        );
    }

    #[tokio::test]
    async fn variable_replacer_components_are_resolved_and_key_filtered() {
        use source_downloader_sdk::SourceItem;
        use std::collections::HashMap;

        let component_manager = Arc::new(ComponentManager::new(Arc::new(
            YamlConfigOperator::new("./tests/resources/config.yaml"),
        )));
        component_manager
            .register_suppliers(get_build_in_component_supplier(&component_manager))
            .unwrap();
        let manager = ProcessorManager::new(
            component_manager.clone(),
            Arc::new(MemoryProcessingStorage::new()),
        );
        let name = "variable-replacer-wiring";
        let config = ProcessorConfig {
            name: name.to_owned(),
            enabled: true,
            save_path: String::new(),
            triggers: Vec::new(),
            source: String::new(),
            item_file_resolver: String::new(),
            downloader: String::new(),
            file_mover: String::new(),
            options: ProcessorOptionConfig {
                variable_replacers: vec![VariableReplacerConfig {
                    id: "windows-path".to_owned(),
                    keys: Some(HashSet::from(["item.title".to_owned()])),
                }],
                trimming: vec![TrimmingConfig {
                    variable_name: "title".to_owned(),
                    trimmers: vec!["force".to_owned()],
                }],
                path_name_length_limit: 42,
                ..Default::default()
            },
            category: None,
            tags: HashSet::new(),
        };

        let renamer = manager.create_renamer(&config).unwrap();
        let item = SourceItem {
            title: "series:01".to_owned(),
            content_type: "video/mp4".to_owned(),
            ..Default::default()
        };
        let variables = renamer.item_rename_variables(&item, &HashMap::new()).await;

        assert_eq!("series：01", variables.variables["item"]["title"]);
        assert_eq!("video/mp4", variables.variables["item"]["contentType"]);
        assert_eq!(42, renamer.path_name_length_limit);
        assert_eq!(1, renamer.trimming["title"].len());
        let component_id =
            ComponentRootType::VariableReplacer.parse_component_id("windows-path");
        let trimmer_id = ComponentRootType::Trimmer.parse_component_id("force");
        assert!(
            component_manager
                .get_component(&trimmer_id)
                .unwrap()
                .get_refs()
                .contains(name)
        );
        assert!(
            component_manager
                .get_component(&component_id)
                .unwrap()
                .get_refs()
                .contains(name)
        );
    }
}
