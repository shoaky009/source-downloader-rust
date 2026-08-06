# Kotlin common-plugin 组件 Rust 完全复刻执行计划

## 1. 目标与范围

目标：把 Kotlin `common-plugin` 中下列 29 个 Supplier 及其实际行为迁移到 Rust `plugins/common`，保持配置键、默认值、组件类型、变量名、Pointer 状态推进、过滤/解析优先级和错误语义一致；HTTP 请求统一使用 `reqwest`。

本计划只描述执行步骤，不在本阶段实现代码。Kotlin 源路径：

`C:/Users/shoaky/GitRepo/source-downloader/plugins/common-plugin/src/main/kotlin/io/github/shoaky/sourcedownloader`

Rust 目标模块：

- `plugins/common/src/component/`：Supplier 与实现。
- `plugins/common/src/http/`：按站点划分的请求/响应 DTO 与请求函数；这是实现内部 seam，不复制 Kotlin 的 `HookedApiClient`/`BaseRequest` 类层次。
- `plugins/common/src/instance/`：仅保留确实需要跨组件共享配置、连接状态或缓存的客户端实例。
- `plugins/common/src/lib.rs`：统一注册 Supplier/InstanceFactory。
- `plugins/common/Cargo.toml` 与 workspace `Cargo.toml`：依赖。

## 2. 已确认的 Rust 基线与先决修正

### 2.1 已有接口

Rust SDK 已有 `ComponentSupplier`、`Source`、`ItemFileResolver`、`Downloader`、`AsyncDownloader`、`SourceFileFilter`、`FileTagger`、`FileReplacementDecider`、`FileExistsDetector`、`VariableProvider`、`Trimmer`、`ItemPointer`、`SourcePointer`。`Source::fetch` 接受 `&dyn SourcePointer` 和 `limit`，返回 `Vec<PointedItem>`；`SourcePointer::update` 是 Pointer 迭代状态的唯一提交 seam。

当前 `plugins/common` 仅注册 `MikanSourceSupplier`。新增 Supplier 必须全部加入 `component/mod.rs` 和 `CommonPlugin::get_component_suppliers()`，不保留未注册实现。

### 2.2 契约差异处理结论

1. **组合规则暂缓**：Kotlin Supplier 的 `rules()` 主要用于配置防呆；本轮不扩展 Rust `ComponentSupplier`，也不实现 `allowSource`、`allowDownloader`、`allowFileResolver`。组件行为和注册不依赖这些规则。
2. **Downloader 完成状态已异步化**：`AsyncDownloader::is_finished` 已改为 async，调用方已迁移，可直接执行 qBittorrent/Transmission HTTP 状态查询。
3. **Resolver 已返回错误**：`ItemFileResolver::resolve_files` 已改为 `Result<Vec<SourceFile>, ProcessingError>`，调用方已迁移；HTTP 和解析失败必须继续向上传播，不能返回空列表伪装成功。
4. **变量提取已异步化**：`VariableProvider::extract_from` 及变量处理链已改为 async，可直接执行 Anime/BgmTv/Chii/DLsite/TMDB/Season 的 HTTP 查询，不得阻塞 Tokio runtime。
5. **元数据暂缓**：本轮 Supplier 的 `get_metadata()` 继续返回 `None`，不实现 JSON Schema 或描述信息。
6. **配置统一 kebab-case**：Rust 所有配置键都使用 kebab-case，不保留 camelCase 特例；`onlyHighResolution` 使用 `only-high-resolution`。配置结构使用 `serde(rename_all = "kebab-case")`，Supplier 不堆叠手工 `Value` 提取。

## 3. HTTP 模块设计与 Mock 策略

### 3.1 Rust 风格 HTTP seam

