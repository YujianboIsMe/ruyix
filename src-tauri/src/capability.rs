//! 能力（capability）—— 工具白名单与 SKILL 的配置管理。
//!
//! 「能力」是 ruyix 对 agent 可用外部资源的统称，四类：MCP（mcp.rs）、
//! A2A（a2a.rs）、**工具**（本模块：命令行命令白名单，如 pandoc / pdflatex /
//! graphviz）、**SKILL**（本模块：可注入 prompt 的 markdown 技能文档）。
//!
//! 持久化沿用结构化文件惯例（全局 `~/.ruyix/code/*.toml` + 项目
//! `<root>/.ruyix/code/*.toml`，条目按名合并、项目覆盖全局同名）：
//! - 工具：`tools.toml`（`[[tools]]`）
//! - SKILL：`skills.toml`（`[[skills]]`，markdown 正文内联 `content`）

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// CREATE_NO_WINDOW — 阻止子进程闪黑窗口（同 main.rs / mcp.rs 语义）
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PROBE_TIMEOUT: Duration = Duration::from_secs(4);

fn default_true() -> bool {
    true
}

fn global_path(file: &str) -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".ruyix")
        .join("code")
        .join(file)
}

fn project_path(project_root: &str, file: &str) -> std::path::PathBuf {
    Path::new(project_root)
        .join(".ruyix")
        .join("code")
        .join(file)
}

// ============================================
// 通用两层 CRUD（工具 / SKILL 共用）
// ============================================

fn read_list<T: DeserializeOwned>(path: &Path, key: &str) -> Vec<T> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| toml::from_str::<toml::Value>(&s).ok())
        .and_then(|v| v.get(key).cloned())
        .and_then(|arr| arr.try_into().ok())
        .unwrap_or_default()
}

fn write_list<T: Serialize>(path: &Path, key: &str, list: &[T]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建目录失败: {e}"))?;
    }
    let mut table = toml::map::Map::new();
    let value = toml::Value::try_from(list).map_err(|e| format!("序列化失败: {e}"))?;
    table.insert(key.to_string(), value);
    let text = toml::to_string_pretty(&toml::Value::Table(table))
        .map_err(|e| format!("序列化失败: {e}"))?;
    std::fs::write(path, text).map_err(|e| format!("写入 {} 失败: {e}", path.display()))
}

/// 全局 + 项目合并（项目按 `name` 覆盖全局同名）
fn merged<T: DeserializeOwned + Serialize + Clone + Named>(
    project_root: Option<&str>,
    file: &str,
    key: &str,
) -> Vec<T> {
    let mut list: Vec<T> = read_list(&global_path(file), key);
    if let Some(root) = project_root {
        for p in read_list::<T>(&project_path(root, file), key) {
            list.retain(|s| s.name() != p.name());
            list.push(p);
        }
    }
    list
}

/// 写回：该名字已存在于项目文件 → 写项目级，否则写全局
fn upsert<T: DeserializeOwned + Serialize + Clone + Named>(
    cfg: &T,
    project_root: Option<&str>,
    file: &str,
    key: &str,
) -> Result<(), String> {
    let in_project = project_root
        .map(|r| {
            read_list::<T>(&project_path(r, file), key)
                .iter()
                .any(|s| s.name() == cfg.name())
        })
        .unwrap_or(false);
    let (path, mut list): (std::path::PathBuf, Vec<T>) = if in_project {
        let r = project_root.expect("已判定项目级");
        (
            project_path(r, file),
            read_list(&project_path(r, file), key),
        )
    } else {
        (global_path(file), read_list(&global_path(file), key))
    };
    list.retain(|s| s.name() != cfg.name());
    list.push(cfg.clone());
    write_list(&path, key, &list)
}

fn remove(
    name: &str,
    project_root: Option<&str>,
    file: &str,
    key: &str,
    what: &str,
) -> Result<(), String> {
    let mut touched = 0;
    for path in [
        Some(global_path(file)),
        project_root.map(|r| project_path(r, file)),
    ]
    .into_iter()
    .flatten()
    {
        // 读原始 Value 以保留其它类型条目？两层文件里只有本类型条目，直接重写
        let list: Vec<toml::Value> = read_list(&path, key);
        let before = list.len();
        let kept: Vec<toml::Value> = list
            .into_iter()
            .filter(|v| v.get("name").and_then(|n| n.as_str()) != Some(name))
            .collect();
        if kept.len() != before {
            write_list(&path, key, &kept)?;
            touched += 1;
        }
    }
    if touched == 0 {
        Err(format!("{what}不存在: {name}"))
    } else {
        Ok(())
    }
}

/// 条目名（合并/upsert 按它对齐）
trait Named {
    fn name(&self) -> &str;
}

