use common::PLUGIN;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentRootType, EMPTY_COMPONENT_CREATE_CONTEXT, FileContent, FileContentStatus,
    FileReplacementDecider, InProcessingItem, SdComponent, SourceFile,
};
use source_downloader_sdk::plugin::Plugin;
use source_downloader_sdk::serde_json::Map;
use source_downloader_sdk::storage::ProcessingStatus;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

fn component() -> Arc<dyn FileReplacementDecider> {
    let supplier = PLUGIN
        .get_component_suppliers()
        .into_iter()
        .find(|supplier| {
            supplier.supply_types().iter().any(|kind| {
                kind.root_type == ComponentRootType::FileReplacementDecider
                    && kind.name == "anime"
            })
        })
        .unwrap();
    let component: Arc<dyn SdComponent> =
        supplier.apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new()).unwrap();
    component.as_file_replacement_decider().unwrap()
}

fn item(title: &str) -> SourceItem {
    source_downloader_sdk::serde_json::from_value(
        source_downloader_sdk::serde_json::json!({
            "title": title, "link": "https://example.test/show",
            "datetime": "2025-01-01T00:00:00Z", "contentType": "video",
            "downloadUri": "https://example.test/show.mkv"
        }),
    )
    .unwrap()
}

fn content() -> FileContent {
    FileContent {
        download_path: PathBuf::new(),
        file_download_path: PathBuf::new(),
        source_save_path: PathBuf::new(),
        pattern_variables: Default::default(),
        file_save_path_pattern: String::new(),
        filename_pattern: String::new(),
        tags: Vec::new(),
        attrs: Map::new(),
        file_uri: None,
        target_save_path: PathBuf::new(),
        target_filename: String::new(),
        exist_target_path: None,
        errors: Vec::new(),
        status: FileContentStatus::Undetected,
        target_path: OnceLock::new(),
        data: None,
        processed_variables: None,
    }
}

fn benchmark_anime_replacement_decider(criterion: &mut Criterion) {
    let decider = component();
    let current_file = content();
    let existing = SourceFile::new(PathBuf::from("show.mkv"));
    let cases = [
        ("version_upgrade", item("Show [1080v3]"), Some(item("Show [1080v2]"))),
        (
            "bilibili_downgrade",
            item("Bilibili Show [1080v9]"),
            Some(item("Show [1080v2]")),
        ),
        ("first_release", item("Show [1080v2]"), None),
    ];
    let id = None;
    let identity = None;
    let variables = Default::default();
    let files = Vec::new();
    let rename_times = 0;
    let status = ProcessingStatus::Renamed;
    let mut group = criterion.benchmark_group("anime_replacement_decider");
    group.throughput(Throughput::Elements(1));
    for (name, current, previous_item) in cases {
        let previous = previous_item.as_ref().map(|source_item| InProcessingItem {
            id: &id,
            processor_name: "benchmark",
            item_hash: "hash",
            item_identity: &identity,
            source_item,
            item_variables: &variables,
            file_contents: &files,
            rename_times: &rename_times,
            status: &status,
            failure_reason: None,
        });
        group.bench_with_input(
            BenchmarkId::new("decide", name),
            &current,
            |bencher, current| {
                bencher.iter(|| {
                    decider.should_replace(
                        black_box(current),
                        black_box(&current_file),
                        previous.as_ref(),
                        black_box(&existing),
                    )
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_anime_replacement_decider);
criterion_main!(benches);
