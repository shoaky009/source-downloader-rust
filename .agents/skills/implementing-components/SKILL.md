---
name: implementing-components
description: 实现 SourceDownloader 组件。用于新增或修改 ComponentSupplier/SdComponent、注册单能力组件，或构建同一实例提供多种能力的复合组件。
---

# 实现 SourceDownloader 组件

以**契约对齐**约束每次实现：

- `ComponentSupplier::supply_types()` 声明运行时可配置、可查找的能力；
- `#[component(...)]` 声明 `Arc<dyn SdComponent>` 可转换到的能力；
- 实现类型提供每个已声明的能力 trait。

三处必须一一对应。

## 1. 锁定契约

读取 `source-downloader-sdk/src/component.rs` 中目标 trait 的当前签名，再读取同层最接近的现有组件和所属注册入口。确定：

- 能力 trait 集合及对应 `ComponentType`；
- 实现名、配置归属和是否允许无配置创建；
- 组件位于 SDK、core、common plugin 或独立 plugin；
- 需要通过 `ComponentCreateContext` 获取的实例依赖；
- 需要更新的注册入口和测试入口。

需要能力与构造函数对照、最小模板或代码位置时，读取 [REFERENCE.md](REFERENCE.md)。

**完成标准：** 每个能力、类型名、配置来源、依赖和注册点都有唯一答案，目标 trait 不存在未确认的签名。

## 2. 实现供应器

实现零大小 supplier 和 `SUPPLIER` 常量；只有 supplier 自身需要运行时依赖时才使用带字段的 supplier。

在 `apply` 中按顺序：

1. 将 `props` 反序列化为 `#[serde(rename_all = "kebab-case")]` 配置类型；
2. 通过 `ComponentCreateContext` 获取命名实例依赖并校验 `TypeId`；
3. 将配置失败映射为带字段路径的 `ComponentError`，实例依赖失败保留实例名；
4. 返回完整可用的 `Arc<dyn SdComponent>`。

**配置错误分支：** 只要 `apply` 解析 `props`，必须读取并执行 [CONFIG_ERRORS.md](CONFIG_ERRORS.md)。非嵌套对象可使用原始 serde API，但错误必须指出具体字段；嵌套对象、集合或枚举载荷使用 `serde_path_to_error` 追踪完整路径。

组件无需配置且应自动创建时覆写 `is_support_no_props() -> true`。按 UI/状态需求返回 `SdComponentMetadata`，无元数据时返回 `None`。

**完成标准：** `supply_types()` 完整声明能力；有效配置创建真实组件；每条无效配置或依赖路径返回可定位的 `ComponentError`，配置错误至少包含出错字段的 wire name；`is_support_no_props` 与配置语义一致。

## 3. 实现组件能力

组件类型满足 `Any + Send + Sync + Debug + Display`，并派生：

```rust
#[derive(Debug, source_downloader_sdk::SdComponent)]
#[component(TargetTrait)]
struct ExampleComponent { /* shared state */ }
```

宏只生成 `SdComponent::as_*` 转换；为属性中的每个能力编写真实 trait 实现。异步 trait 使用 `source_downloader_sdk::async_trait::async_trait`。创建期错误使用 `ComponentError`，处理期错误使用 `ProcessingError`。`Stateful` 仅暴露可观察且不敏感的状态。

**复合分支：** 当 `supply_types()` 返回多个类型，或同一结构体实现多个能力时，先读取并执行 [COMPOSITE.md](COMPOSITE.md) 的共享实例规则和完整核对表。

**完成标准：** `supply_types()`、`#[component(...)]` 和实际 trait impl 完全对齐；每个已供应能力的 `as_*` 转换成功；`Debug`、`Display` 和状态输出不泄露凭据。

## 4. 注册供应器

- core 内置组件：在 `source-downloader-core/src/components/mod.rs` 声明模块并加入 `get_build_in_component_supplier()`。
- plugin 组件：在插件 `lib.rs` 声明模块并由 `Plugin::get_component_suppliers()` 返回 supplier。

保持依赖方向为 SDK → core/plugin 实现；需要 `ComponentManager` 的 supplier 通过构造参数接收它。

**完成标准：** 从真实注册入口可以枚举新 supplier；每个 `ComponentType` 无重复注册；组件可按配置引用取得。

## 5. 验证契约

按变更范围执行：

1. 有效配置创建组件；缺字段、字段类型错误和实例类型错误均返回预期错误，配置错误包含准确字段路径；
2. 对每个供应类型从 manager 取得组件并调用对应 `as_*`；
3. 复合组件从不同能力入口验证共享状态和同名实例销毁行为；
4. 验证所属插件或内置列表暴露 supplier；
5. 运行覆盖变更契约的精确测试，再运行所属 crate 测试或 `cargo check -p <crate>`；
6. 执行 `cargo fmt --all --check`；修改宏或 SDK 公共契约时检查整个 workspace。

**完成标准：** 每个已声明能力、配置错误分支、注册入口及复合共享语义都有执行证据，且适用的编译、测试与格式检查通过。