- 全部请求由注入的 `reqwest::Client` 发出；生产构造器接收 `Client` 和 base URL，Supplier 使用共享的默认 `Client`。
- 每个站点采用小型配置/句柄，例如 `BgmTvHttp { client, base_url, token }`，或直接采用自由函数 `search_subject(&Client, &Url, ...)`。不复制 Kotlin 的泛型请求类、继承、before/after hook。
- 请求 DTO、响应 DTO 使用 `serde`；查询参数使用 `RequestBuilder::query`，JSON 使用 `.json()`，表单使用 `.form()`，multipart 使用 `reqwest::multipart`。
- 统一 `error_for_status()`，并映射为带站点、方法、URL、状态码语境的 `ProcessingError`；认证失败、限流、5xx 保留可重试语义，配置/反序列化错误为不可重试。
- cookie 会话使用 `reqwest::ClientBuilder::cookie_store(true)`；自定义 headers 通过 `HeaderMap` 校验构建。不要每次请求创建 Client。
- base URL 必须可注入。这既支持自建兼容服务，也使测试可把请求指向本地 MockServer。

### 3.2 Source + Pointer 的强制验证方式

Bilibili、Fanbox、Patreon、Pixiv（以及 RSS 的 latest pointer 语义）必须通过本地 HTTP mock 验证真实请求序列，而不是 mock Rust 客户端方法：

1. MockServer 按页返回不同 fixture，并记录 path、query、headers、body 与调用顺序。
2. 以默认 Pointer 调用 `fetch(limit)`，断言返回 Item、ItemPointer、分页请求和 limit 截断。
3. 模拟处理器逐个调用 `SourcePointer::update(source_item, item_pointer)`，序列化再反序列化 Pointer。
4. 用更新后的 Pointer 再次 `fetch`，断言请求参数从正确 cursor/page/month 开始，旧数据被排除，新数据仍被返回。
5. 覆盖空页、最后一页、跨目标（收藏夹/creator/campaign/following）、中途 limit、重复项、无效项和恢复执行。
6. fixture 必须能检测“多请求一页”“漏请求下一页”“Pointer 提前推进”“不同 target 状态串线”等 plausible bug。

推荐把 `wiremock` 或 `httpmock` 作为 dev-dependency；最终选择在实现前做一个最小 spike，要求支持 Tokio、请求顺序/次数、query/body/header 匹配。当前仓库没有既有 HTTP mock 约定。

## 4. 分阶段实施顺序

### 阶段 A：SDK seam 与基础设施

1. 已完成第 2.2 节的 async/Result 接口迁移；组合规则和元数据明确暂缓。
2. 建立 `plugins/common/src/http`：共享 `reqwest::Client` 构建、base URL、header/cookie、错误映射。
3. 建立统一的 kebab-case Supplier 配置解析模式，但不增加元数据生成抽象。
4. 建立 MockServer 测试基建及 Source/Pointer 两轮迭代 helper。
5. 为所有新模块预留明确注册点；每完成一个模块立即注册并做配置构造测试。

### 阶段 B：纯逻辑组件

先迁移无 HTTP 的组件，固定 SDK 行为和测试模式：AnimeFileFilter、AnimeReplacementDecider、AnimeTagger、AnimeTitleVariableProvider、DoujinTitleTrimmer、EmbyImageTagger、EpisodeVariableProvider、LanguageVariableProvider、MediaTypeExistsDetector、ResolutionVariableProvider、SeasonVariableProvider、SimpleFileTagger。

### 阶段 C：单请求/抓取型组件

迁移 Ai、BgmTv、Chii、DLsite、Getchu、HTML、TMDB；每个站点使用可注入 base URL + `reqwest::Client`，以 fixture 验证请求和解析。

### 阶段 D：Source/Pointer 集成

迁移 RSS、Bilibili、Fanbox、Patreon、Pixiv。先写 Pointer 状态转移测试，再写 fetch/resolve 实现；每个 Source 必须通过第 3.2 节的两轮迭代测试。

### 阶段 E：Anime 聚合与 Torrent

1. AnimeVariableProvider、MikanVariableProvider：依赖多个站点及季解析，待底层模块稳定后组合。
2. TorrentFileResolver：先解决 torrent/metainfo 依赖。
3. qBittorrent、Transmission：最后迁移，复用 torrent hash 解析，并完整验证认证/session、选择性下载、状态和取消/移动。

## 5. 逐组件分析与执行项

### 5.1 `AiVariableProviderSupplier`

