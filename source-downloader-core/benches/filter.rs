use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::expression_file_content_filter::SUPPLIER as FILE_FILTER_SUPPLIER;
use source_downloader_core::components::expression_item_content_filter::SUPPLIER as ITEM_CONTENT_FILTER_SUPPLIER;
use source_downloader_core::components::expression_item_filter::SUPPLIER as ITEM_FILTER_SUPPLIER;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, FileContent, ItemContent,
    SdComponent,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use source_downloader_sdk::storage::ProcessingStatus;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

fn expression_component(
    supplier: &dyn ComponentSupplier,
    exclusions: &[&str],
    inclusions: &[&str],
) -> Arc<dyn SdComponent> {
    let props = serde_json::from_value::<Map<String, Value>>(json!({
        "exclusions": exclusions,
        "inclusions": inclusions,
    }))
    .expect("benchmark expression configuration must be an object");
    supplier
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props)
        .expect("benchmark expressions must compile")
}

fn source_item() -> SourceItem {
    serde_json::from_value(json!({
        "title": "show-042-S01E042-1080p",
        "link": "https://example.com/items/042/season/01",
        "datetime": "2025-01-02T03:04:05Z",
        "contentType": "video",
        "downloadUri": "https://example.com/download/042",
        "attrs": { "priority": 2, "source": "benchmark" },
        "tags": ["benchmark", "video"],
        "identity": null
    }))
    .expect("benchmark source item must be valid")
}

fn file_contents() -> Vec<FileContent> {
    [
        ("show-042-S01E042.mkv", 1_073_741_824_u64, "video"),
        ("show-042-S01E042.srt", 65_536_u64, "subtitle"),
    ]
    .into_iter()
    .map(|(name, size, kind)| FileContent {
        download_path: PathBuf::from("/downloads"),
        file_download_path: PathBuf::from("/downloads/show-042").join(name),
        attrs: serde_json::from_value(json!({ "size": size, "kind": kind }))
            .expect("benchmark file attributes must be an object"),
        tags: vec![kind.to_owned()],
        ..Default::default()
    })
    .collect()
}

fn benchmark_expression_components(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("benchmark runtime");
    let item = source_item();
    let files = file_contents();
    let item_variables = HashMap::from([
        ("series".to_owned(), "show-042".to_owned()),
        ("season".to_owned(), "S01".to_owned()),
        ("episode".to_owned(), "E042".to_owned()),
        ("quality".to_owned(), "1080p".to_owned()),
    ]);

    let item_filter = expression_component(
        &ITEM_FILTER_SUPPLIER,
        &["item.contentType == 'audio'", "'blocked' in item.tags"],
        &[
            "item.title.matches('^show-[0-9]{3}-S[0-9]{2}E[0-9]{3}-1080p$')",
            "item.contentType == 'video'",
            "'benchmark' in item.tags",
            "item.attrs.source == 'benchmark'",
        ],
    )
    .as_source_item_filter()
    .expect("source item filter capability");
    let item_content_filter = expression_component(
        &ITEM_CONTENT_FILTER_SUPPLIER,
        &["item.vars.quality == '720p'"],
        &[
            "item.vars.series.matches('show-[0-9]{3}')",
            "item.vars.season.matches('S[0-9]{2}')",
            "item.vars.episode.matches('E[0-9]{3}')",
            "item.files.size() == 2",
        ],
    )
    .as_item_content_filter()
    .expect("item content filter capability");
    let file_filter = expression_component(
        &FILE_FILTER_SUPPLIER,
        &["file.extension == 'tmp'", "file.attrs.size == 0"],
        &[
            "file.name.matches('^show-[0-9]{3}-S[0-9]{2}E[0-9]{3}.*')",
            "file.extension in ['mkv', 'srt']",
            "file.attrs.size > 1024",
        ],
    )
    .as_file_content_filter()
    .expect("file content filter capability");

    let mut group = criterion.benchmark_group("expression_components");
    group.throughput(Throughput::Elements(1));
    group.bench_function("source_item_filter", |bencher| {
        bencher.to_async(&runtime).iter(|| item_filter.filter(&item));
    });
    let item_content = ItemContent {
        source_item: &item,
        file_contents: &files,
        item_variables: &item_variables,
        status: ProcessingStatus::WaitingToRename,
    };
    group.bench_function("item_content_filter", |bencher| {
        bencher.to_async(&runtime).iter(|| item_content_filter.filter(&item_content));
    });
    group.bench_function("file_content_filter", |bencher| {
        bencher.iter(|| file_filter.filter(&files[0]));
    });
    group.finish();
}

criterion_group!(benches, benchmark_expression_components);
criterion_main!(benches);
