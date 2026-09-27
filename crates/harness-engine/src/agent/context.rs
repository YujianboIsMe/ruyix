//! 上下文布局契约 `[S][E][A]`（v1.2 · LSC 迁移）
//!
//! 见 `doc/v1.2/架构-上下文账本与重基线调度-v1.2.md` §1。三条不变式是所有压缩策略的地基：
//!
//! | 不变式 | 含义 | 违反的代价 |
//! |---|---|---|
//! | **I1** | `S`（不可变根：system + 工具声明 + 任务原文）**字节不变** | 改它 ⇒ **它之后的所有缓存块失效**（上游定理 1）。现状 `tool_loop.rs` 每轮重建 `msgs[0]` 把 findings 拼进 system，正在每轮砸整段前缀缓存 —— 这是本模块存在的理由 |
//! | **I2** | `E`（梯子）**只追加**：压缩只能追加一条摘要，不许改写既有条目 | 就地改写 = 同样作废其后缓存，且让"历史不可改"作废 |
//! | **I3** | `A`（易变尾）**可重建**：丢弃后仅凭 `S + E` 与 capsule 侧存即可重建 | 不然"被压掉的内容"只能靠**重跑工具**取回 —— 正是要消灭的那个循环 |
//!
//! 本模块**只负责形状与不变量**（纯函数、无 IO、无 LLM）。真正的调度（何时压缩）见 v1.2 P4；
//! 去重与版本有效期见 P2。**先把形状摆对，再谈策略** —— 顺序反了会把策略建在流沙上。

/// 请求体的三段。构造后 `root` 不可变，`ladder` 只增，`tail` 每步重建。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Regions {
    root: String,
    ladder: Vec<String>,
    tail: String,
    /// `root` 的指纹：I1 的自查手段（也是真 run 里"公共前缀占比"的来源之一）
    root_digest: u64,
}

impl Regions {
    /// 建根。**这是唯一一次写 `root`。**
    pub fn new(root: impl Into<String>) -> Self {
        let root = root.into();
        let d = digest(&root);
        Self {
            root,
            ladder: Vec::new(),
            tail: String::new(),
            root_digest: d,
        }
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn root_digest(&self) -> u64 {
        self.root_digest
    }

    pub fn ladder(&self) -> &[String] {
        &self.ladder
    }

    pub fn tail(&self) -> &str {
        &self.tail
    }

    /// 只追加一条梯子条目。空条目直接忽略（否则梯子会被空行喂肥）。
    pub fn push_ladder(&mut self, item: impl Into<String>) {
        let s = item.into();
        if !s.trim().is_empty() {
            self.ladder.push(s);
        }
    }

    /// 每步重建易变尾。**这是唯一允许整体覆盖的区域。**
    pub fn set_tail(&mut self, tail: impl Into<String>) {
        self.tail = tail.into();
    }

    /// 拼成请求体。顺序**必须**是 S → E → A：任何可变的字节都排在所有稳定字节之后。
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(self.root.len() + 1024);
        out.push_str(&self.root);
        for it in &self.ladder {
            out.push('\n');
            out.push_str(it);
        }
        if !self.tail.is_empty() {
            out.push('\n');
            out.push_str(&self.tail);
        }
        out
    }

    /// 相邻两轮请求的**公共前缀字节数**（G1 的代理指标：真值要看厂商是否回报命中 token）。
    /// 判据用它是"要涨不要跌"，**不建在逐字相同上**。
    pub fn common_prefix_len(&self, other: &Regions) -> usize {
        let a = self.render();
        let b = other.render();
        a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
    }
}