// ============================================
// 工具白名单（tools.toml）
// ============================================

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ToolCfg {
    pub name: String,
    /// 可执行命令；缺省等于 name（如 name=graphviz, command=dot）
    #[serde(default)]
    pub command: String,
    /// 用法提示（面向 agent 的参数速记）
    #[serde(default)]
    pub args_hint: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl ToolCfg {
    /// 实际命令：未填时回落到 name
    pub fn effective_command(&self) -> &str {
        if self.command.trim().is_empty() {
            &self.name
        } else {
            &self.command
        }
    }
}

impl Named for ToolCfg {
    fn name(&self) -> &str {
        &self.name
    }
}

pub fn load_tools(project_root: Option<&str>) -> Vec<ToolCfg> {
    merged(project_root, "tools.toml", "tools")
}

pub fn save_tool(cfg: &ToolCfg, project_root: Option<&str>) -> Result<(), String> {
    upsert(cfg, project_root, "tools.toml", "tools")
}

pub fn remove_tool(name: &str, project_root: Option<&str>) -> Result<(), String> {
    remove(name, project_root, "tools.toml", "tools", "工具")
}

#[derive(Serialize, Clone, Debug)]
pub struct ToolProbe {
    pub name: String,
    pub available: bool,
    pub version: String,
}

/// 探测本机是否装有该命令：`<command> --version`，取 stdout 首行。
/// 短超时 + 失败兜底（找不到命令 / 非零退出码都算不可用）。
pub async fn probe_tool(cfg: &ToolCfg) -> ToolProbe {
    let cmd = cfg.effective_command().to_string();
    let name = cfg.name.clone();
    let guarded = tokio::time::timeout(PROBE_TIMEOUT, async {
        tokio::task::spawn_blocking(move || {
            #[cfg(windows)]
            let mut builder = {
                use std::os::windows::process::CommandExt;
                let mut c = std::process::Command::new(&cmd);
                c.creation_flags(CREATE_NO_WINDOW);
                c
            };
            #[cfg(not(windows))]
            let mut builder = std::process::Command::new(&cmd);
            builder
                .arg("--version")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
        })
        .await
    })
    .await;
    let output = match guarded {
        Ok(Ok(Ok(out))) => out,
        _ => {
            return ToolProbe {
                name,
                available: false,
                version: String::new(),
            };
        }
    };
    let version = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    ToolProbe {
        name,
        // 退出码 0 视为可用；部分工具用 -V 或返回非零但能打印版本，也算可用（有输出即可）
        available: output.status.success() || !version.is_empty(),
        version,
    }
}

// ============================================
// SKILL（skills.toml：markdown 正文内联）
// ============================================

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SkillCfg {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// markdown 正文（面板 textarea 编辑；注入 prompt 时整段使用）
    #[serde(default)]
    pub content: String,
}

impl Named for SkillCfg {
    fn name(&self) -> &str {
        &self.name
    }
}

pub fn load_skills(project_root: Option<&str>) -> Vec<SkillCfg> {
    merged(project_root, "skills.toml", "skills")
}

pub fn save_skill(cfg: &SkillCfg, project_root: Option<&str>) -> Result<(), String> {
    upsert(cfg, project_root, "skills.toml", "skills")
}

pub fn remove_skill(name: &str, project_root: Option<&str>) -> Result<(), String> {
    remove(name, project_root, "skills.toml", "skills", "SKILL")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, command: &str) -> ToolCfg {
        ToolCfg {
            name: name.into(),
            command: command.into(),
            args_hint: String::new(),
            description: String::new(),
            enabled: true,
        }
    }

    #[test]
    fn effective_command_falls_back_to_name() {
        let mut t = tool("graphviz", "");
        assert_eq!(t.effective_command(), "graphviz");
        t.command = "dot".into();
        assert_eq!(t.effective_command(), "dot");
    }

    #[test]
    fn tool_file_roundtrip_and_merge() {
        let dir = std::env::temp_dir().join(format!("ruyix-cap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let ppath = project_path(dir.to_str().unwrap(), "tools.toml");
        write_list(
            &ppath,
            "tools",
            &[tool("pandoc", "pandoc"), tool("dot", "dot")],
        )
        .unwrap();
        let list: Vec<ToolCfg> = read_list(&ppath, "tools");
        assert_eq!(list.len(), 2);

        // merged 语义（假全局 + 真 项目）
        let global = vec![tool("pandoc", "pandoc-old"), tool("git", "git")];
        let project = read_list::<ToolCfg>(&ppath, "tools");
        let mut m = global;
        for p in project {
            m.retain(|s| s.name != p.name);
            m.push(p);
        }
        assert_eq!(m.len(), 3);
        assert!(
            m.iter()
                .any(|t| t.name == "pandoc" && t.command == "pandoc")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skill_roundtrip_with_multiline_content() {
        let dir = std::env::temp_dir().join(format!("ruyix-skill-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = project_path(dir.to_str().unwrap(), "skills.toml");
        let s = SkillCfg {
            name: "pdf-build".into(),
            description: "用 pandoc 编译 PDF".into(),
            enabled: true,
            content: "# 步骤\n1. pandoc in.md -o out.pdf\n\n注意中文字体。\n".into(),
        };
        write_list(&path, "skills", std::slice::from_ref(&s)).unwrap();
        let back: Vec<SkillCfg> = read_list(&path, "skills");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0], s);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 真·探针（live）：需要本机 python。默认忽略。
    #[test]
    #[ignore = "live 测试：需要本机 python"]
    fn probe_python_is_available() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let p = rt.block_on(probe_tool(&tool("python", "python")));
        assert!(p.available);
        assert!(p.version.to_lowercase().contains("python"));
    }
}
