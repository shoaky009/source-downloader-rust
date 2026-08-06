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
6. **配置统一 kebab-case**：Rust 所有配置键都使用 kebab-case，不保留 camelCase 特例；`onlyHighResolution` 使用 `only-high-resolution`。少量标量配置沿用现有 Supplier 风格直接从 `props` 读取并校验；只有嵌套、字段较多或需要复用整体配置时才定义 `serde` 配置结构，避免为每个组件制造低收益类型。

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

HTTP MockServer 已确定使用 `wiremock`（阶段 A 已加入 dev-dependency）；要求支持 Tokio、请求次数/顺序及 query/header/JSON/form/multipart 匹配。

## 4. 分阶段实施顺序

### 阶段 A：SDK seam 与基础设施（已完成）

1. 已完成第 2.2 节的 async/Result 接口迁移；组合规则和元数据明确暂缓。
2. 已建立 `plugins/common/src/http.rs`：统一构建启用 cookie store、10 秒超时的 `reqwest::Client`，集中执行 `error_for_status`，错误包含操作、方法、URL，并按连接/超时/429/408/5xx 分类可重试语义。
3. 已固定配置约定：少量标量直接从 `props` 读取并返回带键名的 `ComponentError`；复杂配置使用 `serde(rename_all = "kebab-case", deny_unknown_fields)`。不建立统一配置 parser，也不强制每个 Supplier 定义配置结构。
4. 已选择 `wiremock` 作为测试依赖并建立真实 HTTP 请求匹配；`test_support::fetch_and_commit` 固定 Source/Pointer fetch→update→dump 的单轮操作，具体 Source 在阶段 D 使用该 helper 完成两轮恢复验证。
5. `component/mod.rs` 是实现注册入口，`CommonPlugin::get_component_suppliers()` 是运行时注册入口；每个后续模块必须在完成时同时加入两处并测试构造。现有 Mikan Source 已迁移到共享 Client和统一错误映射，简单配置仍沿用直接读取方式，移除了请求路径中的临时 Client。

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
- 依赖：字符串相似度固定使用 `rapidfuzz`；动画文件名解析固定使用 Rapptz 的纯 Rust `anitomy-rs`（Cargo 包名 `anitomy`，Git 依赖 `https://github.com/Rapptz/anitomy-rs`），不用 crates.io 上依赖 C++/`anitomy-sys` 的同名 wrapper。AniList GraphQL DTO 用 `serde` 手写；具体 RapidFuzz metric/归一化必须由 Kotlin fixture 锁定，不能仅因库默认分数接近就视为等价。
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
- 依赖：图片尺寸固定使用 `imagesize`，只调用尺寸探测接口，不完整解码像素；jpg/jpeg/png/webp/bmp 的支持和损坏输入错误由 fixture 验证。
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
- HTTP：reqwest 获取原始 response bytes，`scraper` 解析解码后的 HTML；Getchu 的 age-check/cookie 行为需从 Kotlin `GetchuClient` 完整核对后实现。
- 编码：固定采用 `response bytes → charset 判定 → encoding_rs decode`。显式 charset（HTTP `Content-Type` 或页面声明）映射到 `encoding_rs::Encoding` 后解码，例如 `encoding_rs::SHIFT_JIS.decode(bytes)`；缺少或无效 charset 时使用 `chardetng::EncodingDetector` 自动检测，再用检测结果对应的 `encoding_rs::Encoding` 解码。必须检查 `had_errors`，不可用 `String::from_utf8_lossy` 静默替代。
- 测试：编号/keyword、最短标题、详情 fixture、日文编码、无结果、缓存。

### 5.16 `HtmlFileResolverSupplier`

- 类型：`file-resolver:html`；必填 `css-selector`、`extract-attribute`；`direct-mode=false`。
- 行为：GET `sourceItem.download_uri`，按 CSS selector 抽取属性；有扩展名用 URL 最末段，否则 `${item_hash}_${index}.html`；direct=false 设置 download URI，direct=true 把响应 bytes 放入 SourceFile data。
- 修正点：相对 URL 必须按页面 base URL resolve；Kotlin 直接 `URI(attr)` 的限制是否为既有行为需 fixture 锁定。所有网络访问（包括 direct-mode）都必须用同一个 reqwest Client。
- 测试：selector、多节点、绝对/相对 URL、无扩展名、direct bytes、404、非法 selector/URI。

### 5.17 `LanguageVariableProviderSupplier`

