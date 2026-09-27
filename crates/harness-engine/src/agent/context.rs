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
        let d = fnv1a(&root);
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
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
