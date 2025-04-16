use core::*;

fn main() {
    let container = Box::new(ObjectWrapperContainer::new());
    let component_manager = ComponentManager::new(container);
    let mut app = CoreApplication { component_manager };
    app.start();

    let manager = app.component_manager;
    println!("{}", manager)
}
