use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::regex_trimmer::SUPPLIER;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, Trimmer,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::hint::black_box;
use std::sync::Arc;

fn component() -> Arc<dyn Trimmer> {
    let props: Map<String, Value> = serde_json::from_value(json!({
        "regex": "(?i)\\[[^]]*(?:sample|ad|promo)[^]]*\\]"
    }))
    .unwrap();
    let component: Arc<dyn SdComponent> = SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props)
        .expect("regex trimmer component");
    component.as_trimmer().expect("trimmer capability")
}

fn benchmark_regex_trimmer(criterion: &mut Criterion) {
    let trimmer = component();
    let mut group = criterion.benchmark_group("regex_trimmer");
    for repeats in [4, 64, 1_024] {
        let input = "[Group] Show [Sample] Episode 01 [1080p] [Promo] ".repeat(repeats);
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(input.len()),
            &input,
            |bencher, input| {
                bencher.iter(|| trimmer.trim(black_box(input.clone()), 0));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_regex_trimmer);
criterion_main!(benches);
