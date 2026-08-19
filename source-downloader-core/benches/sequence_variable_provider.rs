use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::sequence_variable_provider::SUPPLIER;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, SourceFile,
    VariableProvider,
};
use source_downloader_sdk::serde_json::Map;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

fn component() -> Arc<dyn VariableProvider> {
    let component: Arc<dyn SdComponent> = SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new())
        .expect("sequence variable provider component");
    component.as_variable_provider().expect("variable provider capability")
}

fn item() -> SourceItem {
    source_downloader_sdk::serde_json::from_value(
        source_downloader_sdk::serde_json::json!({
            "title": "show", "link": "https://example.test/show",
            "datetime": "2025-01-01T00:00:00Z", "contentType": "video",
            "downloadUri": "https://example.test/show.mkv"
        }),
    )
    .unwrap()
}

fn benchmark_sequence_variable_provider(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let provider = component();
    let item = item();
    let item_variables = std::collections::HashMap::new();
    let mut group = criterion.benchmark_group("sequence_variable_provider");
    for count in [16, 256, 4_096] {
        let files: Vec<_> = (0..count)
            .map(|index| SourceFile::new(PathBuf::from(format!("file-{index}.mkv"))))
            .collect();
        group.throughput(Throughput::Elements(count as u64));
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
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_sequence_variable_provider);
criterion_main!(benches);
