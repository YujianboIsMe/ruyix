//! 进展记忆与循环守卫：**一份账本，三个消费者**。
//!
//! 见 `doc/v1.1/需求-Agent-进展记忆与循环守卫-v1.1.md`。核心口径一句话：
//!
//! > 对话流的正文可以折（噪声居多），**结论与"已做过什么"不许折**。
//!
//! 现状（ISSUE-8）是反的：`fold_history` 保留窗口外的每一轮只剩
//! `call_shape`（留下 `path+range`）与 `outcome_line`（只留输出首行），而摘要的原文写着
//! 「正文已从上下文移除，**需要时重新 read**」—— 对"答案在文件正文里"的诊断题，
//! 模型每读一次、六轮后内容被折掉、只剩"我读过它"，于是再读一遍，闭环 96 轮 0 结论。
//!
//! 本模块提供三件东西，共用同一份数据：
//!
//! 1. [`Progress::record`] —— 模型写的**结论**（`record_findings` 工具；永不折叠，带 id 与取代语义）；
//! 2. [`Progress::ledger_line`] / [`Progress::note_read`] —— 引擎写的**已做账本**
//!    （不靠模型自觉，越乱的 run 越需要它）；
//! 3. [`Progress::stalled`] —— 停滞判据（连续 K 轮既无新结论、也无文件变更）。
//!
//! ## 三条不变量（改这个模块前先读）
//!
//! - **永不静默丢弃**：超过上限时最老的 `active` 条目**落盘**并在提示词里留指针行，`落盘 ≠ 删除`；
//! - **取代不改历史**：被 `supersede` 的条目留在账本里、只从提示词中退出；
//! - **字节要可观测**：`active_bytes` 报给 trace，否则上限有没有踩到没人知道。

use std::path::{Path, PathBuf};

/// 一条结论（模型写的）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// 引擎分配的短 id（`F1`、`F2`…）—— 模型要 `supersede` 就得先知道它，所以工具结果里要回给模型。
    pub id: String,
    /// 结论本身（一句话能站住的事实）。
    pub claim: String,
    /// 证据指针：`path:line` 或 `命令 + 退出码`。**空证据不许入库**（findings 的价值全在可核对）。
    pub evidence: String,
    /// 补充（可空）
    pub note: String,
    /// 被哪一条取代（`None` = 仍 active）
    pub superseded_by: Option<String>,
    /// 是否已落盘（落盘后不再进提示词，但仍在账本里）
    pub spilled: bool,
}

impl Finding {
    /// 是否仍出现在提示词里（active 且未落盘）
    pub fn in_prompt(&self) -> bool {
        self.superseded_by.is_none() && !self.spilled
    }

    /// 一行渲染（提示词与 trace 共用）
    pub fn line(&self) -> String {
        let extra = if self.note.trim().is_empty() {
            String::new()
        } else {
            format!("；{}", self.note.trim())
        };
        format!(
            "{} {}（证据：{}）{}",
            self.id,
            self.claim.trim(),
            self.evidence.trim(),
            extra
        )
    }
}

/// 一轮记录
#[derive(Clone, Debug)]
struct RoundMark {
    round: usize,
    /// 这一轮有没有产生新结论
    new_finding: bool,
    /// 这一轮有没有文件变更
    wrote: bool,
    /// 这一轮有没有**读到之前没读过的区域**（收集信息就是进展 —— 见 note_round 的注释）
    new_read: bool,
}

/// 一次读取（重复读守卫靠它）
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReadMark {
    path: String,
    start: u32,
    end: u32,
    round: usize,
}

/// 账本在提示词里最多放多少行（更早的显式说省了多少，不静默丢）
const LEDGER_KEEP: usize = 60;

