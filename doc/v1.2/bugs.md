# ruyix v1.2 问题与方案（ISSUE）

> 跨版本的约定留在 `doc/` 根，**版本专指**的 bug 记在本文件所属版本下（`doc/v1.2/`）。
> 每条格式：**症状（可复现）→ 根因 → 证据 → 改法 → 门禁**。修完把根因与门禁写回同一文件。

---

## ISSUE-1：U52 的「跳过测试代码」规则让它只扫了后端的一小部分 —— 45 条后端文案从没被检查过

**状态**：**已修**（2026-09-27）。

**症状**：`ui/scripts/errors.js` 的英译表本应由 U52 `backend-msg-covered` 兜住 —— "扫后端
源码里所有 `Err(...)` / `map_err` / `ok_or` / `bail!` 里的中文字面量，逐条要求
`translate(原文) !== 原文`"。实际上这条门禁**只扫到了每个文件的一部分**，于是英文界面下
还有一批后端文案会露出中文，而门禁一直是绿的。

**根因**：扫描为了"跳过测试代码"做了一次截断 ——

```js
const cut = src.indexOf("#[cfg(test)]");
if (cut > 0) src = src.slice(0, cut);
```

它假定"第一个 `#[cfg(test)]` 之后全是测试"。只要**文件中间**出现一个 `#[cfg(test)]` 门禁的项
（大多是测试专用的 helper），这个假定就错了：那个项之后的生产代码**再也没有被扫过**。

**证据（实测）**：

| 文件 | 改动前扫到 | 该文件总行数 |
|---|---|---|
| `src-tauri/src/main.rs` | 1478 | 3665 |
| `crates/harness-engine/src/agent.rs` | 639 | 3965 |
| `crates/harness-engine/src/agent/capsule.rs` | 88 | 310 |
| ……共 9 个文件有"中段 `#[cfg(test)]` 门禁项" | | |

漏检的规模：把扫描范围修对之后，未翻译的后端文案从 **5 条 → 45 条**
（5 条在 `main.rs`，40 条在引擎侧）。

**怎么暴露出来的**：把 `main.rs` 的测试搬进 `src-tauri/src/tests.rs`（1 拆 2 重构）时，
`main.rs` 里那个测试专用的 `#[cfg(test)] fn utf16_offset()` 跟着搬走了 ⇒ 文件里唯一的
`#[cfg(test)]` 落到文件末尾 ⇒ 这条扫描当场看见被它藏了两千多行的生产代码，红了 5 条。
顺藤摸瓜改规则，又把引擎侧 40 条一起翻出来。

**改法**（三处，都在前端门禁与英译表里，不动后端文案）：

1. `scripts/ui-smoke.js` U52：截断规则从"第一个 `#[cfg(test)]`"改成"第一个**内联测试模块**
   （`#[cfg(test)] … mod x {…}`）"—— 挂在函数/常量上的 `#[cfg(test)]` 不再让扫描提前收摊；
   `mod x_tests;`（模块体在另一个文件里，已被文件名规则排除）也不算。
   实测：67 个 `.rs` 文件里**每个内联测试模块都在文件末尾** ⇒ "砍到文件末尾"就是精确跳过测试。
2. `ui/scripts/errors.js`：补 45 条（EXACT 表 + 5 条窄规则 + `VERBS["备份"]`），
   措辞沿用表里既有风格；标"前缀"的条目按 EXACT 的**前缀**分支用
   （后端在它们后面接 `: {值}` / `：{原因}` / `（补充）`）。
3. `ui/scripts/errors.js` 的 EXACT 前缀分支顺手修一个拼接瑕疵：后缀是值/括号补充时
   **一律留一个空格**（原来只在"（"开头时补，于是 `判据正则写错了 {pattern}: {e}` 会拼成
   `…wrong{pattern}: {e}`）。这条修完语料里多出 1 条译文变化（就是它自己），已核对是变好。

**判据（已跑）**：

```bash
node scripts/ui-smoke.js        # 415/415（U52 backend-msg-covered 绿）
node scripts/check-style.js     # 0 error
```

**回归检查（这次额外做的，值得复用）**：把两个源码根下**所有**中文字面量当语料
（4420 条）喂给 `translate`，改前/改后各存一份快照再逐条 diff ——
本次结果：**45 条译文变化（= 新补的 45 条）+ 1 条空格修复，0 条既有译文被改歪**。
脚本形态：`scripts/ui-smoke.js` 只判"翻不翻得出来"，量不出"有没有把别的句子翻歪"；
这类"翻译表大改"的活，快照 diff 是比逐条眼看更硬的证据。

