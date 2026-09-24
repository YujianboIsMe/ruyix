# 语法高亮插件（v1.0.0）

> 跨版本文档（放 `doc/` 根下）。v0.13 的实现分析在 `doc/v0.x/需求-高亮插件化-v0.14.md`，
> 本文是**落地后的格式与规矩**：写一枚插件需要知道的全部东西都在这里。

## 一句话

**ruyix 的语法高亮不是内建功能，是一枚插件。** 语言表、扩展名、图标、token 配色全在插件里；
IDE 只提供"把插件说的东西跑起来"的机制。出厂自带的那枚叫 `ruyix-builtin`（预装插件）。

## 两种编译模式

| 模式 | 怎么编 | 里面有什么 | 什么时候用 |
|---|---|---|---|
| **预装**（默认） | `cargo build` / `cargo tauri build --no-bundle` | 内置 tree-sitter 解析器（8 门语言）+ 首启把 `ruyix-builtin` 物化到 `<根>/plugins/highlight/` | 正常发行：开箱即用 |
| **纯净** | `cargo build --no-default-features --features custom-protocol` | **一个解析器都不编、一个插件都不预装**；`plugins/` 空着由用户自己填 | 想要"只有我自己那套高亮"、或要一枚更小的二进制 |

纯模式的语义是**明确的降级，不是故障**：没有插件认领的语言按**纯文本**渲染，
状态栏会直说"没有插件提供配色（模式 pure）"。两种模式的差异由
`scripts/highlight-modes-probe.mjs` 跑**真 exe** 验证（物化了什么 / 没物化什么）。

## 插件放在哪

```
<便携根>/
├── plugins/highlight/<插件 id>/        ← 全局插件
└── projects/<项目 key>/plugins/highlight/<插件 id>/   ← 项目级（按 id **整份覆盖**全局）
```

同名时项目级赢（整份覆盖，**不做字段合并** —— 字段级合并的规则没人能记住；
要改就把那份拷过来改）。插件是**启动时**载入的：改完重开 ruyix 生效。

## 一个插件长什么样

```
plugins/highlight/ruyix-builtin/
├── plugin.toml     ← 清单：语言表 / 扩展名 / 图标 / grammar 来源 / query 覆盖 / token 映射
└── theme.css       ← 主题：**只允许 `.tok-*` 规则**
```

### `plugin.toml`

```toml
id = "my-lang"                     # 必填；全局唯一（项目级覆盖按它匹配）
name = "我的语言"                   # 可选，给人看的
version = "1.0.0"                  # 可选
theme = "theme.css"                # 可选；相对插件目录，**必须**在插件目录内

[[lang]]
id = "mylang"                      # 语言名（传给解析器的那一个）
ext = ["my", "myl"]                # 认领的扩展名（大小写不敏感）
icon = "🧪"                        # 文件树/标签页图标
grammar = "builtin"                # builtin / dll:<相对路径> / service:<名>
# highlights = "highlights/mylang.scm"   # 可选：query 覆盖（法子 2）
# token_map = { "function.call" = "function" }   # 可选：capture 名 → token 名
```

| 字段 | 说明 |
|---|---|
| `grammar = "builtin"` | 用**编译进 IDE 的**解析器。纯净模式下这枚语言会被明确拒绝（理由写进留痕） |
| `grammar = "dll:…"` / `"service:…"` | **本版本未启用**（信任边界见下）。写了会被**明确拒绝并留痕**，不是静默忽略 |
| `highlights` | 拿这份 `highlights.scm` 换掉编译进来的 query ⇒ **"什么算关键字"是数据不是代码** |
| `token_map` | 按 **capture 名**把片段映射到 token 名。引入**新** token 名时，主题里必须有对应的 `.tok-<名>` 规则，否则这枚语言被拒（理由写进留痕） |

### `theme.css`

```css
.tok-keyword { color: #569cd6; }
.tok-comment { color: #6a9955; font-style: italic; }
.tok-my-thing { color: #f0f; }      /* 新 token 名 = 必须自带规则 */
```

**只收 `.tok-<名字>` 选择器**，逐条校验：

