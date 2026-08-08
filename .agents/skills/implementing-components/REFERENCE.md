# 组件实现参考

仅在需要查对类型、模板或代码位置时加载本文件。

## 能力与类型

| 能力 trait | `ComponentType` 构造函数 |
| --- | --- |
| `Trigger` | `trigger(name)` |
| `Source` | `source(name)` |
| `Downloader` | `downloader(name)` |
| `ItemFileResolver` | `file_resolver(name)` |
| `FileMover` | `file_mover(name)` |
| `VariableProvider` | `variable_provider(name)` |
| `ProcessListener` | `listener(name)` |
| `SourceItemFilter` | `item_filter(name)` |
| `SourceFileFilter` | `source_file_filter(name)` |
| `ItemContentFilter` | `item_content_filter(name)` |
| `FileContentFilter` | `file_content_filter(name)` |
| `FileTagger` | `file_tagger(name)` |
| `FileReplacementDecider` | `file_replacement_decider(name)` |
| `FileExistsDetector` | `file_exists_detector(name)` |
| `VariableReplacer` | `variable_replacer(name)` |
| `Trimmer` | `trimmer(name)` |

`name` 是实现名。组件引用格式为 `root-type:implementation-name` 或 `root-type:implementation-name:instance-name`。

## Supplier 模板

```rust
pub struct ExampleSupplier;
pub const SUPPLIER: ExampleSupplier = ExampleSupplier;

impl ComponentSupplier for ExampleSupplier {
    fn supply_types(&self) -> Vec<ComponentType> {
        vec![ComponentType::downloader("example".into())]
    }

    fn apply(
        &self,
        context: &dyn ComponentCreateContext,
        props: &Map<String, Value>,
    ) -> Result<Arc<dyn SdComponent>, ComponentError> {
        let config = deserialize_component_config::<ExampleConfig>(props)?;
        Ok(Arc::new(ExampleComponent::new(context, config)?))
    }

    fn get_metadata(&self) -> Option<Box<SdComponentMetadata>> {
        None
    }
}
```

## 命名实例依赖

```rust
let instance = context.get_instance(
    &config.client,
    TypeId::of::<ClientInstance>(),
)?;
let client = instance.downcast::<ClientInstance>().map_err(|_| {
    ComponentError::new(format!(
        "Instance '{}' has an incompatible type",
        config.client,
    ))
})?;
```

无实例依赖的测试可使用 `EMPTY_COMPONENT_CREATE_CONTEXT`。

## 派生宏语义

`component-macro/src/lib.rs` 根据属性中 trait 的最后一段名称生成 snake_case 转换方法：

- 普通能力生成 `as_<trait>(self: Arc<Self>) -> Result<Arc<dyn Trait>, ComponentError>` 并返回 `Ok(self)`；
- `Stateful` 生成 `as_stateful(self: Arc<Self>) -> Option<Arc<dyn Stateful>>` 并返回 `Some(self)`；
- 属性中未列出的能力沿用 `SdComponent` 的默认失败实现。

宏要求存在 `#[component(...)]`，但不会替组件实现能力 trait。

## 实现位置

- 契约与 trait：`source-downloader-sdk/src/component.rs`
- 宏：`component-macro/src/lib.rs`
- core 注册：`source-downloader-core/src/components/mod.rs`
- plugin 注册范例：`plugins/telegram/src/lib.rs`
- 最小单能力范例：`plugins/common/src/component/anime_file_filter.rs`
- 带 manager 依赖的 supplier：`source-downloader-core/src/components/composites.rs`
