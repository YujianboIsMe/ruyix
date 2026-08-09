# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Vision

darkhorse-code is an IDE built on **Tauri 2 + Rust backend**, aiming to eventually use Monaco Editor. Currently the editor is a custom implementation using a transparent `<textarea>` overlaid on a syntax-highlighted backdrop via CSS Grid.

## Architecture

- **Frontend**: Vanilla HTML/CSS/JS (no bundler, no npm). Served from `ui/` via Tauri's custom protocol. All Tauri APIs accessed via `window.__TAURI__` global — no npm dependencies.
- **Backend**: Tauri 2 Rust backend with three modules: `main.rs` (Tauri commands + app entry), `config.rs` (3-scope config system), `pty.rs` (PTY terminal management via `portable-pty`).
- **Communication**: Tauri native IPC. Synchronous calls use `invoke()`. Async push for PTY output uses Tauri's event system `emit()`/`listen()` — events are named `pty-out-{tabId}` and `pty-exit-{tabId}`.
- **Syntax highlighting**: `tree-sitter` + `arborium` crate. Currently Python only (`arborium` feature `lang-python`). Highlighting runs in a `spawn_blocking` thread to avoid blocking the async runtime.
- **Terminal**: `xterm.js` (loaded via `<script>` tag from `ui/xterm.js`) + Rust PTY via `portable-pty` crate.

## Key Implementation Details

### Editor (textarea-over-backdrop)

The editor is NOT Monaco yet — it's a custom implementation:
- A `<div>` backdrop renders syntax-highlighted code with colored `<span>` elements
- A transparent `<textarea>` is stacked on top via CSS Grid (`grid-area: 1 / 1`) with `color: transparent` but visible caret
- Both share identical `font-family`, `font-size`, `line-height`, and `padding` so cursor aligns with rendered text
- Horizontal/vertical scrolling is unified via a single `.editor-view` container (`overflow: auto`)
- The gutter (line numbers) uses `position: sticky; left: 0` to stay fixed during horizontal scroll

### PTY Concurrency Model (pty.rs)

- The reader thread owns its `reader` exclusively — no shared lock, no contention with writes
- `writer`, `master`, and `child` each have their own `Arc<Mutex<...>>` — fine-grained locking avoids read/write deadlocks
- Closing a PTY session: kill child process → pipe closes → reader thread's `read()` returns `Err` → thread exits naturally

### Config System (config.rs)

Three scopes with prefix `darkhorse.code`:
| Scope | Flag | Storage |
|-------|------|---------|
| Global | `-g` | `~/.darkhorse/code/<section>.toml` |
| Project | `-p` | `<project_root>/.darkhorse/code/<section>.toml` |
| Runtime | `-r` | In-memory HashMap (not persisted) |

Config keys follow format: `darkhorse.code.<section>.<key>`. The section name becomes the TOML filename. Within the TOML file, keys are nested under a `[section]` table.

Run targets cannot be saved with global scope — `config_write` rejects them.

### Two Terminal/Execution Paths

1. **`spawn_terminal`**: Uses `std::process::Command` with `CREATE_NEW_CONSOLE` (0x00000010) on Windows. Launches a new OS window — no PTY, no xterm.js. Used when clicking the 🪟 icon in the terminal resources list.

2. **`pty_spawn`**: Creates a PTY pair via `portable-pty`, spawns the command on the slave side, reads output on a dedicated thread, emits Tauri events to the frontend. The frontend renders via xterm.js. Used for the inline terminal experience.

3. **`run_target`**: Runs a command via `std::process::Command::output()`, captures stdout/stderr, returns as a one-shot result. Not interactive — used for run targets displayed in plain text (not xterm.js).

### Windows-Specific Details

- `clean_path()` strips the `\\?\` extended-path prefix that Windows `Path::canonicalize()` adds
- `CREATE_NEW_CONSOLE` flag creates independent console windows for CLI programs

## Project Structure

```
├── src-tauri/            # Rust backend (Tauri 2)
│   ├── src/main.rs       # App entry, all Tauri commands, split_cmd(), highlight helpers
│   ├── src/config.rs     # ConfigManager: 3-scope config, projects persistence, run targets
│   ├── src/pty.rs        # PtyManager: spawn/write/resize/close PTY sessions
│   ├── Cargo.toml        # Dependencies (edition 2024)
│   ├── build.rs          # tauri_build::build()
│   ├── tauri.conf.json   # decorations: false, CSP: null, frontendDist: ../ui
│   └── capabilities/     # Tauri 2 permission grants
├── ui/                   # Frontend (vanilla HTML/CSS/JS)
│   ├── index.html        # Full layout: titlebar, workspace, command bar, statusbar
│   ├── styles.css        # Dark IDE theme + syntax highlighting colors
│   ├── main.js           # All frontend logic: commands, tabs, file tree, xterm, PTY events
│   ├── xterm.js          # xterm.js library (vendored)
│   └── xterm.css         # xterm.js styles (vendored)
├── doc/                  # Design docs (Chinese)
│   ├── ui.md, editor.md, navigator.md, config.md, command.md
└── Cargo.toml            # Workspace manifest
```

## Build & Run

```bash
cargo build              # Build
cargo run                # Run the Tauri app
cargo test               # Run all tests
cargo test <test_name>   # Run a single test
cargo clippy             # Lint
cargo fmt                # Format
```

## Key Constraints

- Rust edition **2024** — requires Rust 1.85+
- Windows-first (developed on Windows 11). WebView2 is included with Windows 11.
- Window decorations disabled (`"decorations": false` in tauri.conf.json) — custom titlebar with `data-tauri-drag-region` handles dragging.
- Frontend has no bundler — Tauri serves files from `ui/` directly (`"frontendDist": "../ui"`).
- Frontend Tauri API access via `window.__TAURI__` global. The code handles slight API shape differences across Tauri 2 versions through helper functions (`getTauriWindow()`, `getTauriCore()`, `getTauriInvoke()`).

## Frontend State Model (main.js)

The `state` object drives the UI:
- `state.currentProject` — `{ name, path } | null`. Controls welcome page vs project workspace.
- `state.tabs` — `[{ id, name, path, content, _isTerminal?, _term?, _highlighted?, ... }]`. Each tab is either a file editor or an xterm.js terminal.
- `state.activeTabId` — which tab is currently shown.

## Command Bar

A single-line input at the bottom. Three top-level commands:
- **open** `project <path>` | `file <path>`
- **close** `project` | `all` | `<index>` | `other` | `left` | `right`
- **config** `add|get|update|remove|delete [-g|-p|-r] darkhorse.code.<section>.<key>[=value]`

Default scope for `config` is `-r` (runtime). The `-p` flag requires an open project.
