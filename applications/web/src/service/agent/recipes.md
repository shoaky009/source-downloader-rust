# 配置场景与组件选择

先查询 catalog，再 inspect 相关组件和 processor。以下说明用于选择组合；属性名、引用和可用组件以当前工具返回的 schema 为准。不要从演示配置复制凭据或旧 client/instance 字段。

## 引用与共享组件

处理器中的引用写 `type:name`；仅 catalog 标明 `noProps=true` 的组件可省略配置，直接写 `type`，例如 `torrent`、`general`、`hardlink`、`episode`、`sequence`。需要自定义可选参数时仍声明命名组件。

一个 supplier 提供多个接口时，只声明一次配置，并在各处理器字段使用相同的 `type:name`：

| 一次声明 | 处理器复用方式 |
| --- | --- |
| source / system-file / local，props 含 path | source 与 downloader 均写 `system-file:local` |
| downloader / qbittorrent / qb，props 含 endpoint | downloader 与 file-mover 均写 `qbittorrent:qb` |
| source / fanbox / account，props 含 cookie | source 与 item-file-resolver 均写 `fanbox:account` |

`system-file` 的 item-file-resolver、file-mover 是另外的 supplier，可分别直接引用 `system-file`。Mikan 的 source 与 variable-provider 也是不同 supplier，不能因 typeName 相同而共享配置。下面三种场景均由组件直接配置 HTTP 客户端，不需要额外创建 Mikan、Bangumi、Fanbox 或 qBittorrent instance。

## Mikan 新番订阅

组合：`mikan:<订阅>` source → `torrent` item-file-resolver → 同名 `qbittorrent:<连接>` downloader 和 file-mover；通常添加 `mikan`、`episode` variable-providers。

- 先确定完整 RSS URL、qBittorrent Web API 地址、登录需求、最终保存目录、刷新频率、字幕组/语言/画质偏好。含订阅 token 的完整 URL、密码和可选 token 用 secret 问题收集。
- Mikan source 必需 `url`，`all-episode=false` 默认仅处理 RSS 返回的条目。`true` 会为新条目扩展其番剧与字幕组 RSS 历史，并记录游标；新建游标以当前时间为界，不能把这个开关描述为首次立即补齐所有历史。它不负责识别名字和集数。
- `mikan` provider 访问条目页面、番剧页面及 Bangumi API，提供 `name`（原名）、`nameCn`（中文名，缺失时退回原名）、两位 `season`，并可能有 `mikanTitle`、`date`、`year`、`month`。它不提供 `episode`；用 `episode` 从文件名提取，通常补齐两位。
- 可以选目录 `{name}/Season {season}`、文件 `{nameCn} - S{season}E{episode}`，也可按用户语言偏好用 `nameCn` 作目录。模板通常省略扩展名，运行时保留原扩展名。不能保证每个文件名都能提取集数。
- Mikan provider 的 `token` 是 `.AspNetCore.Identity.Application` Cookie 的值；`bgmtv-token` 是可选 Bangumi Bearer token，两者都非必需。使用 `mikan-base-url`、`bgmtv-base-url` 自定义服务，不存在 `mikan-client`、`bgmtv-client` 属性。
- `torrent` resolver 读取 torrent 文件元数据；magnet 必须带可下载 torrent 的 `xs` URL，不支持仅靠 magnet 发现文件列表。
- qBittorrent 连接使用 `endpoint`（Web 服务根地址，不加 `/api/v2`），兼容别名 `host`；省略 username 会跳过登录，只有 username 存在时才发送密码。`always-download-all=false` 默认将过滤掉的 torrent 文件设为不下载，`true` 则下载全部文件，整理仍受过滤结果约束。
- qBittorrent file-mover 调用 `renameFile` 和 `setLocation`，保持 torrent 管理文件；必须与 downloader 使用同一命名组件。下载目录来自 qBittorrent 的默认保存目录，没有 `download-path` 或路径映射属性。下载目录、输出目录在服务端与本程序看到的路径需一致，相关检查也会访问本地文件系统；不满足时应说明部署限制。

可选：`source-file-filters=["anime"]` 保留常见视频、字幕及字幕压缩包，并排除片头片尾、预告、扫描等。需要保留特典、封面或文本时先确认此筛选是否适合。`file-tagger` 的 `anime` 可标记 `special`、`ova`、`oad`、`movie`，用于分组；它只做名称启发式匹配。`file-replacement-decider=anime` 按来源标题中的修订版本、Bilibili/先行标记决定替换，不按分辨率比较，需用户确实希望自动替换时才选用。

## 本地动画整理