- 类型：`variable-provider:ai`；必填 `api-keys`；默认 `api-host=https://api.openai.com`、`model=gpt-3.5-turbo`、`temperature=0.85`；可选 `resolve-variables`、`system-role`、`primary`。
- 行为：以 title 为 key 做最大 500 项缓存；请求 chat completion，system + user 两条 message；取第一个 choice 的 message content 并反序列化为 `Map<String,String>`；`primaryVariableName=primary`。Kotlin `includeFile` 未被 Supplier 配置，Rust 保持默认 false，不擅自暴露新配置。
- HTTP：`reqwest` POST `${api-host}/v1/chat/completions`，随机选一个 API key 设置 Bearer；10 秒超时。`api-keys` 列表为空必须在构造时返回配置错误，避免随机选择 panic。
- 测试：配置解析/default、请求 body/Authorization、JSON content、缓存命中只请求一次、坏 content、非 2xx、多 key 不断言具体随机 key只断言来自集合。

### 5.2 `AnimeFileFilterSupplier`

- 类型：`source-file-filter:anime`，无参数自动启用。
- 行为：完整迁移允许扩展名集合、特殊目录/强制排除目录、subtitle 例外、CRC 清理、special 与 normal 两组 regex 的判定顺序；目标只保留动画视频、字幕和档案文件。
- 测试：表驱动覆盖大小写扩展名、NCOP/NCED/OP/ED/PV/CM/Fonts/Scan/Event/Lecture/Preview、special 目录、字幕文件、普通正片、嵌套目录和无扩展名。

### 5.3 `AnimeReplacementDeciderSupplier`

- 类型：`file-replacement-decider:anime`，无参数自动启用。
- 行为：解析 `[...]vN` 版本；Bilibili 源扣 1 分；偷跑/先行固定 -1；没有 before 时仅正分替换；两个都是 prerelease 时不替换；当前是 Bilibili、旧项不是时不替换；其余仅高分替换。
- 测试：v1→v2、无版本、Bilibili 与非 Bilibili、先行、before=None、大小写 B-global/地区文本。

### 5.4 `AnimeTaggerSupplier`

- 类型：`file-tagger:anime`，无参数自动启用。
- 行为：文件名优先标记 `special`、`ova`、`oad`、`movie`，再遍历父目录识别 `SPs`/special/特别篇/特別篇；保持 Kotlin 大小写规则和优先级。
- 测试：文件名与父目录、冲突优先级、无 tag。

### 5.5 `AnimeTitleVariableProviderSupplier`

- 类型：`variable-provider:anime-title`，无参数自动启用，primary=`title`。
- 行为：完整迁移清洗 regex、默认 extractor 链（AniTitle、` / `、` | `、反斜杠）、fallback 链（`/`、`|`、全括号、默认）、字幕组过滤、ASCII 罗马字标题判定；返回 `title`/`romajiTitle`。
- 实现：extractor 各自独立私有模块，公共 interface 仅 `VariableProvider`，避免把链细节暴露给调用方。
- 测试：为每种 extractor 准备真实字幕组标题；覆盖空括号、单标题、双语/三语、字幕组过滤和 fallback。

### 5.6 `AnimeVariableProviderSupplier`

- 类型：`variable-provider:anime`，无参数；`bgmtv-client` 可选，`prefer-bgm-tv=false`。
- 行为：复刻标题提取/清洗、语言决策、AniList GraphQL 与 Bangumi 搜索、候选 fuzzy score、优先站点、缓存、文件名辅助、`Anime` 输出字段和 primary 语义。
- Rust 设计：`AnimeLookup` 内部组合 `BgmTvHttp` 与 `AniListHttp`，Supplier 可引用共享 Bangumi instance；不要创建 Kotlin 式继承客户端。
- 依赖风险：AniList GraphQL DTO 可用 `serde` 手写；模糊匹配库尚未确定，见第 6 节。
- 测试：中/日/罗马字标题路由、prefer 覆盖、候选打分、无结果、缓存、两个后端失败；MockServer 断言 GraphQL body 和 Bangumi query/body。

### 5.7 `BgmTvVariableProviderSupplier`

