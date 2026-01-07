use crate::expression::{CompiledExpression, source_file_variables, source_item_variables};
use crate::process::file::PathPattern;
use sdk::SourceItem;
use sdk::component::{
    FileContentFilter, SourceFile, SourceFileFilter, SourceItemFilter, VariableProvider,
};
use std::collections::HashSet;
use std::sync::Arc;

pub trait SourceItemMatcher: Send + Sync {
    fn matches(&self, item: &SourceItem) -> bool;
}

pub struct ExpressionAndTagMatcher {
    expression: Option<Box<dyn CompiledExpression<bool>>>,
    tags: Option<HashSet<String>>,
}

impl ExpressionAndTagMatcher {
    pub fn new(
        expression: Option<Box<dyn CompiledExpression<bool>>>,
        tags: Option<HashSet<String>>,
    ) -> Self {
        Self { expression, tags }
    }
}

impl SourceItemMatcher for ExpressionAndTagMatcher {
    fn matches(&self, item: &SourceItem) -> bool {
        if let Some(required_tags) = &self.tags {
            return required_tags.iter().all(|t| item.tags.contains(t));
        }
        if let Some(expr) = &self.expression {
            let variables = &source_item_variables(item);
            return expr.execute(variables).unwrap_or(false);
        }
        false
    }
}

pub struct ItemStrategy {
    pub save_path_pattern: Option<Arc<PathPattern>>,
    pub filename_pattern: Option<Arc<PathPattern>>,
    pub item_filters: Option<Vec<Arc<dyn SourceItemFilter>>>,
    pub variable_providers: Option<Vec<Arc<dyn VariableProvider>>>,
}

pub struct ItemRule {
    pub matcher: Box<dyn SourceItemMatcher>,
    pub strategy: ItemStrategy,
}

// ====

pub trait SourceFileMatcher: Send + Sync {
    fn matches(&self, file: &SourceFile) -> bool;
}

impl SourceFileMatcher for ExpressionAndTagMatcher {
    fn matches(&self, file: &SourceFile) -> bool {
        if let Some(required_tags) = &self.tags {
            return required_tags.iter().all(|t| file.tags.contains(t));
        }
        if let Some(expr) = &self.expression {
            let variables = &source_file_variables(file);
            return expr.execute(variables).unwrap_or(false);
        }
        false
    }
}

pub struct FileStrategy {
    pub save_path_pattern: Option<Arc<PathPattern>>,
    pub filename_pattern: Option<Arc<PathPattern>>,
    pub file_content_filter: Option<Vec<Arc<dyn FileContentFilter>>>,
}

pub struct FileRule {
    pub matcher: Box<dyn SourceFileMatcher>,
    pub strategy: FileStrategy,
}
