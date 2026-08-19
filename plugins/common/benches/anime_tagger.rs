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
                kind.root_type == ComponentRootType::FileTagger && kind.name == "anime"
            })
        })
        .unwrap();
    let component: Arc<dyn SdComponent> =
        supplier.apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new()).unwrap();
    component.as_file_tagger().unwrap()
}

fn benchmark_anime_tagger(criterion: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let tagger = component();
    let mut group = criterion.benchmark_group("anime_tagger");
    group.throughput(Throughput::Elements(1));
    for path in [
        "Show [SP] OVA.mkv",
        "Show/Season 1/Show MOVIE.mkv",
        "Show/Season 1/episode-01.mkv",
    ] {
        let file = SourceFile::new(PathBuf::from(path));
        group.bench_with_input(
            BenchmarkId::from_parameter(path),
            &file,
            |bencher, file| {
                bencher.to_async(&runtime).iter(|| tagger.tag(black_box(file)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_anime_tagger);
criterion_main!(benches);