- 类型：`variable-provider:bgmtv`，无参数；可选共享 `client` instance；primary=`nativeName`。
- 行为：清洗/提取 title 后搜索 Bangumi，取第一项 `name` 输出 `nativeName`，空标题/无结果返回空，最大 500 项缓存；`extractFrom` 使用传入文本。
- HTTP：Bangumi base URL/token 可注入，Bearer token；`reqwest` JSON。
- 测试：请求结构、token、空输入不请求、首项映射、无结果、缓存和共享 instance 配置。

### 5.8 `BilibiliSourceSupplier`

- 类型：`source:bilibili`；必填 `favorites: [integer]`，可选 cookie。
- 行为：逐收藏夹从 page 1 请求 `/x/v3/fav/resource/list`（`media_id,pn,ps=20,type=0,order=mtime`）；按 Pointer 的 touch-bottom 状态选择 `favTime <= min` 或 `> max`；过滤 `attr != 0`；生成 video SourceItem、attrs、MediaItemPointer，并正确标记最后一页最旧项。
- Pointer：每个 favorite 独立保存 min/max favTime 和 touchBottom；dump/parse/update 完整 serde 往返。
- 测试：多个 favorite 分页、attr 过滤、has_more、1594053452 重复时间、limit、中断恢复、触底后增量；严格验证第二轮请求和返回集合。

### 5.9 `ChiiVariableProviderSupplier`

- 类型：`variable-provider:chii`，无参数；primary=`subjectName`。
- 行为：向 Chii GraphQL 查询，取第一项，输出 `bgmtvId`、`subjectName`、`subjectNameCn`；item 与 extractFrom 共用请求路径。
- 测试：GraphQL body、空结果、字段映射、错误状态。

### 5.10 `DlsiteVariableProviderSupplier`

- 类型：`variable-provider:dlsite`，无参数；默认 `locale=ja-jp`、`only-extract-id=false`、`search-work-type-categories=[]`、`prefer-suggest=true`；primary=`title`。
- 行为：从文本识别 RJ/VJ 等 ID；按 preferSuggest 顺序调用 suggest/detail 或搜索；keyword 搜索支持类别；解析详情 HTML 为作品变量；404/无结果返回空；最大 500 项 `WorkRequest` 缓存。
- HTTP：所有 HTML/JSON 均由 reqwest 获取，`scraper` 解析；User-Agent 与 locale/header/query 保持 Kotlin 行为。
- 测试：ID、keyword、only-extract-id、suggest 优先和 fallback、类别 query、详情 HTML fixture、404、缓存。

### 5.11 `DoujinTitleTrimmerSupplier`

- 类型：`trimmer:doujin`，无参数。
- 行为：从左到右移除 `【...】` 段，每次达到期望长度立即返回；然后在首个 `。` 截断；仍超长也返回处理后的结果，不做额外截断。
- 测试：多个广告括号、提前满足、句号、无匹配、Unicode 长度。明确按 Kotlin `String.length` 的 UTF-16 语义还是 Rust Unicode scalar 语义；完全复刻应测试并选定兼容计数。

### 5.12 `EmbyImageTaggerSupplier`

- 类型：`file-tagger:emby-image`，无参数。
- 行为：文件名含 thumb/poster 时优先返回；否则仅 jpg/jpeg/png/webp/bmp 且文件存在时读取尺寸，宽≥高为 thumb，否则 poster。
- 依赖风险：图片尺寸探测 crate 待定，见第 6 节；应只读 dimensions，避免完整像素解码和无谓分配。
- 测试：命名优先、横/竖/正方形 fixture、不支持扩展、缺失文件、损坏图片。

### 5.13 `EpisodeVariableProviderSupplier`

- 类型：`variable-provider:episode`，无参数，accuracy=3，primary=`episode`。
- 行为：严格保持 parser chain 顺序：中文话/集、E/EP、SxEx、SP、英文数字词、唯一数字、`#N`、范围、common、`[01(56)]`、OVA/OAD；先清除分辨率、codec、CRC、版本等噪声；按文件生成 episode 变量并补零。
- 测试：每个 parser 至少一例，冲突用例验证优先级；小数、范围、SP、SxxExx、多文件对齐、解析失败返回空。

### 5.14 `FanboxIntegrationSupplier`

