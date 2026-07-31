#[cfg(test)]
#[allow(dead_code, unused)]
pub mod test_support {
    use crate::processor_manager::ProcessorManager;
    use crate::source_processor::{SourceProcessor, decode_files_from_compressed};
    use async_trait::async_trait;
    use mockall::mock;
    use parking_lot::Mutex;
    use serde::Deserialize;
    use serde_json::json;
    use source_downloader_sdk::component::*;
    use source_downloader_sdk::serde_json::{Map, Value};
    use source_downloader_sdk::storage::{ProcessingContentQuery, ProcessingStorage};
    use source_downloader_sdk::time::OffsetDateTime;
    use source_downloader_sdk::{SdComponent, SourceItem, http};
    use std::any::Any;
    use std::collections::VecDeque;
    use std::fmt::{Debug, Display, Formatter};
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, LazyLock, OnceLock};
    use storage_sqlite::SeaProcessingStorage;
    use vfs::VfsPath;

    use crate::component_manager::ComponentManager;
    use crate::components::get_build_in_component_supplier;
    use crate::config::{ConfigOperator, YamlConfigOperator};
    use indexmap::IndexMap;
    use itertools::Itertools;
    use jsonpath_rust::JsonPath;
    use source_downloader_sdk::component::ProcessTask;
    use vfs::MemoryFS;

    static _CM: OnceLock<Arc<ComponentManager>> = OnceLock::new();
    static _PM: tokio::sync::OnceCell<ProcessorManager> =
        tokio::sync::OnceCell::const_new();
    static _S: tokio::sync::OnceCell<Arc<SeaProcessingStorage>> =
        tokio::sync::OnceCell::const_new();
    static _C: OnceLock<Arc<YamlConfigOperator>> = OnceLock::new();
    pub static V_PATH: LazyLock<Arc<VfsPath>> =
        LazyLock::new(|| Arc::new(VfsPath::new(MemoryFS::new())));
    pub static CASES: LazyLock<IndexMap<String, Case>> = LazyLock::new(|| {
        let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("processor_cases.yaml");
        let content = std::fs::read(file).expect("Failed to read processor_cases.yaml");
        serde_yaml::from_slice(&content).expect("Failed to de processor cases")
    });
    pub fn cfg() -> &'static Arc<YamlConfigOperator> {
        _C.get_or_init(|| {
            Arc::new(YamlConfigOperator::new("./tests/resources/config.yaml"))
        })
    }
    pub async fn storage() -> &'static Arc<SeaProcessingStorage> {
        _S.get_or_init(|| async {
            Arc::new(
                SeaProcessingStorage::new("sqlite::memory:")
                    .await
                    .expect("Failed to conn database"),
            )
        })
        .await
    }
    fn component_manager() -> &'static Arc<ComponentManager> {
        _CM.get_or_init(|| {
            let m = Arc::new(ComponentManager::new(cfg().clone()));
            m.register_suppliers(get_build_in_component_supplier()).unwrap();
            m.register_suppliers(get_mock_component_suppliers()).unwrap();
            m.register_supplier(Arc::new(VFS_RESOLVER_SUPPLIER)).unwrap();
            m
        })
    }

    pub async fn processor_manager() -> &'static ProcessorManager {
        _PM.get_or_init(|| async {
            ProcessorManager::new(component_manager().clone(), storage().await.clone())
        })
        .await
    }

    pub struct VfsFileSourceSupplier {
        pub root: Arc<VfsPath>,
    }

    pub struct CustomVfsFileResolverSupplier;
    const VFS_RESOLVER_SUPPLIER: CustomVfsFileResolverSupplier =
        CustomVfsFileResolverSupplier {};

    impl ComponentSupplier for CustomVfsFileResolverSupplier {
        fn supply_types(&self) -> Vec<ComponentType> {
            vec![ComponentType::file_resolver("vfs".to_owned())]
        }

        fn apply(
            &self,
            _: &Map<String, Value>,
        ) -> Result<Arc<dyn SdComponent>, ComponentError> {
            Ok(Arc::new(HardCodeVfsFileResolver {}))
        }

        fn is_support_no_props(&self) -> bool {
            true
        }

        fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
            None
        }
    }

    /// 硬编码Mock VFS文件解析器
    #[derive(Debug)]
    pub struct HardCodeVfsFileResolver;
    impl SdComponent for HardCodeVfsFileResolver {
        fn as_item_file_resolver(
            self: Arc<Self>,
        ) -> Result<Arc<dyn ItemFileResolver>, ComponentError> {
            Ok(self)
        }
    }
    impl Display for HardCodeVfsFileResolver {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "vfs")
        }
    }

    #[async_trait]
    impl ItemFileResolver for HardCodeVfsFileResolver {
        async fn resolve_files(&self, source_item: &SourceItem) -> Vec<SourceFile> {
            let path = PathBuf::from(
                source_item
                    .download_uri
                    .to_string()
                    .strip_prefix("file:/")
                    .expect("Failed to parse file URI"),
            );
            // case for conflict
            if source_item.title == "conflict" {
                return vec![
                    SourceFile::new(path.clone()),
                    SourceFile::new(path.with_file_name("conflict1")),
                ];
            }
            vec![SourceFile::new(path)]
        }
    }

    #[derive(serde::Deserialize, Clone)]
    #[serde(rename_all = "kebab-case")]
    struct MockSourceItem {
        title: String,
        link: String,
        download_uri: String,
        #[serde(default)]
        content_type: Option<String>,
        #[serde(default)]
        attrs: Option<Map<String, Value>>,
        #[serde(default)]
        tags: Option<Vec<String>>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    struct PointedItemConfig {
        pub source_item: SourceItemConfig,
        // raw json
        pub item_pointer: Option<Value>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "kebab-case")]
    struct SourceItemConfig {
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        #[serde(with = "http_serde::option::uri")]
        link: Option<http::Uri>,
        #[serde(default)]
        #[serde(with = "http_serde::option::uri")]
        download_uri: Option<http::Uri>,
        #[serde(default)]
        content_type: Option<String>,
        #[serde(default)]
        attrs: Option<Map<String, Value>>,
        #[serde(default)]
        tags: Option<Vec<String>>,
        #[serde(default)]
        datetime: Option<OffsetDateTime>,
        #[serde(default)]
        identity: Option<String>,
    }

    impl Into<SourceItem> for SourceItemConfig {
        fn into(self) -> SourceItem {
            SourceItem {
                title: self.title.unwrap_or_default(),
                link: self.link.unwrap_or_default(),
                download_uri: self.download_uri.unwrap_or_default(),
                content_type: self.content_type.unwrap_or_default(),
                attrs: self.attrs.unwrap_or_default(),
                tags: self.tags.unwrap_or_default(),
                datetime: self.datetime.unwrap_or(OffsetDateTime::now_utc()),
                identity: self.identity,
            }
        }
    }

    impl Into<PointedItem> for PointedItemConfig {
        fn into(self) -> PointedItem {
            PointedItem {
                source_item: self.source_item.into(),
                item_pointer: Arc::new(MockItemPointer {
                    value: self.item_pointer.unwrap_or(Value::Object(Map::new())),
                }),
            }
        }
    }

    #[derive(Debug)]
    struct MockItemPointer {
        pub value: Value,
    }
    impl ItemPointer for MockItemPointer {
        fn as_any(&self) -> &dyn Any {
            self
        }
    }
    struct MockSourcePointer {
        pub value: Value,
    }
    impl Default for MockSourcePointer {
        fn default() -> Self {
            MockSourcePointer { value: Value::Object(Map::new()) }
        }
    }
    impl SourcePointer for MockSourcePointer {
        fn dump(&self) -> Value {
            self.value.clone()
        }

        fn update(&mut self, _: &SourceItem, item_pointer: &dyn ItemPointer) {
            if let Some(pointer) = item_pointer.as_any().downcast_ref::<MockItemPointer>()
            {
                self.value = pointer.value.clone();
            }
        }

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[derive(Deserialize)]
    struct ComponentFunctionMockConfig {
        // Option, Some, Ok, Err
        returning: ReturningKind,
        value: Option<Value>,
        opt: Option<FunctionOption>,
    }
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "kebab-case")]
    struct FunctionOption {
        once: bool,
        retryable: bool,
        return_once: bool,
    }
    #[derive(Deserialize)]
    enum ReturningKind {
        Ok,
        Err,
        Some,
        None,
    }
    #[derive(Deserialize)]
    struct ComponentMockConfig {
        #[serde(default)]
        fetch: Vec<ComponentFunctionMockConfig>,
    }

    struct FetchBehavior {
        result: Result<Vec<PointedItem>, ProcessingError>,
        consume: bool,
    }

    struct MockSource {
        fetches: Mutex<VecDeque<FetchBehavior>>,
    }

    impl Debug for MockSource {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MockSource").finish()
        }
    }

    impl Display for MockSource {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            write!(f, "mock-source")
        }
    }

    impl SdComponent for MockSource {
        fn as_source(self: Arc<Self>) -> Result<Arc<dyn Source>, ComponentError> {
            Ok(self)
        }
    }

    #[async_trait]
    impl Source for MockSource {
        async fn fetch<'pointer>(
            &self,
            _: &'pointer dyn SourcePointer,
            _: u32,
        ) -> Result<Vec<PointedItem>, ProcessingError> {
            let mut fetches = self.fetches.lock();
            let Some(fetch) = fetches.front() else {
                return Ok(Vec::new());
            };
            let result = fetch.result.clone();
            if fetch.consume {
                fetches.pop_front();
            }
            result
        }

        fn default_pointer(&self) -> Box<dyn SourcePointer> {
            Box::new(MockSourcePointer::default())
        }

        fn parse_raw_pointer(&self, value: Value) -> Box<dyn SourcePointer> {
            Box::new(MockSourcePointer { value })
        }
    }

    struct MockSourceSupplier {}

    impl ComponentSupplier for MockSourceSupplier {
        fn supply_types(&self) -> Vec<ComponentType> {
            vec![ComponentType::source("mock".to_owned())]
        }

        fn apply(
            &self,
            props: &Map<String, Value>,
        ) -> Result<Arc<dyn SdComponent>, ComponentError> {
            let cfg = serde_json::from_value::<ComponentMockConfig>(Value::Object(
                props.clone(),
            ))
            .map_err(|error| ComponentError::new(error.to_string()))?;
            let fetches = cfg
                .fetch
                .into_iter()
                .map(Self::build_fetch_behavior)
                .collect::<Result<VecDeque<_>, _>>()?;
            Ok(Arc::new(MockSource { fetches: Mutex::new(fetches) }))
        }

        fn is_support_no_props(&self) -> bool {
            true
        }

        fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
            None
        }
    }

    impl MockSourceSupplier {
        fn build_fetch_behavior(
            fetch: ComponentFunctionMockConfig,
        ) -> Result<FetchBehavior, ComponentError> {
            let option = fetch.opt.unwrap_or_default();
            let result = match fetch.returning {
                ReturningKind::Ok => {
                    Ok(Self::build_pointed_items(fetch.value.unwrap_or_default())?)
                }
                ReturningKind::Err if option.retryable => {
                    Err(ProcessingError::retryable("Mock retryable"))
                }
                ReturningKind::Err => {
                    Err(ProcessingError::non_retryable("Mock non-retryable"))
                }
                ReturningKind::Some | ReturningKind::None => {
                    return Err(ComponentError::new("Returning type not match"));
                }
            };
            Ok(FetchBehavior { result, consume: option.once || option.return_once })
        }

        fn build_pointed_items(value: Value) -> Result<Vec<PointedItem>, ComponentError> {
            let items: Vec<PointedItemConfig> =
                serde_json::from_value(value).map_err(|error| error.to_string())?;
            Ok(items.into_iter().map(Into::into).collect())
        }
    }

    struct MockComponentSupplier {}

    impl ComponentSupplier for MockComponentSupplier {
        fn supply_types(&self) -> Vec<ComponentType> {
            let name = "mock";
            vec![
                ComponentType::file_resolver(name.to_owned()),
                ComponentType::variable_provider(name.to_owned()),
                ComponentType::downloader(name.to_owned()),
                ComponentType::file_mover(name.to_owned()),
            ]
        }

        fn apply(
            &self,
            _: &Map<String, Value>,
        ) -> Result<Arc<dyn SdComponent>, ComponentError> {
            let mut mock = MockComponent::new();
            Self::apply_file_mover(&mut mock)?;
            Ok(Arc::new(mock))
        }

        fn is_support_no_props(&self) -> bool {
            true
        }

        fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
            None
        }
    }

    impl MockComponentSupplier {
        fn apply_file_mover(mock: &mut MockComponent) -> Result<(), ComponentError> {
            mock.expect_exists()
                .returning(|files| files.iter().map(|_| false).collect_vec());
            Ok(())
        }
    }

    impl SdComponent for MockComponent {
        fn as_item_file_resolver(
            self: Arc<Self>,
        ) -> Result<Arc<dyn ItemFileResolver>, ComponentError> {
            Ok(self)
        }
        fn as_downloader(self: Arc<Self>) -> Result<Arc<dyn Downloader>, ComponentError> {
            Ok(self)
        }
        fn as_file_mover(self: Arc<Self>) -> Result<Arc<dyn FileMover>, ComponentError> {
            Ok(self)
        }
        fn as_variable_provider(
            self: Arc<Self>,
        ) -> Result<Arc<dyn VariableProvider>, ComponentError> {
            Ok(self)
        }
    }
    impl Display for MockComponent {
        fn fmt<'a>(&self, f: &mut Formatter<'a>) -> std::fmt::Result {
            write!(f, "mock")
        }
    }

    mock! {
        #[derive(Debug)]
        pub Component {}
        #[async_trait]
        impl ItemFileResolver for Component {
            async fn resolve_files(&self, item: &SourceItem) -> Vec<SourceFile>;
        }
        #[async_trait]
        impl VariableProvider for Component {
            fn accuracy(&self) -> i32 { 1 }
            async fn item_variables(&self, item: &SourceItem) -> std::collections::HashMap<String, String>;
            async fn file_variables(
                &self,
                item: &SourceItem,
                item_variables: &PatternVariables,
                files: &[SourceFile],
            ) -> Vec<PatternVariables>;
            async fn extract_from(&self, item: &SourceItem, value: &str) -> Option<std::collections::HashMap<String, Value>>;
            fn primary_variable_name(&self) -> Option<String>;
        }
        #[async_trait]
        impl FileMover for Component {
            fn exists<'a>(&self, path: &'a [&'a PathBuf]) -> Vec<bool>;
        }
    }

    #[async_trait]
    impl Downloader for MockComponent {
        async fn submit(&self, task: &DownloadTask) -> Result<(), ProcessingError> {
            Ok(())
        }

        fn default_download_path(&self) -> &str {
            "/downloads"
        }

        async fn cancel(
            &self,
            item: &SourceItem,
            files: &[SourceFile],
        ) -> Result<(), ProcessingError> {
            Ok(())
        }
    }

    #[allow(dead_code)]
    pub fn get_mock_component_suppliers() -> Vec<Arc<dyn ComponentSupplier>> {
        vec![Arc::new(MockSourceSupplier {}), Arc::new(MockComponentSupplier {})]
    }
    //     ==========

    #[derive(Deserialize)]
    pub struct Case {
        // test case files to be created
        pub files: Vec<CaseFile>,
        // assertions to be applied on result json
        pub assertions: Vec<Assertion>,
    }
    #[derive(Deserialize)]
    pub struct CaseFile {
        // relative path of the file
        pub path: String,
        // file content
        pub content: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "kebab-case")]
    pub struct Assertion {
        // JSON path
        pub select: String,
        #[serde(default)]
        // whether to allow empty result set
        pub allow_empty: bool,
        // assertions to be applied on each selected node
        pub asserts: Vec<AssertExpr>,
    }
    #[derive(Deserialize)]
    pub struct AssertExpr {
        // JSON path
        pub path: Option<String>,
        pub pointer: Option<String>,
        // expected value
        pub equals: Option<Value>,
        pub length: Option<usize>,
        pub exists: Option<bool>,
    }

    #[derive(Debug)]
    pub struct AssertionError {
        pub message: String,
        pub context: Vec<String>,
    }

    impl AssertionError {
        pub fn new(msg: impl Into<String>) -> Self {
            Self { message: msg.into(), context: Vec::new() }
        }

        pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
            self.context.push(ctx.into());
            self
        }
    }

    impl Display for AssertionError {
        fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
            for ctx in self.context.iter().rev() {
                writeln!(f, "  at {}", ctx)?;
            }
            write!(f, "Assertion failed: {}", self.message)
        }
    }

    pub fn apply_case_files(root_path: &VfsPath, files: &[CaseFile]) {
        root_path.create_dir_all().unwrap();
        for file in files.into_iter() {
            let path = root_path.join(&file.path).unwrap();
            let parent = path.parent();
            if !parent.exists().unwrap() {
                parent.create_dir_all().unwrap();
            }
            let mut f = path.create_file().unwrap();
            if let Some(content) = &file.content {
                f.write_all(content.as_bytes()).unwrap();
            }
        }
    }

    pub fn apply_assertion(
        node: &Value,
        asserts: &Vec<AssertExpr>,
    ) -> Result<(), AssertionError> {
        for assert in asserts {
            let target = if let Some(pointer) = &assert.pointer {
                node.pointer(pointer).ok_or_else(|| {
                    AssertionError::new(format!("JSONPointer not found: {}", pointer))
                })?
            } else if let Some(path) = &assert.path {
                let mut cur = node;
                for seg in path.trim_start_matches('.').split('.') {
                    cur = cur.get(seg).ok_or_else(|| {
                        AssertionError::new(format!(
                            "Path segment '{}' not found in '{}'",
                            seg, path
                        ))
                    })?;
                }
                cur
            } else {
                node
            };

            if let Some(expected) = &assert.equals {
                if target != expected {
                    return Err(AssertionError::new(format!(
                        "equals failed, expected {}, got {}",
                        expected, target
                    )));
                }
            }

            if let Some(len) = assert.length {
                let arr = target
                    .as_array()
                    .ok_or_else(|| AssertionError::new("target is not an array"))?;

                if arr.len() != len {
                    return Err(AssertionError::new(format!(
                        "length failed, expected {}, got {}",
                        len,
                        arr.len()
                    )));
                }
            }

            if let Some(true) = assert.exists {
                if target.is_null() {
                    return Err(AssertionError::new("expected value to exist"));
                }
            }
        }
        Ok(())
    }

    pub async fn build_result_json(
        storage: &Arc<SeaProcessingStorage>,
        name: &str,
    ) -> Value {
        let contents = storage
            .query_processing_content(&ProcessingContentQuery {
                processor_name: Some(vec![name.to_string()]),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut res = Vec::new();
        for content in contents {
            let files = storage
                .find_file_contents(content.id.unwrap())
                .await
                .unwrap()
                .map(|bytes| decode_files_from_compressed(&bytes).unwrap())
                .unwrap_or_default();

            let mut value = serde_json::to_value(content).unwrap();
            value["files"] = serde_json::to_value(files).unwrap();
            res.push(value);
        }
        json!(res)
    }

    pub fn assert_processor(name: &str, pm: &ProcessorManager) -> Arc<SourceProcessor> {
        let w = pm.get_processor(name).expect("Processor wrapper not found");
        let p = w.processor.clone();
        match p {
            None => {
                panic!(
                    "Processor instance not found cause {}",
                    w.error_message.as_ref().unwrap().to_string()
                )
            }
            Some(p) => p,
        }
    }
}