- 类型：`variable-provider:language`，无参数；`read-content=true`；primary=`language`。
- 行为：先从文件名语言标识识别 zh-CHS/zh-CHT 等；允许时读取 ass/srt 文本，提取 Dialogue/字幕正文后做语言检测；缺失、二进制或 malformed input 安全返回空。
- 依赖：语言检测固定使用 `lingua`；文件名简繁标识规则保持独立且优先，内容检测只处理规则未命中的字幕。若 `lingua` 无法稳定区分 Kotlin fixture 中的简繁文本，则补充明确的字符规则，而不是更换或叠加另一套语言模型。
- 测试：简中/繁中命名、ASS/SRT 内容、read-content=false、缺失/非法 UTF-8、无结论。

### 5.18 `MediaTypeExistsDetectorSupplier`

- 类型：`item-exists-detector:media-type`，无参数。
- 行为：按保存目录列出现有文件，按顶级媒体类型分组；目标文件以同顶级媒体类型 + 相同无扩展文件名判定已存在，返回目标→已有路径。
- 依赖：MIME 探测固定使用 `mimetype-detector`（Rust 模块名 `mimetype_detector`），基于文件内容/magic number 获取 MIME 和顶级类型。由于 Kotlin/Tika 行为及无内容、短文本、字幕扩展名场景可能不同，必须用 fixture 明确 unknown/application fallback；不得另加 `mime_guess`、`infer` 或 `tree_magic_mini` 形成第二套探测路径。
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
- 依赖：bencode/metainfo 解析固定使用 `serde_bencode`；v1 SHA-1、v2 SHA-256 仍需 `sha1`/`sha2`。info hash 必须对 metainfo 中原始 bencoded `info` 字节计算，不能把反序列化结构重新序列化后哈希；v2/hybrid fixture 必须锁定算法和字段语义。

### 5.23 `ResolutionVariableProviderSupplier`

- 类型：`variable-provider:resolution`，无参数；配置键为 `only-high-resolution=true`；无 primary。
- 行为：按映射顺序识别 1920x1080→FullHD、1280x720→HD、2560x1440/2K、3840x2160/4K、7680x4320/8K；Kotlin/Rust 在 `only-high-resolution=true` 时过滤值包含 `HD` 的项，因此 **FullHD 和 HD 都被排除**，必须照此复刻而不是按配置名猜测。
- 测试：所有映射、大小写、冲突顺序、true/false 的 FullHD/HD 行为。

### 5.24 `RssSourceSupplier`

- 类型：`source:rss`；必填 url；可选 tags、attributes、date-format。
- 行为：reqwest GET RSS；解析 title/link/enclosure/content type/pubDate；自定义 item extension tags 加入 tags、自定义 extension→attr；日期先配置 formatter，再兼容内置格式；group=URL host；保持 AlwaysLatestSource 的 Pointer/latest 语义。
- 实现：RSS/XML 固定使用 `quick-xml` 流式解析，并封装 `Extension { namespace: String, name: String, attributes: HashMap<String, String> }`；namespace URI、local name、attributes 和文本值必须保留。标准 RSS 字段与 extension 在同一次解析中完成，不再依赖 `rss-for-mikan` 的 extension 表达能力。
- 测试：标准 RSS、扩展 namespace、attrs/tags、日期格式、enclosure 缺失、host group、第二轮 latest Pointer 不重复。

### 5.25 `SeasonVariableProviderSupplier`

- 类型：`variable-provider:season`，无参数；accuracy=2，primary=`season`。
- 行为：文件变量依次从 filepath/title 经 SP、general、last-string、keyword、extract-title parser；extractFrom 额外用 TMDB fallback；默认 season 开启；两位补零。
- HTTP：只有 TMDB fallback 使用 reqwest，且 `extract_from` 必须异步化后实现。
- 测试：Sxx、Season N、季度文本、SP、标题 fallback、默认 01、TMDB fallback 请求与无结果。

### 5.26 `SimpleFileTaggerSupplier`

- 类型：`file-tagger:simple`，无参数；可选 `external-mapping`，外部映射覆盖默认 `x-subrip→subtitle`。
- 行为：无扩展返回 None；按文件名探测 MIME；顶级类型不是 application 时返回顶级类型；application 时仅通过 subtype mapping 返回 tag；octet-stream 返回 None。
- 依赖：MIME 探测固定使用 `mimetype-detector`；`detect_file`/受限 reader 只读取探测所需前缀。对无内容、短文本、SRT/ASS 和 octet-stream 必须按 Kotlin fixture 定义 fallback，外部 subtype mapping 仍覆盖默认 `x-subrip→subtitle`。
- 测试：video/audio/image/text、srt、外部覆盖、unknown、无扩展。

### 5.27 `TmdbVariableProviderSupplier`

