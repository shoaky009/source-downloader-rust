# 数据源游标描述

Source supplier 使用现有 `SdComponentMetadata.source_pointer_json_schema` 描述游标。客户端通过 `GET /api/component/source/{typeName}/metadata` 的 `sourcePointerJsonSchema` 读取描述，结合 `GET /api/processor/{name}/state` 返回的 `sourceId` 和 `pointer` 渲染字段。

Schema 必须对应 `SourcePointer::dump()` 的实际序列化结果，不能直接从 Rust 类型名称推断表示形式：

- 根节点 `title` 统一为「数据源游标」。字段标题保持简短，`description` 仅补充字段含义或动态键含义，不重复标题，不描述默认值、序列化格式或实现机制。
- 用 `properties` 描述固定字段，`additionalProperties` 描述订阅、作者等动态键，并说明键的组成。
- 正确表达整数、布尔值、字符串、数组和 `null`。`required` 应与实际解析时的必填要求一致。
- RSS 使用 `time` 默认的九整数时间数组；各项独立命名，并使用 draft-07 tuple schema。Mikan 的 `latest` 及 `shows` 中各番组/字幕组的时间使用 RFC3339 字符串，包含时区，可包含小数秒；Schema 声明为 `string` 和 `format: date-time`。Fanbox 的本地时间字符串没有时区，不能声明成 RFC3339 `date-time`。
- 无状态 Source 描述为没有属性且 `additionalProperties: false` 的 object，并使用简短说明「此数据源不使用游标。」`None` 表示未提供描述，客户端继续使用 JSON 编辑。

Mikan 新游标的 `latest` 为当前 UTC 时间，`shows` 为空；旧数组格式或其他无效游标解析失败时，回退到 Unix epoch 且 `shows` 为空。

WebUI 的「数据源游标」支持字段和 JSON 两种编辑方式，保存前按描述校验值。未声明的字段不会因切换视图而被主动剔除；打开表单不会自动填充默认游标。元数据无法取得时保留 JSON 编辑能力。

Dry Run 提供三种选择：初始游标（发送 `{}`）、当前游标（发送 `null`）和自定义游标。初始游标由 Source 决定，不等同于扫描全部历史；演练不会保存修改后的正式游标。

新增或修改 Source 时，应同时校对数据源游标描述及其实际序列化结果。更新描述不改变游标存储格式，也无需调整 SDK 的动态插件接口布局。