- 类型：`source:fanbox` + `file-resolver:fanbox`；必填 cookie，可选 headers、mode=`all|latestOnly`。
- 行为：请求已赞助 creator 列表，按 creator 分页帖子；Pointer 保存每个 creator 的 topId/touchBottom/cursor 状态；`latestOnly` 与全量模式保持 Kotlin 的截断逻辑。resolver 输出正文图片、file、cover/thumbnail 等文件并保留认证下载 headers。
- HTTP：reqwest Client 默认 headers 包含 FANBOXSESSID、Origin、Referer、UA，可由配置 headers 覆盖；base URL 可注入。
- 测试：creator 多目标、nextUrl/cursor、touchBottom、topId 增量、limit、Pointer 往返、不同帖子类型的 resolver、下载 headers；两轮 MockServer 迭代为强制验收。

### 5.15 `GetchuVariableProviderSupplier`

- 类型：`variable-provider:getchu`，无参数；primary=`title`。
- 行为：标题先识别 `[a-zA-Z]+-[a-zA-Z0-9]+` ISBN/编号，否则 keyword；搜索结果取标题最短项再抓详情；最大 500 项缓存；无结果为空。
- HTTP：reqwest + scraper；Getchu 的 age-check/cookie/编码行为需从 Kotlin `GetchuClient` 完整核对后实现。
- 依赖风险：若页面为 Shift_JIS/EUC-JP，需确认 reqwest `charset` + encoding_rs 是否完全覆盖，记录在第 6 节。
- 测试：编号/keyword、最短标题、详情 fixture、日文编码、无结果、缓存。

### 5.16 `HtmlFileResolverSupplier`

- 类型：`file-resolver:html`；必填 `css-selector`、`extract-attribute`；`direct-mode=false`。
- 行为：GET `sourceItem.download_uri`，按 CSS selector 抽取属性；有扩展名用 URL 最末段，否则 `${item_hash}_${index}.html`；direct=false 设置 download URI，direct=true 把响应 bytes 放入 SourceFile data。
- 修正点：相对 URL 必须按页面 base URL resolve；Kotlin 直接 `URI(attr)` 的限制是否为既有行为需 fixture 锁定。所有网络访问（包括 direct-mode）都必须用同一个 reqwest Client。
- 测试：selector、多节点、绝对/相对 URL、无扩展名、direct bytes、404、非法 selector/URI。

### 5.17 `LanguageVariableProviderSupplier`

- 类型：`variable-provider:language`，无参数；`read-content=true`；primary=`language`。
- 行为：先从文件名语言标识识别 zh-CHS/zh-CHT 等；允许时读取 ass/srt 文本，提取 Dialogue/字幕正文后做语言检测；缺失、二进制或 malformed input 安全返回空。
- 依赖风险：Kotlin Optimaize 等价 Rust 语言检测库未确定；见第 6 节。若库不能区分简繁，需将文件名规则与字符集判定独立实现，不以低质量模型冒充完全复刻。
- 测试：简中/繁中命名、ASS/SRT 内容、read-content=false、缺失/非法 UTF-8、无结论。

### 5.18 `MediaTypeExistsDetectorSupplier`

- 类型：`item-exists-detector:media-type`，无参数。
- 行为：按保存目录列出现有文件，按顶级媒体类型分组；目标文件以同顶级媒体类型 + 相同无扩展文件名判定已存在，返回目标→已有路径。
- 依赖风险：Apache Tika 没有直接 Rust 等价，见第 6 节。优先使用扩展名 MIME 数据库；必须用 Kotlin fixture 对照，不能默默改变 unknown/application 类型行为。
- 测试：video 不同扩展同 basename、不同媒体类型、application/unknown、多目录、重复文件。

### 5.19 `MikanVariableProviderSupplier`

- 类型：`variable-provider:mikan`；可选 `bgmtv-client`、`mikan-client`、`tmdb-client`；组合规则本轮暂缓；primary=`name`，accuracy=3。
- 行为：从 Mikan item link 抓 Bangumi href/标题，再取 Bangumi subject；输出 name/nameCn/mikanTitle/date/year/month/season；文件变量运行 season parser chain（SP、general、last string、keyword、TMDB）。缓存 Bangumi subject，保持异常分类。
- 实现：复用现有 Rust `MikanClient`，但让其 reqwest Client/base URL 可注入；Bangumi/TMDB 复用已迁移 HTTP 模块。
- 测试：Mikan HTML→Bangumi ID→subject 的完整 mock 链、season 文件变量、缓存、缺 link/ID、404。

