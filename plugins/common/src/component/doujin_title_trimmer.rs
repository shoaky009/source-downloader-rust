use regex::Regex;
use source_downloader_sdk::SdComponent;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, SdComponent, SdComponentMetadata,
    Trimmer,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::{Arc, LazyLock};

pub struct DoujinTitleTrimmerSupplier;

pub const SUPPLIER: DoujinTitleTrimmerSupplier = DoujinTitleTrimmerSupplier;

impl ComponentSupplier for DoujinTitleTrimmerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::trimmer("doujin".to_string())]
    }
    fn apply(
        &self,
        _: &dyn source_downloader_sdk::component::ComponentCreateContext,
        _props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(DoujinTitleTrimmer))
    }
    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(Trimmer)]
struct DoujinTitleTrimmer;

impl Display for DoujinTitleTrimmer {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "doujin")
    }
}

static AD_BRACKET_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"【[^【】]*】").expect("static regex must compile"));

impl Trimmer for DoujinTitleTrimmer {
    fn trim(&self, value: String, expect_size: usize) -> String {
        let matches: Vec<_> = AD_BRACKET_REGEX
            .find_iter(&value)
            .map(|found| found.as_str().to_string())
            .collect();
        let mut result = value;
        for matched in matches {
            result = result.replace(&matched, "");
            if utf16_len(&result) <= expect_size {
                return result;
            }
        }
        if let Some(index) = result.find('。') {
            result.truncate(index);
        }
        result
    }
}

fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplier_supports_implicit_construction() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::trimmer("doujin".to_string())]
        );
        assert!(SUPPLIER.is_support_no_props());
        assert!(
            SUPPLIER
                .apply(
                    &source_downloader_sdk::component::EMPTY_COMPONENT_CREATE_CONTEXT,
                    &Map::new(),
                )
                .is_ok()
        );
    }

    #[test]
    fn removes_ad_brackets_from_left_to_right_until_short_enough() {
        assert_eq!(
            "正文【第二广告】尾部",
            DoujinTitleTrimmer.trim("【第一广告】正文【第二广告】尾部".to_string(), 10)
        );
        assert_eq!(
            "正文尾部",
            DoujinTitleTrimmer.trim("【第一广告】正文【第二广告】尾部".to_string(), 4)
        );
    }

    #[test]
    fn truncates_at_first_full_stop_after_bracket_removal() {
        assert_eq!(
            "很长的标题",
            DoujinTitleTrimmer.trim("【广告】很长的标题。后续说明。更多".to_string(), 2)
        );
    }

    #[test]
    fn returns_processed_value_even_if_still_too_long() {
        assert_eq!(
            "没有括号也没有句号的长标题",
            DoujinTitleTrimmer.trim("没有括号也没有句号的长标题".to_string(), 1)
        );
    }

    #[test]
    fn uses_kotlin_utf16_length_semantics() {
        assert_eq!(2, utf16_len("😀"));
        assert_eq!(
            "😀正文",
            DoujinTitleTrimmer.trim("【广告】😀正文【保留】".to_string(), 7)
        );
    }
}
