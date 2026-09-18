//! 规划：结构定义 + 提示词 + 容错解析。
//!
//! 模型不是编译器，返回的 JSON 经常有：字段改名（description/detail）、
//! files 写成字符串、steps 少 id、多套一层 {"plan": {...}}。
//! 这些都不该让整个流程失败 —— 我们的兜底策略是"能用就用，缺什么补什么"，
//! 实在缺关键信息（没有步骤）才报错。

use crate::config::LlmConfig;
use crate::llm::{self, ChatMessage};
use serde::{Deserialize, Serialize};

pub const PLAN_SYSTEM: &str = r#"你是一位资深软件架构师兼工程负责人。用户给你一个自然语言任务，你要把它拆成可执行、可验证的步骤序列（MVP 级别，不要过度设计）。

只输出一个 JSON 对象，不要输出任何解释文字，不要用 markdown 代码块包裹。结构如下：
{
  "project_name": "项目目录名，仅限字母数字和 - _，全小写",
  "language": "主要语言，python / rust / javascript / go 之一",
  "summary": "一两句话说明整体方案与关键取舍",
  "entry": "主入口文件的相对路径",
  "test_command": "在该项目目录下运行单元测试的命令，如 python -m pytest -q 或 cargo test",
  "steps": [
    {"id": 1, "title": "短标题", "detail": "本步具体做什么、产出哪些文件、怎么算完成",
     "files": ["相对路径/文件.py"], "kind": "code"}
  ]
}

硬性要求：
1. steps 3-8 个；一步只做一件可验证的事；kind 只能是 code / test / config / doc。
   每个步骤都必须产出至少一个具体文件（files 不能为空）—— 不要规划“跑一遍命令确认一下”
   这类不产出文件的步骤，验证由 harness 自己负责。
2. 必须包含至少一个 kind="test" 的步骤，写真实可运行的单元测试（覆盖正常路径 + 至少一个边界或异常路径）。
3. 优先零依赖或最小依赖方案，确保生成后能被"语法检查 + 单元测试"真正跑通。
4. 步骤之间前后一致：后面步骤可以引用前面步骤产出的文件名。
5. 所有文件路径都是相对路径，用 / 分隔，禁止绝对路径和 ..。
6. test_command 必须与 language 匹配且真实存在（不确定就用最通用的：python -m unittest discover / cargo test / node --test）。"#;

pub const GEN_SYSTEM: &str = r#"你是一位资深工程师，负责按既定规划写出可直接运行的完整代码。

只输出一个 JSON 对象：
{"files": [{"path": "相对路径", "content": "文件完整内容"}], "notes": "本步说明、风险、以及后续步骤需要知道的事"}

硬性要求：
1. content 必须是完整文件内容：禁止 "..."、"此处省略"、"TODO 占位"、伪代码。
2. 需要依赖或配置（requirements.txt / Cargo.toml / package.json / pytest.ini 等）就一并输出。
3. 测试步骤必须给出真实可运行的断言，不要 mock 掉被测逻辑本身，不要写只打印不校验的假测试。
4. 与"已生成文件"保持一致，不要重复定义同名符号或冲突的实现。
5. 路径用相对路径、/ 分隔，禁止绝对路径与 ..。
6. 代码风格干净、有必要的注释，能被人读懂。
7. 如果当前步骤确实不需要新增或修改任何文件（极少见），就返回 "files": [] 并在 notes 里说明原因。"#;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct PlanStep {
    #[serde(default)]
    pub id: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default, alias = "description")]
    pub detail: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default = "default_kind")]
    pub kind: String,
}