### 5.20 `PatreonIntegrationSupplier`

- 类型：`source:patreon` + `file-resolver:patreon`；必填 `session-id`，可选 headers；source/resolver 组合规则本轮暂缓。
- 行为：先请求 pledge 得到 campaignIds；每个 campaign 按 YearMonth、lastPostId、lastOfMonth 分页/恢复；Pointer map 按 campaign 独立更新。resolver 输出 post attachments/media，并透传认证 headers。
- HTTP：reqwest JSON:API，session cookie/header 与自定义 headers；base URL 可注入。
- 测试：多 campaign、跨月、月末、空月、lastPostId、limit、Kotlin 已注明的额外末页请求应先以测试记录，再决定是否保持请求数量或只保持外部结果；Pointer 两轮迭代、resolver 和 headers 为强制验收。

### 5.21 `PixivIntegrationSupplier`

- 类型：`source:pixiv` + `file-resolver:pixiv`；必填 `session-id`；`mode=bookmark`；`user-id` 可显式给出，否则从 session-id 的 `\d+_` 前缀解析；source/resolver 组合规则本轮暂缓。
- 行为：bookmark 模式按 bookmark cursor/topBookmarkId/touchBottom；following 模式拉关注用户并按用户 last illustration ID 增量；过滤/展开插画与 ugoira；resolver 生成原图/多页/ugoira 文件并带 Referer/Cookie headers。
- HTTP：统一 reqwest；禁止保留 Kotlin 中独立全局 `httpClient` 下载 ugoira metadata 的路径。
- 测试：user-id 推导错误、bookmark 多页、following 多用户、Pointer 独立状态、增量第二轮、插画多页/ugoira resolver、headers、limit。

### 5.22 `QbittorrentDownloaderSupplier`

- 类型：`downloader:qbittorrent` + `file-mover:qbittorrent`；必填 endpoint，可选 username/password，`always-download-all=false`；Torrent resolver 组合规则本轮暂缓。
- 行为：登录并持有 SID cookie；提交 torrent/magnet，计算 v1/v2 info hash；按目标 relative paths 设置不下载文件；读取默认保存路径；取消；查询完成状态；列 torrent 文件；按 qBittorrent rename/move 完成批量移动并保持做种语义。
- HTTP：reqwest cookie store + form/multipart；403 时认证失效并**重放原请求一次**，避免 Kotlin after-hook 只登录却不重放的隐患。并发登录使用 async mutex/single-flight，不能持 std mutex 跨 await。
- 测试：登录一次复用、403 重登重放、add torrent/magnet、选择文件优先级、always-all、默认目录、完成/缺失、cancel、batch move、API 非 200。
- 依赖风险：bencode、torrent v1/v2 hash、路径语义库见第 6 节。

### 5.23 `ResolutionVariableProviderSupplier`

- 类型：`variable-provider:resolution`，无参数；配置键为 `only-high-resolution=true`；无 primary。
- 行为：按映射顺序识别 1920x1080→FullHD、1280x720→HD、2560x1440/2K、3840x2160/4K、7680x4320/8K；Kotlin/Rust 在 `only-high-resolution=true` 时过滤值包含 `HD` 的项，因此 **FullHD 和 HD 都被排除**，必须照此复刻而不是按配置名猜测。
- 测试：所有映射、大小写、冲突顺序、true/false 的 FullHD/HD 行为。

### 5.24 `RssSourceSupplier`

- 类型：`source:rss`；必填 url；可选 tags、attributes、date-format。
- 行为：reqwest GET RSS；解析 title/link/enclosure/content type/pubDate；自定义 item extension tags 加入 tags、自定义 extension→attr；日期先配置 formatter，再兼容内置格式；group=URL host；保持 AlwaysLatestSource 的 Pointer/latest 语义。
- 实现：评估现有 `rss-for-mikan` 是否支持任意 extension；不足时用 XML parser 补充，不能丢 tags/attributes。
- 测试：标准 RSS、扩展 namespace、attrs/tags、日期格式、enclosure 缺失、host group、第二轮 latest Pointer 不重复。

