use crate::component_manager::ComponentManager;
use crate::expression::cel::FACTORY;
use crate::expression::{
    CompiledExpression, CompiledExpressionFactory, source_item_variables,
};
use async_trait::async_trait;
use serde::Deserialize;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentError, ComponentRootType, ComponentSupplier, ComponentType, DownloadTask,
    Downloader, ItemFileResolver, SdComponent, SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Debug, Display, Formatter};
use std::sync::{Arc, Weak};

/// Selects one component using the first expression that evaluates to true.
pub struct ComponentSelector<T: ?Sized + SdComponent> {
    pub default: Arc<T>,
    pub rules: Vec<ComponentSelectRule<T>>,
}

impl<T: ?Sized + SdComponent> ComponentSelector<T> {
    pub fn new(default: Arc<T>, rules: Vec<ComponentSelectRule<T>>) -> Self {
        Self { default, rules }
    }

    pub fn select(&self, source_item: &SourceItem) -> Arc<T> {
        let variables = source_item_variables(source_item);
        self.rules
            .iter()
            .find(|rule| rule.expression.execute(&variables).unwrap_or(false))
            .map(|rule| Arc::clone(&rule.component))
            .unwrap_or_else(|| Arc::clone(&self.default))
    }
}

pub struct ComponentSelectRule<T: ?Sized + SdComponent> {
    pub expression: Arc<dyn CompiledExpression<bool>>,
    pub component: Arc<T>,
}

impl<T: ?Sized + SdComponent> ComponentSelectRule<T> {
    pub fn new(expression: Arc<dyn CompiledExpression<bool>>, component: Arc<T>) -> Self {
        Self { expression, component }
    }
}

#[derive(Deserialize)]
struct CompositeComponentConfig {
    default: String,
    #[serde(default)]
    rules: Vec<CompositeComponentRuleConfig>,
}

#[derive(Deserialize)]
struct CompositeComponentRuleConfig {
    expression: String,
    component: String,
}

pub struct CompositeDownloaderSupplier {
    component_manager: Weak<ComponentManager>,
}

impl CompositeDownloaderSupplier {
    pub fn new(component_manager: &Arc<ComponentManager>) -> Self {
        Self { component_manager: Arc::downgrade(component_manager) }
    }
}

impl ComponentSupplier for CompositeDownloaderSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::downloader("composite".to_owned())]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = parse_config(props)?;
        let component_manager = require_component_manager(&self.component_manager)?;
        let default = resolve_downloader(&component_manager, &config.default)?;
        let mut rules = Vec::with_capacity(config.rules.len());
        for rule in config.rules {
            rules.push(ComponentSelectRule::new(
                compile_expression(&rule.expression)?,
                resolve_downloader(&component_manager, &rule.component)?,
            ));
        }
        let selector = ComponentSelector::new(default, rules);
        Ok(Arc::new(CompositeDownloaderComponent(CompositeDownloader::new(selector)?)))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

pub struct CompositeItemFileResolverSupplier {
    component_manager: Weak<ComponentManager>,
}

impl CompositeItemFileResolverSupplier {
    pub fn new(component_manager: &Arc<ComponentManager>) -> Self {
        Self { component_manager: Arc::downgrade(component_manager) }
    }
}

impl ComponentSupplier for CompositeItemFileResolverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_resolver("composite".to_owned())]
    }

    fn apply(
        &self,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = parse_config(props)?;
        let component_manager = require_component_manager(&self.component_manager)?;
        let default = resolve_item_file_resolver(&component_manager, &config.default)?;
        let mut rules = Vec::with_capacity(config.rules.len());
        for rule in config.rules {
            rules.push(ComponentSelectRule::new(
                compile_expression(&rule.expression)?,
                resolve_item_file_resolver(&component_manager, &rule.component)?,
            ));
        }
        let selector = ComponentSelector::new(default, rules);
        Ok(Arc::new(CompositeItemFileResolverComponent(CompositeItemFileResolver::new(
            selector,
        ))))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

fn parse_config(
    props: &Map<String, Value>,
) -> Result<CompositeComponentConfig, ComponentError> {
    serde_json::from_value(Value::Object(props.clone()))
        .map_err(|error| ComponentError::new(format!("Failed to parse config: {error}")))
}

fn require_component_manager(
    component_manager: &Weak<ComponentManager>,
) -> Result<Arc<ComponentManager>, ComponentError> {
    component_manager
        .upgrade()
        .ok_or_else(|| ComponentError::new("Component manager is no longer available"))
}

fn compile_expression(
    expression: &str,
) -> Result<Arc<dyn CompiledExpression<bool>>, ComponentError> {
    FACTORY.create::<bool>(expression).map(Arc::from).map_err(ComponentError::from)
}

fn resolve_downloader(
    component_manager: &ComponentManager,
    component_ref: &str,
) -> Result<Arc<dyn Downloader>, ComponentError> {
    let id = ComponentRootType::Downloader.parse_component_id(component_ref);
    component_manager.get_component(&id)?.require_component()?.as_downloader()
}

fn resolve_item_file_resolver(
    component_manager: &ComponentManager,
    component_ref: &str,
) -> Result<Arc<dyn ItemFileResolver>, ComponentError> {
    let id = ComponentRootType::ItemFileResolver.parse_component_id(component_ref);
    component_manager.get_component(&id)?.require_component()?.as_item_file_resolver()
}

pub struct CompositeDownloader {
    selector: ComponentSelector<dyn Downloader>,
}

impl CompositeDownloader {
    pub fn new(
        selector: ComponentSelector<dyn Downloader>,
    ) -> Result<Self, ComponentError> {
        let paths = std::iter::once(selector.default.default_download_path())
            .chain(
                selector.rules.iter().map(|rule| rule.component.default_download_path()),
            )
            .collect::<Vec<_>>();
        if paths.windows(2).any(|pair| pair[0] != pair[1]) {
            return Err(ComponentError::from(
                "Downloaders must have the same download path",
            ));
        }
        Ok(Self { selector })
    }
}

impl Debug for CompositeDownloader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeDownloader")
            .field("rule_count", &self.selector.rules.len())
            .finish()
    }
}

