use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::windows_path_replacer::SUPPLIER;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, VariableReplacer,
};
use source_downloader_sdk::serde_json::Map;
use std::hint::black_box;
use std::sync::Arc;

fn component() -> Arc<dyn VariableReplacer> {
    let component: Arc<dyn SdComponent> = SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new())
        .expect("windows path replacer component");
    component.as_variable_replacer().expect("variable replacer capability")
}

fn benchmark_windows_path_replacer(criterion: &mut Criterion) {
    let replacer = component();
    let mut group = criterion.benchmark_group("windows_path_replacer");
    for repeats in [8, 128, 2_048] {
        let input = "Season:01/Show? Episode*01 <1080p>|name\\file".repeat(repeats);
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(input.len()),
            &input,
            |bencher, input| {
                bencher.iter(|| replacer.replace("path", black_box(input.clone())));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_windows_path_replacer);
criterion_main!(benches);
