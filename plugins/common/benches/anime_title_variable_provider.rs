use common::PLUGIN;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentRootType, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, VariableProvider,
};
use source_downloader_sdk::plugin::Plugin;
use source_downloader_sdk::serde_json::Map;
use std::hint::black_box;
use std::sync::Arc;

fn component() -> Arc<dyn VariableProvider> {
    let supplier = PLUGIN
        .get_component_suppliers()
        .into_iter()
        .find(|supplier| {
            supplier.supply_types().iter().any(|kind| {
                kind.root_type == ComponentRootType::VariableProvider
                    && kind.name == "anime-title"
            })
        })
        .unwrap();
    let component: Arc<dyn SdComponent> =
        supplier.apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new()).unwrap();
    component.as_variable_provider().unwrap()
}
fn item(title: String) -> SourceItem {
    source_downloader_sdk::serde_json::from_value(source_downloader_sdk::serde_json::json!({"title":title,"link":"https://example.test/item","datetime":"2025-01-01T00:00:00Z","contentType":"video","downloadUri":"https://example.test/item.mkv"})).unwrap()
}
fn benchmark(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let provider = component();
    let mut group = criterion.benchmark_group("anime_title_variable_provider");
    for repeats in [1, 8, 64] {
        let value = item(format!(
            "{}Sample Show / 示例动画 | サンプル作品 [1080p][HEVC][CHS]",
            "[Group] ".repeat(repeats)
        ));
        group.throughput(Throughput::Bytes(value.title.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(value.title.len()),
            &value,
            |bencher, value| {
                bencher
                    .to_async(&runtime)
                    .iter(|| provider.item_variables(black_box(value)))
            },
        );
    }
    group.finish();
}
criterion_group!(benches, benchmark);
criterion_main!(benches);
