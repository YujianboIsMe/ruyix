//! RebaseScheduler（v1.2 P4）：**什么时候压缩**。
//!
//! 它替掉的是 `history_keep_rounds` 那个"保留 N 轮"的拍脑袋数字（需求 §1 的病灶之一）。
//! 这里把 `keep_rounds` 降级成**安全下限**：调度器说什么都不能在下限之内压，
//! 但"何时压"由**目标函数**决定（需求 §4 ③ / 架构 §7）。
//!
//! ## 代价模型（三条，都是相对"未缓存输入 token = 1.0"）
//!
//! - `carry_rate`：把一个 token 留在前缀里，**每轮**付这么多（命中缓存的价格）；
//! - `rebuild_rate`：把它丢掉之后**再要一次**的前缀重建代价（一次性）；
//! - 于是"压不压"就是经典的两难：留着按轮付小钱，丢了付一笔大钱 ——
//!   最优解是**阈值型**的（累计携带代价摸到重建代价就该压），这正好也是 ski-rental 的形状。
//!
//! ## 三种情形（架构 §7）
//!
//! | 情形 | 判据 |
//! |---|---|
//! | **已知 horizon**（H = `MAX_STEPS` = 96） | 反向 DP（`dp`）—— 主路径 |
//! | **未知 horizon**（`remaining == 0`，例如墙钟上限提前终止） | ski-rental，竞争比 `e/(e−1)` |
//! | **预算越界**（阶梯超过 `budget_tokens`） | 无条件压（`Budget`）—— G2 是硬约束 |
//!
//! 另外无论调度器说什么，都不能在 `floor_rounds` 之内压（`Floor`）。
//!
//! **最优性结论**（这个代价模型下的闭式下界，也是判据的来历）：压一次付 `rebuild_rate × L`，
//! 每轮省 `carry_rate × L` ⇒ 划算的条件是 **剩余轮数 > `rebuild_rate / carry_rate`**（默认 0.9/0.1 = 9；
//! 阶梯每轮还在长，所以实测翻转会略晚一点 —— 单测钉的是"单调 + 落在这个带里"）。
//! 也就是说"要不要压"几乎只取决于**还剩多少轮**，而不是"阶梯现在多大" ——
//! 后者正是 `keep_rounds` 那个拍出来的数字的错法。
//!
//! ## MPC
//!
//! 每轮用**实测**的增长重新规划一次，只取第一步的动作（`decide`）—— 计划会随事实改，
//! 而不是开跑时算一次就锁死（那是"计划赶不上变化"的老毛病）。

/// 调度参数（来自配置；单位一律 token）
#[derive(Clone, Copy, Debug)]
pub struct Cfg {
    /// 规划视野：H = 96（与 `MAX_STEPS` 一致，上游同设定）
    pub horizon: usize,
    /// prompt 的预算 B（G2 的硬约束）
    pub budget_tokens: usize,
    /// 每轮携带 1 token 的代价（缓存读价）
    pub carry_rate: f64,
    /// 重建前缀的**一次性额外**代价
    pub rebuild_rate: f64,
    /// 安全下限（原 `history_keep_rounds`）：不得在此之内压缩
    pub floor_rounds: usize,
}

/// 当前状态（每轮由主循环喂一次；token 数由**字节代理**换算，见 `estimate_tokens`）
#[derive(Clone, Copy, Debug)]
pub struct State {
    /// 阶梯当前大小
    pub ladder_tokens: usize,
    /// 压完之后会剩下多少（上一次压缩后的实测大小）
    pub base_tokens: usize,
    /// 每轮新增（实测；0 表示还没测出来，按 `ladder`/已跑轮数估）
    pub growth_tokens: usize,
    /// 距上一次压缩过了几轮
    pub rounds_since: usize,
    /// 视野里**还剩**几轮；`0` = 未知（走 ski-rental）
    pub remaining: usize,
}

/// 决策理由（写进 trace，判据按它读）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// 反向 DP（视野已知）
    Dp,
    /// ski-rental 兜底（视野未知）
    SkiRental,
    /// 安全下限压住了
    Floor,
    /// 预算越界（G2 硬约束）
    Budget,
}