**留的规矩**（已写进 `doc/编码规范.md`）：测试代码要么放**文件末尾**的内联 `mod tests`，
要么单独成文件（`*tests.rs`，按文件名排除）；别把测试模块夹在生产代码中间 ——
U52 靠这两条之一才能把"测试"和"生产"分开。

---

## ISSUE-2：macOS 上全绿、Windows 上**编不过** —— `#[cfg]` 掉的那半句被当成"多余的 mut"

**状态**：**已修**（2026-09-28，拉回 `origin/master` 的 macOS 端口那一批之后）。

**症状（可复现）**

拉回 `19aab97`（commit 说明就一句「fix an issue found by cargo」）之后，Windows 上：

```
$ cargo test
error[E0596]: cannot borrow `s` as mutable, as it is not declared as mutable
   --> crates\harness-engine\src\agent.rs:481:5
    |
481 |     s.push_str( "…Windows 检索提示…" );
    |     ^ cannot borrow as mutable
help: consider changing this to be mutable
479 |     let mut s = AGENT_SYSTEM.to_string();
```

同一次拉取还带进来两处：`cargo fmt --check` 红（`agent/vendor.rs:208`、`main.rs:1954/1980` 三处没格式化）、
`cargo clippy --all-targets` 1 条告警（`paths.rs:264` `.filter_map(|k| std::env::var_os(k))` 冗余闭包）。
而那份 commit 的说明里写的是「cargo test 全绿（439+166）；check-style 通过；ui-smoke 416/416」——
**那是 macOS 上的读数**。

**根因（一条，很清楚）**

`agent_system_prompt()` = `AGENT_SYSTEM` + Windows 检索提示，提示那句挂 `#[cfg(target_os = "windows")]`：

- **macOS**：整句 `push_str` 被编掉 ⇒ `let mut s` 的 `mut` 没人用 ⇒ `unused_mut` 告警
  （19aab97 说的"cargo 发现的问题"就是它），于是把 `mut` 删了 —— 在 mac 上确实干净了；
- **Windows**：那句话**要**编译 ⇒ `let s` 紧接 `s.push_str(...)` ⇒ `E0596`。

**一个平台上"多余的 mut"，正是另一个平台上的编译错误。** 平台无关的两个失真同时存在：
那份"门禁"清单里没有 `cargo fmt --check` 和 `cargo clippy`（恰恰是这两条抓住了格式化与冗余闭包）。

**改法**

1. `agent_system_prompt()` 改成**每个平台一整段、整段套 `#[cfg]`**（非 Windows 段直接
   `AGENT_SYSTEM.to_string()`）—— 两边都既不缺 `mut` 也不多 `mut`。
2. `cargo fmt --all`（上面那三处）。
3. `paths.rs:264` → `.filter_map(std::env::var_os)`。
4. 连带修一条被格式化"误伤"的门禁：U64 的 `/refresh_vendor\(&app, vendor\.inner\(\), …/` 写死了
   **单行**，而 rustfmt 的 `fn_call_width` 会把这条调用折成多行 ⇒ `cargo fmt` 一跑它就红。
   改成空白宽松（`\s*`）—— 断言要钉的是「调用装配对不对」，不是「写没写成一行」。

**判据**

```
cargo test（全工作区）        442 + 168 passed / 0 failed（ignored 8 + 3+3+2）
cargo fmt --check            通过
cargo clippy --all-targets   0 warning
node scripts/ui-smoke.js     424/424
node scripts/check-style.js  0 error
```

**教训（写下来免得再犯）**

跨平台改动**在 mac 上的绿灯不能替代 Windows 上的一次编译**：`cargo test` 在 mac 上永远看不到
Windows 的 cfg 分支，clippy / fmt 也看不见（它们各编各的那一半）。所以：

- 平台分支相关的改动，**在提交前至少在本机能编的那个平台上真跑一次编译**；
- 反过来，`cargo fmt --check` / `cargo clippy --all-targets` / `cargo test` / `ui-smoke` / `check-style`
  五条是**独立**的：任何一条不在清单里，都会有整整一类失真没人看（这次一次漏掉三类中的两类）。

