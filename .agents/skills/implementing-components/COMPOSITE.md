# 复合组件

当一个实例提供多个组件能力时执行本文件。主要范例：`plugins/telegram/src/integration.rs` 的 `TelegramIntegration`。

这里的复合组件是“一个实例，多种能力”；`source-downloader-core/src/components/composites.rs` 则是按配置串联多个组件的另一种适配器。

## 三处对齐

Supplier 列出同一实现名的全部能力：

```rust
fn supply_types(&self) -> Vec<ComponentType> {
    vec![
        ComponentType::file_resolver("telegram".into()),
        ComponentType::downloader("telegram".into()),
    ]
}
```

实现类型列出同一能力集合，并按需加入 `Stateful`：

```rust
#[derive(source_downloader_sdk::SdComponent)]
#[component(ItemFileResolver, Downloader, Stateful)]
struct TelegramIntegration {
    // 各能力共享的依赖和状态。
}
```

随后为同一类型分别实现 `ItemFileResolver`、`Downloader`、`Stateful`、`Debug` 和 `Display`。

## Manager 语义

`ComponentManager`：

- 将同一个 supplier 注册到 `supply_types()` 返回的每个类型；任一类型重复都会拒绝注册；
- 首次查找时调用 `apply`，随后让所有能力 wrapper 共享其 `Arc<dyn SdComponent>`；
- 并发首次查找可能重复执行 `apply`，但只缓存一个结果，因此 `apply` 的正确性不能依赖“全局只执行一次”；
- 销毁任一能力的同名实例时，一并移除该 supplier 其他能力下的实例。

配置查找按 `supply_types()` 顺序使用第一个同名配置。一个复合实例只配置一次；列表顺序保持稳定，最自然的主能力置前。若多个能力分区存在同名配置，必须消除冲突并保留单一配置来源。

## 共享状态

所有能力操作同一个实例：

- 客户端、计数器、取消句柄等状态直接共享；
- 共享字段满足所有能力的 `Send + Sync` 要求；
- 异步路径避免持有同步锁跨越 `.await`；
- `Stateful` 输出只包含可序列化、可观察且不敏感的数据；
- `Display` 返回稳定的实现名；`Debug` 对客户端和凭据脱敏。

## 完整核对表

完成复合实现前逐项验证：

- [ ] 每个 `supply_types()` 项都有对应的 `#[component(...)]` 能力；
- [ ] 每个宏能力都有真实 trait impl；
- [ ] 所有供应类型使用同一预期实现名；
- [ ] 任一能力首次创建后，其他能力取得同一个共享实例；
- [ ] 配置只有一个归属，`supply_types()` 顺序明确；
- [ ] 并发创建不会产生不可逆的重复副作用；
- [ ] 销毁一个能力会移除其他能力下的同名实例；
- [ ] `Debug`、`Display` 和 `Stateful` 不泄露敏感数据。

## 参考位置

- 完整复合实现：`plugins/telegram/src/integration.rs`
- 插件注册：`plugins/telegram/src/lib.rs`
- core 复合能力实现：`source-downloader-core/src/components/fixed_source.rs`
- 另一复合能力实现：`source-downloader-core/src/components/keyword_integration.rs`
- 共享 wrapper 与销毁行为：`source-downloader-core/src/component_manager.rs`
