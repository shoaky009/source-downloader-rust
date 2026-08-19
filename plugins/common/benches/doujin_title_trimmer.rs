use common::PLUGIN;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_sdk::component::{
    ComponentRootType, EMPTY_COMPONENT_CREATE_CONTEXT, SdComponent, Trimmer,
};
use source_downloader_sdk::plugin::Plugin;
use source_downloader_sdk::serde_json::Map;
use std::hint::black_box;
use std::sync::Arc;

fn component() -> Arc<dyn Trimmer> {
    let supplier = PLUGIN
        .get_component_suppliers()
        .into_iter()
        .find(|supplier| {
            supplier.supply_types().iter().any(|kind| {
                kind.root_type == ComponentRootType::Trimmer && kind.name == "doujin"
            })
        })
        .expect("doujin title trimmer supplier");
    let component: Arc<dyn SdComponent> = supplier
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new())
        .expect("doujin title trimmer component");
    component.as_trimmer().expect("trimmer capability")
}

fn benchmark_doujin_title_trimmer(criterion: &mut Criterion) {
    let trimmer = component();
    let mut group = criterion.benchmark_group("doujin_title_trimmer");
    for ads in [2, 16, 128] {
        let input = format!("{}正文标题。后续说明", "【推广信息】".repeat(ads));
        group.throughput(Throughput::Bytes(input.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(ads),
            &input,
            |bencher, input| {
                bencher.iter(|| trimmer.trim(black_box(input.clone()), black_box(8)));
            },
        );
    }
    group.finish();
}

criterion_group!(benches, benchmark_doujin_title_trimmer);
criterion_main!(benches);
