use common::PLUGIN;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_sdk::component::{
    ComponentRootType, EMPTY_COMPONENT_CREATE_CONTEXT, FileTagger, SdComponent,
    SourceFile,
};
use source_downloader_sdk::plugin::Plugin;
use source_downloader_sdk::serde_json::Map;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

fn component() -> Arc<dyn FileTagger> {
    let supplier = PLUGIN
        .get_component_suppliers()
        .into_iter()
        .find(|supplier| {
            supplier.supply_types().iter().any(|kind| {
                kind.root_type == ComponentRootType::FileTagger && kind.name == "simple"
            })
        })
        .expect("simple file tagger supplier");
    let component: Arc<dyn SdComponent> =
        supplier.apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new()).unwrap();
    component.as_file_tagger().unwrap()
}

fn benchmark_simple_file_tagger(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let tagger = component();
    let mut group = criterion.benchmark_group("simple_file_tagger");
    for count in [16, 256, 4_096] {
        let files: Vec<_> = (0..count)
            .map(|index| {
                let extension = ["srt", "ass", "vtt", "txt", "css"][index % 5];
                SourceFile::new(PathBuf::from(format!("subtitle-{index}.{extension}")))
            })
            .collect();
        group.throughput(Throughput::Elements(count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &files,
            |bencher, files| {
                bencher.to_async(&runtime).iter(|| async {
                    let mut tagged = 0usize;
                    for file in files {
                        tagged +=
                            usize::from(tagger.tag(black_box(file)).await.is_some());
                    }
                    black_box(tagged)
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_simple_file_tagger);
criterion_main!(benches);