fn default_kind() -> String {
    "code".to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Plan {
    #[serde(default)]
    pub project_name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub entry: String,
    #[serde(default)]
    pub test_command: String,
    #[serde(default)]
    pub steps: Vec<PlanStep>,
}

impl Plan {
    /// 目录名/日志标题用的安全短名
    pub fn slug(&self) -> String {
        let base = if self.project_name.trim().is_empty() {
            "harness-project"
        } else {
            self.project_name.trim()
        };
        slugify(base)
    }

    /// 有没有测试步骤 —— UI 用它给"验证"阶段一个预期
    pub fn has_test_step(&self) -> bool {
        self.steps
            .iter()
            .any(|s| s.kind.eq_ignore_ascii_case("test"))
    }
}

/// 把任意文本变成安全的目录名（保留中文，剔除路径分隔符与空白）
pub fn slugify(s: &str) -> String {
    // **必须只产出 ASCII**：这个 slug 会进运行目录名，还会进 docker 容器名
    // （`docker run --name harness-<run_id>-0`）—— 容器名只允许 [a-zA-Z0-9_.-]，
    // 中文会被 docker 直接拒掉（exit 125），表现为"语法检查莫名失败"。
    // 踩过：v0.7 评估任务的标题是中文 → 每一次语法检查都 125，测试全被跳过。
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else if ch.is_whitespace() || !ch.is_ascii() {
            // 空白与非 ASCII（中文等）都当分隔符 —— 不能让它们进名字
            out.push('-');
        }
        // 其余 ASCII 符号（/ \ : * ? " < > | 等）直接丢弃
    }
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    let out = out.trim_matches(['-', '.', '_']).to_string();
    if out.is_empty() {
        // 走到这里有两种输入，要分开处理：
        // ① 纯符号（"///"）：没有信息可用 → 沿用固定的 harness-project 兜底 名；
        // ② 内容全是非 ASCII（中文标题）：**必须用内容哈希**，否则两个不同的中文标题
        //    会撞成同一个 slug（运行目录/容器名重名）。
        let had_non_ascii = s.chars().any(|c| !c.is_ascii() && !c.is_whitespace());
        if had_non_ascii {
            format!("p-{}", short_hash(s))
        } else {
            "harness-project".to_string()
        }
    } else {
        out.chars().take(40).collect()
    }
}

/// 8 位十六进制的 FNV-1a：给 slugify 兜底用（不引依赖、确定性、跨进程稳定）。
fn short_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}").chars().take(8).collect()
}

/// 容错解析：优先强类型，失败后走 Value 手工映射。
pub fn parse_plan(raw: &str) -> Result<Plan, String> {
    let json = llm::extract_json_object(raw);
    // 注意：Plan 的字段全是 #[serde(default)]，所以 {"plan": {...}} 这种多包一层的
    // 输出也能"解析成功"但 steps 为空。必须要求 steps 非空才认这次强类型解析。
    if let Ok(p) = serde_json::from_str::<Plan>(&json)
        && !p.steps.is_empty()
    {
        return normalize(p);
    }

    let v: serde_json::Value = serde_json::from_str(&json).map_err(|e| {
        format!(
            "规划结果不是合法 JSON: {e}；片段: {}",
            crate::exec::clip(&json, 400)
        )
    })?;

    // 有的模型会多包一层 {"plan": {...}} 或 {"result": {...}}
    let v = if v.get("steps").is_none() {
        ["plan", "result", "data", "output"]
            .iter()
            .find_map(|k| v.get(*k).filter(|inner| inner.get("steps").is_some()))
            .cloned()
            .unwrap_or(v)
    } else {
        v
    };

    let steps_v = v
        .get("steps")
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();

    let mut steps = Vec::new();
    for (i, sv) in steps_v.iter().enumerate() {
        let title = get_str(sv, &["title", "name", "step", "summary"])
            .unwrap_or_else(|| format!("步骤 {}", i + 1));
        let detail =
            get_str(sv, &["detail", "description", "desc", "what", "content"]).unwrap_or_default();
        let kind = get_str(sv, &["kind", "type", "category"]).unwrap_or_else(|| "code".into());
        let files = get_str_list(sv, &["files", "paths", "output_files", "artifacts"]);
        let id = sv
            .get("id")
            .and_then(|x| x.as_u64())
            .unwrap_or((i + 1) as u64) as u32;
        steps.push(PlanStep {
            id,
            title,
            detail,
            files,
            kind,
        });
    }

    let plan = Plan {
        project_name: get_str(&v, &["project_name", "projectName", "name", "project"])
            .unwrap_or_default(),
        language: get_str(&v, &["language", "lang", "stack"]).unwrap_or_default(),
        summary: get_str(&v, &["summary", "overview", "description"]).unwrap_or_default(),
        entry: get_str(&v, &["entry", "entrypoint", "main"]).unwrap_or_default(),
        test_command: get_str(&v, &["test_command", "testCommand", "test"]).unwrap_or_default(),
        steps,
    };
    normalize(plan)
}