impl Display for CompositeDownloader {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("composite")
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(Downloader)]
struct CompositeDownloaderComponent(CompositeDownloader);

impl Display for CompositeDownloaderComponent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

#[async_trait]
impl Downloader for CompositeDownloaderComponent {
    async fn submit(
        &self,
        task: &DownloadTask,
    ) -> Result<(), source_downloader_sdk::component::ProcessingError> {
        self.0.selector.select(&task.source_item).submit(task).await
    }

    fn default_download_path(&self) -> &str {
        self.0.selector.default.default_download_path()
    }

    async fn cancel(
        &self,
        source_item: &SourceItem,
        files: &[SourceFile],
    ) -> Result<(), source_downloader_sdk::component::ProcessingError> {
        self.0.selector.select(source_item).cancel(source_item, files).await
    }
}

/// Builds a composite downloader component after validating its path invariant.
pub fn composite_downloader(
    selector: ComponentSelector<dyn Downloader>,
) -> Result<Arc<dyn Downloader>, ComponentError> {
    Ok(Arc::new(CompositeDownloaderComponent(CompositeDownloader::new(selector)?)))
}

pub struct CompositeItemFileResolver {
    selector: ComponentSelector<dyn ItemFileResolver>,
}

impl CompositeItemFileResolver {
    pub fn new(selector: ComponentSelector<dyn ItemFileResolver>) -> Self {
        Self { selector }
    }
}

impl Debug for CompositeItemFileResolver {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeItemFileResolver")
            .field("rule_count", &self.selector.rules.len())
            .finish()
    }
}

impl Display for CompositeItemFileResolver {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("composite")
    }
}

#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(ItemFileResolver)]
struct CompositeItemFileResolverComponent(CompositeItemFileResolver);

impl Display for CompositeItemFileResolverComponent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

#[async_trait]
impl ItemFileResolver for CompositeItemFileResolverComponent {
    async fn resolve_files(&self, source_item: &SourceItem) -> Vec<SourceFile> {
        self.0.selector.select(source_item).resolve_files(source_item).await
    }
}

/// Builds a composite resolver from an already-resolved component selector.
pub fn composite_item_file_resolver(
    selector: ComponentSelector<dyn ItemFileResolver>,
) -> Arc<dyn ItemFileResolver> {
    Arc::new(CompositeItemFileResolverComponent(CompositeItemFileResolver::new(selector)))
}

impl<T: ?Sized + SdComponent> Debug for ComponentSelector<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentSelector")
            .field("rule_count", &self.rules.len())
            .finish()
    }
}

