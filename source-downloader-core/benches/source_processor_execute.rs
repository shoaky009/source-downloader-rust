use async_trait::async_trait;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use source_downloader_core::components::fixed_source::SUPPLIER as FIXED_SOURCE_SUPPLIER;
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

impl FileMover for NoopIo {
    fn exists(&self, paths: &[&PathBuf]) -> Vec<bool> {
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
                    "title": format!("item-{index}"),
                    "link": format!("https://example.com/items/{index}"),
                    "datetime": "1970-01-01T00:00:00Z",
                    "contentType": "benchmark",
                    "downloadUri": format!("https://example.com/download/{index}"),
                    "attrs": {},
                    "tags": [],
                    "identity": null
                },
                "files": []
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
        ProcessorOptions {
            parallelism: 16,
            retry_backoff: Duration::ZERO,
            ..Default::default()
        },
    )
}

fn benchmark_execute(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("benchmark runtime");
    let temp = tempfile::tempdir().expect("benchmark temporary directory");

    criterion.bench_function("source_processor/execute/fixed_256", |bencher| {
        bencher.to_async(&runtime).iter_batched(
            || processor(temp.path()),
            |processor| async move {
                processor.run().await.expect("benchmark processor run");
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(benches, benchmark_execute);
criterion_main!(benches);
