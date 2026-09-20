# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Development Workflow & Code Style

**Workflow**: develop **directly on `master`** — do not open feature branches. Branches are version
**snapshots/backups** only (`0.0.2` / `0.0.3` / `0.0.4` are frozen snapshots; never commit new work to them).

**Code style**: Google style guides. Rust follows Google/Fuchsia Rust style (= rustfmt), everything else
follows Google JS / HTML / CSS / JSON style guides. Full rules: `doc/编码规范.md`.

```bash
cargo fmt --check                  # Rust 格式（rustfmt.toml: max_width 100 等）
node scripts/check-style.js        # JS/HTML/CSS/JSON 风格（零依赖，0 error 才算过）
node scripts/ui-smoke.js           # UI 冒烟：契约静态断言 + agent 面板演示回放（零依赖）
cargo clippy --all-targets         # 静态检查，必须 0 warning
cargo test                         # 单元测试（引擎 159+8 ignored / ruyix 41）
```

Notes: `ui/xterm.js` / `ui/xterm.css` are vendored (MIT) and excluded from style checks; the frontend has
no npm/bundler, so never add npm tooling — `scripts/check-style.js` is the style gate.

## Project Vision

ruyix is an IDE built on **Tauri 2 + Rust backend**, aiming to eventually use Monaco Editor. Currently the editor is a custom implementation using a transparent `<textarea>` overlaid on a syntax-highlighted backdrop via CSS Grid.

**Architecture principle**: The command system is the **sole bridge** between frontend and backend. All GUI operations (context menu, buttons, clicks) MUST route through `handleCommand()`. No `invoke()` calls from UI event handlers — that creates AI integration blind spots.

## Architecture