### 5.25 `SeasonVariableProviderSupplier`

- 类型：`variable-provider:season`，无参数；accuracy=2，primary=`season`。
- 行为：文件变量依次从 filepath/title 经 SP、general、last-string、keyword、extract-title parser；extractFrom 额外用 TMDB fallback；默认 season 开启；两位补零。
- HTTP：只有 TMDB fallback 使用 reqwest，且 `extract_from` 必须异步化后实现。
- 测试：Sxx、Season N、季度文本、SP、标题 fallback、默认 01、TMDB fallback 请求与无结果。

### 5.26 `SimpleFileTaggerSupplier`

- 类型：`file-tagger:simple`，无参数；可选 `external-mapping`，外部映射覆盖默认 `x-subrip→subtitle`。
- 行为：无扩展返回 None；按文件名探测 MIME；顶级类型不是 application 时返回顶级类型；application 时仅通过 subtype mapping 返回 tag；octet-stream 返回 None。
- 依赖风险：Tika/MIME 检测等价物见第 6 节。
- 测试：video/audio/image/text、srt、外部覆盖、unknown、无扩展。

### 5.27 `TmdbVariableProviderSupplier`

- 类型：`variable-provider:tmdb`，无参数；`language=zh-CN`；primary=`originalName`。
- 行为：搜索 TV，取第一项，输出 `tmdbId`、`tmdbName`、`originalName`；最大 500 项缓存；extractFrom 先完整文本，无结果再取第一个空格前 token。
- HTTP：reqwest `/3/search/tv`，query 包含 api_key/query/language；base URL 和 key 可注入。Kotlin 内置公开 API key 不应新复制到源码；兼容配置来源需要在实施时确定。
- 测试：query、语言、首项、fallback、缓存、无结果、认证失败。

### 5.28 `TorrentFileResolverSupplier`

- 类型：`file-resolver:torrent`，无参数。
- 行为：`.torrent` URL 用 reqwest 下载并解析单/多文件 metainfo；magnet 可取 metadata 后列文件；移除空 query 参数；生成相对 SourceFile path。
- 安全：拒绝绝对路径、`..`、非法 UTF-8/路径穿越；设置 torrent 大小、metadata、DHT 超时，避免无限等待与内存滥用。
- 依赖风险：Rust torrent/metainfo + magnet metadata/DHT 库尚未确定。可先完成 `.torrent` HTTP/bytes 解析；但在选定 magnet 实现前不得宣称组件完成。
- 测试：v1 单/多文件、v2/hybrid、路径安全、HTTP 错误、magnet 成功/超时。

### 5.29 `TransmissionDownloaderSupplier`

- 类型：`downloader:transmission`；必填 url，可选 username/password。Kotlin 暂未注册 mover，Rust 也只注册 downloader，除非完整实现并验证命名/移动支持。
- 行为：RPC 先处理 409 返回的 `X-Transmission-Session-Id` 并重放；Basic auth 仅在凭证存在时设置；torrent-add、torrent-get、torrent-set files-unwanted、session-get、remove、完成状态和 batch move 按 Kotlin 行为复刻。
- 注意：Kotlin `getTorrentFiles` 是 TODO。用户要求完全复刻组件时，Rust 不得留 TODO；应依据 Transmission RPC `torrent-get files` 实现，或在依赖/契约未决时保持此组件为未完成项而非假实现。
- 测试：409 握手重放、auth、有/无 torrent、选择性文件、默认路径、完成状态、cancel、files、移动；MockServer 验证每个 RPC JSON body。

## 6. 尚不明确的 Rust 依赖（实施前记录/决策）

以下依赖当前 workspace 未提供明确选择。按要求可以先不实现对应组件，但必须保持任务显式未完成，不能使用低保真假替代：