fn normalize(mut p: Plan) -> Result<Plan, String> {
    if p.steps.is_empty() {
        return Err("规划结果里没有任何可执行步骤（steps 为空）".into());
    }
    if p.project_name.trim().is_empty() {
        p.project_name = "harness-project".into();
    }
    if p.language.trim().is_empty() {
        p.language = "python".into();
    }
    if p.summary.trim().is_empty() {
        p.summary = "（模型未给出方案说明）".into();
    }
    for (i, s) in p.steps.iter_mut().enumerate() {
        s.id = (i + 1) as u32;
        if s.title.trim().is_empty() {
            s.title = format!("步骤 {}", i + 1);
        }
        let k = s.kind.to_ascii_lowercase();
        s.kind = match k.as_str() {
            "code" | "test" | "config" | "doc" => k,
            "tests" | "testing" | "unit_test" => "test".into(),
            "documentation" | "readme" => "doc".into(),
            "setup" | "deps" | "dependencies" | "build" => "config".into(),
            _ => "code".into(),
        };
    }
    Ok(p)
}

fn get_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(s) = v.get(*k).and_then(|x| x.as_str())
            && !s.trim().is_empty()
        {
            return Some(s.trim().to_string());
        }
    }
    None
}

fn get_str_list(v: &serde_json::Value, keys: &[&str]) -> Vec<String> {
    for k in keys {
        match v.get(*k) {
            Some(serde_json::Value::Array(a)) => {
                return a
                    .iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect();
            }
            Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                return s
                    .split([',', ';'])
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect();
            }
            _ => {}
        }
    }
    Vec::new()
}

/// 组装规划阶段的 user 消息。
///
/// **知识块只进这里（user 段落），绝不进 system prompt** —— 否则等于让外部文本
/// 获得指令级权限（需求 §2.5 的硬要求，被单测钉着）。
pub fn build_user_prompt(task: &str, kb_block: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(b) = kb_block
        && !b.trim().is_empty()
    {
        out.push_str(b);
        out.push_str("\n\n（以上为参考资料，可能过期；请优先遵守任务本身的要求。）\n\n");
    }
    out.push_str(&format!(
        "任务描述：\n{}\n\n请输出计划 JSON。注意：所有步骤、文件路径、测试命令都要在后续真的能落地执行。",
        task.trim()
    ));
    out
}

/// 调 DeepSeek 生成计划。`kb_block` 是知识库注入块（None = 没有知识库）。
pub async fn generate(
    cfg: &LlmConfig,
    task: &str,
    kb_block: Option<&str>,
) -> Result<(Plan, llm::Usage, String, u128), String> {
    if task.trim().is_empty() {
        return Err("任务描述不能为空".into());
    }
    let user = build_user_prompt(task, kb_block);
    let out = llm::chat(
        cfg,
        &[ChatMessage::system(PLAN_SYSTEM), ChatMessage::user(user)],
        true,
    )
    .await?;
    let plan = parse_plan(&out.content)?;
    Ok((plan, out.usage, out.model, out.elapsed_ms))
}

#[cfg(test)]
mod tests {
    use super::*;
    /// slug 必须**只含 ASCII**：它会进运行目录名，还会进 docker 容器名。
    /// 中文标题以前会原样进名字 → `docker: Invalid container name` → 语法检查 exit 125。
    #[test]
    fn slug_is_ascii_safe_for_docker_names() {
        for s in [
            "私有约定只有知识库知道",
            "strutil 模块 v2",
            "订单 Order 服务/重构",
            "   ",
            "中文mixed english 123",
        ] {
            let slug = slugify(s);
            assert!(!slug.is_empty(), "空 slug：{s:?}");
            assert!(
                slug.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
                "slug 里出现了非 ASCII 字符：{s:?} → {slug:?}"
            );
            assert!(slug.len() <= 40, "slug 太长：{slug:?}");
        }
        // 稳定：同样的输入给同样的兜底名（不是随机）
        assert_eq!(slugify("私有约定"), slugify("私有约定"));
        assert!(slugify("私有约定").starts_with("p-"));
        // 英文照旧
        assert_eq!(slugify("strutil 模块 v2"), "strutil-v2");
    }

