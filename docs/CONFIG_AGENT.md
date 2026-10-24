# Web 配置助手

在 `source-downloader-webui` 的「配置助手」页面（`/agent`）使用。后端为本仓库的 `applications/web`。

## 使用

1. 点击「模型连接」，选择 OpenAI Responses 或 Chat Completions，填写 Base URL、模型名称和 API Key。模型需支持流式回复和 function calling。Base URL 示例为 `https://api.openai.com/v1`，不含最终的 `/responses` 或 `/chat/completions`。
2. 描述下载或整理目标。助手查询实际注册的组件与已有配置，优先复用；遇到关键选择时显示选项和输入框。密码、Cookie、私密订阅 URL 通过凭据输入框填写，模型只得到 `$secret:<id>` 引用。
3. 在方案预览中检查来源 → 整理 → 保存目录、目录/文件名模板、分组例外和执行时间。模板保留变量表达式，不代表真实 dry-run 结果。方案说明及完整配置默认折叠；使用「调整」按钮继续对话，发送修改需求后旧 Review 自动失效。必要问题逐个展示，可以返回修改，最后统一提交。
4. 点击「确认方案并创建」。后端按实例、组件、处理器的顺序执行已审核版本，默认启用处理器。注册的触发器可能开始下载或整理；没有触发器的处理器需手动触发。
5. 若中途失败，已创建的资源保留。当前页面可重试未完成步骤；如需修改方案，助手重新查询资源并提交新 Review。

## 会话与设置

聊天、模型上下文、临时凭据、Review 和执行进度只保存在内存中，不写数据库或文件，不提供历史记录或恢复。离开页面/新对话会关闭临时会话；刷新后开始新会话。服务器为异常断开的会话设置两小时闲置清理条件，创建新会话时回收；重启后全部消失。

模型连接设置单独保存在数据目录的 `agent-model.json`，使用原子替换和仅当前用户可读写的临时文件保存。设置接口不返回 API Key。相同 Base URL 下保存空 Key 会保留原 Key，改变服务地址时不会沿用旧 Key。更改接口协议后需开始新对话。

停止生成会中止模型请求，确认前不会创建资源。确认后的创建不因浏览器断开而回滚；执行期间应留在页面查看结果。会话结束后仍可通过正常组件和处理器页面检查已创建资源。

## 执行约束

- 模型仅能查询能力、追问、提交方案，没有直接创建或删除工具；普通聊天中的「确认」不能触发执行。
- 后端确认接口只接受服务器已有方案的版本号，不接受客户端替换后的配置。
- 新消息使旧 Review 失效；实际配置在 Review 后发生变化时必须重新生成方案。
- 实例/组件/处理器仅新建，已有资源可复用，不覆盖已有同名配置。共享 supplier 的多个接口只需要一份组件配置。
- Review 前只做静态检查：已注册能力、组件参数 Schema、引用、共享接口、兼容性、Processor 字段、表达式及模板语法。不会构造组件、连接下载器或运行任务。
- 远程认证、服务可达性、磁盘权限以及实际变量识别仍以创建/运行结果为准。
- 单轮最多 16 次模型请求，HTTP 请求超时 180 秒；返回错误后可在当前会话继续。

## 维护

`source-downloader-core/src/config_agent.rs` 负责方案校验和创建操作；`applications/web/src/service/agent` 负责临时会话、两种模型协议、流式接口及 Review 闸门。组件知识来自真实 supplier 元数据和 `recipes.md`。增加组件时应维护参数 Schema、描述、变量来源、共享接口和兼容规则。

模型协议参考 [OpenAI Function calling](https://developers.openai.com/api/docs/guides/function-calling)。Responses 保留工具调用返回的 reasoning/output items 并回传 function_call_output；Chat Completions 使用 assistant.tool_calls 与 tool_call_id。两种协议共享业务工具，不依赖额外 Agent 框架。

验证命令：

```sh
cargo test -p web -p source-downloader-core
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

前端仓库使用 `bun run typecheck` 和 `bun run build`。后端集成测试使用本地模拟模型服务，覆盖两种协议的追问、凭据引用、零写入 Review、过期确认、真实组件/处理器创建以及失败后重试。Mikan 和 Fanbox 方案通过真实已注册组件的静态验证，测试不调用远程下载服务。