组合：同名 `system-file:<输入>` source 和 downloader → `system-file` resolver → 用户选择的 `general` 或 `hardlink` file-mover。必须确定输入目录、输出目录、扫描粒度、整理方式和触发安排。

- source 必需 `path`。`mode=0`（默认）把输入目录的每个直接子文件或子目录作为一个 item，目录由 resolver 递归展开并按路径排序；适合一部番剧一个目录。`mode=1` 递归把每个文件作为独立 item；此时 `item.title` 是文件名，`sequence` 也会对每个文件重新开始。两种模式不是监视文件变化；自动扫描要配置 trigger。
- downloader 不复制、不下载文件，使用 source 的输入目录作下载根目录。`general` 真正移动文件，跨文件系统时复制成功后删除源文件；`hardlink` 创建硬链接并保留源文件，要求同一文件系统。硬链接目标与源文件共享内容；当前 hardlink 的替换实现只处理符号链接目标，不能承诺覆盖现有普通文件。
- 输入与输出应避免相互包含，以免后续扫描又读入整理结果。不自动添加 `delete-empty-directory` listener；它会在成功后删除源侧空目录，只有用户要求清理时才考虑。
- 常用 providers 是 `anime`、`season`、`anitom`、`episode`，字幕需要时加 `language`。`anime` 依赖网络，返回 `nativeName`、可能的 `romajiName`，不返回 `name` 或 `nameCn`。离线整理可使用 `anitom` 的 `animeTitle`，例如目录 `{animeTitle}/Season {season}`；存在识别差异，应询问代表性文件名和目录结构来选模板。
- `season` 从文件路径再从 item 标题识别，缺失默认 `01`，OVA/OAD/SP 等识别为 `00`。`anitom` 的原始季度、集数变量是 `animeSeason`、`episodeNumber`，不等于 `season`、`episode`；需要统一时显式使用 `variable-name-replace`，不要假设自动别名。
- 普通视频可用 `{nativeName} - S{season}E{episode}`（需要 anime provider）；字幕可用同一文件主体加 `:{'.' + language}`，语言未识别时连同点号一起省略。
- 通过 file-grouping 按扩展名或预先添加的标签区分字幕、普通剧集、特典，分别选模板。不要使用尚未提取的 `file.vars.season` 作分组条件。`anime` file-tagger 的名称识别有大小写限制，不能承诺所有 Special/ova 写法都会被标记。

## Fanbox 订阅备份

组合：同名 `fanbox:<账号>` source 和 item-file-resolver → `http:<暂存>` downloader → `general` file-mover；通常添加 `sequence` provider。确定账号 Cookie、暂存目录、最终目录、历史/最新模式、轮询频率和需要保留的内容类型。

- 必需 `cookie`，用 secret 问题收集完整 Cookie 请求头值。默认 `mode=all` 从账号当前正在支持的作者分页获取可访问历史，保存作者游标，后续扫描查找更新；`mode=latestOnly` 每次只查询 supporting 最新 30 条，不补历史。`all` 的 fetch-limit 不是严格条数上限，会保留整页并访问其他支持的作者。
- 没有任意作者 ID/URL 列表参数，也不能获取未获账号权限的文章。可使用 item 过滤条件按 `item.attrs.creatorId` 缩小处理范围，但 source 仍查询账号支持范围。受限文章被跳过。
- source 会把 Cookie、Referer 等请求头传给 HTTP downloader；无需把同一 Cookie 再复制进 downloader 配置。Fanbox `headers` 会完整替换默认请求头，使用后必须自行包含认证等必要请求头；通常保留默认值即可。
- API 请求默认间隔 `request-interval-ms=3000`，post.info 还有 `post-info-interval-ms=5000` 的额外间隔；HTTP 429 冷却仍生效。HTTP downloader 必需 `download-path`，`parallelism=5` 默认限制文件下载并发，和 Fanbox API 的节流设置不同。
- 目录可以是 `{item.attrs.username}/{item.date} {item.title}-{item.attrs.postId}`。稳定作者目录可用 `{item.attrs.creatorId}`。来源属性还有 `likes`、`comments`、`adult`、`fee`，条目 tags 来自文章标签。
- resolver 输出封面、正文图片、附件、正文 `.txt` 及 URL 嵌入的 `.html`，`file.attrs.type` 分别为 `cover`、`image`、`file`、`text`、`html`。附件冲突名称会自动加数字后缀。它不下载正文链接指向的任意外部网站内容，也不解压附件。
- 图片通常使用原图；原图 HEAD 返回 400 时尝试 thumbnail，成功后带 `thumbnail` tag；原图和缩略图均不可用则跳过，不能承诺永远得到原图。

推荐图片命名 `{sequence}`，按 file-grouping 顺序增加这些例外：

