use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::fixed_source::SUPPLIER;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, Source,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::hint::black_box;
use std::sync::Arc;

fn component(count: usize) -> Arc<dyn Source> {
    let content: Vec<_> = (0..count).map(|index| json!({
        "item": {
            "title": format!("item-{index}"), "link": format!("https://example.test/{index}"),
            "datetime": "2025-01-01T00:00:00Z", "contentType": "video",
            "downloadUri": format!("https://example.test/{index}.mkv")
        },
        "files": [{ "path": format!("item-{index}.mkv") }]
    })).collect();
    let props: Map<String, Value> =
        serde_json::from_value(json!({ "content": content, "offset-mode": true }))
            .unwrap();
    let component: Arc<dyn SdComponent> =
        SUPPLIER.apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props).unwrap();
    component.as_source().unwrap()
}

fn benchmark_fixed_source(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut group = criterion.benchmark_group("fixed_source");
    for count in [16, 256, 4_096] {
        let source = component(count);
        let pointer = source.default_pointer();
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &count,
            |bencher, count| {
                bencher
                    .to_async(&runtime)
                    .iter(|| source.fetch(black_box(pointer.as_ref()), *count as u32));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_fixed_source);
criterion_main!(benches);
