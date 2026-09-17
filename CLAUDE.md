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
cargo clippy --all-targets         # 静态检查，必须 0 warning
cargo test -p ruyix       # 单元测试
```

Notes: `ui/xterm.js` / `ui/xterm.css` are vendored (MIT) and excluded from style checks; the frontend has
no npm/bundler, so never add npm tooling — `scripts/check-style.js` is the style gate.

## Project Vision

ruyix is an IDE built on **Tauri 2 + Rust backend**, aiming to eventually use Monaco Editor. Currently the editor is a custom implementation using a transparent `<textarea>` overlaid on a syntax-highlighted backdrop via CSS Grid.

**Architecture principle**: The command system is the **sole bridge** between frontend and backend. All GUI operations (context menu, buttons, clicks) MUST route through `handleCommand()`. No `invoke()` calls from UI event handlers — that creates AI integration blind spots.

## Architecture

- **Frontend**: Vanilla HTML/CSS/JS (no bundler, no npm). Served from `ui/` via Tauri's custom protocol. All Tauri APIs accessed via `window.__TAURI__` global.
- **Backend**: Tauri 2 Rust backend, modules: `main.rs` (Tauri commands + app entry), `config.rs` (3-scope config, projects, run targets), `pty.rs` (PTY terminal management), `ai.rs` (LLM integration), `git.rs` (git status/commands), `runner.rs` (run-command inference from manifest contents), `rag.rs` (semantic search index/retrieval), `instance.rs` (single-instance detection).
- **Communication**: Tauri native IPC. Synchronous calls use `invoke()`. Async push for PTY output uses Tauri's event system `emit()`/`listen()`.
- **Syntax highlighting**: `tree-sitter` + `arborium` crate. Highlighting runs in a `spawn_blocking` thread to avoid blocking the async runtime.
- **Terminal**: `xterm.js` (vendored from `ui/xterm.js`) + Rust PTY via `portable-pty` crate.

## Project Structure

```
├── src-tauri/            # Rust backend (Tauri 2)
│   ├── src/main.rs       # App entry, Tauri commands, file operations
│   ├── src/config.rs     # ConfigManager: 3-scope config, projects, run targets
│   ├── src/pty.rs        # PtyManager: spawn/write/resize/close PTY sessions
│   ├── src/ai.rs         # AI translation: natural language → standard commands
│   ├── src/command.md    # AI system prompt (compiled via include_str!)
│   ├── src/command-emoji.md  # Emoji-flavored AI system prompt
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
│   ├── welcome-zh.html   # Welcome page (Chinese), injected by loadWelcome()
│   ├── welcome-en.html   # Welcome page (English), injected by loadWelcome()
│   ├── xterm.js          # xterm.js library (vendored)
│   ├── xterm.css         # xterm.js styles (vendored)
│   └── lang/             # Language files
│       ├── zh-CN.json    # 🇨🇳 Chinese (default)
│       └── en.json       # 🇬🇧 English
├── doc/                  # Design docs (Chinese)
└── Cargo.toml            # Workspace manifest
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
- **No project**: Shows "项目列表" tab with history projects (from global config).
- **Project open**: Shows "项目文件" / "终端资源" / "运行目标" tabs.

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
| `config add\|get\|update\|remove` | `handleConfigCommand` | `-g`/`-p`/`-r` scope flags, default `-r` |
| `new py\|rs\|md\|c\|file\|folder` | `handleNewCommand` | Uses `resolveProjectPath()` |
| `del\|delete\|remove\|rm` | `handleDeleteCommand` | `skipConfirm` param for GUI path |
| `rename\|mv` | `handleRenameCommand` | `rename <old> <new>`, validates illegal chars, checks target exists |
| `refresh [<path>]` | `handleRefreshCommand` | No arg = whole tree; arg = targeted folder refresh (`refreshTreeNode`), keeps expansion state |
| `run <name>=<cmd>` | `handleRunCommand` | Shortcut: two `config add -p` calls, index = max(existing)+1 |
| `project lang\|edit\|delete\|migrate` | `handleProjectCommand` | `project lang <lang> <path>` sets language (path = remainder, may contain spaces); `project edit "<path>" "<name>" <lang>` updates name+icon (quote-aware, path read-only); `project delete <path>` removes from list (remainder path; does NOT delete the folder); `project migrate` migrates legacy path-only project config |
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
| `ai_translate` | `(input, project_root?)` → `String` | Calls LLM |
| `highlight_code` | `(language, code)` → `Vec<LineHighlight>` | tree-sitter |
| `run_target` | `(cmd, project_root?, bind?)` → `RunOutput` | One-shot, not interactive. cwd = directory of the `bind` manifest file (`resolve_run_dir()`), else project root |
| `spawn_terminal` | `(cmd, project_root?)` → `()` | New OS console window |
| `pty_spawn` | `(cmd, tabId, project_root?)` → `()` | PTY for inline xterm.js |
| `pty_write` | `(tabId, data)` → `()` | |
| `pty_close` | `(tabId)` → `()` | |