- 类型：`variable-provider:tmdb`，无参数；`language=zh-CN`；primary=`originalName`。
- 行为：搜索 TV，取第一项，输出 `tmdbId`、`tmdbName`、`originalName`；最大 500 项缓存；extractFrom 先完整文本，无结果再取第一个空格前 token。
- HTTP：reqwest `/3/search/tv`，query 包含 api_key/query/language；base URL 和 key 可注入。Kotlin 内置公开 API key 不应新复制到源码；兼容配置来源需要在实施时确定。
- 测试：query、语言、首项、fallback、缓存、无结果、认证失败。

### 5.28 `TorrentFileResolverSupplier`

- 类型：`file-resolver:torrent`，无参数。
- 行为：`.torrent` URL 用 reqwest 下载；使用 `serde_bencode` 解析单/多文件 metainfo；magnet metadata/DHT 固定使用 `librqbit`，以 list-only/只取 metadata 的方式取得文件列表，不下载 payload；移除空 query 参数；生成相对 SourceFile path。v1/v2 info hash 分别使用 SHA-1/SHA-256，并直接哈希原始 bencoded `info` slice。
- 安全：拒绝绝对路径、`..`、非法 UTF-8/路径穿越；限制 torrent/metainfo 大小；`librqbit::Session` 必须使用临时/受控输出目录、禁用上传及持久化，metadata 获取包裹 Tokio timeout，并在成功、错误或超时后停止 session/取消后台任务，不能留下监听端口或下载任务。
- 依赖：magnet metadata/DHT 使用稳定版 `librqbit`（已核实 8.1.1 提供 `Session`、DHT、`add_torrent` 和 cancellation/stop；不要跟随 9.0.0-rc）；关闭 default features，仅启用需要的纯 Rust TLS/禁用上传能力，避免 `default-tls`、HTTP API client/server、Web UI、SQLx 等无关依赖。`serde_bencode` 继续负责本地 metainfo 解析，`sha1`/`sha2` 负责显式 hash 验证。
- 测试分层：v1 单/多文件、v2/hybrid、路径安全、HTTP 错误、metadata 映射和 session option 构造属于确定性单元测试。真实 magnet peer discovery、DHT metadata 获取、网络超时/取消以及 `Session::stop` 后端口和后台任务清理不适合伪装成单元测试，必须放入独立集成测试；集成测试使用本地可控 peer/DHT fixture，禁止依赖公网 DHT，支持显式超时和环境不满足时的明确 skip 原因，且不得混入默认快速单测命令。

### 5.29 `TransmissionDownloaderSupplier`

- 类型：`downloader:transmission`；必填 url，可选 username/password。Kotlin 暂未注册 mover，Rust 也只注册 downloader，除非完整实现并验证命名/移动支持。
- 行为：RPC 先处理 409 返回的 `X-Transmission-Session-Id` 并重放；Basic auth 仅在凭证存在时设置；torrent-add、torrent-get、torrent-set files-unwanted、session-get、remove、完成状态和 batch move 按 Kotlin 行为复刻。
- 注意：Kotlin `getTorrentFiles` 是 TODO。用户要求完全复刻组件时，Rust 不得留 TODO；应依据 Transmission RPC `torrent-get files` 实现，或在依赖/契约未决时保持此组件为未完成项而非假实现。
- 测试：409 握手重放、auth、有/无 torrent、选择性文件、默认路径、完成状态、cancel、files、移动；MockServer 验证每个 RPC JSON body。

## 6. Rust 依赖决策与剩余缺口

以下依赖已由本轮确定。实现时统一加入 workspace dependencies，再由 `plugins/common` 引用；禁止同一能力并存第二套库：

| 能力 | 选定依赖 | 实施约束 |
|---|---|---|
| 字符串相似度 | `rapidfuzz` | 用 Kotlin fixture 固定 metric、归一化、阈值和候选排序 |
| 动画文件名解析 | Rapptz `anitomy-rs`，Cargo 包名 `anitomy`，Git 依赖 | 纯 Rust 实现；当前 crates.io 无 `anitomy-rs` 包名，且 crates.io `anitomy 0.2.0` 是 `anitomy-sys` C++ wrapper；应固定 Git revision，Element 输出与 AnitomyJ fixture 对齐 |
| 图片尺寸 | `imagesize` | 只探测尺寸，不完整解码图片 |
| 语言检测 | `lingua` | 文件名规则优先；简繁差异以独立规则补足 |
| MIME 探测 | crates.io 包 `mimetype-detector`，Rust 模块 `mimetype_detector` | 已核实提供 magic-number `detect`/`detect_file`，文件探测只读有限前缀；unknown、字幕和 extension fallback 由 fixture 明确 |
| 网页编码 | `encoding_rs`，必要时 `chardetng` | response bytes→显式 charset/自动检测→`Encoding::decode`；检查 `had_errors` |
| Torrent bencode/metainfo | `serde_bencode` | 保留并哈希原始 `info` bytes；另用 `sha1`/`sha2` 实现 v1/v2 hash |
| Magnet metadata/DHT | `librqbit` 稳定版（基线 8.1.1） | list-only/metadata-only；关闭 default features，启用纯 Rust TLS与禁用上传；受控临时目录、超时、取消并停止 session，不下载 payload |
| RSS/XML extension | `quick-xml` | 流式解析 namespace/local name/attributes/text，映射到 `Extension` |
| HTTP MockServer | `wiremock` | 已加入；验证真实请求、次数、顺序和请求内容 |
| HTML parser | `scraper` | 解析经过 charset 流程解码后的 HTML；相对 URL 由页面 base URL resolve |

