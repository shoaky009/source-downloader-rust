use common::PLUGIN;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentRootType, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, SourceFile,
    VariableProvider,
};
use source_downloader_sdk::plugin::Plugin;
use source_downloader_sdk::serde_json::Map;
use std::collections::HashMap;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

fn component() -> Arc<dyn VariableProvider> {
    let supplier = PLUGIN
        .get_component_suppliers()
        .into_iter()
        .find(|supplier| {
            supplier.supply_types().iter().any(|kind| {
                kind.root_type == ComponentRootType::VariableProvider
                    && kind.name == "season"
            })
        })
        .unwrap();
    let component: Arc<dyn SdComponent> =
        supplier.apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new()).unwrap();
    component.as_variable_provider().unwrap()
}
fn item() -> SourceItem {
    source_downloader_sdk::serde_json::from_value(source_downloader_sdk::serde_json::json!({"title":"Show Season 12","link":"https://example.test/show","datetime":"2025-01-01T00:00:00Z","contentType":"video","downloadUri":"https://example.test/show.mkv"})).unwrap()
}
fn benchmark_season_variable_provider(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let provider = component();
    let item = item();
    let item_variables = HashMap::new();
    let mut group = criterion.benchmark_group("season_variable_provider");
    for count in [1, 16, 256] {
        let files: Vec<_> = (0..count)
            .map(|index| {
                SourceFile::new(PathBuf::from(format!("Show/S12/episode-{index:03}.mkv")))
            })
            .collect();
        group.throughput(Throughput::Elements(count));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &files,
            |bencher, files| {
                bencher.to_async(&runtime).iter(|| {
                    provider.file_variables(
                        black_box(&item),
                        black_box(&item_variables),
                        black_box(files),
                    )
                })
            },
        );
    }
    group.finish();
}
criterion_group!(benches, benchmark_season_variable_provider);
criterion_main!(benches);