/// 一个短小稳定的 FNV-1a：只用于"同一段字节"的自查，不承担密码学职责。
pub(crate) fn digest(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

// ============================================================================
// 计量仪器：公共前缀占比（v1.2 P1 的 G1 代理）
// ============================================================================

/// 相邻两次**请求**的公共前缀统计。
///
/// 为什么量"消息流"而不是量 `Regions`：真正发给厂商的是那条消息序列，
/// 而布局契约约束的也正是它（`[S][E][A]` 说的是**请求体**的形状，不是某个字符串）。
///
/// 口径与误差：按 `role + content` 的字节累加，不含 JSON 每条的引号/转义开销
/// （那是每条约 30 字节的常数，两臂一样多，不影响"涨还是跌"的判断）。
/// 真值仍是厂商回报的命中 token（**未验证**，见需求 §5 的否定判据与拍板 6）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrefixStat {
    /// 与上一轮请求的公共前缀字节数（首轮没有上一轮 ⇒ 0）
    pub prefix: usize,
    /// 本轮请求的总字节数
    pub total: usize,
}

impl PrefixStat {
    /// 占比（0.0~1.0）。`total == 0` 返回 0（不做除零）。
    pub fn ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.prefix as f64 / self.total as f64
        }
    }

    /// 日志里那一段：`87.3%（12.0KB / 13.7KB）`
    pub fn render(&self) -> String {
        format!(
            "{:.1}%（{} / {}）",
            self.ratio() * 100.0,
            kb(self.prefix),
            kb(self.total)
        )
    }
}

fn kb(n: usize) -> String {
    format!("{:.1}KB", n as f64 / 1024.0)
}

/// 一条消息在字节流里的长度
fn msg_len(m: &crate::llm::ChatMessage) -> usize {
    m.role.len() + m.content.len()
}

/// 公共前缀字节数：逐条比 role/content，第一条不等的消息就停在它的公共字节上。
fn common_bytes(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b.iter()).take_while(|(x, y)| x == y).count()
}

/// 本轮请求（`cur`）与上一轮请求（`prev`）的公共前缀统计。
///
/// `prev = None`（首轮）⇒ 前缀 0：**没有上一轮就没有可复用的缓存**，
/// 这条要如实为 0，别拿"自己跟自己比"凑一个漂亮的 100%。
pub fn prefix_stat(
    prev: Option<&[crate::llm::ChatMessage]>,
    cur: &[crate::llm::ChatMessage],
) -> PrefixStat {
    let total = cur.iter().map(msg_len).sum();
    let Some(prev) = prev else {
        return PrefixStat { prefix: 0, total };
    };
    let mut prefix = 0usize;
    for (p, c) in prev.iter().zip(cur.iter()) {
        if p.role != c.role {
            break;
        }
        prefix += p.role.len();
        let n = common_bytes(p.content.as_bytes(), c.content.as_bytes());
        prefix += n;
        if n < p.content.len() || n < c.content.len() {
            break; // 这条消息只对上 n 个字节 ⇒ 前缀到此为止
        }
    }
    PrefixStat { prefix, total }
}

/// 一次 run 的**加权**复用率账：`Σ prefix / Σ total`。
///
/// 为什么加权、而不是"把每轮占比取平均"：占比的权重是字节 —— 一个 100KB 的请求复用了 90%
/// 和一个 2KB 的请求复用了 10%，对账单的影响差着量级。首轮**计总字节、不记前缀**：
/// 它本来就没有可复用的东西，把它排除在外等于自己给自己刷分。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrefixTally {
    pub prefix: usize,
    pub total: usize,
    pub rounds: usize,
}

impl PrefixTally {
    pub fn add(&mut self, s: PrefixStat) {
        self.prefix += s.prefix;
        self.total += s.total;
        self.rounds += 1;
    }