当前库选择已完整；Torrent 阶段仍有两个必须先验证的实现细节：

1. **Torrent hash**：依赖名确定为 `sha1`/`sha2`，但 v2/hybrid 的原始 `info` slice 提取和 hash fixture 必须先做 spike；普通 serde round-trip 不能保证原始字节一致。
2. **librqbit 生命周期**：用本地可控 fixture 验证 metadata-only 行为、超时取消和 `Session::stop` 后无残留任务/监听端口；若 8.1.1 的公开接口不能保证不下载 payload，必须通过 `AddTorrentOptions`/文件选择在开始传输前禁用全部文件，而不是接受隐式下载。

`reqwest` 已启用 `cookies`、TLS、JSON、charset；HTTP body 仍统一先取 bytes，网页字符集按上表显式处理。新增 crate 前继续优先选择纯 Rust、Windows 支持、维护活跃、依赖面小的实现。

## 7. 测试与验收矩阵

每个组件完成时必须同时满足：

1. **Supplier**：正确 `ComponentType`、support-no-props、kebab-case 配置/default、非法配置返回 `ComponentError`；少量配置直接读取，复杂配置才定义结构；`get_metadata()` 暂时返回 `None`。
2. **行为**：Kotlin fixture 驱动的单元/集成测试覆盖正常、边界、空结果和错误。
3. **测试分层**：纯解析、状态转换、请求构造和错误映射使用单元测试；涉及真实 socket、DHT/peer discovery、外部进程、数据库或完整运行时生命周期的行为使用独立集成测试。不得为了单测方便 mock 掉被验收的协议交互后仍声称端到端行为已验证。
4. **HTTP**：只出现 reqwest；生产 Client 可复用；base URL 可注入；MockServer 校验 method/path/query/header/body/status。
5. **Pointer Source**：默认 Pointer、dump/parse/update、两轮 fetch、分页、limit、多 target、恢复执行全部通过。
6. **注册**：Supplier/InstanceFactory 在 `CommonPlugin` 可发现，无孤立模块。
7. **错误**：网络/5xx/429 可重试，配置/解析错误不可重试；没有 runtime `unwrap`/`expect`/`todo!`。
8. **性能**：Client 不重复构建；缓存上限 500 与 Kotlin 一致；避免完整图片解码、无谓 body copy、循环 clone。

分阶段验证命令：

```text
cargo fmt --all --check
cargo test -p common --lib
cargo clippy -p common --all-targets -- -D warnings
cargo test --workspace
cargo test -p common --test <integration-test-name> -- --ignored --nocapture
cargo run -p web --bin web -- --help
```

常规单元测试和 workspace 测试不得访问公网。DHT 等网络生命周期集成测试默认标记 `#[ignore]`，在具备本地 fixture/端口条件的专用验证环境显式运行；其结果是相关网络行为的完成证据，不能用单元测试或公网偶然成功代替。最后一项只验证插件链接/应用启动参数路径；HTTP Source/Downloader 的确定性行为证明来自本地 MockServer 场景。


## 8. 完成定义

- 29 个 Supplier 全部实现并注册；相关 Kotlin 行为均有 Rust fixture 证明；元数据不属于本轮完成条件。
- HTTP 仅使用 reqwest，不存在 Java 风格通用请求继承层。
- Bilibili/Fanbox/Patreon/Pixiv/RSS 的 Pointer 与分页交互通过两轮迭代 Mock 测试。
- qBittorrent/Transmission 的认证/session 与原请求重放经过 mock 验证。
- 所有调用方适配必要的 async/Result 接口；组合规则明确暂缓；没有兼容 shim、TODO 或 panic fallback。
- 第 6 节依赖全部已决并实现；若任何一项未决，则对应组件明确仍未完成，不能将整体标记为完成。