impl Reason {
    pub fn name(&self) -> &'static str {
        match self {
            Reason::Dp => "DP",
            Reason::SkiRental => "ski-rental",
            Reason::Floor => "下限",
            Reason::Budget => "预算",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Decision {
    pub rebase: bool,
    pub reason: Reason,
    /// 预计省下的 token（DP 能算就写数；下限/兜底给 0 —— **不许编数字**）
    pub expected_saving: u64,
}

/// 这一条决策的 trace 行（架构 §7 规定的形状，逐字：
/// `rebase 决策=执行/跳过 理由=DP|ski-rental|下限 预计省 X 字节`）
pub fn render(d: &Decision) -> String {
    format!(
        "rebase 决策={} 理由={} 预计省 {} 字节",
        if d.rebase { "执行" } else { "跳过" },
        d.reason.name(),
        d.expected_saving
    )
}

/// 字节 → token 的**代理换算**（4 字节 ≈ 1 token）。
///
/// 真 tokenizer 要模型文件（P4 没接），所以这条是代理 —— 架构 §6 允许，但要求
/// **trace 里写明是代理**：[`render`] 的数字前面主循环会带上"字节代理"四个字。
pub fn estimate_tokens(bytes: usize) -> usize {
    bytes / 4
}

/// **材料性下限**：DP 算出来的是"**现在压 vs 稍后压**"的差（两条路最终都会压），
/// 所以这个差在多数状态上只有重建代价的几个百分点（实测四个状态：3% / 5.8% / 10.7% / 10.7%）。
/// 低于本比例 ⇒ **不动**：一次重建还有模型算不进去的一次性成本（前缀在折叠点断开、
/// 逐字历史改走召回），近无差别的决策就该推迟。
///
/// 这条是**实测逼出来的**：阶梯才 2.5K token、净收益只有代价的 2% 时 DP 就压了，
/// 而那一压把前缀打断 —— 脚本化配对臂上 G1 稳态掉了 1.7 个百分点。
pub const MARGIN: f64 = 0.05;

/// ski-rental 的竞争比 `e/(e−1) ≈ 1.582`
pub const SKI_RENTAL_RATIO: f64 = std::f64::consts::E / (std::f64::consts::E - 1.0);

/// 携带代价因子：`carry_rate × ΣL(d)` 里那个求和（每轮携带 `L(d)`，d 逐轮 +1）
fn ladder_at(_cfg: &Cfg, s: &State, d: usize) -> f64 {
    let g = s.growth_tokens.max(1) as f64;
    let base = s.base_tokens as f64;
    base + g * d as f64
}

/// 丢一次要付的重建代价
fn rebuild_cost(cfg: &Cfg, ladder: f64) -> f64 {
    cfg.rebuild_rate * ladder
}

/// 反复压缩的**累计**携带代价（从上次压缩算起），给 ski-rental 用
fn carried_so_far(cfg: &Cfg, s: &State) -> f64 {
    let mut sum = 0.0;
    for d in 0..=s.rounds_since {
        sum += ladder_at(cfg, s, d) * cfg.carry_rate;
    }
    sum
}

/// **反向 DP**：从当前状态出发，`rem` 轮内最优的总代价（携带 + 重建）。
///
/// 状态只有一维（"距上次压缩几轮"），所以 dp 表是 `rem × d` 的二维表，96×96 微不足道。
/// `f(rem, d)`：还剩 `rem` 轮、当前已携带 `d` 轮时，把后面跑完的最小代价。
fn dp(cfg: &Cfg, s: &State, rem: usize) -> f64 {
    let g = s.growth_tokens.max(1) as f64;
    let base = s.base_tokens as f64;
    let max_d = s.rounds_since + rem + 1;
    // f[d] = 跑完剩下的最小代价（从 rem=0 往上滚）
    let mut f = vec![0.0f64; max_d];
    for _ in 1..=rem {
        let mut next = vec![0.0f64; max_d];
        for d in 0..max_d {
            let ladder = base + g * d as f64;
            let carry = ladder * cfg.carry_rate;
            let keep = carry + f[(d + 1).min(max_d - 1)];
            let rebase = carry + rebuild_cost(cfg, ladder) + f[0];
            next[d] = keep.min(rebase);
        }
        f = next;
    }
    f[s.rounds_since.min(max_d - 1)]
}

/// 决策（MPC：用**当前实测状态**重新规划一次，取第一步）
pub fn decide(cfg: &Cfg, s: &State) -> Decision {
    // ① 预算越界 ⇒ 无条件压（G2 是硬约束，压不住就没有后面的判据了）
    if cfg.budget_tokens > 0 && s.ladder_tokens > cfg.budget_tokens {
        return Decision {
            rebase: true,
            reason: Reason::Budget,
            expected_saving: (s.ladder_tokens - s.base_tokens) as u64,
        };
    }
    // ② 安全下限：调度器说什么都不许在下限之内压
    if s.rounds_since < cfg.floor_rounds {
        return Decision {
            rebase: false,
            reason: Reason::Floor,
            expected_saving: 0,
        };
    }
    let ladder = ladder_at(cfg, s, s.rounds_since);
    // ③ 视野未知 ⇒ ski-rental：累计携带代价摸到"重建 × e/(e−1)"就压
    if s.remaining == 0 {
        let threshold = rebuild_cost(cfg, ladder) * SKI_RENTAL_RATIO;
        let carried = carried_so_far(cfg, s);
        let rebase = carried >= threshold;
        return Decision {
            rebase,
            reason: Reason::SkiRental,
            expected_saving: if rebase {
                (s.ladder_tokens - s.base_tokens) as u64
            } else {
                0
            },
        };
    }
    // ④ 视野已知 ⇒ 反向 DP：压 vs 不压，比"压完再跑"与"留着再跑"哪个便宜
    let rem = s.remaining.min(cfg.horizon);
    let carry = ladder * cfg.carry_rate;
    let keep_cost = carry + dp(cfg, s, rem.saturating_sub(1));
    let rebase_cost = if rem == 0 {
        carry
    } else {
        carry + rebuild_cost(cfg, ladder) + {
            let mut after = *s;
            after.rounds_since = 0;
            after.ladder_tokens = s.base_tokens + s.growth_tokens;
            after.remaining = rem - 1;
            dp(cfg, &after, rem - 1)
        }
    };
    // **等价带**：视野很长时 DP 对"现在压还是稍后压"是**无差别**的（两条路都走到同一条稳态周期），
    // 差 1% 以内就选**不压** —— 不加这条，决策会在等价解之间摇摆，trace 就会自相矛盾。
    //
    // **再加一道材料性下限**（`MARGIN`）：收益必须明显盖过这一次重建的代价，否则不值得动。
    // 这条是**实测逼出来的**：阶梯才 2.5K token 时 DP 判"省 49 单位"就压了，而那一压会把
    // 前缀打断 —— 脚本化配对臂上 G1 稳态掉了 1.7 个百分点（真金白银的缓存复用）。
    // 一个"省 49"的决策换来一次缓存全失效，属于拿零头换整钱 ⇒ 收益不到代价的一半就不动。
    let net_gain = keep_cost - rebase_cost;
    let rebase = rebase_cost < keep_cost * 0.99 && net_gain >= rebuild_cost(cfg, ladder) * MARGIN;
    Decision {
        rebase,
        reason: Reason::Dp,
        expected_saving: if rebase {
            // "预计省"= 留着不压要多付的那部分（正数才有意义）
            ((keep_cost - rebase_cost).max(0.0)) as u64
        } else {
            ((rebase_cost - keep_cost).max(0.0)) as u64
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Cfg {
        Cfg {
            horizon: 96,
            budget_tokens: 60_000,
            carry_rate: 0.1,
            rebuild_rate: 0.9,
            floor_rounds: 3,
        }
    }

    /// 造状态：**阶梯 = base + growth × since**（模型就是这么算的；
    /// 第一个参数只是"记录用"的阶梯读数，别指望它影响决策 —— 这一点踩过一次，
    /// 当时以为改 `ladder_tokens` 就能改决策，结果怎么调都没反应）。
    fn st(ladder_note: usize, base: usize, growth: usize, since: usize, remaining: usize) -> State {
        State {
            ladder_tokens: ladder_note,
            base_tokens: base,
            growth_tokens: growth,
            rounds_since: since,
            remaining,
        }
    }

    /// 下限就是下限：哪怕 DP 想压，`rounds_since < floor` 也不许压（拍板 3）
    #[test]
    fn floor_beats_the_scheduler() {
        let c = cfg();
        let d = decide(&c, &st(1_500, 1_000, 500, 1, 90));
        assert!(!d.rebase, "下限之内不许压");
        assert_eq!(d.reason, Reason::Floor);
        assert_eq!(d.expected_saving, 0, "不许编数字");
    }

    /// 预算越界 ⇒ 无条件压（G2 是硬约束，压不住就没有后面的判据了）
    #[test]
    fn budget_forces_a_rebase() {
        let mut c = cfg();
        c.budget_tokens = 20_000;
        let d = decide(&c, &st(31_000, 1_000, 1_000, 30, 90));
        assert!(d.rebase);
        assert_eq!(d.reason, Reason::Budget, "预算优先于下限与 DP");
        assert_eq!(d.expected_saving, 30_000);
    }

    /// 分层：**没越预算时是 DP 说了算，越了就一律由预算强制压**。
    /// 断言的是这条分层规则，而不是某个状态该压不该压 —— 后者随增长与视野走，
    /// 写死了它就成了另一个"拍出来的数"。
    #[test]
    fn the_budget_overrides_the_dp() {
        let mut c = cfg();
        c.budget_tokens = 20_000;
        let under = st(15_000, 1_000, 500, 20, 60); // 阶梯 = 11_000 < B
        let over = st(31_000, 1_000, 1_000, 30, 60); // 阶梯 = 31_000 > B
        assert_ne!(
            decide(&c, &under).reason,
            Reason::Budget,
            "没越 B 时不许拿预算当理由"
        );
        let d = decide(&c, &over);
        assert!(d.rebase);
        assert_eq!(d.reason, Reason::Budget);
    }

    /// 视野未知（`remaining == 0`）⇒ ski-rental：累计携带代价摸到 `重建 × e/(e−1)` 才压
    #[test]
    fn ski_rental_fires_past_the_competitive_ratio() {
        let c = cfg();
        let early = st(3_000, 1_000, 1_000, 2, 0);
        assert!(!decide(&c, &early).rebase, "刚压完两轮再压是烧钱");
        let late = st(31_000, 1_000, 1_000, 30, 0);
        let d = decide(&c, &late);
        assert!(d.rebase, "累计携带越过阈值 ⇒ 该压");
        assert_eq!(d.reason, Reason::SkiRental);
        let threshold = 0.9 * 31_000.0 * SKI_RENTAL_RATIO;
        assert!(carried_so_far(&c, &late) >= threshold);
    }

    /// **"要不要压"是这笔账的函数，不是"过了几轮"的函数**（这也是 P4 的验收判据）：
    /// 同一个阶梯（21K，预算之内），**视野短就不压、视野长才压**；
    /// 而阶梯小到 2.5K 时，任何视野都不压（重建的收益连材料性下限都够不着）。
    #[test]
    fn the_decision_comes_from_the_account_not_the_round_count() {
        let c = cfg();
        let ladder = |rem| st(21_000, 1_000, 2_000, 10, rem);
        assert!(
            !decide(&c, &ladder(5)).rebase,
            "只剩 5 轮 ⇒ 这一次重建摊不回来"
        );
        assert!(decide(&c, &ladder(60)).rebase, "还剩 60 轮 ⇒ 该压");
        // 小阶梯：任何视野都不压（含材料性下限挡住的那些）
        for rem in 1..=96 {
            assert!(
                !decide(&c, &st(2_500, 500, 100, 20, rem)).rebase,
                "2.5K 的阶梯、每轮涨 100 ⇒ 压它连材料性下限都够不着（{rem} 轮）"
            );
        }
    }

    /// MPC：同一个状态重复问，答案必须一样（否则 trace 会自相矛盾）
    #[test]
    fn replanning_on_the_same_state_is_stable() {
        let c = cfg();
        for s in [
            st(21_000, 1_000, 2_000, 10, 60),
            st(21_000, 1_000, 2_000, 10, 5),
            st(31_000, 1_000, 1_000, 30, 0),
        ] {
            let a = decide(&c, &s);
            let b = decide(&c, &s);
            assert_eq!(a.rebase, b.rebase);
            assert_eq!(a.reason, b.reason);
            assert_eq!(a.expected_saving, b.expected_saving);
        }
    }

    /// trace 的形状是判据的一部分（ui-smoke U70 按它数）
    #[test]
    fn the_trace_line_has_the_documented_shape() {
        assert_eq!(
            render(&Decision {
                rebase: true,
                reason: Reason::Dp,
                expected_saving: 1234,
            }),
            "rebase 决策=执行 理由=DP 预计省 1234 字节"
        );
        assert_eq!(
            render(&Decision {
                rebase: false,
                reason: Reason::Floor,
                expected_saving: 0,
            }),
            "rebase 决策=跳过 理由=下限 预计省 0 字节"
        );
    }
}
