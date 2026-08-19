use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::force_trimmer::SUPPLIER;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, Trimmer,
};
use source_downloader_sdk::serde_json::Map;
use std::hint::black_box;
use std::sync::Arc;

fn component() -> Arc<dyn Trimmer> {
    let component: Arc<dyn SdComponent> = SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new())
        .expect("force trimmer component");
    component.as_trimmer().expect("trimmer capability")
}

fn benchmark_force_trimmer(criterion: &mut Criterion) {
    let trimmer = component();
    let mut group = criterion.benchmark_group("force_trimmer");
    for size in [64, 1_024, 16_384] {
        let input = "示例-title-".repeat(size / 13 + 1);
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &input,
            |bencher, input| {
                bencher.iter(|| trimmer.trim(black_box(input.clone()), black_box(size)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_force_trimmer);
criterion_main!(benches);
