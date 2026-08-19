use common::PLUGIN;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_sdk::component::{
    ComponentType, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, SourceFile,
    SourceFileFilter,
};
use source_downloader_sdk::plugin::Plugin;
use source_downloader_sdk::serde_json::Map;
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

fn component() -> Arc<dyn SourceFileFilter> {
    let supplier = PLUGIN
        .get_component_suppliers()
        .into_iter()
        .find(|supplier| {
            supplier
                .supply_types()
                .contains(&ComponentType::source_file_filter("anime".to_owned()))
        })
        .expect("anime file filter supplier");
    let component: Arc<dyn SdComponent> = supplier
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new())
        .expect("anime file filter component");
    component.as_source_file_filter().expect("source file filter capability")
}
fn benchmark_anime_file_filter(criterion: &mut Criterion) {
    let filter = component();
    let mut group = criterion.benchmark_group("anime_file_filter");
    for count in [16, 256, 4_096] {
        let files: Vec<_> = (0..count)
            .map(|index| {
                let category = if index % 7 == 0 { "NCOP" } else { "Season 01" };
                SourceFile::new(PathBuf::from(format!(
                    "/Anime/{category}/Show - {:03} [1080p].mkv",
                    index % 24
                )))
            })
            .collect();
        group.throughput(Throughput::Elements(count));
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &files,
            |bencher, files| {
                bencher.iter(|| {
                    let mut accepted = 0usize;
                    for file in files {
                        accepted += usize::from(filter.filter(black_box(file)));
                    }
                    black_box(accepted)
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_anime_file_filter);
criterion_main!(benches);
