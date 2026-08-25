use async_trait::async_trait;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use source_downloader_core::components::expression_file_content_filter::SUPPLIER as FILE_EXPRESSION_FILTER_SUPPLIER;
use source_downloader_core::components::expression_item_content_filter::SUPPLIER as ITEM_CONTENT_EXPRESSION_FILTER_SUPPLIER;
use source_downloader_core::components::expression_item_filter::SUPPLIER as ITEM_EXPRESSION_FILTER_SUPPLIER;
use source_downloader_core::components::fixed_source::SUPPLIER as FIXED_SOURCE_SUPPLIER;
use source_downloader_core::components::regex_variable_provider::SUPPLIER as REGEX_VARIABLE_PROVIDER_SUPPLIER;
use source_downloader_core::source_processor::{ProcessorOptions, SourceProcessor};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentSupplier, DownloadTask, Downloader, EMPTY_COMPONENT_CREATE_CONTEXT,
    FileMover, ProcessTask, ProcessingError, SdComponent, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, json};
use source_downloader_sdk::storage::{
    Error as StorageError, ProcessingContent, ProcessingContentQuery, ProcessingStorage,
    ProcessingTargetPath, ProcessorSourceState,
};
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const ITEM_COUNT: usize = 256;

#[derive(Debug)]
struct NoopIo {
    download_path: String,
}

impl Display for NoopIo {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("benchmark-noop-io")
    }
}

impl SdComponent for NoopIo {}

#[async_trait]
impl Downloader for NoopIo {
    async fn submit(&self, _: &DownloadTask) -> Result<(), ProcessingError> {
        Ok(())
    }

    fn default_download_path(&self) -> &str {
        &self.download_path
    }

    async fn cancel(
        &self,
        _: &SourceItem,
        _: &[SourceFile],
    ) -> Result<(), ProcessingError> {
        Ok(())
    }
}

#[async_trait]
impl FileMover for NoopIo {
    async fn move_file(
        &self,
        _: &SourceItem,
        _: &source_downloader_sdk::component::FileContent,
    ) -> Result<(), ProcessingError> {
        Ok(())
    }
    async fn exists(&self, paths: &[&PathBuf]) -> Vec<bool> {
        vec![false; paths.len()]
    }
}

#[derive(Default)]
struct NoopStorage;

#[async_trait]
impl ProcessingStorage for NoopStorage {
    async fn save_processing_content(
        &self,
        _: &ProcessingContent,
    ) -> Result<i64, StorageError> {
        Ok(1)
    }

    async fn processing_content_exists(
        &self,
        _: &str,
        _: &str,
    ) -> Result<bool, StorageError> {
        Ok(false)
    }

    async fn delete_processing_content(&self, _: i64) -> Result<(), StorageError> {
        Ok(())
    }

    async fn delete_processing_contents_by_processor(
        &self,
        _: &str,
    ) -> Result<u64, StorageError> {
        Ok(0)
    }

    async fn find_by_name_and_hash(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<ProcessingContent>, StorageError> {
        Ok(None)
    }

    async fn find_content_by_id(
        &self,
        _: i64,
    ) -> Result<Option<ProcessingContent>, StorageError> {
        Ok(None)
    }

    async fn query_processing_content(
        &self,
        _: &ProcessingContentQuery,
    ) -> Result<Vec<ProcessingContent>, StorageError> {
        Ok(Vec::new())
    }

    async fn save_file_contents(&self, _: i64, _: Vec<u8>) -> Result<(), StorageError> {
        Ok(())
    }

    async fn find_file_contents(&self, _: i64) -> Result<Option<Vec<u8>>, StorageError> {
        Ok(None)
    }

    async fn find_processor_source_state(
        &self,
        _: &str,
        _: &str,
    ) -> Result<Option<ProcessorSourceState>, StorageError> {
        Ok(None)
    }

    async fn save_processor_source_state(
        &self,
        state: &ProcessorSourceState,
    ) -> Result<ProcessorSourceState, StorageError> {
        Ok(state.clone())
    }

    async fn save_paths(&self, _: Vec<ProcessingTargetPath>) -> Result<(), StorageError> {
        Ok(())
    }

    async fn delete_paths_by_processor(&self, _: &str) -> Result<u64, StorageError> {
        Ok(0)
    }
}

fn fixed_source() -> Arc<dyn SdComponent> {
    let content = (0..ITEM_COUNT)
        .map(|index| {
            json!({
                "item": {
                    "title": format!("show-{index:03}-S01E{index:03}-1080p"),
                    "link": format!("https://example.com/items/{index:03}/season/01"),
                    "datetime": "1970-01-01T00:00:00Z",
                    "contentType": "video",
                    "downloadUri": format!("https://example.com/download/{index:03}"),
                    "attrs": { "priority": index % 4, "source": "benchmark" },
                    "tags": ["benchmark", "video"],
                    "identity": null
                },
                "files": [
                    {
                        "path": format!("show-{index:03}-S01E{index:03}.mkv"),
                        "attrs": { "size": 1073741824, "kind": "video" },
                        "tags": ["video", "primary"]
                    },
                    {
                        "path": format!("show-{index:03}-S01E{index:03}.srt"),
                        "attrs": { "size": 65536, "kind": "subtitle" },
                        "tags": ["subtitle"]
                    }
                ]
            })
        })
        .collect::<Vec<_>>();
    let props = serde_json::from_value::<
        Map<String, source_downloader_sdk::serde_json::Value>,
    >(json!({ "content": content, "offset-mode": false }))
    .expect("fixed source benchmark configuration must be valid JSON");
    FIXED_SOURCE_SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props)
        .expect("fixed source benchmark configuration must be valid")
}