    pub fn ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.prefix as f64 / self.total as f64
        }
    }

    pub fn render(&self) -> String {
        format!(
            "{:.1}%（{} / {}）",
            self.ratio() * 100.0,
            kb(self.prefix),
            kb(self.total)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;

    /// I1：尾段怎么变，根段字节都不许动（这正是现状被违反的那一条）。
    #[test]
    fn i1_root_is_byte_identical_when_tail_changes() {
        let mut r = Regions::new("SYSTEM+TOOLS+TASK");
        r.set_tail("第 1 步的账");
        let d1 = r.root_digest();
        r.set_tail("第 2 步完全不同的账，长度也不一样");
        r.push_ladder("压缩摘要：前 10 步做过什么");
        assert_eq!(
            r.root_digest(),
            d1,
            "I1：root 被改写了 —— 它之后的所有缓存块都会失效"
        );
        assert!(r.render().starts_with("SYSTEM+TOOLS+TASK"));
    }

    /// I2：梯子只追加 ⇒ 新一轮的渲染必须以旧一轮的渲染为前缀。
    #[test]
    fn i2_ladder_is_append_only() {
        let mut r = Regions::new("S");
        r.push_ladder("E1");
        let before = r.render();
        r.push_ladder("E2");
        r.set_tail("tail2");
        let after = r.render();
        assert!(
            after.starts_with(&before),
            "I2：梯子被改写了 ⇒ 前缀不再稳定"
        );
        assert_eq!(r.ladder().len(), 2);
    }

    /// I3：尾段整体重建是**无损**的 —— 它只承载能从 S/E/侧存重算出来的东西。
    #[test]
    fn i3_tail_is_rebuildable_and_prefix_keeps_growing() {
        let mut r = Regions::new("S");
        r.push_ladder("E1");
        r.set_tail("账：读了 a.rs 1-100");
        let a = r.clone();
        r.set_tail("账：读了 a.rs 1-100，又跑了 cargo test"); // 重算同一段
        let b = r.clone();
        // 根 + 梯子完全相同 ⇒ 前缀长度只被尾段拉长，不会被"重建"打回
        assert!(b.common_prefix_len(&a) >= r.root().len() + "E1".len());
        // 反例守卫：尾段若被误当成稳定区（塞进 root），公共前缀会立刻缩短
        let mut wrong = Regions::new("S\n账：读了 a.rs 1-100");
        wrong.push_ladder("E1");
        wrong.set_tail("账：读了 a.rs 1-100，又跑了 cargo test");
        assert!(
            b.common_prefix_len(&a) > a.common_prefix_len(&wrong),
            "把可变内容放进 root 会缩短公共前缀 —— 这正是现状踩的坑"
        );
    }

    /// 空梯子条目不许把梯子喂肥（否则每步都推一条空行，白占预算）。
    #[test]
    fn empty_ladder_items_are_ignored() {
        let mut r = Regions::new("S");
        r.push_ladder("   ");
        r.push_ladder("");
        assert!(r.ladder().is_empty());
    }

    // ---- 计量仪器（v1.2 P1 的 G1 代理）----

    /// 首轮如实记 0：没有上一轮就没有可复用的缓存，不许"自己跟自己比"凑 100%。
    #[test]
    fn first_round_reports_zero_prefix_but_still_counts_its_bytes() {
        let cur = vec![ChatMessage::system("S"), ChatMessage::user("任务")];
        let s = prefix_stat(None, &cur);
        assert_eq!(s.prefix, 0);
        // 口径写清楚：**role 也计入字节**（真发出去的每条消息都带 role）
        assert_eq!(
            s.total,
            "system".len() + "S".len() + "user".len() + "任务".len()
        );
        assert_eq!(s.ratio(), 0.0);
        // 自己跟自己比才是 100%（也说明 `render` 的口径：role + content 逐条累加）
        let same = prefix_stat(Some(&cur), &cur);
        assert_eq!(same.ratio(), 1.0);
    }

    /// 公共前缀是**字节流**上的一件事：块里一分叉，块之后的消息就只是"内容相同"，
    /// 位置已经不同了 —— 所以它们一个字节都算不上复用。
    #[test]
    fn a_change_inside_the_block_stops_the_prefix_even_if_later_messages_are_identical() {
        let prev = vec![
            ChatMessage::system("SYSTEM+账v1"),
            ChatMessage::user("任务"),
            ChatMessage::user("第 1 轮读到的正文"),
        ];
        let cur = vec![
            ChatMessage::system("SYSTEM+账v2"),
            ChatMessage::user("任务"),
            ChatMessage::user("第 1 轮读到的正文"),
        ];
        let s = prefix_stat(Some(&prev), &cur);
        assert_eq!(
            s.prefix,
            "system".len() + "SYSTEM+账v".len(),
            "前缀只该停在「账v1/账v2」分叉的那一个字节上，后面的相同内容不算复用"
        );
    }

    /// **否定判据的可证伪形状**：同一段历史，两种布局，占比必须"尾布局 > 塞根布局"。
    ///
    /// 这里用**对旧布局最有利**的假设：块只追加、从不改写（真实块会被 supersede / 落盘 /
    /// 账本窗口滑走证伪）。即便如此它也赢不了：块在请求体的最前面，
    /// 它一动，**任务头 + 整段历史**就全部不在前缀里了。
    #[test]
    fn tail_layout_wins_the_common_prefix_measure_even_when_the_block_only_appends() {
        let base = "S".repeat(400); // AGENT_SYSTEM 那条常量
        let task = "T".repeat(200); // 首条 user 消息（项目根 + 连接清单 + 任务）
        let bulk = "H".repeat(4000); // 历史正文：真实 run 里这才是大头（读过的文件）
        let block = |k: usize| format!("## 已确认的事实\n{}", "f".repeat(200 * k));
        let sys = |k: usize| format!("{base}{}", block(k));
        let (e1, e2) = (
            "〔已发出调用〕r1".to_string(),
            "〔已发出调用〕r2".to_string(),
        );

        // 旧布局：块拼在 system 尾部（每轮重建）
        let off = [
            vec![ChatMessage::system(sys(1)), ChatMessage::user(task.clone())],
            vec![
                ChatMessage::system(sys(2)),
                ChatMessage::user(task.clone()),
                ChatMessage::assistant(e1.clone()),
                ChatMessage::user(bulk.clone()),
            ],
            vec![
                ChatMessage::system(sys(3)),
                ChatMessage::user(task.clone()),
                ChatMessage::assistant(e1.clone()),
                ChatMessage::user(bulk.clone()),
                ChatMessage::assistant(e2.clone()),
                ChatMessage::user(bulk.clone()),
            ],
        ];
        // 尾布局：块是易变尾（每轮整块重建，只活这一次请求）
        let on = [
            vec![
                ChatMessage::system(base.clone()),
                ChatMessage::user(task.clone()),
                ChatMessage::user(block(1)),
            ],
            vec![
                ChatMessage::system(base.clone()),
                ChatMessage::user(task.clone()),
                ChatMessage::assistant(e1.clone()),
                ChatMessage::user(bulk.clone()),
                ChatMessage::user(block(2)),
            ],
            vec![
                ChatMessage::system(base.clone()),
                ChatMessage::user(task.clone()),
                ChatMessage::assistant(e1.clone()),
                ChatMessage::user(bulk.clone()),
                ChatMessage::assistant(e2.clone()),
                ChatMessage::user(bulk.clone()),
                ChatMessage::user(block(3)),
            ],
        ];
        let tally = |run: &[Vec<ChatMessage>]| {
            let mut t = PrefixTally::default();
            for (i, cur) in run.iter().enumerate() {
                t.add(prefix_stat(
                    if i == 0 { None } else { Some(&run[i - 1]) },
                    cur,
                ));
            }
            t
        };
        let (a, b) = (tally(&off), tally(&on));
        assert!(
            b.ratio() > a.ratio(),
            "尾布局的加权复用率必须更高：尾 {} vs 塞根 {}",
            b.render(),
            a.render()
        );
        // 方向要明显（不是靠小数点后的零头赢的）
        assert!(
            b.ratio() > a.ratio() * 1.5,
            "差距该是量级上的：尾 {} vs 塞根 {}",
            b.render(),
            a.render()
        );
    }
}