    #[test]
    fn parses_well_formed_plan() {
        let raw = r#"{"project_name":"Todo CLI","language":"python","summary":"s","entry":"main.py",
        "test_command":"python -m pytest -q",
        "steps":[{"id":1,"title":"核心模块","detail":"写 add/list","files":["todo.py"],"kind":"code"},
                 {"id":2,"title":"单元测试","detail":"覆盖空列表","files":["test_todo.py"],"kind":"test"}]}"#;
        let p = parse_plan(raw).unwrap();
        assert_eq!(p.slug(), "todo-cli");
        assert_eq!(p.steps.len(), 2);
        assert!(p.has_test_step());
        assert_eq!(p.steps[1].kind, "test");
    }

    #[test]
    fn repairs_loose_model_output() {
        // description 而不是 detail、files 是字符串、kind 是 "tests"、还多包了一层 plan
        let raw = r#"{"plan":{"projectName":"X","language":"PYTHON",
        "steps":[{"title":"A","description":"do a","files":"a.py, b.py","type":"tests"}]}}"#;
        let p = parse_plan(raw).unwrap();
        assert_eq!(p.steps.len(), 1);
        assert_eq!(p.steps[0].detail, "do a");
        assert_eq!(p.steps[0].files, vec!["a.py", "b.py"]);
        assert_eq!(p.steps[0].kind, "test");
        assert_eq!(p.steps[0].id, 1);
    }

    #[test]
    fn errors_when_no_steps() {
        let err = parse_plan("{\"project_name\":\"x\"}").unwrap_err();
        assert!(err.contains("步骤"), "{err}");
    }

    #[test]
    fn slugify_drops_path_unsafe_chars() {
        assert_eq!(slugify("My Project/v2: demo"), "my-projectv2-demo");
        // 这里原来断言中文**保留**（`slugify("中文项目") == "中文项目"`）—— 那是在钉一个 bug：
        // slug 会进 docker 容器名，而容器名只允许 [a-zA-Z0-9_.-]，
        // 中文会让 `docker run --name ...` 直接失败（exit 125），
        // 症状是"语法检查莫名失败、测试被跳过"（v0.7 评估第一次跑就撞上）。
        // 现在的行为：非 ASCII 当分隔符 → 全中文标题用内容哈希兜底（非空且稳定）。
        let cn = slugify("中文项目");
        assert!(cn.starts_with("p-"), "全中文标题要落到哈希兜底：{cn}");
        assert!(cn.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert_eq!(slugify("///"), "harness-project");
        assert_eq!(slugify("订单 service 层"), "service");
    }

    #[test]
    fn every_step_gets_sequential_id() {
        let raw = r#"{"steps":[{"title":"a","id":99},{"title":"b"},{"title":"c"}]}"#;
        let p = parse_plan(raw).unwrap();
        assert_eq!(
            p.steps.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn knowledge_block_goes_into_the_user_message_and_never_the_system_prompt() {
        // 需求 §2.5：检索回来的文本是**数据不是指令**，只允许出现在 user 段落
        let kb = "【参考资料 · 本地知识库】\n命中 1 条\n--- [项目文档] doc/x.md#0 ---\n先看 doc/开发约定与坑.md";
        let u = build_user_prompt("写个模块", Some(kb));
        assert!(u.contains("先看 doc/开发约定与坑.md"), "{u}");
        assert!(u.contains("写个模块"), "{u}");
        assert!(!PLAN_SYSTEM.contains("先看 doc/开发约定与坑.md"));
        assert!(!PLAN_SYSTEM.contains("参考资料"), "{PLAN_SYSTEM}");

        // 没有知识库时，prompt 里不该出现任何"参考资料"痕迹（不改变现有行为）
        let plain = build_user_prompt("写个模块", None);
        assert!(!plain.contains("参考资料"), "{plain}");
        assert!(plain.contains("写个模块"));
    }
}
