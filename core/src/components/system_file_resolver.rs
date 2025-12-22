use async_trait::async_trait;
use sdk::component::{
    ComponentError, ComponentSupplier, ComponentType, ItemFileResolver, SdComponent,
    SdComponentMetadata, SourceFile,
};
use sdk::{SdComponent, SourceItem};
use serde_json::{Map, Value};
use std::path::PathBuf;
use std::sync::Arc;
use url::Url;
use walkdir::WalkDir;

pub struct SystemFileResolverSupplier;
pub const SUPPLIER: SystemFileResolverSupplier = SystemFileResolverSupplier {};
const INSTANCE: SystemFileResolver = SystemFileResolver {};

impl ComponentSupplier for SystemFileResolverSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::file_resolver("system-file".to_owned())]
    }

    fn apply(&self, _: &Map<String, Value>) -> Result<Arc<dyn SdComponent>, ComponentError> {
        Ok(Arc::new(INSTANCE))
    }

    fn is_support_no_props(&self) -> bool {
        true
    }
    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        todo!()
    }
}

#[derive(SdComponent, Debug)]
#[component(ItemFileResolver)]
struct SystemFileResolver {}

#[async_trait]
impl ItemFileResolver for SystemFileResolver {
    async fn resolve_files(&self, source_item: &SourceItem) -> Vec<SourceFile> {
        let path = Url::parse(&source_item.download_uri.to_string())
            .unwrap()
            .to_file_path()
            // 可能有问题，中文和前缀没处理
            .unwrap_or_else(|_| PathBuf::from(&source_item.download_uri.to_string()));
        if !path.exists() {
            return vec![];
        }
        if path.is_dir() {
            let mut entries: Vec<SourceFile> = WalkDir::new(path)
                .into_iter()
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .map(|e| SourceFile::new(e.into_path()))
                .collect();
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            entries
        } else {
            vec![SourceFile::new(path)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sdk::http::Uri;
    use std::fs;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_resolve_single_file() {
        let dir = tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        fs::write(&file_path, "hello").unwrap();

        let download_uri = Uri::builder()
            .path_and_query(format!("file://{}", file_path.to_str().unwrap()))
            .build()
            .unwrap();
        let item = SourceItem {
            download_uri,
            ..Default::default()
        };

        let resolver = INSTANCE;
        let result = resolver.resolve_files(&item).await;

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, file_path);
    }

    #[tokio::test]
    async fn test_resolve_directory_recursively_and_sorted() {
        let dir = tempdir().unwrap();
        let root = dir.path();

        // 创建目录结构:
        // /b_file.txt
        // /a_dir/c_file.txt
        let a_dir = root.join("a_dir");
        fs::create_dir(&a_dir).unwrap();

        let file_b = root.join("b_file.txt");
        let file_c = a_dir.join("c_file.txt");

        fs::write(&file_b, "b").unwrap();
        fs::write(&file_c, "c").unwrap();

        let download_uri = Uri::builder()
            .path_and_query(format!("file://{}", root.to_str().unwrap()))
            .build()
            .unwrap();
        let item = SourceItem {
            download_uri,
            ..Default::default()
        };

        let resolver = INSTANCE;
        let result = resolver.resolve_files(&item).await;

        // 验证结果数量
        assert_eq!(result.len(), 2);

        // 验证排序：路径应该是升序的
        // 顺序通常是: .../a_dir/c_file.txt, 然后是 .../b_file.txt
        assert!(result[0].path.to_str().unwrap().contains("c_file.txt"));
        assert!(result[1].path.to_str().unwrap().contains("b_file.txt"));
    }

    #[tokio::test]
    async fn test_resolve_with_spaces_in_path() {
        let dir = tempdir().unwrap();
        // 创建带空格的文件名
        let file_path = dir.path().join("my test file.txt");
        fs::write(&file_path, "content").unwrap();

        // URI 中空格会被编码为 %20
        let encoded_path = file_path.to_str().unwrap().replace(" ", "%20");
        let download_uri = Uri::builder()
            .path_and_query(format!("file://{}", encoded_path))
            .build()
            .unwrap();
        let item = SourceItem {
            download_uri,
            ..Default::default()
        };
        let resolver = INSTANCE;
        let result = resolver.resolve_files(&item).await;

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path, file_path);
        assert!(result[0].path.exists());
    }

    #[tokio::test]
    async fn test_resolve_non_existent_path() {
        let download_uri = Uri::builder()
            .path_and_query("file:///non/existent/path")
            .build()
            .unwrap();
        let item = SourceItem {
            download_uri,
            ..Default::default()
        };

        let resolver = INSTANCE;
        let result = resolver.resolve_files(&item).await;

        assert_eq!(result.len(), 0);
    }
}