/// 本 run 的进展状态：结论 + 账本 + 读取索引。**step 子步骤与主循环共用同一份**
/// （子步骤借的是同一个 `&mut Ctx`，所以这里天然共享，不需要再造一套）。
#[derive(Clone, Debug)]
pub struct Progress {
    findings: Vec<Finding>,
    /// 引擎账本（已做过什么）。只增不改。
    ledger: Vec<String>,
    rounds: Vec<RoundMark>,
    reads: Vec<ReadMark>,
    next_id: usize,
    /// 本 run 跑过的命令（去重）。调研型任务的主力是 `git grep` 这类 `execute`：
    /// 只把 `read` 算进展，会把"查得很对"判成停滞（实测 22 轮 / 40 轮两次都死在这儿）。
    cmds: Vec<String>,
    /// 本轮是否跑过新命令（由 `ledger_line` 置位、`note_round` 消费）
    fresh_cmd: bool,
    /// 上一次"有进展"的轮次（新增结论或文件变更）
    last_progress: usize,
    /// 落盘文件（一旦写过就固定，避免每轮换路径）
    spill_file: Option<PathBuf>,
    /// 提示词里 findings 块的字节上限（0 = 不限）。放在状态里而不是每次传参：
    /// `byte_account` 与 `prompt_block` 必须用同一个数，否则账与行为会各说各话。
    cap_bytes: usize,
}

/// findings 块的默认字节上限 —— **与引擎配置的默认值保持一致**（`d_agent_findings_max_bytes` = 8192）。
/// 写成常量而不是散字面量：两边一旦不一致，"账"与"行为"就会各说各话。
///
/// 0 = 不限，是合法值。**显示上不许把它印成 `0.0KB`** —— 用户实测就是这么读错的
/// （"一个字都不许记？"），而实际上一个字都没少记。
pub const DEFAULT_FINDINGS_CAP_BYTES: usize = 8192;

impl Default for Progress {
    fn default() -> Self {
        Self {
            findings: Vec::new(),
            ledger: Vec::new(),
            rounds: Vec::new(),
            reads: Vec::new(),
            next_id: 0,
            last_progress: 0,
            spill_file: None,
            cap_bytes: DEFAULT_FINDINGS_CAP_BYTES,
            cmds: Vec::new(),
            fresh_cmd: false,
        }
    }
}

impl Progress {
    pub fn new() -> Self {
        Self::default()
    }

    // ---------------------------------------------------------------- 结论

    /// 记一条结论。`supersedes` 给的是**旧条目的 id**（模型从工具结果里拿到）。
    ///
    /// fail-closed：`claim` 或 `evidence` 为空 ⇒ 拒（findings 的价值全在"可核对"，
    /// 收下一句没有证据的断言，只是把幻觉抬进提示词）。
    pub fn record(
        &mut self,
        claim: &str,
        evidence: &str,
        note: &str,
        supersedes: Option<&str>,
    ) -> Result<String, String> {
        let claim = claim.trim();
        let evidence = evidence.trim();
        if claim.is_empty() {
            return Err("findings 的 claim 不能为空".into());
        }
        if evidence.is_empty() {
            return Err("findings 的 evidence 不能为空（给 path:line 或 命令+退出码）".into());
        }
        // 先校验取代目标，再分配新 id：写了一半才发现 id 不存在，会留下一条半成品。
        let superseded = match supersedes.map(str::trim).filter(|s| !s.is_empty()) {
            None => None,
            Some(old) => {
                let target = self
                    .findings
                    .iter_mut()
                    .find(|f| f.id.eq_ignore_ascii_case(old))
                    .ok_or_else(|| {
                        format!("没有 id 为 `{old}` 的 finding（先用工具结果里回给过你的 id）")
                    })?;
                if target.superseded_by.is_some() {
                    return Err(format!("`{old}` 已经被取代过了，别再取代它"));
                }
                Some(old.to_string())
            }
        };
        self.next_id += 1;
        let id = format!("F{}", self.next_id);
        if let Some(old) = superseded
            && let Some(t) = self
                .findings
                .iter_mut()
                .find(|f| f.id.eq_ignore_ascii_case(&old))
        {
            t.superseded_by = Some(id.clone());
        }
        self.findings.push(Finding {
            id: id.clone(),
            claim: claim.to_string(),
            evidence: evidence.to_string(),
            note: note.trim().to_string(),
            superseded_by: None,
            spilled: false,
        });
        Ok(id)
    }

