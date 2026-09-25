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
node scripts/editor-layout.js      # 编辑器真实布局（无头 Edge；找不到浏览器时自行 SKIP）
node scripts/session-trace-layout.js  # 会话执行轨迹的真实布局（同上，省略号/折行/滚动条）
cargo clippy --all-targets         # 静态检查，必须 0 warning
cargo test                         # 单元测试（引擎 322+8 ignored / ruyix 120+3 ignored）
```

Notes: `ui/xterm.js` / `ui/xterm.css` are vendored (MIT) and excluded from style checks; the frontend has
no npm/bundler, so never add npm tooling — `scripts/check-style.js` is the style gate.
`scripts/editor-layout.js` and `scripts/session-trace-layout.js` are the exception to "Node-only gates":
they drive the real `index.html` + `styles.css` + panel JS in headless Edge/Chrome because
scrollbar/geometry bugs (double scrollbars, caret vs. backdrop misalignment, a line that should
ellipsize but wraps or widens its box instead) do not exist in a DOM stub — ui-smoke U32 / U41
call them (and skip loudly when no browser is installed, so a green run there is only claimable
when a browser was actually found).

## Project Vision

ruyix is an IDE built on **Tauri 2 + Rust backend**, aiming to eventually use Monaco Editor. Currently the editor is a custom implementation using a transparent `<textarea>` overlaid on a syntax-highlighted backdrop via CSS Grid.

**Architecture principle**: The command system is the **sole bridge** between frontend and backend. All GUI operations (context menu, buttons, clicks) MUST route through `handleCommand()`. No `invoke()` calls from UI event handlers — that creates AI integration blind spots.

## Architecture

- **Frontend**: Vanilla HTML/CSS/JS (no bundler, no npm). Served from `ui/` via Tauri's custom protocol. All Tauri APIs accessed via `window.__TAURI__` global.
- **Frontend globals**: `main.js` owns the app state; panel modules (`session.js` / `mcp.js` / `a2a.js` / `capability.js` / `config.js` / `service.js`) read it as `window.state`. A top-level `const` does NOT land on `window`, so `main.js` must keep the explicit `window.state = state;` export — drop it and every panel loses `currentProject` (config project scope then falsely reports "no project open"). Guarded by ui-smoke U10.
- **Backend**: Tauri 2 Rust backend, modules: `main.rs` (Tauri commands + app entry), `config.rs` (3-scope config, projects, run targets), `pty.rs` (PTY terminal management), `ai.rs` (LLM integration), `git.rs` (git status/commands), `runner.rs` (run-command inference from manifest contents), `instance.rs` (single-instance detection), `agent/` (Agent bridge: `agent_*` commands + `agent://*` events + config_bridge + `sessions.rs` chat-session store, see `doc/v0.x/融合计划-Agent集成-v0.2.md`), `mcp.rs` (MCP client over stdio JSON-RPC: server registry in `mcp.toml`, tools discovery & invocation), `a2a.rs` (A2A client: agent card discovery + task delegation, registry in `a2a.toml`), `capability.rs` (capability system: CLI tool whitelist `tools.toml` + SKILL docs `skills.toml`, see `doc/capability.md`), `paths.rs` (portable root + project buckets).
- **Agent engine**: `crates/harness-engine` — zero-tauri lib crate imported from darkhorse-harness (agent tool loop + plan/generate/lint/verify/repair pipeline; unit tests via `cargo test -p harness-engine`). The ruyix `agent/` module wraps it; engine code lives in the workspace, don't re-vendor.
`modelstore` is the **shared** on-demand model cache (self-check + `.part` atomic write + per-file sha256 +
sources; used by both the memory embedding model and the voice whisper weights — one mechanism, two specs),
and `voice/` is local speech-to-text (see *本地语音转写* below). **Agent tool loop** (`engine::agent`): four atomic capabilities — Read / Write / Execute / Connect — model-driven rounds until `{"final":...}`, max 96 rounds (`MAX_STEPS`); Read and Write are path-jailed to the project, there is no surgical `edit` tool (a change is a whole-file Write: read first, hand back the complete content); Execute runs in the project root with timeout + output clipping + a destructive-pattern deny list (tripwire, not a sandbox — real isolation is the verify pipeline's docker mode). **Connect** is the one primitive the engine cannot implement alone: it declares a `Connector` trait (`list` / `call`) and the host lands it — ruyix wires `src-tauri/src/agent/connect.rs` (`RuyixConnector`) over MCP servers (`mcp.toml`, auto-handshake on first call), A2A remote agents (`a2a.toml`, delegation progress on `a2a://status`), and an **environment-prep** target (`kind = "env"`, `agent::ENV_CONNECTOR_KIND`); `NoConnector` is the empty impl for tests/eval. The available-connection list is injected into the run's first user message so the model never guesses names — and so is the **write policy** when it is `Stage`: in confirm mode a `write` only lands in `.ruyix/stage/`, so the overlay-aware `read` shows the staged content while `execute` (which runs against the real project disk) still returns the *pre-change* files. That contradiction is invisible unless it is stated, and run `agent-20260920-152312` is what it costs: told nothing about the mode, the model spent 45+ of its 65 rounds re-`findstr`ing a `pom.xml` it had already fixed correctly, because the standing "verify with execute" rule kept reporting failure. The injected note names the three facts (writes are staged / `execute` sees the old file / **don't** use `execute` to verify this change) and tells it to deliver `final` and state what the user should run after applying; `policy_system_note` returns `None` for `Apply`, where the disk really does change and there is no such split, so the prompt never claims a mode the run isn't in. `staged_execute_note` re-states it as a one-line banner on `execute` results whenever a Stage run already holds staged changes (belt and braces for a model that skips the preamble). Both the main loop and `step_agent::run_step` append the same note, so a sub-step cannot restart the same empty loop. **Command discovery** (`engine::discover`, v0.5): the first user message also carries the machine's *actual* command inventory, because the engine already probed it and the model simply could not see it — run `agent-20260920-152312` burned 5-6 of its 65 rounds (`dir /s /b %USERPROFILE%\.m2\...`, `jar tf`, `type pom.xml`, `python -c "import zipfile"`) finding out whether `mvn` and `java` existed. `discover::TOOLS` is a single const table (name / bin / version args / marker files / extensions); relevance comes from root-level markers (`pom.xml` → probe `mvn` + `java`) plus resident entries (`git`), and `ruyix.code.harness.discover.extra` (comma-separated, exposed in the config UI) appends binaries without touching code — the growth lives in data, not in match arms. Probing is **two steps** and lives in `exec::resolve_bin` / `exec::bin_version`, shared with `exec::probe` so the host's environment panel gets the same fix: `Command::new("mvn")` is blind to Windows `.cmd` shims (measured: `program not found`, while `where mvn` resolves `D:\Tools\Maven\...\bin\mvn.cmd`), and `cmd /C` cannot answer "does it exist" either (a missing command exits **1**, indistinguishable from a tool that exited 1). Availability therefore comes from `where` / `command -v` alone, the version command goes **through the shell**, and a version line that fails to arrive is *not* treated as "unavailable". Results are cached per binary under `ruyix.code.harness.discover.ttl_secs` (default 300, `0` = always re-probe) and probed **concurrently** (JVM tools cost ~1 s each: a Maven project measured 2.7 s serial, cache hit ~100 µs). The note states **both** what exists and **what does not** — saying only "you have `mvn`" does not stop the spinning; the missing list points at `connect` only when the host actually offers an `env`-kind target (`agent::ENV_CONNECTOR_KIND`), otherwise it tells the model to report the gap, because a prompt must not advertise a capability that is not there. **Environment prep** (software installation, v0.5): discovery tells the model *what is missing*; closing the loop needs a way to *install it*, and that primitive is **not new** — it is an ordinary Connect target. The engine holds **zero** package-manager knowledge: it only owns the `ENV_CONNECTOR_KIND` convention and the rule that missing commands may point at `connect`. The host lands it in `src-tauri/src/agent/env_setup.rs`: `Pm` is a single table (winget / scoop / choco / brew / apt-get / dnf / pacman) whose `install_line` maps a tool name to the manager's package id and emits a **non-interactive** command, `pick_manager()` returns the first platform candidate that `where`/`command -v` resolves (cached in-process), and `install()` runs the line **through the shell** (`exec::run_line` — `.cmd` shims like `scoop` cannot be launched by `CreateProcess`) inside `spawn_blocking`. `RuyixConnector` lists the env target only when `ruyix.code.harness.env.install_enabled` (default **on**, one-line rollback) and routes `connect {action:"call", server:"env", tool:"install"}` to it **before** `call_mcp` (otherwise it would fall into "no such server"). Every action is **recorded** — one JSONL line in `<root>/.ruyix/env/installs.jsonl` (tool, reason, manager, exact command, exit code, output tail) plus an `env://install` UI event — and a success invalidates the discovery cache so the next probe sees the new tool. New capability = a new table row, not a new match arm. **Execute pre-flight & output decoding** (v0.6): before it touches the shell, `agent::preflight_execute` refuses the three kinds of command that are *certainly* junk and answers with a readable correction instead of burning a round — a **launcher** that hands the work to the shell (`start` / `explorer` / `open` / `rundll32`, and PowerShell's `Start-Process` / `Invoke-Item` by text, also matched through a `cmd /C` wrapper), because those detach stdout/stderr *and*, in a real console, turn a missing target into a desktop dialog; a **path-shaped first token that exists nowhere** (measured: `\` and `\admin-run\` were handed to `cmd` verbatim, so the model got back one line it could not read); and a **command that is not installed**, which is answered with the discovery table's actual inventory (`discover::is_available` / `available_names`, reusing the same table and cache) because "no such command" without "here is what you do have" does not stop the model from guessing again. Everything else runs: the gate is deliberately conservative, since wrongly blocking one legitimate command costs more than letting one junk command through. The other half is decoding — `exec::decode_output` returns bytes as UTF-8 when they *are* UTF-8 and otherwise goes through the Windows **active code page** (`GetACP` + `MultiByteToWideChar`, zero new dependencies), because on a Chinese Windows `cmd` / `java` / `mvn` / `git` emit GBK and `from_utf8_lossy` turned `不是内部或外部命令` into `'\' �����ڲ����…` — the model could not read its own failure, so it kept replacing one wrong command with a worse one. Measured bytes for `cmd /C \admin-run\`: `27 5C 61 64 6D 69 6E 2D 72 75 6E 5C 27 20` then `B2 BB CA C7 C4 DA B2 BF BB F2 CD E2 B2 BF …`. One implementation, four consumers: the engine's `run_with_cap`, the host's run panel (`run_target`), the host's environment panel (`probe_tool`) and `git.rs`. **Prompt & failure feedback**: the system message is `AGENT_SYSTEM` plus an OS-gated suffix — `agent_system_prompt()` appends a Windows retrieval hint (prefer `git grep -i -l`; `findstr`'s multi-mask behaviour is unreliable) while the base constant stays byte-identical so tests can keep asserting it. An unparseable round never aborts the run: the feedback distinguishes truncation (`finish_reason=length` → tell the model to *shorten*) from malformed output (→ *resend*), and the error text goes through `serde_json::to_string` because `parse_action`'s message carries a JSON fragment — raw interpolation used to break the feedback's own JSON. Pipeline self-repair: verify-failure stderr + lint diagnostics + failed-generation steps fed back → find/replace patch (atomic, CRLF-normalized) → re-verify; max 5 rounds by default (`ruyix.code.harness.lint.max_repair_rounds`), stops when issue count stops dropping. The model may answer `{"need": [...]}` to ask for file/dir contents (path-jailed; up to 3 ask-rounds, not counted against repair rounds), and may create missing files via the whole-file shape. **Quality gate** (`v0.3`, fact-driven — never a task classifier): a write triggers a *narrow* syntax layer (`verify::staged_syntax_checks`, runs on the change's content in a temp dir, so confirm-mode keeps its "disk untouched" invariant), and delivery (`{"final":...}`) triggers a *full* layer (`verify::run` + lint, Apply mode only; Stage mode reports an explicit `skipped` with the reason). A failed full verification **blocks the final** and feeds the report back (max `gate.max_full_attempts`, then it releases with an honest "未通过" footer). Before releasing, a **reflection agent** (`engine::reflect`) reviews the artifacts in a *clean context* — its own `messages`, only `read` (no write path exists in that module by construction), rubric chosen by the fact "were files changed?" (artifact review vs evidence check), structured findings `{claim, evidence, verdict}` fed back to the main loop (never a second writer), and any failure degrades to a `note` instead of failing the run. Conclusions stream out as `agent://verify` / `agent://reflect` and are rendered as a per-message 验证/复核 section (`ui/session.js`). **Step agent** (`engine::step_agent`, v0.4): a *plan step is itself an agent loop with its own clean context* — `run_step` builds `messages` from `STEP_SYSTEM` (deliberately not `AGENT_SYSTEM`: no connect/plan/history) plus one input package (task, position i/n, the step's declared files, engine-written one-line summaries of finished steps, capped at 12 lines / 1.5 KB), and only `read` / `write` / `execute` survive the narrowing `parse_step_action` (a `plan`/`connect` action is refused with a substitute, not a failed step). It borrows the **same** `&mut Ctx` as the main loop, so the overlay never splits and `emit_step_progress`'s "declared files landed" criterion holds automatically; it never emits `sink.step` (that index is one-dimensional in `ui/session.js`). Writes trigger the same narrow syntax layer, failures are fed back as observations, and the step hands back an *engine-written* fact line (`files` + verify verdict) — the model's own `final` is kept as `self_report` and is explicitly not a source of truth. Budget exhaustion / cancellation land as `status=error` with the reason (already-written files stay in the overlay), while a fatal model error propagates `Err` exactly like the main loop. Per-step budget is `step.max_steps` (default **96**, aligned with the parent budget — a tighter sub-budget only buys a parent intervention round on top of the unfinished step; what really bounds a runaway step is the wall-clock gate `agent.max_elapsed_secs`, default 1800s). **Wired into the main loop** behind `ruyix.code.harness.step.execute_plan` (default **on** since v0.4 — leaving it off meant the outline's per-step state could only be *inferred* from "did the step's declared files land", and that inference mislabels real runs at scale: run `agent-20260920-142405` reported 6 of 8 steps as "skipped" when 4 of them had written more than half their files and one had declared a file that already existed on disk. The engine *knows* each step's outcome, so that is the default path; `false` still restores the old display-only behaviour line for line). With it on the **engine, not the model, holds the step cursor**: one parent round *is* one step (or one intervention round), so letting the model pick "which step next" would only produce out-of-order/duplicate work plus an extra round spent choosing. `MAX_STEPS` therefore changes meaning from "at most N model calls" to "at most N steps/interventions" — a step's own rounds stop counting against the parent budget, which is exactly what "96 rounds is not enough" was about. Dispatch emits `running` *before* the step starts (previously `running` only appeared after a write, leaving the user staring at ⌛) and `done`/`error` when it returns; `emit_step_progress`'s "did the declared files land" heuristic is switched off in that mode — the engine now knows the step's real outcome, and running both channels only makes them contradict each other. A failed step lands ❌ (the `error` state's first legitimate source on the agent path) and **stops to hand the model one intervention round** (re-plan / fix it yourself / deliver and say why), because continuing after step 1 blew up just burns what is left. Re-planning resets the cursor to step 1 and is capped (`MAX_PLAN_RESETS = 2`) so "fail → re-plan → fail → re-plan" cannot eat the whole budget. `settle_steps` takes the executed steps' real terminal states instead of re-deriving them from file landings (it still re-emits one terminal event per step, so "every step has a final state" holds no matter how the run ended). Run-level wall-clock guard: `ruyix.code.harness.agent.max_elapsed_secs` (default 1800, `0` = unlimited), checked by the parent loop *and* threaded into `run_step` — one step can burn dozens of rounds with the parent none the wiser.
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
│   ├── src/agent/          # Agent bridge + sessions (see doc/v0.x/融合计划-Agent集成-v0.2.md)
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
│   ├── service.js        # Service panel: managed processes from the engine's table + stop by pid (window.ServiceUI)
│   ├── external.js       # External-link gate: capture-phase click/auxclick → preventDefault → `open url` (window.ExternalLinks)
│   ├── welcome-zh.html   # Welcome page (Chinese), injected by loadWelcome()
│   ├── welcome-en.html   # Welcome page (English), injected by loadWelcome()
│   ├── xterm.js          # xterm.js library (vendored)
│   ├── xterm.css         # xterm.js styles (vendored)
│   └── lang/             # Language files
│       ├── zh-CN.json    # 🇨🇳 Chinese (default)
│       └── en.json       # 🇬🇧 English
├── crates/harness-engine/  # Agent engine lib (zero tauri; plan/generate/lint/verify/repair/kb/exec/sandbox/gitops)
│   └── src/agent/tool_loop.rs # 主循环本体 run_with_ask（780 行；v0.12 从 agent.rs 抽离，agent.rs 里只剩 mod + pub use）
│   └── src/step_agent.rs      # 计划步骤执行体（子 agent，上下文干净，v0.4）
│   └── src/testllm.rs         # 脚本化假 LLM（单测与 examples 共用，不联网）
│   └── examples/plan_only.rs  # plan-only e2e smoke (real LLM call via DEEPSEEK_API_KEY env)
│   └── examples/agent_loop_smoke.rs  # 四原语工具循环 e2e（脚本化假 LLM，无需 Key，跑完自断言）
├── tools/lint/             # harness_lint python package (engine lint stage; HARNESS_LINT_DIR can override)
├── scripts/                # check-style.js (style gate), ui-smoke.js (UI smoke: contracts + panel replays),
│                           # nav-guard-probe.mjs (真窗口证明：外链闸门，CDP 连 WebView2)
├── doc/                  # Cross-version design docs (Chinese); v0.x/ = archived 0.x-era docs
│                         # (需求-v0.x / bug-v0.x / release / 融合计划 … 全在 doc/v0.x/，见其 README.md)
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

### 详细调试日志（`--debug`）

排查"模型这一轮为什么跑偏"（典型症状：`第 N 轮输出无法解析：未知能力 ""`）要看**模型的原始
返回**和**完整提示词**，常规日志只有一行摘要 —— 于是加了这个启动开关：

```bash
ruyix.exe --debug                     # 绿色版
cargo run -p ruyix -- --debug         # 开发（工作区有两个 member，必须带 -p）
# 别名：-d / -v / --verbose
```

- 落盘位置**固定且可写死进文档**：`~/.ruyix/code/debug.log`（追加；每次启动写一段会话头）。
  **必须落盘而不是只打 stdout**：正式版是 Windows GUI 子系统（`main.rs` 首行
  `cfg_attr(not(debug_assertions), windows_subsystem = "windows")`），**没有控制台**，
  `println!` 的输出会被丢掉。
- 每次 LLM 调用记三段，缺一不可：
  1. **请求** —— 端点 / 鉴权方式 / `json_mode` / 请求体 JSON（含工具声明、`response_format`）
     / 模型实际看到的消息；
  2. **原始响应体** —— **解析前**就落盘。顺序不能反：解析失败时这段原文本是唯一证据；
  3. **解析结果** —— `finish_reason` 带注解（`length` = 被 `max_tokens` 截断，**不是**格式问题，
     两种病的纠偏方向相反）。
- 常规运行日志行（`[info] [agent] 第 N 轮 …`）**同时**进这个文件 —— 排查时要的就是把它和
  原始返回放在同一个文件里对着看。
- 实现：引擎 `crates/harness-engine/src/debug.rs`（全局开关 + 落盘 + **写入层脱敏**，复用
  `observe::redact`）；埋点在 `llm.rs::attempt_loop` 与 `pipeline.rs::log_observing`。
- **关闭时是纯 no-op**：默认关，常规运行的输出、行为、开销一字不变。
- 单条上限 `DEBUG_CAP`（20 万字符），超限**留痕**而不是静默截断。

## Key Constraints

- Rust edition **2024** — requires Rust 1.85+
- **v1.0.0 (拍板 2026-09-24, 未开工)**: the release is **one executable + `global/` + `projects/` + `plugins/`, all
  beside the exe** — green/portable, uninstall = delete the folder. `global/` = global config **and** global data;
  `projects/<key>/` = one bucket per project (its scoped config **and** its state: stage / backups / proc / env /
  sessions / verify / plugins); **nothing is ever written into the user's repo** (today's `<project>/.ruyix/`, 7
  kinds of writes, is the thing being removed). Plan: `doc/需求-便携形态与零残留-v1.0.0.md` (R1–R14 / A1–A12),
  `doc/架构-便携根与项目状态搬迁-v1.0.0.md`, `doc/排期-1.0.0.md`. 0.x-era docs are archived under `doc/v0.x/`.
  **No migration code** (2026-09-24): the only user's legacy dirs were scanned (`D:\Projects`, two levels → 3 hit,
  all deleted) and `~/.ruyix/code` was already empty, so R7/A10 were retired and the migration phase dropped —
  `Paths` has exactly one shape, there is no "old path" branch.
- Windows-first (developed on Windows 11). WebView2 is included with Windows 11.
- Window decorations disabled (`"decorations": false`) — custom titlebar with `data-tauri-drag-region`.
- Frontend has no bundler — Tauri serves files from `ui/` directly (`"frontendDist": "../ui"`).
- **No npm dependencies**. All vendored or vanilla JS.

## Frontend State Model (main.js)

The `state` object drives the UI:
- `state.currentProject` — `{ name, path } | null`. Controls project workspace.
- `state.tabs` — `[{ id, name, path, content, _isTerminal?, _isService?, _term?, _highlighted?, ... }]` (`_isConfig` / `_isHelp` / `_isSession` / `_isService` are dedicated views: they own the editor area and keep `content` empty).
- `state.activeTabId` — which tab is currently shown.

`project-workspace` is **always visible**. The navigator sidebar adapts:
- **No project**: Shows "项目列表" + "能力" tabs only (会话 tab hidden — agent needs project context; `agent` verb is gated the same way).
- **Project open**: Shows "项目文件" / "终端资源" / "运行目标" / "会话" / "能力" tabs ("能力" panel hosts MCP/A2A/工具/技能 as sub-tabs).

### Sessions (session.js — multi-session chat)

`state.tabs` entries may carry `_isSession: true` + `_session` (session object) + `_sessionEl` (chat DOM, lazily built by `SessionUI.ensureChatEl`, re-mounted into `#session-container` on every switchTab — same pattern as terminal tabs re-mounting xterm). The nav "会话" panel lists the project's sessions (`agent_session_*`, persisted in `<root>/.ruyix/code/agent/sessions/*.json`); clicking opens a chat tab. Each message invokes `agent_reply` — the engine's **agent tool loop** (Read / Write / Execute / Connect atomic capabilities): the model decides per round which capability to call until it emits `{"final": ...}`; questions get answered by reading the project, tasks by writing whole files + verifying via execute, anything outside the repo by connecting to MCP tools / remote A2A agents. The model may declare a `plan` (emitted as `agent://plan`, shown in the outline with ✅/⌛/⛏️/⚠️/❌/⏹️ states driven by `agent://step`); with `step.execute_plan` on (the default since v0.4), that same plan is also the engine's **dispatch list** (see *Step agent* below). Live progress is inferred from "did the step's declared files land": all → ✅, some → ⛏️. That inference alone can never settle a step, so `agent.rs::settle_steps` runs once when the loop ends and gives every step a terminal state. Its criterion is deliberately **loose**, because `PlanStep::files` is the model's *pre-work* guess and drifts: `missing` counts only files that this run didn't write **and** that aren't already on disk (a declared-but-preexisting file like a `vite.config.js` from an earlier day is not "missing" — the step just didn't need to rewrite it); and "produced something, but not what it declared" lands ⚠️ `partial` ("未对齐") rather than ⏹️, because `skipped` means *this step did nothing* and using a heavier word than the facts — as with the hourglass — is a lie. A step with no declared files (a "编译验证"-style step with no file-level criterion) is ✅ once the run delivered a `final`; a step that wrote nothing at all becomes ⏹️ with the missing paths in its tooltip instead of a permanent ⌛ ("waiting", which would be a lie once the run is over); and steps whose declared files are all satisfied stay ✅ even when the run ends by cancel / max-rounds. `delivered` (set only on the accepted-final path) is what separates those cases, so nothing is ever marked done on a run that never delivered. The settled plan is then **snapshotted onto the assistant message** (`m.plan` = engine `PlanStep` definitions + per-step terminal states) and saved with the session: a chat run *is* the tool loop, which writes **no RunRecord**, so its `run_id` is null and there is nothing to re-read — without that snapshot the outline's task list simply vanishes when the app restarts. `ui/session.js::hydratePlan` prefers the snapshot and falls back to the old `agent_run_load` route for sessions written before the field existed. Before a `final` is accepted the engine runs the **quality gate** (v0.3): full verification (Apply mode) + a clean-context reflection agent; both conclusions arrive as `agent://verify` / `agent://reflect` and land under the bubble as a 验证/复核 section (`SessionUI`'s `gateHtml`), persisted on the message (`m.verify` / `m.reflect`) so reopening a session still shows "this run was verified". **Live progress meanwhile was invisible until v0.0.5** — the `agent://log` / `agent://stage` listeners only overwrote `placeholder.text` and never re-rendered, so a running bubble sat on `…` forever (and the one `level === "ok"` line *replaced* the "busy" text). Those events now feed a per-message **trace** (`m.trace` = `TraceSnap { kind, text }` rows: `think` / `do` / `fail` / `done`, rendered by `traceHtml`/`traceRowHtml`, one line each with CSS ellipsis and the full text in `title`). Classification is done by the pure function `traceFromLog` and follows only what the log line itself says — there is **no** chain-of-thought channel (`Action` has no `thought` field and the system prompt forbids prose, so inventing "the model is thinking…" text would be a lie); "thinking" rows are the model's declared `plan` and the engine's own stage verdicts. `m.trace` is capped at 200 rows and the bubble draws the newest 24. Such message-level fields **must be declared in `agent::sessions::SessionMsg`** — `agent_session_save` round-trips the session through serde and the UI then does `Object.assign(s, saved)`, so an undeclared field is silently erased from both disk and the live tab (the same reason `m.plan` needs `PlanSnap` and `m.trace` needs `TraceSnap`). Write-back per toolbar mode: 确认 → `WritePolicy::Stage` (changes staged under `.ruyix/stage/`, diff panel opens in-chat, `agent_stage_apply` only via explicit confirm), 写入/自主 → `WritePolicy::Apply` (direct write, overwritten files backed up to `.ruyix/backups/`). Closing the project (`setNavigatorMode("projects")`) drops all chat tabs (`SessionUI.projectClosed`).

### 产物写回（agent/apply.rs + session.js 的三模式）

引擎**只在沙箱里生成**：`<runs_root>/<run_id>/project/`（`workspace::project_dir`）。这是刻意的隔离，沙箱与真实仓库之间没有隐式搬运 —— 写回的门禁强度由会话工具栏的三个模式决定：

- **确认模式**（默认）：run 结束自动在助手气泡下打开差异面板（逐文件勾选 + 修改前/后内容），`data-confirm` 才写入。
- **写入模式**：验证通过（`status=verified`）自动写入全部 add/modify；未通过回落差异面板转人工。
- **自主模式**：run 结束无条件自动写入（写前仍备份）。

- `agent_apply_preview(run_id, project_root)` → **只读**，逐文件给 `add | modify | same`，附 `before`（项目现状）与 `after`（产物内容）以及 `dirty`（git 工作树是否脏）。
- `agent_apply_run(run_id, project_root, paths, backup?)` → 只写 `paths` 里列出的文件；内容一致的跳过并记原因；写前把被覆盖的原文件备份到 `<项目>/.ruyix/backups/<run_id>-<时间戳>/`。

三条硬约束（改动 apply.rs 时不得绕过）：路径封闭（拒绝绝对路径 / `..` / `.git`）、拒绝把产物写进沙箱自身（否则上一次产物会变成项目源码）、写前备份。UI 侧所有写入（面板确认与自动模式）收敛到 `session.js` 的 `applyPaths` 唯一入口（恒 `backup: true`）；由 ui-smoke U14 守住"三模式完整 + 唯一写入口 + 写入模式验证门禁"。

### 本地语音转写（v1.1）

**需求**：会话界面能录音，但"把录音发给模型"这条腿**不存在** —— 实测 DeepSeek 四种入口
（`/models` 的 `input_modalities` 只有 text/image、`/audio/transcriptions` 404、`input_audio` 422、
`/files` 只收 webp/png/jpeg/gif）。所以做成**本机转写**：录音 → 本机出文字 → 文本进输入框
（模型收到的仍是文本）。**音频一个字节都不出本机**。

**为什么是 candle 而不是 whisper.cpp**：发行形态是"一个可执行文件 + `global/` + `projects/` +
`plugins/`"，whisper.cpp 要么静态编进 exe（引 cmake + C++ 工具链）要么多带一个 sidecar 文件，
两条都破坏它；candle 已在依赖里（记忆的嵌入模型就是它）。**音频解码也不用进 Rust**：WebView 自带
WebAudio，前端 `decodeAudioData` + 16kHz 重采样，Rust 侧只吃浮点数组。

**模型格式是本次最大的坑（三种命名，只有一种能用）**：whisper.cpp 官方 `ggml-*.bin` 是老 GGML
（11 个 hparams + 文件内 mel filters + 无 score 词表），而 candle 的 `ggml_file` 按 **llama.cpp
布局**读（7 个 hparams + token/score 词表）⇒ 读不进来；社区 GGUF 张量名是 whisper.cpp 系
（`enc.`/`dec.` 或 `encoder.blocks.0.attn.query`），而 candle 按 **HF/transformers 命名**取
（`model.encoder.layers.0.self_attn.q_proj`）⇒ 也对不上；**只有 candle 原生格式的那批能用**。
采用 `Demonthos/candle-quantized-whisper-large-v3-turbo`（453.6MiB，q4_k + f32 归一化层，587 张量，
命名逐字对得上）。**没有 candle 原生格式的量化 small**（社区量化版全是 whisper.cpp 命名），
tiny 太小（中文不行），所以 turbo 是当下唯一"质量够 + 纯 Rust"的选择。

**mel 滤波器用官方表、不自己算公式**：按 librosa（Slaney）实现的一版与 whisper 发布的
`mel_filters.npz` 比，**第一个系数就差 0.5%**（0.024747 vs 0.024863）——形状对、数值不等于对，
而这种错**不报错**、只会让识别率悄悄变差。所以把 `mel_128`（128×201 f32，103KB）编进二进制
（`voice/mel-filters.bin`，来源 `openai/whisper` 的 `assets/mel_filters.npz`，MIT）。

**两个必须机器化的坑**（都在 `voice/asr.rs` 里，单测 + ui-smoke U61 钉住）：

1. `pcm_to_mel` **恒补零到 30 秒**（输出帧数永远是 3000，与给的音频多长无关）且布局是
   `mel[mel_bin * n_len + frame]`。所以"按真实长度编码"（whisper 官方永远按 30 秒算，实测 3.8 秒
   音频要付 62.6 秒编码）必须**自己按该布局逐通道切片**（`slice_mel`）。把整块 mel 交给更小的
   shape，`from_vec` 会**取前 N 个元素** —— 那不是截断时间轴，是把 mel 轴切掉、帧轴错位，
   模型听见噪声并幻觉（中文音频转出 **"Thank you."**），**不报错、只能靠实测抓**。
   修好之后：编码 62.6s → 9.2s（**6.5~6.8×**），**识别内容与 Full 一致**（三种中文语料字都对上）。
**但不是逐位等价**（2026-09-25 三臂同框实测纠正过一次过度声称）：Trim 吐出 `Cargo build`、Full 吐出
`CargoBuild` —— 差在中英边界那个空格。机制可解释：两者的 log-mel 归一化各取**自己窗口内**的 max，
窗口不同就会让这种边界 token 翻转。所以口径是"**实测不掉字**的加速"，不是"等价加速"。
2. 缺模型不许静默降级：按需下载（**456MB，绝不自动下** —— 与记忆模型刻意不同）、
   三类状态三种说法、进度走 `voice://model`、转写失败**兜底把音频存进项目桶**再说清路径。

**实测（真机，16 逻辑核，SAPI 合成中文 + 已知原文）**：4 段中文（含中英混说、数字）**内容全对**，
1 处同音字（爆/报）；端到端 **RTF 2.8~3.6×**（说 5 秒 ≈ 等 17 秒）；加载 0.34s（453MB 走 mmap）。
对照臂 `faster-whisper`（CTranslate2 small int8）：同段音频文本完全正确、**RTF 0.53×** ——
证明音频没问题、也给出"同样正确但快 ~6 倍"的参照，代价是 Python 依赖（不采用）。
**不能声称**：合成音≠真人、噪声/口音未测、3 句不构成准确率估计、未与 whisper.cpp 同机对比。
完整口径与缺口见 `doc/需求-本地语音转写-v1.1.md`。

### 常驻服务托管（execute 的第三个生命周期维度，v0.6）

**不是第五个原语。** 四个原语（Read / Write / Execute / Connect）编码的是**效果**，而"活多久"是**时长**——它不属于任何一个效果。所以 `execute` 多一个维度，模型侧仍是同一个工具：

| 维度 | 请求 | 语义 |
|------|------|------|
| 前台（默认） | `{cmd, timeout_secs}` | 跑完才回，退出码就是结论 |
| 后台 | `{cmd, background:true, ready_cmd, ready_timeout_secs?, keep_alive?}` | 起完按 `ready_cmd` 等就绪；返回 `handle` |
| 句柄 | `{op:"status"\|"log"\|"stop", handle}` | 查状态 / 读日志尾 / 停（连子进程树） |

- **就绪判据是一条命令**：退出码 0 即就绪。引擎不认识它测的是什么（端口？文件？HTTP？），这就是"零 app 知识"的落点——加一个 app 不改引擎。
- **起之前先探一次判据**：若启动前就已命中，说明有别的东西满足它（最常见是上一轮的进程还占着端口），这次"就绪"不可能是真的 → 拒绝并附证据。这条守卫直接掐掉"孤儿占端口 → 每次重启都拿到假失败 → 自我强化"的循环。
- **三个出口都带证据**：Ready 给命中的那行；Exited 给退出码 + 日志尾；NotReady 给日志尾 + "没命中"的说明。
- **日志落项目内** `<proj>/.ruyix/proc/<handle>.log`，路径由引擎给（模型不写重定向）；走文件不走管道 → 不会被写满阻塞。读日志按字节读、最后一步再按活动代码页解码（复用 `exec::decode_output`）。
- **引擎持有就引擎收**：run 结束 `shutdown_for(proj, honor_keep_alive=true)` **只收本项目的**（并行/嵌套/测试的进程不归这次 run 管）；宿主退出 `RunEvent::Exit` 里 `shutdown_all(false)` **全收**。少这一步，关掉 IDE 就又攒下一批占端口的孤儿。
- **宿主看得见也停得掉**：那些服务是**宿主** spawn 的，却不在宿主的进程树里（Windows 上 cmd → mvn.cmd → java 三代），以前只活在引擎的进程表 → UI 上等于不存在。顶栏【服务】面板（`ui/service.js`）读的就是这份表（`proc_list` → `proc::listing`），停止按 **pid** 发（`proc_stop` → `proc::stop_pid`）。进程表的键因此是 **pid 不是 handle**：netstat / tasklist / 用户嘴里的一手证据只有 pid，拿自造编号当主键就等于在证据和表之间插一层翻译；`handle` 降级为字段，只服务模型侧那句短引用（`proc::status` / `log_tail` / `stop` 仍按 handle 收，只是内部先翻译成 pid）。面板表格是学术三线表（PID / 启动时间 / 完整命令行三项，启动时间 = 墙钟时刻 + 括号里的 h/m/s 时长），已退出的进程不进面板；契约见 ui-smoke U23。
- **shell 构造只有一份**：`exec::shell_command`，Windows 走 `cmd /S /C` + `raw_arg` + 自己裹一层引号。两条都是实测踩出来的——用 std 的 `arg` 会把内部引号转义成 `\"`（cmd 回"不是内部或外部命令"）；只换 `raw_arg` 又会撞上 `cmd /C` 对"以引号开头的串"的剥引号规则（回"文件名、目录名或卷标语法不正确"）。`examples/proc_demo.rs` 是这条的可复现证明。
- 可复现证明：`cargo run -q -p harness-engine --example proc_demo`（用示例自身当"永不退出的服务"替身，不依赖 java/maven，任何平台都能跑出同一份结论）。契约见 ui-smoke U23。

### 外链：WebView 是画布，不是浏览器（P0）

**这条 P0 的真身**：agent 回一句「服务已起，访问 `http://localhost:8080`」，点一下那个链接，**整块 IDE 变成那张网页**
—— 标签栏、文件树、会话全没了，而且回不去。前端全活在**一个文档**里（标签页、文件树、会话都只是 DOM 状态），
文档一换状态一起没。病灶不是链接，是**没人拦导航**：agent 输出走 markdown-it 且开了 `linkify`，裸 URL 会变成真
`<a href>`，而 WebView2 对普通导航的默认动作就是在本 WebView 里导航过去（`target="_blank"` 反而不中招：wry 没设
新窗口处理器时会把请求静默吞掉 —— 那只是"点了没反应"，不是"IDE 没了"）。

规矩一句话：**外链一律交给操作系统浏览器，WebView 只准待在自家文档里。**两层闸门，缺一不可：

- **兜底**：窗口的 `on_navigation`（`build_main_window`）+ 纯函数 `nav_verdict` —— 任何来源的导航都要过它
  （`a` 标签 / `location.href` / form / `window.open` / 以后某个忘了拦的角落）。**因此主窗口必须建在 Rust 里**，
  `tauri.conf.json` 的 `app.windows` 留空：配置里生出来的窗口挂不上闸门（ui-smoke U24 钉住这一条）。
- **显式**：`ui/external.js` 在捕获阶段接 `click` / `auxclick`，按下那一刻就 `preventDefault` 并把意图变成
  `handleCommand("open url …")`（走命令系统，不直接 `invoke`），拦下的链接还能给一句说明 —— 后端那层没有地方说话。
  只靠前端＝下一处漏网就是又一次 P0；只靠后端＝用户点了没反应（浏览器不开）。

判定只有一条"**这条 URL 是不是应用自己的文档**"，三结局：`Nav::Allow` / `Nav::External`（拒掉导航 + 交给系统浏览器）/
`Nav::Refuse`（拒掉，且没有可交给浏览器的出口）。两个易错点：

- **`localhost` 不是自家页面**：agent 起的服务就在 localhost 上，放行它等于没拦。自家站点只有自定义协议的落地形式
  （Windows/Android `http://tauri.localhost`，其它平台 `tauri://localhost`），且只认**文档路径**（`/` 或 `*.html`：
  子资源不走导航事件，同源的 `tauri.localhost/main.rs` 这种相对链接导航过去只是一张 404 白页）。
  `build.devUrl` 配了才额外认它那一条 —— 认的依据是配置里写的那条，不是"凡是 localhost"。
- **出口白名单**：`open_external` 只放行 http / https / mailto，Windows 走 `ShellExecuteW`（系统默认处理程序，**不走 shell**：
  URL 带 `&`/空格/中文是常态）。`file:` / `javascript:` / `data:` 一律拦在**能执行之前**。前端 `link.opened` /
  `link.blocked` 两条文案就是这条链路的回执。

可复现证明（真窗口实测，六条路子）：`node scripts/nav-guard-probe.mjs` —— 用 CDP 直连运行中的 WebView2，
亲手试原生导航 / 点 markdown 渲染出的链接 / 同源非文档 / `window.open` / `javascript:`，每次读回 `location.href`、
page target 数与 `#app` 是否还在。用法与预注册判据见脚本头注释。契约见 ui-smoke U24。

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
| `project lang\|edit\|delete` | `handleProjectCommand` | `project lang <lang> <path>` sets language (path = remainder, may contain spaces); `project edit "<path>" "<name>" <lang>` updates name+icon (quote-aware, path read-only); `project delete <path>` removes from list (remainder path; does NOT delete the folder) |
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
```

- Config keys: `ruyix.code.ui.lang`
- `I18N.init()` reads config, loads the right JSON file
- `I18N.t(key, params)` returns translated string with `{param}` substitution
- `I18N.setLang()` persists to config
- Language menu: 中文 / 英文 (radio)
- All `setStatus()` calls use `I18N.t()` keys

## AI Integration (ai.rs)

- System prompt: `src-tauri/src/command.md` compiled via `include_str!`
- Project context: When project is open, project root path is prepended to user message
- All AI config read from `ruyix.code.ai.*` keys (runtime → project → global fallback)

## LLM Call Layer (`crates/harness-engine/src/llm.rs`)

The **agent** LLM calls live here (`ai.rs` above is a separate, independent Lua/command-translation
client — do not conflate the two). `llm::chat(cfg, fallback, messages, json_mode)` picks one of
three protocols via `request_plan`:

| `api_format` | Endpoint | Auth | Notes |
|---|---|---|---|
| `openai` + `web_search` on | `/responses` | `Authorization: Bearer` | server-side web search (DeepSeek only) |
| `openai` (default) | `/chat/completions` | `Authorization: Bearer` | supports `response_format` |
| `anthropic` | `/v1/messages` | `x-api-key` + `anthropic-version: 2023-06-01` | `system` goes to a top-level field; **no** `response_format`; web search uses **Anthropic's own server tool** `web_search_20250305` (declare + parse `server_tool_use` → `web_queries`) |

`request_plan` checks `anthropic` **first** — check it after `web_search_on` and anthropic requests
get wrapped as `/responses`. All three protocols share one retry/error-classification path;
only the URL, body, extractor and auth scheme differ.

**Web search is a `(protocol × model)` capability, not a per-model one** (measured 2026-09-25, four
models × both routes): on DeepSeek's `/anthropic` **all four models search** (flash included), while
on `/responses` only `deepseek-v4-pro` does — so `ModelCaps` carries `web_search` (OpenAI-compatible
route) **and** `web_search_anthropic`, `llm::web_search_capable(model, api_format)` is the single
place that picks the right dimension, and `web_search_on` must **never** early-return on
`api_format = anthropic` again: that blanket ban silently ate the user's explicit `on` (config said
on, request carried nothing, UI said nothing — the reported "why can't I use web search").
`on` = force (ignore the table), `auto` = DeepSeek endpoint **and** capable on *this* route.

**Failover (v0.5)**: when `cfg.llm_fallback` is `Some`, a primary failure switches to it — but only
for *availability* faults (`is_switchable_error`: network / 5xx / 429 / timeout / retries exhausted).
Auth (401/403), quota (402) and rejected requests (400/422) are **never** switched — retrying will
not help and the backup would likely fail the same way, hiding the real error. With a fallback
configured, the primary runs in **resilient mode**: 2 attempts, per-request timeout capped at 30s
(so a hung primary cannot burn the full 300s before switching). With no fallback the behaviour is
byte-for-byte the old one (3 attempts, full timeout, same message). Switching is recorded as an
`llm-failover` observation span.

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
- `ruyix.code.ai.api_key` / `api_url` / `model` / `api_format`（`openai` 默认 / `anthropic`）
- `ruyix.code.ai_fallback.api_url` / `api_key` / `model` / `api_format`（v0.5 备用 LLM；配了才启用故障切换，全空 = 不切换）
- `ruyix.code.ui.lang`
- `ruyix.code.run.target<N>.cmd` / `target<N>.name`
- `ruyix.code.harness.gate.narrow` / `gate.full` / `gate.max_full_attempts` / `gate.staged_timeout_secs`（v0.3 机械验证门禁）
- `ruyix.code.harness.reflect.enabled` / `reflect.max_rounds` / `reflect.model`（v0.3 复核 agent）
- `ruyix.code.harness.discover.enabled` / `discover.ttl_secs` / `discover.extra`（v0.5 命令发现；`extra` 逗号分隔，追加工具表没覆盖的命令）
- `ruyix.code.harness.env.install_enabled`（v0.5 环境准备：缺失工具按需安装；默认开，关掉后宿主不再把 env 目标摆进 connect 清单）
- `ruyix.code.harness.proc.enabled` / `proc.max` / `proc.ready_timeout_secs`（v0.6 常驻服务托管；默认开，`max` 上限 16，`ready_timeout_secs` 范围 1~600）

## Tauri Commands (main.rs)

| Command | Signature | Notes |
|---------|-----------|-------|
| `open_project` | `(path)` → `ProjectInfo` | Sets current project in global config; `ProjectInfo { name, path, lang }` |
| `get_last_project` | `()` → `Option<String>` | |
| `get_projects` | `()` → `Vec<ProjectEntry>` | All known projects; `ProjectEntry { name, path, lang }`, lang ∈ PROJECT_LANGS (10 values, default `unknown`) |
| `set_project_lang` | `(path, lang)` → `()` | Validates lang against PROJECT_LANGS |
| `update_project` | `(path, name, lang)` → `()` | Updates name+lang (path immutable); rejects empty name / invalid lang |
| `delete_project` | `(path)` → `()` | Removes entry from list (folder untouched); clears `current` if it matches |
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
| `proc_list` | `()` → `Vec<ProcInfo>` | Managed processes, read from the engine's **same** table (`harness_engine::proc::listing`) — the host must not keep a second list |
| `proc_stop` | `(pid)` → `ProcInfo` | Stop by **pid**, whole child tree (`proc::stop_pid`, on `spawn_blocking` because kill+wait is seconds) |
| `voice_status` | `()` → json | 语音模型自检：`ready` / 一句人话 / 目录 / 要下多少字节（三类状态：没装 / 装坏了 / 没配目录） |
| `voice_model_fetch` | `()` → json | **按需**下载语音权重（456MB）：立即返回，进度走 `voice://model`（**绝不自动下** —— 与记忆模型不同） |
| `voice_transcribe` | `(data, language?)` → json | 本机转写：`data` 是 16kHz 单声道 f32 的 **base64**（前端 WebAudio 解码 + 重采样）。非就绪态照实回报（`voice_line()`）；PCM 长度/时长有守卫；在 `spawn_blocking` 里跑且复用进程级 `VOICE_ASR` |
| `open_external` | `(url)` → `String` | The **only** exit for external links: whitelists http / https / mailto, then hands the URL to the OS default handler (`ShellExecuteW`, no shell). Everything else is refused before it can execute |
| `spawn_terminal` | `(cmd, project_root?)` → `()` | New OS console window |
| `pty_spawn` | `(cmd, tabId, project_root?)` → `()` | PTY for inline xterm.js |
| `pty_write` | `(tabId, data)` → `()` | |
| `pty_close` | `(tabId)` → `()` | |
| `agent_run` … `agent_env_probe` | see `doc/v0.x/融合计划-Agent集成-v0.2.md` 附录 A | 12 commands: run/plan/generate/verify/lint/repair/cancel/runs/run_load/run_delete/read_artifact/env_probe |
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
