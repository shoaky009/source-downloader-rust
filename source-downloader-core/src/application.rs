use crate::component_manager::ComponentManager;
use crate::components::get_build_in_component_supplier;
use crate::components::webhook_trigger::{
    WebhookAdapter, WebhookEndpoint, WebhookTrigger,
};
use crate::config::ConfigOperator;
use crate::instance_manager::InstanceManager;
use crate::plugin::PluginManager;
use crate::processor_manager::ProcessorManager;
use crate::processor_run_manager::ProcessorRunManager;
use source_downloader_sdk::component::{ComponentId, ComponentRootType, ComponentType};
use source_downloader_sdk::plugin::PluginContext;
use std::any::Any;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tracing::info;

pub struct CoreApplication {
    pub config_operator: Arc<dyn ConfigOperator>,
    pub component_manager: Arc<ComponentManager>,
    pub instance_manager: Arc<InstanceManager>,
    pub processor_manager: Arc<ProcessorManager>,
    pub run_manager: Arc<ProcessorRunManager>,
    pub plugin_manager: PluginManager,
    pub data_location: Box<Path>,
    pub plugin_location: Option<Box<Path>>,
    pub webhook_adapter: Option<Arc<dyn WebhookAdapter>>,
}

fn validate_webhook_endpoints(
    endpoints_to_validate: impl IntoIterator<Item = WebhookEndpoint>,
) -> Result<(), String> {
    let mut endpoints = HashSet::new();
    for endpoint in endpoints_to_validate {
        if !endpoints.insert(endpoint.clone()) {
            return Err(format!(
                "Duplicate webhook endpoint: {} {}",
                endpoint.method().as_str(),
                endpoint.route_path()
            ));
        }
    }
    Ok(())
}

impl CoreApplication {
    pub fn start(&self) -> Result<(), String> {
        self.init_plugin();
        self.register_instance_factory();
        self.register_component_supplier();
        info!("{}", self.component_manager);
        self.create_processors();
        self.initialize_triggers()?;
        self.configure_webhook_triggers()?;
        self.start_triggers()
    }

    pub fn shutdown(&self) {
        info!("Core application shutting down");
        self.stop_triggers();
        self.destroy_all_processor();
        self.destroy_all_component();
        self.destroy_all_instance();
        info!("Core application stopped");
    }

    pub fn set_webhook_adapter(&mut self, adapter: Arc<dyn WebhookAdapter>) {
        self.webhook_adapter = Some(adapter);
    }

    fn init_plugin(&self) {
        let path = match &self.plugin_location {
            Some(p) => p,
            None => {
                info!("未配置插件路径不加载外部插件");
                return;
            }
        };
        info!("从目录加载插件: {}", path.display());
        self.plugin_manager.load_dylib_plugins(path.to_str().unwrap());
    }

    fn register_instance_factory(&self) {
        self.plugin_manager.with_plugins(|plugins| {
            for plugin in plugins {
                plugin.get_instance_factories().iter().for_each(|x| {
                    // 有重复的直接crash
                    self.instance_manager.register_instance_factory(x.clone()).unwrap();
                });
            }
        })
    }

    fn register_component_supplier(&self) {
        self.component_manager
            .register_suppliers(get_build_in_component_supplier(&self.component_manager))
            .unwrap();

        self.plugin_manager.with_plugins(|plugins| {
            for plugin in plugins {
                plugin.get_component_suppliers().iter().for_each(|x| {
                    // 因为插件目前没有卸载重载等周期
                    self.component_manager.register_supplier(x.clone()).unwrap();
                })
            }
        })
    }

    fn create_processors(&self) {
        let configs = self.config_operator.get_all_processor_config();
        info!("Total {} processors to be created", configs.len());
        for cfg in configs {
            self.processor_manager.create_processor(&cfg)
        }
    }
    fn initialize_triggers(&self) -> Result<(), String> {
        let configs = self.config_operator.get_all_component_config();
        let Some(triggers) = configs.get(ComponentRootType::Trigger.name()) else {
            return Ok(());
        };
        for config in triggers {
            let id = ComponentId::new(
                ComponentType::trigger(config.component_type.clone()),
                &config.name,
            );
            self.component_manager
                .get_component(&id)
                .and_then(|wrapper| wrapper.require_component())
                .and_then(|component| component.as_trigger())
                .map_err(|error| {
                    format!("Failed to initialize trigger {}: {}", id.display(), error)
                })?;
        }
        Ok(())
    }

    fn configure_webhook_triggers(&self) -> Result<(), String> {
        let endpoints = self
            .component_manager
            .get_all_component()
            .into_iter()
            .filter_map(|wrapper| {
                let component = wrapper.component.as_ref()?;
                let component: &dyn Any = component.as_ref();
                component
                    .downcast_ref::<WebhookTrigger>()
                    .map(|trigger| trigger.endpoint_spec().clone())
            })
            .collect::<Vec<_>>();
        validate_webhook_endpoints(endpoints)?;

        if let Some(adapter) = self.webhook_adapter.clone() {
            for wrapper in self.component_manager.get_all_component() {
                let Some(component) = wrapper.component.as_ref() else {
                    continue;
                };
                let component: &dyn Any = component.as_ref();
                if let Some(trigger) = component.downcast_ref::<WebhookTrigger>() {
                    trigger.set_adapter(adapter.clone())?;
                }
            }
        }
        Ok(())
    }