    pub fn active(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.superseded_by.is_none())
    }

    pub fn all(&self) -> &[Finding] {
        &self.findings
    }

    /// 进提示词的那些（active 且未落盘）
    pub fn in_prompt(&self) -> Vec<&Finding> {
        self.findings.iter().filter(|f| f.in_prompt()).collect()
    }

    /// 已取代的条数（提示词里只报个数，不展开 —— 保留"不改历史"又不占字节）
    pub fn superseded_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.superseded_by.is_some())
            .count()
    }

    pub fn spilled_count(&self) -> usize {
        self.findings.iter().filter(|f| f.spilled).count()
    }

    /// 提示词里那一块的字节数（上限判据用它）
    pub fn active_bytes(&self) -> usize {
        self.in_prompt().iter().map(|f| f.line().len() + 1).sum()
    }

    /// 一行字节账（进 trace / 工具结果，别让上限悄悄生效）
    pub fn byte_account(&self) -> String {
        let kb = |n: usize| format!("{:.1}KB", n as f64 / 1024.0);
        format!(
            "findings {} 条 active / {}（上限 {}），已取代 {}，已落盘 {}",
            self.in_prompt().len(),
            kb(self.active_bytes()),
            kb(self.cap_bytes),
            self.superseded_count(),
            self.spilled_count()
        )
    }

    // ---------------------------------------------------------------- 账本

    /// 引擎写一行账（每轮调用形状 + 退出码/命中摘要）。只增不改。
    ///
    /// 顺带把这轮的 `execute` 命令去重记下 —— 它能回答「我是不是在重复同一件事」。
    pub fn ledger_line(&mut self, line: impl Into<String>) {
        let line = line.into();
        if let Some(rest) = line.split(" execute ").nth(1) {
            let cmd = rest
                .trim_end_matches('\u{2713}')
                .trim_end_matches('\u{2717}')
                .trim()
                .to_string();
            if !cmd.is_empty() && !self.cmds.iter().any(|c| c == &cmd) {
                self.cmds.push(cmd);
                self.fresh_cmd = true;
            }
        }
        self.ledger.push(line);
    }

    pub fn ledger(&self) -> &[String] {
        &self.ledger
    }

    /// 记一次读取，返回**同一区间上次是哪一轮读的**（重复读守卫用）。
    ///
    /// 区间**相交**即算重复（`296-334` 与 `330-390` 是同一段的重叠读，也算）——
    /// 判据要抓的是"又读了一遍同一块地方"，不是"字节级逐字相同"。
    pub fn note_read(&mut self, path: &str, start: u32, end: u32, round: usize) -> Option<usize> {
        let hit = self
            .reads
            .iter()
            .filter(|r| r.path == path)
            .filter(|r| !(end < r.start || r.end < start))
            .map(|r| r.round)
            .min();
        self.reads.push(ReadMark {
            path: path.to_string(),
            start,
            end,
            round,
        });
        hit
    }

    // ---------------------------------------------------------------- 停滞

    /// 记一轮的产出（新结论 / 文件变更）
    /// 记一轮的产出。
    ///
    /// `new_read` = 本轮读到了之前没读过的区域 —— **必须算进展**（用户实测报的 bug：
    /// 模型分 8 段系统地读 `main.js`，到第 8 轮被判"连续 8 轮无进展"直接掐掉 ✗）。
    /// 收集信息就是进展；只有"没读新东西、没新结论、也没改文件"才是真在原地打转。
    pub fn note_round(&mut self, round: usize, new_finding: bool, wrote: bool, new_read: bool) {
        // 新命令 = 拿到了新信息（调研型任务的主力就是 execute 式检索）。
        let new_read = new_read || self.fresh_cmd;
        self.fresh_cmd = false;
        if new_finding || wrote || new_read {
            self.last_progress = round;
        }
        self.rounds.push(RoundMark {
            round,
            new_finding,
            wrote,
            new_read,
        });
    }

    /// 到第 `round` 轮为止，连续多少轮没有进展（无新结论、无文件变更）。
    ///
    /// 从**逐轮记录**倒着数，而不是拿 `round - last_progress`：后者在"这一轮还没记账"的
    /// 时序下会多算一轮，而判据的边界（8 还是 9）正是会被这种偏差绊倒的地方。
    pub fn stalled(&self, round: usize) -> usize {
        if self.rounds.is_empty() {
            // 记账缺失（调用方还没 note_round）⇒ 拿"上次进展轮"兜底，绝不返回比实际更小的值
            return round.saturating_sub(self.last_progress);
        }
        let mut n = 0usize;
        for m in self.rounds.iter().rev() {
            if m.round > round {
                continue;
            }
            if m.new_finding || m.wrote || m.new_read {
                break;
            }
            n += 1;
        }
        n
    }

    // ---------------------------------------------------------------- 提示词块

    /// 设上限（字节）。0 = 不限。
    pub fn set_cap(&mut self, cap: usize) {
        self.cap_bytes = cap;
    }

    /// 生成提示词块（system 尾部追加）。超上限时把最老的 `active` 落盘并留指针。
    ///
    /// 返回 `Some(落盘路径)` 表示这一轮发生了落盘（调用方据此报 trace）。
    pub fn prompt_block(&mut self, spill_dir: Option<&Path>) -> Option<PathBuf> {
        let mut spilled_now = None;
        if self.cap_bytes > 0
            && self.active_bytes() > self.cap_bytes
            && let Some(dir) = spill_dir
        {
            {
                if self.spill_file.is_none() {
                    self.spill_file = Some(dir.join("findings.md"));
                }
                // 把最老的 active 逐条移到落盘（直到进上限）
                while self.active_bytes() > self.cap_bytes {
                    let victim = self.findings.iter().position(|f| f.in_prompt());
                    match victim {
                        // 只剩一条时不再落盘：再多就什么都不剩了（宁可超一点也不空手）
                        Some(i) if self.in_prompt().len() > 1 => {
                            let f = &mut self.findings[i];
                            f.spilled = true;
                            let record = format!(
                                "- {} {}（证据：{}）{}\n",
                                f.id,
                                f.claim,
                                f.evidence,
                                if f.note.is_empty() {
                                    String::new()
                                } else {
                                    format!("；{}", f.note)
                                }
                            );
                            let path = self.spill_file.clone().unwrap();
                            if let Some(parent) = path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            use std::io::Write as _;
                            if let Ok(mut h) = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&path)
                            {
                                let _ = h.write_all(record.as_bytes());
                            }
                            spilled_now = Some(path);
                        }
                        _ => break,
                    }
                }
            }
        }
        spilled_now
    }

    /// 渲染注入 system 尾部的那一块（findings + 账本）。
    pub fn render(&self) -> String {
        let mut out =
            String::from("\n\n## 已确认的事实（本 run 内不折叠；这是你的记忆，别重复探索）\n");
        let items = self.in_prompt();
        if items.is_empty() {
            out.push_str(
                "（还没有。**每读出一件会改变后续决策的事实，就立刻用 record_findings 落一条**",
            );
            out.push_str("（claim + 证据 path:line）。下一轮你仍会看到它，不必靠重读回忆。\n");
        } else {
            for f in &items {
                out.push_str(&f.line());
                out.push('\n');
            }
        }
        if self.superseded_count() > 0 || self.spilled_count() > 0 {
            out.push_str(&format!(
                "（另有 {} 条已被取代、{} 条已落盘：{}\n  历史不删，需要时读上面那个文件。）\n",
                self.superseded_count(),
                self.spilled_count(),
                self.spill_file
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "（未落盘）".into())
            ));
        }
        if !self.ledger.is_empty() {
            out.push_str("\n## 引擎账本（你已经做过什么；**别重复做已完成的事**）\n");
            // 账本有界：只放最近 `LEDGER_KEEP` 行，更早的**显式**说省略了多少
            // （不静默截断 —— 那会让模型以为"没做过"，正是要治的病）。
            let keep = LEDGER_KEEP.min(self.ledger.len());
            let omitted = self.ledger.len() - keep;
            if omitted > 0 {
                out.push_str(&format!(
                    "- （更早的 {omitted} 条已省略；需要时重新精确读）\n"
                ));
            }
            for l in self.ledger[self.ledger.len() - keep..].iter() {
                out.push_str("- ");
                out.push_str(l);
                out.push('\n');
            }
        }
        out
    }

    /// 重复读守卫要追加到工具结果后面的那一行
    pub fn repeat_read_note(&self, path: &str, earlier_round: usize) -> String {
        let related: Vec<&Finding> = self
            .findings
            .iter()
            .filter(|f| f.superseded_by.is_none() && f.evidence.contains(path))
            .collect();
        let tail = if related.is_empty() {
            "当时你没有为它落结论 —— 若这次读出的是新事实，立刻 record_findings；\
             若没有新信息，别再读了，去做下一步。"
                .to_string()
        } else {
            format!(
                "当时的结论：{}。若这次读出的是新事实，立刻 record_findings 取代它；否则别重复探索。",
                related
                    .iter()
                    .map(|f| f.id.as_str())
                    .collect::<Vec<_>>()
                    .join("/")
            )
        };
        format!("\n（注意：第 {earlier_round} 轮已读过 {path} 的同一段。{tail}）\n")
    }
}
