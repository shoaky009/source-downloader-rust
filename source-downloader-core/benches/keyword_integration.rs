use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::keyword_integration::SUPPLIER;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, SourceItemFilter,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::hint::black_box;
use std::sync::Arc;

fn component(count: usize) -> Arc<dyn SourceItemFilter> {
    let keywords: Vec<_> =
        (0..count).map(|index| format!("token-{index:05}|1|alias-{index}")).collect();
    let props: Map<String, Value> =
        source_downloader_sdk::serde_json::from_value(json!({ "keywords": keywords }))
            .unwrap();
    let component: Arc<dyn SdComponent> = SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props)
        .expect("keyword integration component");
    component.as_source_item_filter().expect("item filter capability")
}

fn item(title: String) -> SourceItem {
    source_downloader_sdk::serde_json::from_value(json!({
        "title": title, "link": "https://example.test/show",
        "datetime": "2025-01-01T00:00:00Z", "contentType": "video",
        "downloadUri": "https://example.test/show.mkv"
    }))
    .unwrap()
}

fn benchmark_keyword_integration(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut group = criterion.benchmark_group("keyword_integration");
    for count in [16, 256, 4_096] {
        let filter = component(count);
        let item = item(format!("release for token-{:05} in 1080p", count - 1));
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &item,
            |bencher, item| {
                bencher.to_async(&runtime).iter(|| filter.filter(black_box(item)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_keyword_integration);
criterion_main!(benches);
