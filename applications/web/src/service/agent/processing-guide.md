# 处理阶段与配置规则

组件参数及输出以 inspect_capability(kind=component) 返回的 metadata 为准。outputSchema 描述可能的运行时数据，不是配置输入；不能把输出变量填进 props。

## 引用

引用格式为 type:name；仅 catalog 标明 noProps=true 的组件可不声明命名配置而直接引用 type。一个 supplier 的 types 列出它提供的所有接口，同一份命名配置可用于这些接口。相同 typeName 的不同 supplier 不一定共享配置。先复用已有资源，再创建缺少的资源。

## 数据可用阶段

1. source 生成条目：item 的 title、link、downloadUri、datetime、contentType、attrs、tags；额外 attrs / tags 由 source 输出说明决定。
2. item 过滤在条目变量提取前执行，不能使用尚未生成的 provider 变量；通过后再提取条目变量。
3. resolver 生成 SourceFile，先进行 source-file 过滤，再由 tagger 补充标签，然后 file-grouping 分组。这些都早于文件变量提取；source-file 过滤不能依赖 tagger 稍后生成的标签。分组表达式可用 file.name、file.extension、file.tags、file.attrs、fileCount，没有 item 或 file.vars。
4. provider 在组内生成文件变量，随后构造命名变量；variable-process 的 input 是 item.title、file.name 等字段路径，不是 CEL 表达式，chain 调用 provider 的 extract_from，output.key-mapping 可重命名输出。extractedVariables 不表示能直接放入普通 variable-providers。
5. file-content 过滤才可使用 file.vars、file.paths；命名模板使用解析后的变量。属性和标签的生产阶段必须早于消费阶段。

## 分组与命名

- 分组按首个匹配规则生效，未匹配文件使用全局模板。tags 要求包含所有指定标签；tags 与 expression-matching 同时配置时以 tags 为准。空规则不是兜底，expression-matching="true" 才匹配全部。
- provider 在各文件组内分别运行，分组可能影响编号等输出。具体行为查看 provider 的 outputSchema。
- 排除表达式 true 时排除；包含列表非空时所有表达式都为 true 才保留。
- `{表达式}` 使用 CEL，`:{表达式}` 为可选表达式，失败时省略该表达式。provider 变量可直接引用，文件变量优先于同名条目变量；`variable-name-replace` 可显式统一名称。
- 模板中的 item.date 是条目发布时间/本地文件日期；不要与 provider 的 date 混淆。
- filename-pattern 空字符串保留原文件名；save-path-pattern 相对处理器 save-path。常规重命名保留原扩展名。默认变量错误策略 STAY 会保留原文件名或目录位置。静态校验不保证实际变量提取成功。

## 执行

triggers=[] 为手动触发。具体时间格式、启动行为、时区查阅对应 trigger 元数据。目录、联网认证、权限及真实效果未经过静态校验验证。输入与输出目录应避免相互包含；是否移动、保留或清理源文件由用户选择及组件实际行为决定。
