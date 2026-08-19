use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::regex_variable_provider::SUPPLIER;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, VariableProvider,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::hint::black_box;
use std::sync::Arc;

fn component(count: usize) -> Arc<dyn VariableProvider> {
    let regexes: Vec<_> = (0..count)
        .map(|index| {
            json!({
                "name": format!("value{index}"),
                "regex": format!("token-{index:03}"),
                "field": "title"
            })
        })
        .collect();
    let props: Map<String, Value> =
        serde_json::from_value(json!({ "regexes": regexes })).unwrap();
    let component: Arc<dyn SdComponent> = SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props)
        .expect("regex variable provider component");
    component.as_variable_provider().expect("variable provider capability")
}

fn benchmark_regex_variable_provider(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut group = criterion.benchmark_group("regex_variable_provider");
    for count in [4, 32, 256] {
        let provider = component(count);
        let item = SourceItem {
            title: (0..count)
                .map(|index| format!("token-{index:03}"))
                .collect::<Vec<_>>()
                .join(" "),
            ..Default::default()
        };
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &item,
            |bencher, item| {
                bencher
                    .to_async(&runtime)
                    .iter(|| provider.item_variables(black_box(item)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_regex_variable_provider);
criterion_main!(benches);