    fn start_triggers(&self) -> Result<(), String> {
        let mut start_error = None;
        self.component_manager.for_each_trigger(|wrapper, trigger| {
            if start_error.is_some() {
                return;
            }
            info!(
                "Starting trigger {}:{}",
                wrapper.id.component_type.name, wrapper.id.name
            );
            let Some(component) = wrapper.component.as_ref() else {
                trigger.start();
                return;
            };
            let component: &dyn Any = component.as_ref();
            if let Some(webhook) = component.downcast_ref::<WebhookTrigger>() {
                if let Err(error) = webhook.start_checked() {
                    start_error = Some(format!(
                        "Failed to start webhook {} {}: {}",
                        webhook.endpoint_spec().method().as_str(),
                        webhook.endpoint_spec().route_path(),
                        error
                    ));
                }
            } else {
                trigger.start();
            }
        });
        if let Some(error) = start_error {
            self.stop_triggers();
            Err(error)
        } else {
            Ok(())
        }
    }

    fn stop_triggers(&self) {
        self.component_manager.for_each_trigger(|_, trigger| {
            trigger.stop();
        });
    }

    // Reload is intentionally ordered as teardown followed by setup. It is not
    // transactional; a setup failure leaves the old configuration removed,
    // rather than mixing stale and new webhook registrations.
    pub fn reload(&self) -> Result<(), String> {
        self.destroy_all_processor();
        self.destroy_all_component();
        self.destroy_all_instance();
        self.create_processors();
        self.initialize_triggers()?;
        self.configure_webhook_triggers()?;
        self.start_triggers()
    }

    fn destroy_all_processor(&self) {
        for name in self.processor_manager.get_all_processor_names() {
            self.processor_manager.destroy_processor(&name)
        }
        info!("All processors destroyed");
    }

    fn destroy_all_component(&self) {
        for wrapper in self.component_manager.get_all_component() {
            self.component_manager.destroy(&wrapper.id);
        }
        info!("All components destroyed");
    }

    fn destroy_all_instance(&self) {
        self.instance_manager.destroy_all_instances();
        info!("All instances destroyed");
    }
}

pub struct CorePluginContext {
    pub data_location: Box<Path>,
}

impl PluginContext for CorePluginContext {
    fn get_persistent_data_path(&self) -> &Path {
        self.data_location.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::YamlConfigOperator;
    use async_trait::async_trait;
    use source_downloader_sdk::component::{ComponentRootType, ProcessTask};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use storage_memory::MemoryProcessingStorage;
    use tempfile::NamedTempFile;

    #[derive(Debug)]
    struct CountingTask(Arc<AtomicUsize>);

    #[async_trait]
    impl ProcessTask for CountingTask {
        async fn run(&self) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn name(&self) -> &str {
            "counting"
        }

        fn group(&self) -> Option<String> {
            None
        }
    }

    #[test]
    fn duplicate_webhook_endpoints_are_rejected_before_start() {
        let first = WebhookTrigger::new("updates", "POST").unwrap();
        let second = WebhookTrigger::new("updates", "post").unwrap();

        let error = validate_webhook_endpoints([
            first.endpoint_spec().clone(),
            second.endpoint_spec().clone(),
        ])
        .unwrap_err();

        assert!(error.contains("POST"));
        assert!(error.contains("/webhook/updates"));
    }

    #[tokio::test(start_paused = true)]
    async fn start_initializes_unreferenced_configured_triggers() {
        let config_file = NamedTempFile::new().unwrap();
        std::fs::write(
            config_file.path(),
            "instances: []\ncomponents:\n  trigger:\n  - name: idle\n    type: fixed\n    props:\n      interval: PT0.02S\nprocessors: []\n",
        )
        .unwrap();
        let config_operator = Arc::new(YamlConfigOperator::new_path(config_file.path()));
        let instance_manager = Arc::new(InstanceManager::new(config_operator.clone()));
        let component_manager = Arc::new(ComponentManager::with_create_context(
            config_operator.clone(),
            instance_manager.clone(),
        ));
        let run_manager = Arc::new(ProcessorRunManager::default());
        let processor_manager = Arc::new(ProcessorManager::new(
            component_manager.clone(),
            Arc::new(MemoryProcessingStorage::new()),
            run_manager.clone(),
        ));
        let application = CoreApplication {
            config_operator,
            component_manager: component_manager.clone(),
            instance_manager,
            processor_manager,
            run_manager,
            plugin_manager: PluginManager::new(Arc::new(CorePluginContext {
                data_location: Path::new(".").into(),
            })),
            data_location: Path::new(".").into(),
            plugin_location: None,
            webhook_adapter: None,
        };

        application.start().unwrap();
        let trigger = component_manager
            .get_component(&ComponentRootType::Trigger.parse_component_id("fixed:idle"))
            .unwrap()
            .require_component()
            .unwrap()
            .as_trigger()
            .unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        trigger.add_task(Arc::new(CountingTask(count.clone())));
        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_millis(20)).await;
        tokio::task::yield_now().await;

        assert_eq!(count.load(Ordering::SeqCst), 1);
        application.shutdown();
    }
}
