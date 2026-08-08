# ComponentSupplier 配置错误

在 `ComponentSupplier::apply` 解析或校验 `props` 时执行本文件。目标是让错误直接回答：**哪一个配置路径、拿到了什么、期望什么**。

## 输出契约

错误格式：

```text
Invalid configuration at '<path>': <cause>
```

示例：

```text
Invalid configuration at 'chat-id': invalid type: integer `1`, expected a string
Invalid configuration at 'headers.authorization': invalid type: sequence, expected a string
Invalid configuration at 'rules[2].matcher.tags': invalid type: string "anime", expected a sequence
```

路径使用配置的 wire name（例如 kebab-case 字段），并用单引号包裹。对象字段使用 `.`，数组元素使用 `[index]`。

该格式同时约束反序列化后的业务校验，例如正则、URI、枚举值和数值范围。集合元素的校验循环必须保留原始索引，并报告 `items[index].field`。

以下结果不满足契约：

```text
Invalid Telegram source config: invalid type: integer `1`, expected a string
Invalid configuration
Failed to parse properties
```

## 非嵌套配置

非嵌套对象可以继续使用原始 `serde_json` API，但只有在每个失败分支都能附加具体字段名时才合格。逐字段读取时，把字段名写进 `ComponentError`：

```rust
let timeout = props
    .get("timeout")
    .map(|value| serde_json::from_value(value.clone()))
    .transpose()
    .map_err(|error| {
        ComponentError::new(format!(
            "Invalid configuration at 'timeout': {error}"
        ))
    })?
    .unwrap_or(DEFAULT_TIMEOUT);
```

整对象 `serde_json::from_value(Value::Object(props.clone()))` 的类型错误通常不包含字段名；仅包装 `{error}` 不合格。若不能保证原始 serde 错误带字段，改用路径追踪解析。

## 嵌套配置

结构体包含嵌套对象、集合元素或枚举载荷时，使用 `serde_path_to_error`，避免手工猜测路径。将它收敛在共享 helper 中，统一：

- 输入 `Map<String, Value>`；
- 路径格式；
- 根级错误的显示；
- `ComponentError` 前缀。

调用点只提供 `props`。组件类型由 `ComponentManager` 和 processor 创建错误的上层上下文负责：

```rust
let config = deserialize_component_config::<TelegramSourceConfig>(props)?;
```

共享 helper 的错误必须使用 `serde_path_to_error::Error::path()`，并保留底层 serde cause。路径为空时使用 `'<root>'`；不能丢掉原始类型、缺字段或未知字段信息。

## 完成标准

对每个新增或修改的解析器，至少验证：

- [ ] 顶层字段类型错误包含该字段的 wire name；
- [ ] 缺失字段包含缺失字段名；
- [ ] 嵌套字段错误包含完整对象路径；
- [ ] 集合元素错误包含索引；
- [ ] 错误保留 serde 的实际值类型和期望类型；
- [ ] 日志和 `ComponentWrapper::error_message` 不再添加会掩盖路径的重复包装。

配置结构没有对应形态时，可省略不适用的测试；所有实际可失败形态都必须有路径证据。