| 能力 | 涉及组件 | 候选/调查项 | 验收条件 |
|---|---|---|---|
| AniList/动画标题 fuzzy score | AnimeVariableProvider | `strsim`、`fuzzy-matcher` 或手写与 fuzzywuzzy 对照算法 | Kotlin fixture 的候选排序一致 |
| 动画文件名解析 Anitomy | AnimeVariableProvider | `anitomy`/`anitomy-rs` 的维护状态与行为 | Element 提取与 Kotlin AnitomyJ fixture 一致 |
| 图片尺寸与格式 | EmbyImageTagger | `imagesize`（仅尺寸）或 `image` | jpg/jpeg/png/webp/bmp，损坏文件不 panic，避免完整解码 |
| 语言检测/简繁区分 | LanguageVariableProvider | `whatlang`、`lingua`、`compact_lang_det` + 独立简繁规则 | ASS/SRT fixture 与 Optimaize 输出一致 |
| MIME 探测 | SimpleFileTagger、MediaTypeExistsDetector | `mime_guess`、`tree_magic_mini`、`infer` | 文件名模式和顶级 media type 与 Tika fixture 一致，Windows 可构建 |
| Getchu 页面编码 | GetchuVariableProvider | reqwest `charset`/`encoding_rs` | Shift_JIS/EUC-JP fixture 无乱码 |
| Torrent bencode/metainfo/hash | TorrentFileResolver、qBittorrent、Transmission | `lava_torrent`、`bip_metainfo`、`serde_bencode`、`sha1`/`sha2` | v1/v2/hybrid info hash 与官方样本一致 |
| Magnet metadata/DHT | TorrentFileResolver | `librqbit`、`rustybit` 或更小的维护中 crate | 有超时/取消、Windows 支持、不会启动不可控后台 runtime |
| RSS 任意 extension | RssSource | 现有 `rss-for-mikan`、`quick-xml` | namespace tags/attributes 不丢失 |
| HTTP MockServer | 所有 HTTP、特别是 Source | `wiremock` 或 `httpmock` | Tokio、请求次数/顺序、query/header/JSON/form/multipart 匹配 |

另外，`reqwest` cookie store 需要确认 feature（通常为 `cookies`）；TLS、JSON、charset 已在 `plugins/common` 开启。新增 crate 前优先选择纯 Rust、Windows 支持、维护活跃、依赖面小的实现。

## 7. 测试与验收矩阵

每个组件完成时必须同时满足：

1. **Supplier**：正确 `ComponentType`、support-no-props、kebab-case 配置/default、非法配置返回 `ComponentError`；`get_metadata()` 暂时返回 `None`。
2. **行为**：Kotlin fixture 驱动的单元/集成测试覆盖正常、边界、空结果和错误。
3. **HTTP**：只出现 reqwest；生产 Client 可复用；base URL 可注入；MockServer 校验 method/path/query/header/body/status。
4. **Pointer Source**：默认 Pointer、dump/parse/update、两轮 fetch、分页、limit、多 target、恢复执行全部通过。
5. **注册**：Supplier/InstanceFactory 在 `CommonPlugin` 可发现，无孤立模块。
6. **错误**：网络/5xx/429 可重试，配置/解析错误不可重试；没有 runtime `unwrap`/`expect`/`todo!`。
7. **性能**：Client 不重复构建；缓存上限 500 与 Kotlin 一致；避免完整图片解码、无谓 body copy、循环 clone。

分阶段验证命令：

```text
cargo fmt --all --check
cargo test -p common
cargo clippy -p common --all-targets -- -D warnings
cargo test --workspace
cargo run -p web --bin web -- --help
```

最后一项只验证插件链接/应用启动参数路径；Source/Downloader 的行为证明来自本地 MockServer 场景，不访问真实第三方站点。

## 8. 完成定义

- 29 个 Supplier 全部实现并注册；相关 Kotlin 行为均有 Rust fixture 证明；元数据不属于本轮完成条件。
- HTTP 仅使用 reqwest，不存在 Java 风格通用请求继承层。
- Bilibili/Fanbox/Patreon/Pixiv/RSS 的 Pointer 与分页交互通过两轮迭代 Mock 测试。
- qBittorrent/Transmission 的认证/session 与原请求重放经过 mock 验证。
- 所有调用方适配必要的 async/Result 接口；组合规则明确暂缓；没有兼容 shim、TODO 或 panic fallback。
- 第 6 节依赖全部已决并实现；若任何一项未决，则对应组件明确仍未完成，不能将整体标记为完成。