- **Frontend**: Vanilla HTML/CSS/JS (no bundler, no npm). Served from `ui/` via Tauri's custom protocol. All Tauri APIs accessed via `window.__TAURI__` global.
- **Frontend globals**: `main.js` owns the app state; panel modules (`session.js` / `mcp.js` / `a2a.js` / `capability.js` / `config.js`) read it as `window.state`. A top-level `const` does NOT land on `window`, so `main.js` must keep the explicit `window.state = state;` export — drop it and every panel loses `currentProject` (config project scope then falsely reports "no project open"). Guarded by ui-smoke U10.
- **Backend**: Tauri 2 Rust backend, modules: `main.rs` (Tauri commands + app entry), `config.rs` (3-scope config, projects, run targets), `pty.rs` (PTY terminal management), `ai.rs` (LLM integration), `git.rs` (git status/commands), `runner.rs` (run-command inference from manifest contents), `instance.rs` (single-instance detection), `agent/` (Agent bridge: `agent_*` commands + `agent://*` events + config_bridge + `sessions.rs` chat-session store, see `doc/融合计划-Agent集成-v0.2.md`), `mcp.rs` (MCP client over stdio JSON-RPC: server registry in `mcp.toml`, tools discovery & invocation), `a2a.rs` (A2A client: agent card discovery + task delegation, registry in `a2a.toml`), `capability.rs` (capability system: CLI tool whitelist `tools.toml` + SKILL docs `skills.toml`, see `doc/capability.md`).
- **Agent engine**: `crates/harness-engine` — zero-tauri lib crate imported from darkhorse-harness (agent tool loop + plan/generate/lint/verify/repair pipeline; unit tests via `cargo test -p harness-engine`). The ruyix `agent/` module wraps it; engine code lives in the workspace, don't re-vendor. **Agent tool loop** (`engine::agent`): four atomic capabilities — Read / Write / Execute / Connect — model-driven rounds until `{"final":...}`, max 96 rounds (`MAX_STEPS`); Read and Write are path-jailed to the project, there is no surgical `edit` tool (a change is a whole-file Write: read first, hand back the complete content); Execute runs in the project root with timeout + output clipping + a destructive-pattern deny list (tripwire, not a sandbox — real isolation is the verify pipeline's docker mode). **Connect** is the one primitive the engine cannot implement alone: it declares a `Connector` trait (`list` / `call`) and the host lands it — ruyix wires `src-tauri/src/agent/connect.rs` (`RuyixConnector`) over MCP servers (`mcp.toml`, auto-handshake on first call) and A2A remote agents (`a2a.toml`, delegation progress on `a2a://status`); `NoConnector` is the empty impl for tests/eval. The available-connection list is injected into the run's first user message so the model never guesses names. **Prompt & failure feedback**: the system message is `AGENT_SYSTEM` plus an OS-gated suffix — `agent_system_prompt()` appends a Windows retrieval hint (prefer `git grep -i -l`; `findstr`'s multi-mask behaviour is unreliable) while the base constant stays byte-identical so tests can keep asserting it. An unparseable round never aborts the run: the feedback distinguishes truncation (`finish_reason=length` → tell the model to *shorten*) from malformed output (→ *resend*), and the error text goes through `serde_json::to_string` because `parse_action`'s message carries a JSON fragment — raw interpolation used to break the feedback's own JSON. Pipeline self-repair: verify-failure stderr + lint diagnostics + failed-generation steps fed back → find/replace patch (atomic, CRLF-normalized) → re-verify; max 5 rounds by default (`ruyix.code.harness.lint.max_repair_rounds`), stops when issue count stops dropping. The model may answer `{"need": [...]}` to ask for file/dir contents (path-jailed; up to 3 ask-rounds, not counted against repair rounds), and may create missing files via the whole-file shape. **Quality gate** (`v0.3`, fact-driven — never a task classifier): a write triggers a *narrow* syntax layer (`verify::staged_syntax_checks`, runs on the change's content in a temp dir, so confirm-mode keeps its "disk untouched" invariant), and delivery (`{"final":...}`) triggers a *full* layer (`verify::run` + lint, Apply mode only; Stage mode reports an explicit `skipped` with the reason). A failed full verification **blocks the final** and feeds the report back (max `gate.max_full_attempts`, then it releases with an honest "未通过" footer). Before releasing, a **reflection agent** (`engine::reflect`) reviews the artifacts in a *clean context* — its own `messages`, only `read` (no write path exists in that module by construction), rubric chosen by the fact "were files changed?" (artifact review vs evidence check), structured findings `{claim, evidence, verdict}` fed back to the main loop (never a second writer), and any failure degrades to a `note` instead of failing the run. Conclusions stream out as `agent://verify` / `agent://reflect` and are rendered as a per-message 验证/复核 section (`ui/session.js`).
- **Communication**: Tauri native IPC. Synchronous calls use `invoke()`. Async push for PTY output uses Tauri's event system `emit()`/`listen()`.
- **Syntax highlighting**: `tree-sitter` + `arborium` crate. Highlighting runs in a `spawn_blocking` thread to avoid blocking the async runtime.
- **Terminal**: `xterm.js` (vendored from `ui/xterm.js`) + Rust PTY via `portable-pty` crate.

## Project Structure

```
├── src-tauri/            # Rust backend (Tauri 2)
│   ├── src/main.rs       # App entry, Tauri commands, file operations
│   ├── src/config.rs     # ConfigManager: 3-scope config, projects, run targets, config form scan/save/apply
│   ├── src/pty.rs        # PtyManager: spawn/write/resize/close PTY sessions
│   ├── src/ai.rs         # AI translation: natural language → standard commands
│   ├── src/command.md    # AI system prompt (compiled via include_str!)
│   ├── src/command-emoji.md  # Emoji-flavored AI system prompt
│   ├── src/agent/          # Agent bridge + sessions (see doc/融合计划-Agent集成-v0.2.md)
│   ├── src/mcp.rs          # MCP client: stdio JSON-RPC, server registry (mcp.toml)
│   ├── src/a2a.rs          # A2A client: agent card discovery + task delegation (a2a.toml)
│   ├── src/capability.rs   # capability system: CLI tool whitelist + SKILL docs (tools/skills.toml)
│   ├── Cargo.toml
│   ├── build.rs
│   ├── tauri.conf.json   # decorations: false, CSP: null, frontendDist: ../ui
│   └── capabilities/     # Tauri 2 permission grants
├── ui/                   # Frontend (vanilla HTML/CSS/JS)
│   ├── index.html        # Full layout: titlebar, workspace, command bar, statusbar
│   ├── styles.css        # Dark IDE theme + syntax highlighting colors
│   ├── i18n.js           # Multi-language: I18N.init / setLang / getLang / t()
│   ├── command.js        # Command parser, dispatcher, all command implementations
│   ├── main.js           # UI state, file tree, tabs, xterm, context menu, modal
│   ├── session.js        # Session chat (nav list + chat tabs, run artifacts apply-back panel)
│   ├── mcp.js            # MCP panel (servers, tools, tool calls)
│   ├── a2a.js            # A2A panel (remote agents, task delegation)
│   ├── capability.js     # capability panels: tool whitelist + SKILL editor
│   ├── config.js         # Config form: scan 3 scopes → form → save/apply/cancel (window.ConfigUI)
│   ├── welcome-zh.html   # Welcome page (Chinese), injected by loadWelcome()
│   ├── welcome-en.html   # Welcome page (English), injected by loadWelcome()
│   ├── xterm.js          # xterm.js library (vendored)
│   ├── xterm.css         # xterm.js styles (vendored)
│   └── lang/             # Language files
│       ├── zh-CN.json    # 🇨🇳 Chinese (default)
│       └── en.json       # 🇬🇧 English
├── crates/harness-engine/  # Agent engine lib (zero tauri; plan/generate/lint/verify/repair/kb/exec/sandbox/gitops)
│   └── examples/plan_only.rs  # plan-only e2e smoke (real LLM call via DEEPSEEK_API_KEY env)
│   └── examples/agent_loop_smoke.rs  # 四原语工具循环 e2e（脚本化假 LLM，无需 Key，跑完自断言）
├── tools/lint/             # harness_lint python package (engine lint stage; HARNESS_LINT_DIR can override)
├── scripts/                # check-style.js (style gate), ui-smoke.js (UI smoke: contracts + session/config replay)
├── doc/                  # Design docs (Chinese)
└── Cargo.toml            # Workspace manifest (members: src-tauri, crates/harness-engine)
```

## Build & Run

```bash
cargo build              # Build
cargo run                # Run the Tauri app
cargo test               # Run all tests
cargo clippy             # Lint
cargo fmt                # Format
```

## Key Constraints

- Rust edition **2024** — requires Rust 1.85+
- Windows-first (developed on Windows 11). WebView2 is included with Windows 11.
- Window decorations disabled (`"decorations": false`) — custom titlebar with `data-tauri-drag-region`.
- Frontend has no bundler — Tauri serves files from `ui/` directly (`"frontendDist": "../ui"`).
- **No npm dependencies**. All vendored or vanilla JS.

## Frontend State Model (main.js)

The `state` object drives the UI:
- `state.currentProject` — `{ name, path } | null`. Controls project workspace.
- `state.tabs` — `[{ id, name, path, content, _isTerminal?, _term?, _highlighted?, ... }]`.
- `state.activeTabId` — which tab is currently shown.

`project-workspace` is **always visible**. The navigator sidebar adapts:
- **No project**: Shows "项目列表" + "能力" tabs only (会话 tab hidden — agent needs project context; `agent` verb is gated the same way).
- **Project open**: Shows "项目文件" / "终端资源" / "运行目标" / "会话" / "能力" tabs ("能力" panel hosts MCP/A2A/工具/技能 as sub-tabs).

### Sessions (session.js — multi-session chat)

`state.tabs` entries may carry `_isSession: true` + `_session` (session object) + `_sessionEl` (chat DOM, lazily built by `SessionUI.ensureChatEl`, re-mounted into `#session-container` on every switchTab — same pattern as terminal tabs re-mounting xterm). The nav "会话" panel lists the project's sessions (`agent_session_*`, persisted in `<root>/.ruyix/code/agent/sessions/*.json`); clicking opens a chat tab. Each message invokes `agent_reply` — the engine's **agent tool loop** (Read / Write / Execute / Connect atomic capabilities): the model decides per round which capability to call until it emits `{"final": ...}`; questions get answered by reading the project, tasks by writing whole files + verifying via execute, anything outside the repo by connecting to MCP tools / remote A2A agents. The model may declare a `plan` (emitted as `agent://plan`, shown in the outline with ✅/⌛/⛏️/❌ states driven by `agent://step`). Before a `final` is accepted the engine runs the **quality gate** (v0.3): full verification (Apply mode) + a clean-context reflection agent; both conclusions arrive as `agent://verify` / `agent://reflect` and land under the bubble as a 验证/复核 section (`SessionUI`'s `gateHtml`), persisted on the message (`m.verify` / `m.reflect`) so reopening a session still shows "this run was verified". Write-back per toolbar mode: 确认 → `WritePolicy::Stage` (changes staged under `.ruyix/stage/`, diff panel opens in-chat, `agent_stage_apply` only via explicit confirm), 写入/自主 → `WritePolicy::Apply` (direct write, overwritten files backed up to `.ruyix/backups/`). Closing the project (`setNavigatorMode("projects")`) drops all chat tabs (`SessionUI.projectClosed`).

### 产物写回（agent/apply.rs + session.js 的三模式）

引擎**只在沙箱里生成**：`<runs_root>/<run_id>/project/`（`workspace::project_dir`）。这是刻意的隔离，沙箱与真实仓库之间没有隐式搬运 —— 写回的门禁强度由会话工具栏的三个模式决定：

- **确认模式**（默认）：run 结束自动在助手气泡下打开差异面板（逐文件勾选 + 修改前/后内容），`data-confirm` 才写入。
- **写入模式**：验证通过（`status=verified`）自动写入全部 add/modify；未通过回落差异面板转人工。
- **自主模式**：run 结束无条件自动写入（写前仍备份）。

- `agent_apply_preview(run_id, project_root)` → **只读**，逐文件给 `add | modify | same`，附 `before`（项目现状）与 `after`（产物内容）以及 `dirty`（git 工作树是否脏）。
- `agent_apply_run(run_id, project_root, paths, backup?)` → 只写 `paths` 里列出的文件；内容一致的跳过并记原因；写前把被覆盖的原文件备份到 `<项目>/.ruyix/backups/<run_id>-<时间戳>/`。

三条硬约束（改动 apply.rs 时不得绕过）：路径封闭（拒绝绝对路径 / `..` / `.git`）、拒绝把产物写进沙箱自身（否则上一次产物会变成项目源码）、写前备份。UI 侧所有写入（面板确认与自动模式）收敛到 `session.js` 的 `applyPaths` 唯一入口（恒 `backup: true`）；由 ui-smoke U14 守住"三模式完整 + 唯一写入口 + 写入模式验证门禁"。

## Command System (command.js)

### Architecture

The command system is the **one and only bridge** between frontend GUI and Rust backend. Every data-mutating operation must go through `handleCommand()`. Context menu handlers, buttons, etc. construct command strings and pass them to `handleCommand()` rather than calling `invoke()` directly.

```
  GUI (click / right-click / keyboard)
        │
        ▼
  handleCommand("del path", _fromAi)
        │
        ▼
  handleDeleteCommand(raw, skipConfirm)
        │
        ▼
  invoke("delete_path", ...)   ←  only command handlers call invoke()
```

### Command verbs

| Verb | Handler | Notes |
|------|---------|-------|
| `open project\|file` | `handleOpenCommand` | File paths must be relative; `resolveProjectPath()` enforces. `open project <other>` = switch: auto `teardownProject()` (close PTYs, clear tabs) then open; same path → "already current" status |
| `close project\|all\|<idx>\|other\|left\|right` | `handleCloseCommand` | |
| `config add\|get\|update\|remove\|form\|save\|apply\|cancel` | `handleConfigCommand` | `-g`/`-p`/`-r` scope flags, default `-r`; `form\|save\|apply\|cancel` 交给 `window.ConfigUI`（配置表单标签） |
| `new py\|rs\|md\|c\|file\|folder` | `handleNewCommand` | Uses `resolveProjectPath()` |
| `del\|delete\|remove\|rm` | `handleDeleteCommand` | `skipConfirm` param for GUI path |
| `rename\|mv` | `handleRenameCommand` | `rename <old> <new>`, validates illegal chars, checks target exists |
| `refresh [<path>]` | `handleRefreshCommand` | No arg = whole tree; arg = targeted folder refresh (`refreshTreeNode`), keeps expansion state |
| `run <name>=<cmd>` | `handleRunCommand` | Shortcut: two `config add -p` calls, index = max(existing)+1 |
| `project lang\|edit\|delete\|migrate` | `handleProjectCommand` | `project lang <lang> <path>` sets language (path = remainder, may contain spaces); `project edit "<path>" "<name>" <lang>` updates name+icon (quote-aware, path read-only); `project delete <path>` removes from list (remainder path; does NOT delete the folder); `project migrate` migrates legacy path-only project config |
| `agent [task]` | `window.SessionUI` | Session chat: no arg = new session tab; with task = new session + send (project required) |
| `mcp [list\|call <server> <tool> {json}]` | `window.McpUI` | MCP servers; no arg = `list` |
| `a2a [list\|send <name> <text>]` | `window.A2aUI` | Remote A2A agents; no arg = `list` |
| `tools [list\|add <name> [cmd] [desc]\|probe <name>\|remove <name>]` | `window.ToolsUI` | CLI tool whitelist (`tools.toml`) |
| `skill [list]` / `skills [list]` | `window.SkillsUI` | SKILL docs (`skills.toml`); editing via panel |
| `help` | `openHelp()` | |
| *unknown* | `handleAiCommand` | Falls through to LLM translation |

### Path Security

`resolveProjectPath(rawPath)` — central security function for path validation:
1. Checks `state.currentProject` is set
2. Rejects absolute paths (regex: `/^\/|^[A-Za-z]:[/\\]/i`)
3. Returns `projectRoot + "\\" + rawPath`

Used by: `openFile`, `handleNewCommand`, `handleDeleteCommand`, `handleRenameCommand`.

## Multi-Language (i18n.js)

```
  lang: "zh-CN" | "en"        (mutually exclusive base language)
  emoji: true | false          (overlay toggle, independent)
```

- Config keys: `ruyix.code.ui.lang`, `ruyix.code.ui.emoji`
- `I18N.init()` reads config, loads the right JSON file
- `I18N.t(key, params)` returns translated string with `{param}` substitution
- `I18N.setLang()` / `I18N.setEmoji()` persist to config
- Language menu: 中文 / 英文 (radio) + Emoji (toggle checkbox)
- All `setStatus()` calls use `I18N.t()` keys

## AI Integration (ai.rs)

- System prompt: `src-tauri/src/command.md` compiled via `include_str!`
- Emoji mode: `src-tauri/src/command-emoji.md` (selected when `ruyix.code.ui.emoji == "true"`)
- Project context: When project is open, project root path is prepended to user message
- All AI config read from `ruyix.code.ai.*` keys (runtime → project → global fallback)

## UI Components

### Welcome Page
- 3×3 CSS grid of feature cards (no SVG, no PNG), grouped into 3 dimension rows with labels: 功能 / 性能 / 智能 (Features / Performance / Intelligence)
- Content lives in `welcome-zh.html` / `welcome-en.html`, fetched by `loadWelcome()` based on `I18N.getLang()` and injected into `#welcome-content` (token-guarded against races)
- Bottom text: `ruyix` / `越来越懂你` (zh) or `The more you use it, the more it understands you.` (en)

### Context Menu (file tree right-click)
- Folder: Delete / Rename / Create File
- File: Delete / Rename
- All routed through `handleCommand()`

### Titlebar Project Switcher
- Click the project name in the titlebar (`#project-switch-trigger`, `-webkit-app-region: no-drag`) → dropdown of `get_projects` (current project highlighted + ✔), click = switch via `handleCommand("open project ...")`
- Dropdown is `.menu-dropdown` so `setupMenuBar`'s outside-click-close covers it; rebuilt on each open by `renderProjectSwitchDropdown()`

### Custom Modal
- `showConfirm(title, message)` → `Promise<boolean>`
- `showPrompt(title, defaultValue)` → `Promise<string|null>`
- Dark theme, matches IDE. Replaces browser-native `window.confirm`/`window.prompt`.

### File Icons
- 🐍 Python, 🦀 Rust, 🌏 HTML, 🎨 CSS, Ⓙ JS, ☕ Java, Ⓜ️ Markdown, 🛢️ SQL, 🖼️ Images
- 🚫 .gitignore, ⚙️ .toml, 🧩 .json, 📄 unknown
- Folder icons: ➡️ collapsed, ⬇️ expanded with children, 🈳 empty

## Config System (config.rs)

Three scopes with prefix `ruyix.code`:
| Scope | Flag | Storage |
|-------|------|---------|
| Global | `-g` | `~/.ruyix/code/<section>.toml` |
| Project | `-p` | `<project_root>/.ruyix/code/<section>.toml` |
| Runtime | `-r` | In-memory HashMap (not persisted) |

Known config keys:
- `ruyix.code.ai.api_key` / `api_url` / `model`
- `ruyix.code.ui.lang` / `ruyix.code.ui.emoji`
- `ruyix.code.run.target<N>.cmd` / `target<N>.name`
- `ruyix.code.harness.gate.narrow` / `gate.full` / `gate.max_full_attempts` / `gate.staged_timeout_secs`（v0.3 机械验证门禁）
- `ruyix.code.harness.reflect.enabled` / `reflect.max_rounds` / `reflect.model`（v0.3 复核 agent）

## Tauri Commands (main.rs)

| Command | Signature | Notes |
|---------|-----------|-------|
| `open_project` | `(path)` → `ProjectInfo` | Sets current project in global config; `ProjectInfo { name, path, lang }` |
| `get_last_project` | `()` → `Option<String>` | |
| `get_projects` | `()` → `Vec<ProjectEntry>` | All known projects; `ProjectEntry { name, path, lang }`, lang ∈ PROJECT_LANGS (10 values, default `unknown`) |
| `set_project_lang` | `(path, lang)` → `()` | Validates lang against PROJECT_LANGS |
| `update_project` | `(path, name, lang)` → `()` | Updates name+lang (path immutable); rejects empty name / invalid lang |
| `delete_project` | `(path)` → `()` | Removes entry from list (folder untouched); clears `current` if it matches |
| `migrate_projects` | `()` → `usize` | Converts legacy path-string `projects.list` entries to `ProjectEntry` (name = folder basename, lang = unknown), rewrites projects.toml; returns migrated count |
| `get_run_targets` | `(project_root?)` → `Vec<RunTarget>` | |
| `list_dir` | `(path)` → `Vec<DirEntry>` | |
| `read_file` | `(path)` → `FileContent` | |
| `write_file` | `(path, content)` → `()` | |
| `create_file` | `(path)` → `()` | |
| `create_dir` | `(path)` → `()` | |
| `delete_path` | `(path)` → `()` | Removes file or directory |
| `rename_path` | `(from, to)` → `()` | `std::fs::rename` |
| `path_exists` | `(path)` → `bool` | |
| `config_get` | `(scope, key, project_root?)` → `Option<String>` | |
| `config_set` | `(scope, key, value, project_root?)` → `()` | Rejects run targets with global scope |
| `config_delete` | `(scope, key, project_root?)` → `()` | |
| `config_form_load` | `(scope, project_root?)` → `ScopeEntriesDump` | Scans a scope + its fallback chain into flat entries `{section,key,full_key,value,inherited?}` for the config form; excludes structured files (projects/execute/mcp/a2a/tools/skills) |
| `config_form_save` | `(scope, entries, project_root?)` → `ScopeSaveReport` | Incremental write of the submitted keys (empty value = delete key); preserves non-scalar content in touched sections; rejects run targets in global scope |
| `config_form_apply` | `(scope, entries, project_root?)` → `ScopeSaveReport` | Same as save **plus** refreshes the in-memory runtime object for every non-empty value (highest lookup priority, lost on restart) |
| `ai_translate` | `(input, project_root?)` → `String` | Calls LLM |
| `highlight_code` | `(language, code)` → `Vec<LineHighlight>` | tree-sitter |
| `run_target` | `(cmd, project_root?, bind?)` → `RunOutput` | One-shot, not interactive. cwd = directory of the `bind` manifest file (`resolve_run_dir()`), else project root |
| `spawn_terminal` | `(cmd, project_root?)` → `()` | New OS console window |
| `pty_spawn` | `(cmd, tabId, project_root?)` → `()` | PTY for inline xterm.js |
| `pty_write` | `(tabId, data)` → `()` | |
| `pty_close` | `(tabId)` → `()` | |
| `agent_run` … `agent_env_probe` | see `doc/融合计划-Agent集成-v0.2.md` 附录 A | 12 commands: run/plan/generate/verify/lint/repair/cancel/runs/run_load/run_delete/read_artifact/env_probe |
| `agent_reply` | `(task, history, mode?, project_root?)` → `ReplyAgent` | Session entry: runs the engine agent tool loop (Read/Write/Execute/Connect; max 96 rounds; Connect = `mcp_mgr` state → `connect::RuyixConnector` over MCP + A2A; cancel = shared `AgentState` flag). Mode → write policy: 确认=Stage, 写入/自主=Apply. Returns `verifications` + `reflections` for the in-chat 验证/复核 section |
| `agent_stage_preview` / `agent_stage_apply` | `(stage_id, paths?, backup?, project_root?)` | Staged agent changes under `.ruyix/stage/`: read-only diff vs live project / write selected paths (backup first, `.ruyix` itself blocked) |
| `agent_session_list` … `agent_session_delete` | `(project_root?, id?, session_json?)` | 5 commands; sessions persisted in `<root>/.ruyix/code/agent/sessions/*.json` |
| `agent_apply_preview` | `(run_id, project_root)` → `Preview` | **只读**：沙箱产物 vs 真实项目，逐文件 add/modify/same + 现状/产物内容 + git dirty |
| `agent_apply_run` | `(run_id, project_root, paths, backup?)` → `ApplyResult` | 写回勾选的文件；一致项跳过；覆盖前备份到 `<项目>/.ruyix/backups/<run_id>-<ts>/` |
| `mcp_servers` | `(project_root?)` → `Vec<ServerStatus>` | Config + running state per server |
| `mcp_add_server` | `(name, command, args?, env?, project_root?)` → `Vec<McpServerCfg>` | Persists to `mcp.toml` (global or project) |
| `mcp_remove_server` | `(name, project_root?)` → `Vec<ServerStatus>` | Also stops the connection |
| `mcp_start` / `mcp_stop` | `(name, project_root?)` | spawn + initialize handshake / kill subprocess |
| `mcp_tools` | `(name)` → `Vec<ToolInfo>` | Tools of a connected server |
| `mcp_call_tool` | `(name, tool, argsJson)` → `CallOutcome{text, is_error}` | argsJson empty = `{}` |
| `a2a_agents` | `(project_root?)` → `Vec<A2aAgentCfg>` | Registry from `a2a.toml` |
| `a2a_discover` | `(url)` → `A2aAgentCfg` | Fetches `/.well-known/agent-card.json` (fallback `agent.json`), saves to global `a2a.toml` |
| `a2a_remove` | `(name, project_root?)` → `Vec<A2aAgentCfg>` | |
| `a2a_send` | `(name, text, project_root?)` → `A2aTaskResult{agent, state, text}` | `message/send` + polls `tasks/get` to terminal state; progress via `a2a://status` events |
| `tools_list` / `tools_add` / `tools_remove` | see `doc/capability.md` | CLI whitelist in `tools.toml` (global + project merge) |
| `tools_probe` | `(name)` → `ToolProbe{name, available, version}` | runs `<command> --version` (4s timeout) |
| `skills_list` / `skills_save` / `skills_remove` | see `doc/capability.md` | SKILL markdown docs in `skills.toml` (content inline) |
