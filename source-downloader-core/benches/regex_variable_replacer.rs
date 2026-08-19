use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::regex_variable_replacer::SUPPLIER;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, VariableReplacer,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::hint::black_box;
use std::sync::Arc;

fn component() -> Arc<dyn VariableReplacer> {
    let props: Map<String, Value> = serde_json::from_value(json!({
        "regex": "(?i)(?:bdrip|web-dl|webrip)", "replacement": "SOURCE"
    }))
    .unwrap();
    let component: Arc<dyn SdComponent> = SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props)
        .expect("regex variable replacer component");
    component.as_variable_replacer().expect("variable replacer capability")
}

fn benchmark_regex_variable_replacer(criterion: &mut Criterion) {
    let replacer = component();
    let mut group = criterion.benchmark_group("regex_variable_replacer");
    for repeats in [8, 128, 2_048] {
        let input = "Show BDRip Episode WEB-DL Alternate WEBRip ".repeat(repeats);
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(input.len()),
            &input,
            |bencher, input| {
                bencher.iter(|| replacer.replace("source", black_box(input.clone())));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_regex_variable_replacer);
criterion_main!(benches);
