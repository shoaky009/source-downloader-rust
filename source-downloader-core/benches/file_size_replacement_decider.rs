use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use source_downloader_core::components::file_replacement_decider_size::SUPPLIER;
use source_downloader_sdk::SourceItem;
use source_downloader_sdk::component::{
    ComponentSupplier, EMPTY_COMPONENT_CREATE_CONTEXT, FileContent, FileContentStatus,
    FileReplacementDecider, SdComponent, SourceFile,
};
use source_downloader_sdk::serde_json::{Map, Value};
use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;

fn component() -> Arc<dyn FileReplacementDecider> {
    let component: Arc<dyn SdComponent> = SUPPLIER
        .apply(&EMPTY_COMPONENT_CREATE_CONTEXT, &Map::new())
        .expect("file size replacement decider component");
    component.as_file_replacement_decider().expect("replacement decider capability")
}

fn benchmark_file_size_replacement_decider(criterion: &mut Criterion) {
    let decider = component();
    let item: SourceItem = serde_json::from_value(source_downloader_sdk::serde_json::json!({
        "title": "item", "link": "https://example.test/item", "datetime": "2025-01-01T00:00:00Z",
        "contentType": "video", "downloadUri": "https://example.test/item.mkv"
    })).unwrap();
    let mut current = FileContent {
        download_path: PathBuf::new(),
        file_download_path: PathBuf::new(),
        source_save_path: PathBuf::new(),
        pattern_variables: Default::default(),
        file_save_path_pattern: String::new(),
        filename_pattern: String::new(),
        tags: Vec::new(),
        attrs: Map::new(),
        file_uri: None,
        target_save_path: PathBuf::new(),
        target_filename: String::new(),
        exist_target_path: None,
        errors: Vec::new(),
        status: FileContentStatus::Undetected,
        target_path: Default::default(),
        data: None,
        processed_variables: None,
    };
    current.attrs.insert("size".to_owned(), Value::String("2147483648".to_owned()));
    let mut existing = SourceFile::new(PathBuf::from("item.mkv"));
    existing.attrs.insert("size".to_owned(), Value::from(1_073_741_824_u64));
    let mut group = criterion.benchmark_group("file_size_replacement_decider");
    group.throughput(Throughput::Elements(1));
    group.bench_function("mixed_size_types", |bencher| {
        bencher.iter(|| {
            decider.should_replace(
                black_box(&item),
                black_box(&current),
                None,
                black_box(&existing),
            )
        });
    });
    group.finish();
}

criterion_group!(benches, benchmark_file_size_replacement_decider);
criterion_main!(benches);
