# 可选组合示例

这些示例用于理解组件如何组合，不是支持场景的白名单，也不是可直接套用的最终配置。组件属性、输出变量、attrs、tags、默认值和限制只以当前 supplier 元数据及兼容规则为准。模板中使用某个变量前必须查询对应生产组件的 outputSchema。

| 用户目标 | 可探索的组合 |
| --- | --- |
| 从 Mikan 订阅下载新番 | mikan source → torrent resolver → qbittorrent downloader / mover；按所需命名信息选择变量 provider |
| 整理本地动画 | system-file source / downloader → system-file resolver → general 或 hardlink mover；根据输入命名与联网需求选择 provider |
| Fanbox 订阅备份 | fanbox source / resolver → http downloader → general mover；按内容类型制定分组与命名 |

新场景使用相同步骤：拆解来源、文件解析、下载/整理、变量提取、筛选分组及调度需求；查询 catalog；检查候选组件输入/输出 schema 与共享接口；复用已有配置；只询问缺少的关键选择；生成并校验方案。缺少必要能力时明确说明，不能用相似示例假装支持。

例如用户要保留图片、保留附件原名、排除封面时，应先查 resolver 是否提供内容类型或标签，再查编号 provider 的作用范围，最后在相应处理阶段组合规则，不假设每个来源都有统一的 type、image 或 cover 字段。