impl<T: ?Sized + SdComponent> Debug for ComponentSelectRule<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComponentSelectRule").finish()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::get_build_in_component_supplier;
    use crate::config::YamlConfigOperator;
    use source_downloader_sdk::component::ProcessingError;
    use tempfile::TempDir;

    struct ConstantExpression(bool);

    impl CompiledExpression<bool> for ConstantExpression {
        fn execute(&self, _: &Map<String, Value>) -> Result<bool, String> {
            Ok(self.0)
        }
    }

    #[derive(Debug, source_downloader_sdk::SdComponent)]
    #[component(Downloader)]
    struct TestDownloader {
        path: String,
    }

    impl Display for TestDownloader {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.write_str("test")
        }
    }

    #[async_trait]
    impl Downloader for TestDownloader {
        async fn submit(&self, _: &DownloadTask) -> Result<(), ProcessingError> {
            Ok(())
        }

        fn default_download_path(&self) -> &str {
            &self.path
        }

        async fn cancel(
            &self,
            _: &SourceItem,
            _: &[SourceFile],
        ) -> Result<(), ProcessingError> {
            Ok(())
        }
    }

    struct TelegramDownloaderSupplier;

    impl ComponentSupplier for TelegramDownloaderSupplier {
        fn supply_types(&self) -> Vec<ComponentType> {
            vec![ComponentType::downloader("telegram".to_owned())]
        }

        fn apply(
            &self,
            _: &Map<String, Value>,
        ) -> Result<Arc<dyn SdComponent>, ComponentError> {
            Ok(Arc::new(TestDownloader { path: "downloads".to_owned() }))
        }

        fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
            None
        }
    }

    fn downloader(path: &str) -> Arc<dyn Downloader> {
        Arc::new(TestDownloader { path: path.to_owned() })
    }

    fn configured_component_manager() -> (TempDir, Arc<ComponentManager>) {
        let temp_dir = tempfile::tempdir().unwrap();
        let config_path = temp_dir.path().join("config.yaml");
        std::fs::write(
            &config_path,
            r#"
instances: []
components:
  downloader:
    - name: telegram
      type: telegram
      props: {}
    - name: http
      type: http
      props:
        download-path: downloads
    - name: telegram-message
      type: composite
      props:
        default: telegram
        rules:
          - expression: "has(item.attrs.site) && item.attrs.site == 'Telegraph'"
            component: http
  item-file-resolver:
    - name: selected-resolver
      type: composite
      props:
        default: url
        rules:
          - expression: "has(item.attrs.site)"
            component: url
processors: []
"#,
        )
        .unwrap();
        let manager = Arc::new(ComponentManager::new(Arc::new(
            YamlConfigOperator::new_path(&config_path),
        )));
        manager.register_suppliers(get_build_in_component_supplier(&manager)).unwrap();
        manager.register_supplier(Arc::new(TelegramDownloaderSupplier)).unwrap();
        (temp_dir, manager)
    }

    #[test]
    fn selector_uses_the_first_matching_rule() {
        let default = downloader("downloads");
        let selected = downloader("downloads");
        let selector = ComponentSelector::new(
            Arc::clone(&default),
            vec![ComponentSelectRule::new(
                Arc::new(ConstantExpression(true)),
                Arc::clone(&selected),
            )],
        );

        let actual = selector.select(&SourceItem::default());

        assert!(Arc::ptr_eq(&actual, &selected));
    }

    #[test]
    fn selector_matches_configured_source_item_attribute_expression() {
        let default = downloader("downloads");
        let selected = downloader("downloads");
        let selector = ComponentSelector::new(
            default,
            vec![ComponentSelectRule::new(
                compile_expression(
                    "has(item.attrs.site) && item.attrs.site == 'Telegraph'",
                )
                .unwrap(),
                Arc::clone(&selected),
            )],
        );
        let mut source_item = SourceItem::default();
        source_item
            .attrs
            .insert("site".to_owned(), Value::String("Telegraph".to_owned()));

        let actual = selector.select(&source_item);

        assert!(Arc::ptr_eq(&actual, &selected));
    }

    #[test]
    fn composite_downloader_rejects_mismatched_download_paths() {
        let selector = ComponentSelector::new(
            downloader("downloads"),
            vec![ComponentSelectRule::new(
                Arc::new(ConstantExpression(false)),
                downloader("other"),
            )],
        );

        let error = CompositeDownloader::new(selector).unwrap_err();

        assert_eq!(error.to_string(), "Downloaders must have the same download path");
    }

    #[test]
    fn downloader_supplier_builds_component_from_configured_references() {
        let (_temp_dir, manager) = configured_component_manager();
        let id = ComponentRootType::Downloader
            .parse_component_id("composite:telegram-message");

        let downloader = manager
            .get_component(&id)
            .unwrap()
            .require_component()
            .unwrap()
            .as_downloader()
            .unwrap();

        assert_eq!(downloader.default_download_path(), "downloads");
    }

    #[test]
    fn item_file_resolver_supplier_builds_component_from_configured_references() {
        let (_temp_dir, manager) = configured_component_manager();
        let id = ComponentRootType::ItemFileResolver
            .parse_component_id("composite:selected-resolver");

        let resolver = manager
            .get_component(&id)
            .unwrap()
            .require_component()
            .unwrap()
            .as_item_file_resolver();

        assert!(resolver.is_ok(), "resolver creation failed: {resolver:?}");
    }
}