- `.tab-bar { display: none; }` → 丢弃（插件不许碰 IDE 自己的样式）；
- `.a, .tok-b { … }` → 整条丢弃（选择器必须**单独**是 `.tok-*`）；
- 注释里的规则不算数（不会被当成声明）。

被丢弃的规则会**逐条写进留痕** —— 插件作者得知道自己的主题为什么没生效。

## 内置 token 名（27 个）

```
keyword function string comment type variable constant number operator punctuation
property attribute tag macro label namespace constructor title strong emphasis link
literal strikethrough diff-add diff-delete embedded error
```

名字 ↔ 语义的唯一来源是 `src-tauri/src/main.rs` 的 `TOK_TABLE`；
**主题里必须每个都有 `.tok-<名>`**，缺一个就是"那类片段渲染成默认色、看起来像高亮丢了"，
由 `every_tag_has_a_css_class` 守着（它读的就是预装插件的 `theme.css`）。

## 留痕（出问题先看这里）

`<根>/global/logs/plugins.jsonl`，一行一条：加载摘要（模式 / 插件数 / 语言数 / 有没有内置解析器）、
每个插件（id、目录、语言数）、以及**每一条被拒绝或被丢弃的理由**。

为什么**成功也写**：出问题时第一个要回答的是"这台机器上到底加载了哪些插件"，
而"没加载成功"与"根本没装"在界面上长得一样（与 `env install` 同一套纪律）。

## 信任边界（本版本刻意**没**开的）

| 路子 | 状态 | 为什么 |
|---|---|---|
| 数据型插件（本页描述的） | ✅ 开 | 没有可执行代码：manifest + scm + css |
| `dll:` 动态库（法子 4） | ⛔ 未启用 | 本机代码 = 进程权限。开了就等于"装插件=装程序"，需要签名/来源/隔离那一整套 |
| `service:` 外部服务（法子 5） | ⛔ 未启用 | 常驻进程 + 每次往返，且要处理它挂了/超时/乱输出 |
| 路径越界（`theme = "../x.css"`、绝对路径） | ⛔ 拒绝 | 插件只许读自己目录里的东西 |
| 盖 IDE 样式 | ⛔ 过滤 | 只放行 `.tok-*` |

## 写一枚自己的插件（照抄这五步）

1. `mkdir -p plugins/highlight/my-lang`；
2. 写 `plugin.toml`：`id`、`[[lang]]`（`id`/`ext`/`icon`/`grammar = "builtin"`）；
3. 写 `theme.css`：想改哪一类的颜色就写哪一条 `.tok-*`（不改也行，会沿用你给它们写的规则）；
4. 想换"什么算关键字"：加 `highlights = "highlights/x.scm"` + 那个文件（tree-sitter query 语法）；
5. 重开 ruyix，看 `<根>/global/logs/plugins.jsonl` 里有没有它、有没有被丢弃的理由。

**最省事的起点**：把 `<根>/plugins/highlight/ruyix-builtin/` 整个拷成你的目录，
改 `id` 与颜色/扩展名 —— 那枚就是出厂自带的样例。

## 门禁（改这里必须同时改它们）

| 门禁 | 钉住什么 |
|---|---|
| `plugin::tests::*` | 清单解析 / 只收 `.tok-*` / 路径封闭 / 新 token 名必须自带 CSS / 纯净模式下 `builtin` 被拒 / dll+service 明确拒绝 / 项目级覆盖全局 |
| `preinstalled::tests::*` | 物化写两个文件、**幂等且不覆盖**（哨兵）、物化出来的那份能通过加载器校验、纯净模式什么都不写 |
| `tests::every_tag_has_a_css_class` | `TOK_TABLE` 的 27 个名字在**预装插件主题**里都有 `.tok-*` |
| `tests::preinstalled_plugin_covers_builtin_languages` | 预装清单覆盖编译进来的 8 门语言 |
| `scripts/highlight-modes-probe.mjs` | 真 exe：预装模式物化了什么、纯净模式没物化什么、**用户改过的文件不被覆盖** |
| `ui-smoke` U53 | 前端的唯一来源是插件（CSS 注入 + 图标走注册表 + 空注册表时的降级提示） |
