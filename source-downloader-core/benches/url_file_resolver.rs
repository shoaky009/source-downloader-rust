use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::url_file_resolver::SUPPLIER;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, ItemFileResolver, SdComponent,
};
use source_downloader_sdk::serde_json::Map;
use std::hint::black_box;
use std::sync::Arc;

fn component() -> Arc<dyn ItemFileResolver> {
    let component: Arc<dyn SdComponent> = SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new())
        .expect("URL file resolver component");
    component.as_item_file_resolver().expect("file resolver capability")
}

fn item(uri: &str) -> SourceItem {
    source_downloader_sdk::serde_json::from_value(
        source_downloader_sdk::serde_json::json!({
            "title": "show", "link": "https://example.test/show",
            "datetime": "2025-01-01T00:00:00Z", "contentType": "video",
            "downloadUri": uri
        }),
    )
    .unwrap()
}

fn benchmark_url_file_resolver(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let resolver = component();
    let mut group = criterion.benchmark_group("url_file_resolver");
    for (name, item) in [
        ("short", item("https://example.test/video.mkv")),
        (
            "encoded",
            item(
                "https://example.test/releases/Show%20S01E01%20%5B1080p%5D.mkv?token=abc",
            ),
        ),
        ("empty_path", item("https://example.test/")),
    ] {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::new("resolve", name),
            &item,
            |bencher, item| {
                bencher
                    .to_async(&runtime)
                    .iter(|| resolver.resolve_files(black_box(item)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_url_file_resolver);
criterion_main!(benches);