| expression-matching | filename-pattern | 效果 |
| --- | --- | --- |
| `file.attrs.type == 'file'` | 空字符串 | 附件保留原名 |
| `file.attrs.type == 'cover'` | `0` | 封面名为 0，保留扩展名 |
| `file.attrs.type == 'text'` | `text` | 正文名为 text.txt |
| `file.attrs.type == 'html'` | 空字符串 | 嵌入 HTML 保留原名 |

未匹配的图片按自己的组从 1 编号；封面、附件、正文不会占用图片序号。也可以明确为 image 添加分组。不要给所有类型统一的常量文件名，否则多张图片或附件可能发生目标路径冲突。

## 变量速查

| variable-provider | 实际变量与条件 |
| --- | --- |
| mikan | item: name、nameCn、season；可选 mikanTitle、date、year、month；文件阶段复制 item 的 season。网络查询。 |
| anime | item: nativeName、romajiName（取决于查询）；部分多级文件路径可补充文件名称。AniList/Bangumi 网络查询，prefer-bgm-tv 默认 false。 |
| anitom | 文件名解析：animeTitle、animeSeason、episodeNumber、animeType、releaseGroup、videoResolution、animeYear、language 等。无网络、无必需属性。 |
| episode | 文件名解析 episode，通常至少两位；识别失败时变量缺失。 |
| season | 文件路径或条目标题解析 season，两位字符串，通常 01，特典 00。 |
| language | 文件变量 language，例如 zh-CHS、zh-CHT、JP、EN，多语言用点连接。优先文件名标记；read-content 默认 true，可读本地 ASS/SRT 内容作为补充。 |
| resolution | 文件变量 resolution，识别尺寸及 2K/4K/8K 标记；only-high-resolution 默认 true，排除 HD/FullHD；不能假设裸 1080p 会产出变量。 |
| sequence | 文件组内从 1 编号，按组中文件总数补零；不是跨文章计数器。 |
| regex | item 指定字段的正则完整匹配文本，不是捕获组；field 默认为 title，也可 link、downloadUri、contentType、datetime。普通文件阶段不提取变量。 |
| anime-title | 从标题提取 title、可能的 romajiTitle；用在 variable-process chain 的 extract_from。当前直接放进 variable-providers 会因文件变量数量不匹配失败。 |
| bgmtv | 网络查询 nativeName；同样应在 variable-process chain 中使用，当前普通文件变量接口不返回逐文件结果。 |

`variable-process` 的 input 是字段路径，例如 `item.title`、`file.name`，不是 CEL 表达式；chain 依次使用 provider 的 extract_from，并可通过 output.key-mapping 重命名输出。只有确有需要时才组合处理链；mikan provider 的 extract_from 不提供结果。

## 触发、分组与表达式

- `triggers=[]` 表示手动触发，即便 enabled=true 也不会自动扫描。`fixed` 必需正的 ISO 8601 duration，如 `PT20M`；`on-start-run-tasks=false` 默认等第一个间隔再执行，true 在触发器启动时执行。`cron` 使用系统本地时区，必须六段（含秒），例如 `0 0 3 * * *` 为每天 03:00。
- item 过滤在条目变量提取前执行；source-file 过滤和 file-grouping 在文件变量提取前执行。file-grouping 的 expression-matching 可用 `file.name`（含扩展名）、`file.extension`、`file.tags`、`file.attrs` 以及总 `fileCount`，此处没有 `item` 或 `file.vars`。file-content 过滤才有 `file.vars`、`file.paths`。
- 分组按首个匹配规则生效，未匹配文件使用全局模板。tags 匹配要求包含指定全部标签；一条规则同时配置 tags 与 expression-matching 时，当前实现只判断 tags。空规则不会成为兜底，显式 `expression-matching="true"` 才能匹配全部。变量 provider 在各组内单独运行，因此分组也影响 sequence 和补零宽度。
- 排除表达式 true 时排除；包含列表非空时所有表达式都为 true 才保留。表达式执行上下文取决于阶段，不能把命名模板变量直接搬到来源过滤中。
- 命名模板 `{表达式}` 使用 CEL；`:{表达式}` 表示可选表达式，失败时仅省略该表达式。provider 变量可直接写 `{episode}` 等，文件变量优先于同名 item 变量。模板中的 `item.date` 是发布时间/本地文件创建日期，provider 的 `date` 可能是番剧开播日期，不要混用。
- filename-pattern 空字符串保留原文件名，save-path-pattern 相对处理器 save-path；常规重命名保留源文件扩展名。缺失变量的默认策略 STAY 会保留原文件名或目录位置，静态校验成功不代表每个实际文件都能正确识别；不要用未经证实的变量向用户保证整理结果。
