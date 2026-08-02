use crate::expression::{source_item_variables, CompiledExpression};
use async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, DownloadTask, Downloader, ItemFileResolver, SdComponent, SourceFile,
};
use source_downloader_sdk::SourceItem;
use std::fmt::{Debug, Display, Formatter};
use std::sync::Arc;

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
    use crate::expression::CompiledExpression;
    use source_downloader_sdk::component::ComponentSupplier;
    use source_downloader_sdk::serde_json::Value;

    struct ConstantExpression(bool);

    impl CompiledExpression<bool> for ConstantExpression {
        fn execute(
            &self,
            _: &source_downloader_sdk::serde_json::Map<String, Value>,
        ) -> Result<bool, String> {
            Ok(self.0)
        }
    }

    fn downloader(path: &str) -> Arc<dyn Downloader> {
        let props =
            serde_json::json!({"download-path": path}).as_object().unwrap().clone();
        crate::components::mock_downloader::SUPPLIER
            .apply(&props)
            .unwrap()
            .as_downloader()
            .unwrap()
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
}
