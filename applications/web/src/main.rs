use sdk::component::ComponentRootType;

fn main() {
    core::add(1, 2);
    println!("{}", ComponentRootType::Downloader.name())
}
