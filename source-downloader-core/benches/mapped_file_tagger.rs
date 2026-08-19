use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::mapped_file_tagger::SUPPLIER;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, FileTagger, SdComponent,
    SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value, json};
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

fn component(count: usize) -> Arc<dyn FileTagger> {
    let mapping: Map<String, Value> = (0..count)
        .map(|index| (format!("file-{index}.mkv"), Value::String(format!("tag-{index}"))))
        .collect();
    let props: Map<String, Value> =
        serde_json::from_value(json!({ "mapping": mapping })).unwrap();
    let component: Arc<dyn SdComponent> =
        SUPPLIER.apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &props).unwrap();
    component.as_file_tagger().unwrap()
}

fn benchmark_mapped_file_tagger(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut group = criterion.benchmark_group("mapped_file_tagger");
    for count in [16, 256, 4_096] {
        let tagger = component(count);
        let file =
            SourceFile::new(PathBuf::from(format!("/downloads/file-{}.mkv", count - 1)));
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &file,
            |bencher, file| {
                bencher.to_async(&runtime).iter(|| tagger.tag(black_box(file)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_mapped_file_tagger);
criterion_main!(benches);