fn component_props(
    value: source_downloader_sdk::serde_json::Value,
) -> Map<String, source_downloader_sdk::serde_json::Value> {
    serde_json::from_value(value)
        .expect("benchmark component configuration must be an object")
}

fn regex_variable_provider(
    regexes: source_downloader_sdk::serde_json::Value,
) -> Arc<dyn source_downloader_sdk::component::VariableProvider> {
    REGEX_VARIABLE_PROVIDER_SUPPLIER
        .apply(
            &EMPTY_COMPONENT_CREATE_CONTEXT,
            &component_props(json!({ "regexes": regexes, "primary": "series" })),
        )
        .expect("regex variable provider benchmark configuration must be valid")
        .as_variable_provider()
        .expect("regex variable provider capability")
}

fn expression_filter(
    supplier: &dyn ComponentSupplier,
    exclusions: &[&str],
    inclusions: &[&str],
) -> Arc<dyn SdComponent> {
    supplier
        .apply(
            &EMPTY_COMPONENT_CREATE_CONTEXT,
            &component_props(json!({
                "exclusions": exclusions,
                "inclusions": inclusions,
            })),
        )
        .expect("expression filter benchmark configuration must be valid")
}

fn benchmark_options() -> ProcessorOptions {
    let title_provider = regex_variable_provider(json!([
        { "name": "series", "regex": "show-[0-9]{3}", "field": "title" },
        { "name": "season", "regex": "S[0-9]{2}", "field": "title" },
        { "name": "episode", "regex": "E[0-9]{3}", "field": "title" },
        { "name": "quality", "regex": "[0-9]{4}p", "field": "title" },
        { "name": "mediaType", "regex": "video", "field": "contentType" }
    ]));
    let uri_provider = regex_variable_provider(json!([
        { "name": "series", "regex": "items/[0-9]{3}", "field": "link" },
        { "name": "season", "regex": "season/[0-9]{2}", "field": "link" },
        { "name": "episode", "regex": "[0-9]{3}", "field": "downloadUri" },
        { "name": "mediaType", "regex": "video", "field": "contentType" }
    ]));
    let item_filter = expression_filter(
        &ITEM_EXPRESSION_FILTER_SUPPLIER,
        &["item.contentType == 'audio'", "'blocked' in item.tags"],
        &[
            "item.title.matches('^show-[0-9]{3}-S[0-9]{2}E[0-9]{3}-1080p$')",
            "item.contentType == 'video'",
            "'benchmark' in item.tags",
            "item.attrs.source == 'benchmark'",
        ],
    )
    .as_source_item_filter()
    .expect("item expression filter capability");
    let item_content_filter = expression_filter(
        &ITEM_CONTENT_EXPRESSION_FILTER_SUPPLIER,
        &["item.vars.mediaType == 'audio'", "item.vars.quality == '720p'"],
        &[
            "item.vars.series.matches('show-[0-9]{3}|items/[0-9]{3}')",
            "item.vars.season.matches('S[0-9]{2}|season/[0-9]{2}')",
            "item.vars.episode.matches('E?[0-9]{3}')",
            "item.vars.mediaType == 'video'",
        ],
    )
    .as_item_content_filter()
    .expect("item content expression filter capability");
    let file_content_filter = expression_filter(
        &FILE_EXPRESSION_FILTER_SUPPLIER,
        &["file.extension == 'tmp'", "file.attrs.size == 0"],
        &[
            "file.name.matches('^show-[0-9]{3}-S[0-9]{2}E[0-9]{3}.*')",
            "file.extension in ['mkv', 'srt']",
            "file.attrs.size > 1024",
        ],
    )
    .as_file_content_filter()
    .expect("file content expression filter capability");

    ProcessorOptions {
        variable_providers: vec![title_provider, uri_provider],
        item_filters: vec![item_filter],
        item_content_filters: vec![item_content_filter],
        file_content_filters: vec![file_content_filter],
        parallelism: 16,
        retry_backoff: Duration::ZERO,
        ..Default::default()
    }
}

fn processor(download_path: &Path) -> SourceProcessor {
    let source_component = fixed_source();
    let io =
        Arc::new(NoopIo { download_path: download_path.to_string_lossy().into_owned() });
    SourceProcessor::new(
        "execute-benchmark".to_owned(),
        "fixed:benchmark".to_owned(),
        download_path.into(),
        source_component.clone().as_source().expect("fixed source capability"),
        source_component.as_item_file_resolver().expect("fixed file resolver capability"),
        io.clone(),
        io,
        Arc::new(NoopStorage),
        None,
        Default::default(),
        Default::default(),
        benchmark_options(),
    )
}

fn benchmark_execute(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("benchmark runtime");
    let temp = tempfile::tempdir().expect("benchmark temporary directory");

    criterion.bench_function(
        "source_processor/execute/expression_variables_256",
        |bencher| {
            bencher.to_async(&runtime).iter_batched(
                || processor(temp.path()),
                |processor| async move {
                    processor.run().await.expect("benchmark processor run");
                },
                BatchSize::SmallInput,
            );
        },
    );
}

criterion_group!(benches, benchmark_execute);
criterion_main!(benches);
