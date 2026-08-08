# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Vision

darkhorse-code is an IDE built on **Tauri 2 + Monaco Editor + Rust backend** (inspired by SideX and other open-source IDEs).

## Architecture (from README)

- **Frontend**: Monaco Editor as the editor kernel, TypeScript for IDE shell (sidebar, terminal, status bar, file tree). Runs directly in Tauri's system WebView — no WASM compilation needed.
- **Backend**: Tauri 2's Rust backend manages LSP processes (e.g., pylsp) via `std::process::Command`, with `tokio` handling async JSON-RPC messaging — no extra WebSocket service required.
- **Communication**: Tauri native IPC replaces WebSocket. Synchronous calls (open/save file) use `invoke()`. Async push (diagnostics, completions) uses Tauri's event system `emit()`/`listen()`.

## Project Structure

```
├── src-tauri/            # Rust backend (Tauri 2)
│   ├── src/main.rs       # Tauri app entry point
│   ├── Cargo.toml        # Rust dependencies
│   ├── build.rs          # Tauri build script
│   ├── tauri.conf.json   # Tauri configuration
│   ├── capabilities/     # Permission grants for the frontend
│   └── icons/            # App icons
├── ui/                   # Frontend (vanilla HTML/CSS/JS for now)
│   ├── index.html        # Main layout: titlebar, workspace, command bar, status bar
│   ├── styles.css        # Dark IDE theme
│   └── main.js           # Window controls + command bar logic
└── Cargo.toml            # Workspace manifest
```

## Build & Run

```bash
# Build (from project root)
cargo build

# Run the Tauri app
cargo run

# Run tests
cargo test

# Run a single test
cargo test <test_name>

# Lint
cargo clippy

# Format
cargo fmt
```

## UI Layout (from doc/ui.md)

- **眉部 (Titlebar)**: Custom titlebar with menu bar on the left, Windows min/max/close buttons on the right. Uses `decorations: false` + `data-tauri-drag-region`.
- **工作区 (Workspace)**: Shows welcome page ("最好用的IDE，个人使用") when no project is open.
- **命令区 (Command bar)**: Single-line command input at the bottom. Enter to execute. No buttons.
- **状态栏 (Status bar)**: Blue bar at the bottom. Content varies with workspace.

## Key Constraints

- Rust edition **2024** — requires Rust 1.85+.
- Windows-first (developed on Windows 11). WebView2 is required and included with Windows 11.
- Frontend currently uses vanilla HTML/CSS/JS (no bundler). Tauri serves files from `ui/` via custom protocol.
- Frontend accesses Tauri APIs via `window.__TAURI__` global object — no npm dependencies needed for now.
- Window decorations are disabled; the custom titlebar handles drag (`data-tauri-drag-region`) and window controls.
