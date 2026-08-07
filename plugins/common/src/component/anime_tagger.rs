use source_downloader_sdk::SdComponent;
use source_downloader_sdk::async_trait::async_trait;
use source_downloader_sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, FileTagger, SdComponent,
    SdComponentMetadata, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::fmt::{Display, Formatter};
use std::sync::Arc;

pub struct AnimeTaggerSupplier;

pub const SUPPLIER: AnimeTaggerSupplier = AnimeTaggerSupplier;

impl ComponentSupplier for AnimeTaggerSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_tagger("anime".to_string())]
    }

    fn apply(
        &self,
        _props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(AnimeTagger))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}

#[derive(Debug, SdComponent)]
#[component(FileTagger)]
struct AnimeTagger;

impl Display for AnimeTagger {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "anime")
    }
}

#[async_trait]
impl FileTagger for AnimeTagger {
    async fn tag(&self, source_file: &SourceFile) -> Option<String> {
        let filename = source_file.path.file_stem()?.to_str()?;
        if ["特别篇", "[SP]", "[sp]", "special"]
            .iter()
            .any(|marker| filename.contains(marker))
        {
            return Some("special".to_string());
        }
        if filename.contains("OVA") {
            return Some("ova".to_string());
        }
        if filename.contains("OAD") {
            return Some("oad".to_string());
        }
        if filename.contains("剧场版")
            || filename.contains("劇場版")
            || filename.to_lowercase().contains("movie")
        {
            return Some("movie".to_string());
        }

        source_file.path.parent().and_then(|parent| {
            parent.components().find_map(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .filter(|name| is_special_directory(name))
                    .map(|_| "special".to_string())
            })
        })
    }
}

fn is_special_directory(name: &str) -> bool {
    name.eq_ignore_ascii_case("SPs")
        || name.to_lowercase().contains("special")
        || name == "特别篇"
        || name == "特別篇"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    async fn tag(path: &str) -> Option<String> {
        AnimeTagger.tag(&SourceFile::new(PathBuf::from(path))).await
    }

    #[test]
    fn supplier_supports_implicit_construction() {
        assert_eq!(
            SUPPLIER.supply_types(),
            vec![ComponentType::file_tagger("anime".to_string())]
        );
        assert!(SUPPLIER.is_support_no_props());
        assert!(SUPPLIER.apply(&Map::new()).is_ok());
    }

    #[tokio::test]
    async fn tags_filename_categories_in_priority_order() {
        assert_eq!(Some("special".to_string()), tag("Show [SP] OVA.mkv").await);
        assert_eq!(Some("special".to_string()), tag("Show 特别篇.mkv").await);
        assert_eq!(Some("ova".to_string()), tag("Show OVA OAD.mkv").await);
        assert_eq!(Some("oad".to_string()), tag("Show OAD.mkv").await);
        assert_eq!(Some("movie".to_string()), tag("Show MOVIE.mkv").await);
        assert_eq!(Some("movie".to_string()), tag("Show 劇場版.mkv").await);
    }

    #[tokio::test]
    async fn keeps_kotlin_filename_case_rules() {
        assert_eq!(None, tag("Show ova.mkv").await);
        assert_eq!(None, tag("Show Special.mkv").await);
        assert_eq!(Some("movie".to_string()), tag("Show MoViE.mkv").await);
    }

    #[tokio::test]
    async fn tags_special_parent_directories() {
        for path in [
            "Show/SPs/01.mkv",
            "Show/SPECIALS/01.mkv",
            "Show/特别篇/01.mkv",
            "Show/特別篇/nested/01.mkv",
        ] {
            assert_eq!(Some("special".to_string()), tag(path).await, "path={path}");
        }
    }

    #[tokio::test]
    async fn returns_none_without_matching_filename_or_parent() {
        assert_eq!(None, tag("Show/Season 1/01.mkv").await);
        assert_eq!(None, tag("01.mkv").await);
    }
}
