use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

/// 配置键前缀
pub const PREFIX: &str = "ruyix.code";

/// 运行目标
#[derive(Debug, Clone, Serialize)]
pub struct RunTarget {
    pub key: String,
    pub name: Option<String>,
    pub cmd: Option<String>,
    pub bind: Option<String>,
}

/// 知名清单文件定义（预置为可运行）
#[derive(Debug, Clone, Copy)]
pub struct ManifestDef {
    pub name: &'static str, // 文件名（小写）
    pub cmd: &'static str,  // 建议运行命令
}

pub const MANIFEST_FILES: &[ManifestDef] = &[
    ManifestDef {
        name: "cargo.toml",
        cmd: "cargo run",
    },
    ManifestDef {
        name: "package.json",
        cmd: "npm start",
    },
    ManifestDef {
        name: "makefile",
        cmd: "make",
    },
];

/// 根据路径返回匹配的清单定义
pub fn manifest_for_path(path: &std::path::Path) -> Option<&'static ManifestDef> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    MANIFEST_FILES.iter().find(|m| m.name == name)
}

// ============================================
// 配置作用域
// ============================================

#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    Global,
    Project,
    Runtime,
}

/// 配置表单的一行（配置菜单 → 配置标签）
#[derive(Debug, Clone, Serialize)]
pub struct ConfigEntryDump {
    /// section = `<section>.toml` 的文件名去后缀
    pub section: String,
    /// 相对 section 的键名，如 `api_key` / `llm.temperature`
    pub key: String,
    /// 完整配置键 `ruyix.code.<section>.<key>`
    pub full_key: String,
    /// 本作用域内的值（未设置 = 空串）
    pub value: String,
    /// 本作用域未设置时，回退链上取到的有效值
    pub inherited: Option<InheritedValue>,
}

/// 回退链上的有效值及其来源作用域
#[derive(Debug, Clone, Serialize)]
pub struct InheritedValue {
    /// 来源作用域：global / project
    pub scope: String,
    pub value: String,
}

/// 一个作用域 + 其回退链上的全部平铺配置项（配置表单的数据源）
#[derive(Debug, Clone, Serialize)]
pub struct ScopeEntriesDump {
    pub scope: String,
    /// 配置目录（runtime 为内存占位说明）
    pub dir: String,
    pub entries: Vec<ConfigEntryDump>,
}

/// 表单提交的一行；`value` 为空串 = 删除该键（回落到回退链）
#[derive(Debug, Clone, Deserialize)]
pub struct ConfigEntryInput {
    pub section: String,
    pub key: String,
    pub value: String,
}

/// 保存 / 应用的结果计数
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct ScopeSaveReport {
    /// 写入（新增或更新）的键数
    pub saved: usize,
    /// 删除的键数
    pub removed: usize,
    /// 刷新进运行时内存对象的键数（保存路径为 0）
    pub applied: usize,
}

impl Scope {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "global" | "g" => Some(Scope::Global),
            "project" | "p" => Some(Scope::Project),
            "runtime" | "r" => Some(Scope::Runtime),
            _ => None,
        }
    }

    /// 作用域名（配置表单 / 状态栏展示）
    pub fn name(&self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Project => "project",
            Scope::Runtime => "runtime",
        }
    }

    /// 回退链上优先级更低的 scope（runtime → project → global）。
    ///
    /// 与 `ai.rs` / `agent::config_bridge` 的读法一致：runtime 覆盖 project，
    /// project 覆盖 global。
    fn fallbacks(&self) -> &'static [Scope] {
        match self {
            Scope::Global => &[],
            Scope::Project => &[Scope::Global],
            Scope::Runtime => &[Scope::Project, Scope::Global],
        }
    }
}

// ============================================
// Projects 配置
// ============================================

/// 项目语言可选值（下拉列表顺序）
pub const PROJECT_LANGS: &[&str] = &[
    "unknown", "mix", "java", "c", "python", "rust", "web", "golang", "document", "kotlin",
];

/// 默认项目语言
pub fn default_lang() -> String {
    "unknown".to_string()
}

/// 从路径提取文件夹名作为项目名
fn name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
        .to_string()
}

/// 项目条目：名称 + 路径 + 语言（未来可扩展更多属性）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProjectEntry {
    pub name: String,
    pub path: String,
    #[serde(default = "default_lang")]
    pub lang: String,
}

/// 项目列表条目：兼容旧版纯路径字符串
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum ProjectListEntry {
    Legacy(String),
    Entry(ProjectEntry),
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct ProjectsConfig {
    pub current: Option<String>,
    pub list: Vec<ProjectEntry>,
}

impl<'de> Deserialize<'de> for ProjectsConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Raw {
            current: Option<String>,
            #[serde(default)]
            list: Vec<ProjectListEntry>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let list = raw
            .list
            .into_iter()
            .map(|e| match e {
                // 旧版：纯路径字符串 → 补上默认 name/lang
                ProjectListEntry::Legacy(path) => ProjectEntry {
                    name: name_from_path(&path),
                    path,
                    lang: default_lang(),
                },
                ProjectListEntry::Entry(entry) => entry,
            })
            .collect();
        Ok(ProjectsConfig {
            current: raw.current,
            list,
        })
    }
}

// ============================================
// ConfigManager
// ============================================

#[derive(Debug)]
pub struct ConfigManager {
    global_dir: PathBuf,
    /// 运行时配置（不持久化）
    runtime: HashMap<String, String>,
}

impl ConfigManager {
    /// `global_dir` 由调用方注入（宿主 = `paths::current().global_dir()`）。
    /// 这里**不许再自己找家** —— 那正是 v1.0.0 P1 收敛掉的东西。
    pub fn new(global_dir: PathBuf) -> Self {
        Self::new_with_dir(global_dir)
    }

    /// 使用指定配置目录（测试用）
    pub fn new_with_dir(global_dir: PathBuf) -> Self {
        let _ = fs::create_dir_all(&global_dir);
        Self {
            global_dir,
            runtime: HashMap::new(),
        }
    }

    // ============================================
    // 通用配置读写
    // ============================================

    /// 读取配置值。key 格式: ruyix.code.<section>.<path>
    /// scope=global → ~/.ruyix/code/<section>.toml
    /// scope=project → <project_root>/.ruyix/code/<section>.toml
    /// scope=runtime → 内存
    pub fn config_read(
        &self,
        scope: &Scope,
        key: &str,
        project_root: Option<&str>,
    ) -> Result<Option<String>, String> {
        let (section, sub_key) = self.split_key(key)?;

        match scope {
            Scope::Runtime => Ok(self.runtime.get(key).cloned()),
            Scope::Global | Scope::Project => {
                let dir = match scope {
                    Scope::Global => self.global_dir.clone(),
                    Scope::Project => self.resolve_project_dir(project_root)?,
                    _ => unreachable!(),
                };
                let path = dir.join(format!("{}.toml", section));
                let map = self.read_toml_file(&path, &section)?;
                Ok(map.get(&sub_key).cloned())
            }
        }
    }

    /// 写入配置值
    pub fn config_write(
        &mut self,
        scope: &Scope,
        key: &str,
        value: &str,
        project_root: Option<&str>,
    ) -> Result<(), String> {
        if *scope == Scope::Global && self.is_run_target_key(key) {
            return Err("运行目标不能保存为全局".to_string());
        }

        let (section, sub_key) = self.split_key(key)?;

        match scope {
            Scope::Runtime => {
                self.runtime.insert(key.to_string(), value.to_string());
                Ok(())
            }
            Scope::Global | Scope::Project => {
                let dir = match scope {
                    Scope::Global => self.global_dir.clone(),
                    Scope::Project => self.resolve_project_dir(project_root)?,
                    _ => unreachable!(),
                };
                fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败: {}", e))?;

                let path = dir.join(format!("{}.toml", section));
                let mut map = self.read_toml_file(&path, &section)?;
                map.insert(sub_key, value.to_string());
                self.write_toml_file(&path, &section, &map)?;
                Ok(())
            }
        }
    }

    /// 删除配置值
    pub fn config_delete(
        &mut self,
        scope: &Scope,
        key: &str,
        project_root: Option<&str>,
    ) -> Result<(), String> {
        let (section, sub_key) = self.split_key(key)?;

        match scope {
            Scope::Runtime => {
                self.runtime.remove(key);
                Ok(())
            }
            Scope::Global | Scope::Project => {
                let dir = match scope {
                    Scope::Global => self.global_dir.clone(),
                    Scope::Project => self.resolve_project_dir(project_root)?,
                    _ => unreachable!(),
                };
                let path = dir.join(format!("{}.toml", section));
                let mut map = self.read_toml_file(&path, &section)?;
                if map.remove(&sub_key).is_none() {
                    return Err(format!("配置键不存在: {}", key));
                }
                self.write_toml_file(&path, &section, &map)?;
                Ok(())
            }
        }
    }

    // ============================================
    // Projects 配置 (保持兼容)
    // ============================================

    fn projects_path(&self) -> PathBuf {
        self.global_dir.join("projects.toml")
    }

    // ============================================
    // 配置表单（配置菜单）：扫描 / 保存 / 应用
    // ============================================

    /// 结构化文件：由专门功能管理（各自的读写器维护数组/嵌套结构），
    /// 平铺成表单会破坏格式 —— 扫描时排除，保存时拒绝。
    const SCOPE_EXCLUDED_FILES: [&str; 6] = [
        "projects.toml", // 项目菜单
        "execute.toml",  // 运行目标面板
        "mcp.toml",      // 能力 → MCP 面板
        "a2a.toml",      // 能力 → A2A 面板
        "tools.toml",    // 能力 → 工具面板
        "skills.toml",   // 能力 → SKILL 面板
    ];

    /// 一个作用域里已有的平铺配置项：(section, key, value)。
    /// - global/project：目录下各 `<section>.toml`（结构化文件排除），空 section 跳过
    /// - runtime：内存键按首个点号切成 section + key
    fn scope_pairs(
        &self,
        scope: &Scope,
        project_root: Option<&str>,
    ) -> Result<Vec<(String, String, String)>, String> {
        let mut out: Vec<(String, String, String)> = Vec::new();

        if *scope == Scope::Runtime {
            for (full, value) in &self.runtime {
                let rest = full
                    .strip_prefix(PREFIX)
                    .and_then(|s| s.strip_prefix('.'))
                    .unwrap_or(full);
                if let Some((section, key)) = rest.split_once('.') {
                    out.push((section.to_string(), key.to_string(), value.clone()));
                }
            }
            out.sort();
            return Ok(out);
        }

        let dir = self.scope_dir_for(scope, project_root)?;
        let mut names: Vec<String> = Vec::new();
        if let Ok(entries) = fs::read_dir(&dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.ends_with(".toml") && !Self::SCOPE_EXCLUDED_FILES.contains(&name.as_str()) {
                    names.push(name);
                }
            }
        }
        names.sort();
        for name in names {
            let section = name.trim_end_matches(".toml").to_string();
            for (key, value) in self.read_toml_file(&dir.join(&name), &section)? {
                out.push((section.clone(), key, value));
            }
        }
        out.sort();
        Ok(out)
    }

    /// 扫描一个作用域 → 配置表单的数据源：
    /// 本作用域的键 ∪ 回退链上的键（`runtime → project → global`），
    /// 每个键带自己的值 + 未设置时的继承值（含来源作用域），供表单展示"当前生效值"。
    pub fn scan_scope_entries(
        &self,
        scope: &Scope,
        project_root: Option<&str>,
    ) -> Result<ScopeEntriesDump, String> {
        let own: HashMap<(String, String), String> = self
            .scope_pairs(scope, project_root)?
            .into_iter()
            .map(|(s, k, v)| ((s, k), v))
            .collect();

        let mut inherited: HashMap<(String, String), InheritedValue> = HashMap::new();
        for fb in scope.fallbacks() {
            // 无项目时项目作用域不存在，跳过（同 ai.rs / config_bridge 的读法）
            if *fb == Scope::Project && project_root.is_none() {
                continue;
            }
            // 回退链按优先级从高到低遍历，先到先得
            for (section, key, value) in self.scope_pairs(fb, project_root)? {
                let id = (section, key);
                if own.contains_key(&id) || inherited.contains_key(&id) {
                    continue;
                }
                inherited.insert(
                    id,
                    InheritedValue {
                        scope: fb.name().to_string(),
                        value,
                    },
                );
            }
        }

        let mut ids: Vec<(String, String)> = own.keys().cloned().collect();
        ids.extend(inherited.keys().cloned());
        ids.sort();
        ids.dedup();

        let entries = ids
            .into_iter()
            .map(|(section, key)| ConfigEntryDump {
                full_key: format!("{}.{}.{}", PREFIX, section, key),
                value: own
                    .get(&(section.clone(), key.clone()))
                    .cloned()
                    .unwrap_or_default(),
                inherited: inherited.get(&(section.clone(), key.clone())).cloned(),
                section,
                key,
            })
            .collect();

        Ok(ScopeEntriesDump {
            scope: scope.name().to_string(),
            dir: self.scope_dir_label(scope, project_root)?,
            entries,
        })
    }

    /// 保存配置表单（增量语义）：
    /// - 只写提交上来的行，未提交的键不动
    /// - `value` 为空串 = 删除该键（回落到回退链）
    /// - global/project 落盘 `<section>.toml`；runtime 改内存对象
    pub fn save_scope_entries(
        &mut self,
        scope: &Scope,
        entries: &[ConfigEntryInput],
        project_root: Option<&str>,
    ) -> Result<ScopeSaveReport, String> {
        let mut by_section: BTreeMap<String, Vec<&ConfigEntryInput>> = BTreeMap::new();
        for e in entries {
            let section = e.section.trim();
            if section.is_empty()
                || section.contains('.')
                || section.contains('/')
                || section.contains('\\')
            {
                return Err(format!("非法 section: {}", e.section));
            }
            if Self::SCOPE_EXCLUDED_FILES.contains(&format!("{section}.toml").as_str()) {
                return Err(format!("[{section}] 由专门功能管理，不能通过配置表单写入"));
            }
            // 运行目标不能保存为全局（同 config_write 的规则）
            if *scope == Scope::Global && section == "run" {
                return Err(format!("运行目标不能保存为全局: {}", e.key));
            }
            by_section.entry(section.to_string()).or_default().push(e);
        }

        let mut report = ScopeSaveReport::default();
        for (section, list) in by_section {
            if *scope == Scope::Runtime {
                for e in list {
                    let full = format!("{}.{}.{}", PREFIX, section, e.key);
                    if e.value.trim().is_empty() {
                        if self.runtime.remove(&full).is_some() {
                            report.removed += 1;
                        }
                    } else {
                        self.runtime.insert(full, e.value.clone());
                        report.saved += 1;
                    }
                }
                continue;
            }
            let dir = self.scope_dir_for(scope, project_root)?;
            fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败: {e}"))?;
            let (saved, removed) = self.write_section_entries(&dir, &section, &list)?;
            report.saved += saved;
            report.removed += removed;
        }
        Ok(report)
    }

    /// 应用配置表单 = 保存 + 刷新 IDE 运行时内存里的配置对象。
    ///
    /// 运行时对象在查找链上优先级最高（runtime → project → global），所以应用之后
    /// 这些值立刻对正在运行的 IDE 生效；它不落盘，重启即失效。
    /// 只把非空值刷进运行时对象 —— 清空表单不做反向删除，避免误伤显式设过的运行时键
    /// （要清运行时覆盖，到「运行」作用域的表单里清）。
    pub fn apply_scope_entries(
        &mut self,
        scope: &Scope,
        entries: &[ConfigEntryInput],
        project_root: Option<&str>,
    ) -> Result<ScopeSaveReport, String> {
        let mut report = self.save_scope_entries(scope, entries, project_root)?;

        if *scope == Scope::Runtime {
            // 运行时作用域本身就是在内存对象上写，保存即生效
            report.applied = report.saved;
            return Ok(report);
        }

        let mut applied = 0usize;
        for e in entries {
            // 运行目标不进运行时对象（由运行目标面板 / run.toml 管理）
            if e.section == "run" || e.value.trim().is_empty() {
                continue;
            }
            let full = format!("{}.{}.{}", PREFIX, e.section, e.key);
            self.runtime.insert(full, e.value.clone());
            applied += 1;
        }
        report.applied = applied;
        Ok(report)
    }

    /// 增量写一个 `<section>.toml`：只动提交的键，其余既有内容（含非标量）原样保留。
    /// 表被清空则删掉文件，不留空 section。返回 (写入, 删除)。
    fn write_section_entries(
        &self,
        dir: &std::path::Path,
        section: &str,
        entries: &[&ConfigEntryInput],
    ) -> Result<(usize, usize), String> {
        let path = dir.join(format!("{section}.toml"));
        let mut root = match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<toml::Value>(&text) {
                Ok(toml::Value::Table(t)) => t,
                Ok(_) => return Err(format!("{} 顶层不是表", path.display())),
                Err(e) => return Err(format!("TOML 解析错误 ({}): {e}", path.display())),
            },
            Err(_) => toml::map::Map::new(),
        };

        // 取 [section] 表；兼容旧格式（文件里是根级平铺键、且没有任何子表）
        let section_table = match root.get(section) {
            Some(toml::Value::Table(t)) => Some(t.clone()),
            _ => None,
        };
        let legacy_flat =
            section_table.is_none() && !root.is_empty() && root.values().all(|v| !v.is_table());
        let mut table = section_table.unwrap_or_else(|| {
            if legacy_flat {
                root.clone()
            } else {
                toml::map::Map::new()
            }
        });

        let mut saved = 0usize;
        let mut removed = 0usize;
        for e in entries {
            if e.value.trim().is_empty() {
                if table.remove(&e.key).is_some() {
                    removed += 1;
                }
            } else {
                table.insert(e.key.clone(), toml::Value::String(e.value.clone()));
                saved += 1;
            }
        }

        if table.is_empty() {
            if path.exists() {
                fs::remove_file(&path).map_err(|e| format!("删除配置失败: {e}"))?;
            }
            return Ok((saved, removed));
        }

        if legacy_flat {
            root.clear();
        }
        root.insert(section.to_string(), toml::Value::Table(table));
        let text = toml::to_string_pretty(&toml::Value::Table(root)).map_err(|e| e.to_string())?;
        fs::write(&path, text).map_err(|e| format!("写入配置失败: {e}"))?;
        Ok((saved, removed))
    }

    /// 表单头部展示的存储位置（runtime 无目录）
    fn scope_dir_label(&self, scope: &Scope, project_root: Option<&str>) -> Result<String, String> {
        match scope {
            Scope::Runtime => Ok("(内存 · 不落盘)".to_string()),
            _ => Ok(self
                .scope_dir_for(scope, project_root)?
                .to_string_lossy()
                .to_string()),
        }
    }

    fn scope_dir_for(&self, scope: &Scope, project_root: Option<&str>) -> Result<PathBuf, String> {
        match scope {
            Scope::Global => Ok(self.global_dir.clone()),
            Scope::Project => self.resolve_project_dir(project_root),
            Scope::Runtime => Err("运行时配置在内存中".to_string()),
        }
    }

    pub fn load_projects(&self) -> ProjectsConfig {
        let path = self.projects_path();
        match fs::read_to_string(&path) {
            Ok(s) => {
                #[derive(Deserialize)]
                struct File {
                    projects: ProjectsConfig,
                }
                toml::from_str::<File>(&s)
                    .map(|f| f.projects)
                    .unwrap_or_default()
            }
            Err(_) => ProjectsConfig::default(),
        }
    }

    pub fn save_projects(&self, cfg: &ProjectsConfig) -> Result<(), String> {
        #[derive(Serialize)]
        struct File {
            projects: ProjectsConfig,
        }
        let file = File {
            projects: cfg.clone(),
        };
        let toml_str = toml::to_string_pretty(&file).map_err(|e| e.to_string())?;
        fs::write(self.projects_path(), toml_str).map_err(|e| format!("写入配置失败: {}", e))
    }

    /// 记录当前项目。返回该条目保存的语言（已有条目保留原语言，新条目默认 unknown）。
    pub fn set_current_project(
        &self,
        project_path: &str,
        project_name: &str,
    ) -> Result<String, String> {
        let mut cfg = self.load_projects();
        // 保留已有条目的语言，仅刷新名称
        let lang = cfg
            .list
            .iter()
            .find(|e| e.path == project_path)
            .map(|e| e.lang.clone())
            .unwrap_or_else(default_lang);
        cfg.list.retain(|e| e.path != project_path);
        cfg.list.push(ProjectEntry {
            name: project_name.to_string(),
            path: project_path.to_string(),
            lang: lang.clone(),
        });
        cfg.current = Some(project_path.to_string());
        self.save_projects(&cfg)?;
        Ok(lang)
    }

    /// 设置项目语言
    pub fn set_project_lang(&self, project_path: &str, lang: &str) -> Result<(), String> {
        if !PROJECT_LANGS.contains(&lang) {
            return Err(format!("未知语言: {}", lang));
        }
        let mut cfg = self.load_projects();
        let entry = cfg
            .list
            .iter_mut()
            .find(|e| e.path == project_path)
            .ok_or_else(|| format!("项目不在列表中: {}", project_path))?;
        entry.lang = lang.to_string();
        self.save_projects(&cfg)
    }

    /// 更新项目名称与语言（路径不可修改）
    pub fn update_project(&self, path: &str, name: &str, lang: &str) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("项目名称不能为空".to_string());
        }
        if !PROJECT_LANGS.contains(&lang) {
            return Err(format!("未知语言: {}", lang));
        }
        let mut cfg = self.load_projects();
        let entry = cfg
            .list
            .iter_mut()
            .find(|e| e.path == path)
            .ok_or_else(|| format!("项目不在列表中: {}", path))?;
        entry.name = name.trim().to_string();
        entry.lang = lang.to_string();
        self.save_projects(&cfg)
    }

    /// 从项目列表删除条目（不删除项目文件夹）。若删除的是当前项目，同时清空 current。
    pub fn delete_project(&self, path: &str) -> Result<(), String> {
        let mut cfg = self.load_projects();
        let before = cfg.list.len();
        cfg.list.retain(|e| e.path != path);
        if cfg.list.len() == before {
            return Err(format!("项目不在列表中: {}", path));
        }
        if cfg.current.as_deref() == Some(path) {
            cfg.current = None;
        }
        self.save_projects(&cfg)
    }

    // ============================================
    // 运行目标
    // ============================================

    /// **具名目标配置的扫描器**（`run.toml` / `term.toml` 共用这一份）。
    ///
    /// 为什么抽出来：终端目标与运行目标是**同一套形状** —— 面板上增删改、落成
    /// `<key>.cmd` / `<key>.name`（运行目标多一个 `.bind`），差别只在文件名。复制一份 40 行的
    /// 扫描器，改一处就得记得改两处，而漏改的那次不会有任何测试失败、只会在某个面板上表现为
    /// "加了不显示"。
    fn scan_target_file(
        &self,
        file: &str,
        section: &str,
        project_root: Option<&str>,
    ) -> Result<Vec<RunTarget>, String> {
        let dir = self.resolve_project_dir(project_root)?;
        let path = dir.join(file);
        let map = self.read_toml_file(&path, section)?;

        // 按 key 分组：build.cmd → build, build.name → build, build.bind → build
        let mut groups: HashMap<String, Option<String>> = HashMap::new();
        let mut names: HashMap<String, Option<String>> = HashMap::new();
        let mut binds: HashMap<String, Option<String>> = HashMap::new();

        for (k, v) in &map {
            if let Some(rest) = k.strip_suffix(".cmd") {
                groups.entry(rest.to_string()).or_insert(None);
            } else if let Some(rest) = k.strip_suffix(".name") {
                names.insert(rest.to_string(), Some(v.clone()));
            } else if let Some(rest) = k.strip_suffix(".bind") {
                binds.insert(rest.to_string(), Some(v.clone()));
            }
        }

        // 合并
        let mut targets: Vec<RunTarget> = Vec::new();
        for key in groups.keys() {
            let cmd = map.get(&format!("{}.cmd", key)).cloned();
            let name = names.get(key).cloned().flatten();
            let bind = binds.get(key).cloned().flatten();
            targets.push(RunTarget {
                key: key.clone(),
                name,
                cmd,
                bind,
            });
        }
        // 按 key 排序
        targets.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(targets)
    }

    /// 加载项目运行目标（`run.toml`）。
    pub fn load_run_targets(&self, project_root: Option<&str>) -> Result<Vec<RunTarget>, String> {
        self.scan_target_file("run.toml", "run", project_root)
    }

    /// 加载项目的**终端目标**（`term.toml`）：导航区「终端资源」里那些可点的终端。
    ///
    /// 与运行目标同一个形状不是巧合：两者都是"给我起一条命令"，差别只在起在哪 ——
    /// 运行目标一次性跑完，终端是**常驻的交互式会话**（PTY + xterm）。
    pub fn load_term_targets(&self, project_root: Option<&str>) -> Result<Vec<RunTarget>, String> {
        self.scan_target_file("term.toml", "term", project_root)
    }

    // ============================================
    // execute.toml — 自学习：记录哪些后缀可以运行
    // ============================================

    /// 加载 execute.toml，返回 HashMap<键, can_run>。
    /// 键可以是扩展名（如 "py"）或文件名（如 "Cargo.toml"）。
    /// 文件格式: [py]\ncan_run = true
    pub fn load_execute_map(&self) -> HashMap<String, bool> {
        let path = self.global_dir.join("execute.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return HashMap::new();
        };
        let Ok(root) = content.parse::<toml::Value>() else {
            return HashMap::new();
        };
        let mut map = HashMap::new();
        if let toml::Value::Table(t) = root {
            for (ext, v) in t {
                if let toml::Value::Table(inner) = v
                    && let Some(toml::Value::Boolean(can_run)) = inner.get("can_run")
                {
                    map.insert(ext, *can_run);
                }
            }
        }
        map
    }

    /// 写入一条 execute 记录（key 可以是扩展名如 "py"，或文件名如 "Cargo.toml"）
    pub fn save_execute_entry(&self, key: &str, can_run: bool) -> Result<(), String> {
        let mut map = self.load_execute_map();
        map.insert(key.to_string(), can_run);
        let mut root = toml::map::Map::new();
        for (key, can_run) in &map {
            let mut inner = toml::map::Map::new();
            inner.insert("can_run".to_string(), toml::Value::Boolean(*can_run));
            root.insert(key.clone(), toml::Value::Table(inner));
        }
        let toml_str = toml::to_string_pretty(&root).map_err(|e| e.to_string())?;
        let path = self.global_dir.join("execute.toml");
        std::fs::write(&path, toml_str).map_err(|e| format!("写入 execute.toml 失败: {}", e))
    }

    // ============================================
    // learn.lua — 自学习脚本
    // ============================================

    /// 加载项目的 learn.lua，文件不存在返回空字符串
    pub fn load_lua_script(&self, project_root: &str) -> Result<String, String> {
        let path = crate::paths::current()
            .project_dir(project_root)
            .join("learn.lua");
        if !path.exists() {
            return Ok(String::new());
        }
        std::fs::read_to_string(&path).map_err(|e| format!("读取 learn.lua 失败: {}", e))
    }

    /// 追加 Lua 代码到 learn.lua
    pub fn append_lua_script(&self, project_root: &str, lua_code: &str) -> Result<(), String> {
        let dir = crate::paths::current().project_dir(project_root);
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败: {}", e))?;
        let path = dir.join("learn.lua");
        let entry = format!("\n{}\n", lua_code.trim());
        let mut existing = if path.exists() {
            std::fs::read_to_string(&path).unwrap_or_default()
        } else {
            String::from("-- learn.lua — 自动生成，请勿手动编辑\n")
        };
        existing.push_str(&entry);
        std::fs::write(&path, existing).map_err(|e| format!("写入 learn.lua 失败: {}", e))
    }

    // ============================================
    // 内部辅助
    // ============================================

    /// 拆分 key 为 (section, sub_key)
    /// "ruyix.code.run.target0.cmd" → ("run", "target0.cmd")
    fn split_key(&self, key: &str) -> Result<(String, String), String> {
        let rest = key
            .strip_prefix(PREFIX)
            .and_then(|s| s.strip_prefix('.'))
            .unwrap_or(key);

        let dot_pos = rest.find('.').ok_or_else(|| {
            format!(
                "配置键格式错误 (需为 {0}.<section>.<key>): {1}",
                PREFIX, key
            )
        })?;

        let section = rest[..dot_pos].to_string();
        let sub_key = rest[dot_pos + 1..].to_string();

        if section.is_empty() || sub_key.is_empty() {
            return Err(format!("配置键格式错误: {}", key));
        }

        Ok((section, sub_key))
    }

    /// 判断是否为运行目标配置键
    fn is_run_target_key(&self, key: &str) -> bool {
        if let Ok((section, _)) = self.split_key(key) {
            section == "run"
        } else {
            false
        }
    }

    fn resolve_project_dir(&self, project_root: Option<&str>) -> Result<PathBuf, String> {
        match project_root {
            // 项目作用域配置 = 该项目的状态桶（`<便携根>/projects/<key>/`，与项目数据同一个桶）
            Some(root) if !root.is_empty() => Ok(crate::paths::current().project_dir(root)),
            Some(_) => Err("项目路径为空字符串".to_string()),
            None => Err("未打开项目，无法使用项目配置 (-p)".to_string()),
        }
    }

    /// 读取 TOML 文件，从 `[section]` 中提取 key-value。
    /// 兼容旧格式：如果没有 `[section]`，则读取根级别的 key。
    fn read_toml_file(
        &self,
        path: &PathBuf,
        section: &str,
    ) -> Result<HashMap<String, String>, String> {
        let content = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Ok(HashMap::new()),
        };

        let value: toml::Value =
            toml::from_str(&content).map_err(|e| format!("TOML 解析错误: {}", e))?;

        let mut map = HashMap::new();
        if let toml::Value::Table(root) = value {
            // 优先读取 `[section]` 表
            if let Some(toml::Value::Table(section_table)) = root.get(section) {
                for (k, v) in section_table {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), s.to_string());
                    } else {
                        map.insert(k.clone(), v.to_string());
                    }
                }
            } else {
                // 兼容旧格式：根级别扁平 key
                for (k, v) in &root {
                    if let Some(s) = v.as_str() {
                        map.insert(k.clone(), s.to_string());
                    } else {
                        map.insert(k.clone(), v.to_string());
                    }
                }
            }
        }
        Ok(map)
    }

    /// 写入 TOML 文件，key-value 放入 `[section]` 表
    fn write_toml_file(
        &self,
        path: &PathBuf,
        section: &str,
        map: &HashMap<String, String>,
    ) -> Result<(), String> {
        // 构建 [section] 内的表
        let mut section_table = toml::map::Map::new();
        for (k, v) in map {
            section_table.insert(k.clone(), toml::Value::String(v.clone()));
        }

        // 外层包裹 [section]
        let mut root = toml::map::Map::new();
        root.insert(section.to_string(), toml::Value::Table(section_table));

        let toml_str = toml::to_string_pretty(&root).map_err(|e| e.to_string())?;
        fs::write(path, toml_str).map_err(|e| format!("写入配置失败: {}", e))
    }
}

// ============================================
// 测试
// ============================================

#[cfg(test)]
mod tests {
    use super::*;

    /// 唯一临时目录（测试内自清理不追求严格，进程退出后由系统兜底）
    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join("ruyix-config-test")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// 测试用项目路径：name_from_path 依赖平台路径分隔符，
    /// Windows 用反斜杠盘符路径，Unix 用斜杠路径，两个平台各测真实场景
    #[cfg(windows)]
    const PATH_A: &str = r"C:\foo\bar";
    #[cfg(windows)]
    const PATH_B: &str = r"C:\baz\qux";
    #[cfg(not(windows))]
    const PATH_A: &str = "/foo/bar";
    #[cfg(not(windows))]
    const PATH_B: &str = "/baz/qux";

    #[derive(Serialize, Deserialize)]
    struct File {
        projects: ProjectsConfig,
    }

    // ===== 配置表单（配置菜单）=====

    /// 表单行的构造助手
    fn entry(section: &str, key: &str, value: &str) -> ConfigEntryInput {
        ConfigEntryInput {
            section: section.to_string(),
            key: key.to_string(),
            value: value.to_string(),
        }
    }

    /// 扫描：本作用域 ∪ 回退链；结构化文件必须被排除
    #[test]
    fn scan_unions_fallback_chain_and_excludes_structured_files() {
        let dir = temp_dir("scan");
        let mut mgr = ConfigManager::new_with_dir(dir.clone());
        mgr.config_write(&Scope::Global, "ruyix.code.ai.api_key", "sk-1", None)
            .unwrap();
        mgr.config_write(&Scope::Global, "ruyix.code.ui.lang", "zh-CN", None)
            .unwrap();
        // 结构化文件：各自的读写器维护数组结构，平铺成表单会破坏格式
        fs::write(dir.join("projects.toml"), "[projects]\ncurrent = '/x'\n").unwrap();
        fs::write(dir.join("execute.toml"), "[exec]\ncmd = 'ls'\n").unwrap();
        fs::write(dir.join("mcp.toml"), "[[servers]]\nname = 'fs'\n").unwrap();

        let dump = mgr.scan_scope_entries(&Scope::Global, None).unwrap();
        let keys: Vec<&str> = dump.entries.iter().map(|e| e.full_key.as_str()).collect();
        assert_eq!(keys, vec!["ruyix.code.ai.api_key", "ruyix.code.ui.lang"]);
        assert!(dump.entries.iter().all(|e| e.inherited.is_none()));
        assert_eq!(dump.scope, "global");
        assert_eq!(dump.dir, dir.to_string_lossy());
    }

    /// 扫描 project 作用域：项目值优先，未设置的行带全局继承值
    #[test]
    fn scan_project_marks_inherited_from_global() {
        let dir = temp_dir("scan-proj");
        let mut mgr = ConfigManager::new_with_dir(dir.clone());
        mgr.config_write(&Scope::Global, "ruyix.code.ai.model", "global-model", None)
            .unwrap();
        mgr.config_write(&Scope::Global, "ruyix.code.ai.api_key", "sk-g", None)
            .unwrap();

        let proj = temp_dir("scan-proj-root");
        let root = proj.to_string_lossy().to_string();
        mgr.config_write(
            &Scope::Project,
            "ruyix.code.ai.model",
            "proj-model",
            Some(&root),
        )
        .unwrap();

        let dump = mgr
            .scan_scope_entries(&Scope::Project, Some(&root))
            .unwrap();
        let model = dump
            .entries
            .iter()
            .find(|e| e.key == "model")
            .expect("model 行缺失");
        assert_eq!(model.value, "proj-model");
        assert!(model.inherited.is_none(), "本作用域已设置就不该有继承值");

        let api_key = dump
            .entries
            .iter()
            .find(|e| e.key == "api_key")
            .expect("api_key 行缺失");
        assert_eq!(api_key.value, "");
        let inh = api_key.inherited.as_ref().expect("应有全局继承值");
        assert_eq!(inh.scope, "global");
        assert_eq!(inh.value, "sk-g");
    }

    /// 保存：增量写 + 空值删除 + 拒绝保留 section
    #[test]
    fn save_is_incremental_and_removes_on_empty_value() {
        let dir = temp_dir("save");
        let mut mgr = ConfigManager::new_with_dir(dir.clone());
        let report = mgr
            .save_scope_entries(
                &Scope::Global,
                &[
                    entry("ai", "api_key", "sk-2"),
                    entry("ai", "model", "deepseek-chat"),
                    entry("ui", "lang", "en"),
                ],
                None,
            )
            .unwrap();
        assert_eq!((report.saved, report.removed, report.applied), (3, 0, 0));

        // 读回：落盘格式与 config_read 兼容
        assert_eq!(
            mgr.config_read(&Scope::Global, "ruyix.code.ai.api_key", None)
                .unwrap()
                .as_deref(),
            Some("sk-2")
        );

        // 增量：只提交 model 的更新，未提交的 api_key 不动
        let report = mgr
            .save_scope_entries(&Scope::Global, &[entry("ai", "model", "deepseek-v3")], None)
            .unwrap();
        assert_eq!((report.saved, report.removed), (1, 0));
        assert_eq!(
            mgr.config_read(&Scope::Global, "ruyix.code.ai.api_key", None)
                .unwrap()
                .as_deref(),
            Some("sk-2")
        );

        // 空值 = 删除该键；section 清空后不留空文件
        let report = mgr
            .save_scope_entries(
                &Scope::Global,
                &[entry("ai", "api_key", ""), entry("ai", "model", "")],
                None,
            )
            .unwrap();
        assert_eq!((report.saved, report.removed), (0, 2));
        assert!(!dir.join("ai.toml").exists());
        assert!(dir.join("ui.toml").exists());

        // 保留 section 直接拒绝
        assert!(
            mgr.save_scope_entries(&Scope::Global, &[entry("projects", "current", "/x")], None)
                .is_err()
        );
        // 运行目标不能保存为全局
        assert!(
            mgr.save_scope_entries(&Scope::Global, &[entry("run", "target0.cmd", "ls")], None)
                .is_err()
        );
    }

    /// 保存非标量既存内容时原样保留（不把数组/嵌套表拍平）
    #[test]
    fn save_preserves_non_scalar_sections() {
        let dir = temp_dir("preserve");
        let mut mgr = ConfigManager::new_with_dir(dir.clone());
        fs::write(
            dir.join("custom.toml"),
            "[custom]\nanswer = 42\nnested = { a = 1 }\n[self]\nname = 'x'\n",
        )
        .unwrap();

        mgr.save_scope_entries(&Scope::Global, &[entry("custom", "name", "y")], None)
            .unwrap();
        let text = fs::read_to_string(dir.join("custom.toml")).unwrap();
        assert!(text.contains("answer = 42"), "数字标量应保留原类型: {text}");
        assert!(text.contains("nested"), "嵌套表应保留: {text}");
        assert!(
            text.contains("name = \"y\"") || text.contains("name = 'y'"),
            "{text}"
        );
        assert!(text.contains("[self]"), "其它 section 应保留: {text}");
    }

    /// 运行时作用域：写内存对象，自带的键在表单里显示为普通行（无继承）
    #[test]
    fn runtime_scope_reads_and_writes_memory_object() {
        let mut mgr = ConfigManager::new_with_dir(temp_dir("rt"));
        mgr.config_write(&Scope::Runtime, "ruyix.code.ai.api_key", "old", None)
            .unwrap();

        let dump = mgr.scan_scope_entries(&Scope::Runtime, None).unwrap();
        assert_eq!(dump.dir, "(内存 · 不落盘)");
        assert_eq!(dump.entries.len(), 1);

        let report = mgr
            .save_scope_entries(
                &Scope::Runtime,
                &[entry("ai", "model", "deepseek-chat")],
                None,
            )
            .unwrap();
        assert_eq!((report.saved, report.removed), (1, 0));
        assert_eq!(
            mgr.config_read(&Scope::Runtime, "ruyix.code.ai.model", None)
                .unwrap(),
            Some("deepseek-chat".into())
        );
        // 增量语义：未提交的 ai.api_key 还在
        assert_eq!(
            mgr.config_read(&Scope::Runtime, "ruyix.code.ai.api_key", None)
                .unwrap(),
            Some("old".into())
        );
    }

    /// 应用 = 保存 + 把非空值刷进运行时内存对象（全局/项目作用域才有这一步）
    #[test]
    fn apply_refreshes_runtime_object() {
        let mut mgr = ConfigManager::new_with_dir(temp_dir("apply"));
        let report = mgr
            .apply_scope_entries(
                &Scope::Global,
                &[
                    entry("ai", "model", "deepseek-chat"),
                    entry("ui", "lang", "en"),
                ],
                None,
            )
            .unwrap();
        assert_eq!((report.saved, report.applied), (2, 2));
        // 运行时对象拿到值 → 读 runtime 命中（优先级最高）
        assert_eq!(
            mgr.config_read(&Scope::Runtime, "ruyix.code.ai.model", None)
                .unwrap(),
            Some("deepseek-chat".into())
        );

        // 空值不反向删除运行时键（避免误伤显式设过的运行时覆盖）
        let report = mgr
            .apply_scope_entries(&Scope::Global, &[entry("ai", "model", "")], None)
            .unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(
            mgr.config_read(&Scope::Runtime, "ruyix.code.ai.model", None)
                .unwrap(),
            Some("deepseek-chat".into())
        );

        // 项目作用域没有 project_root：连保存路径都拿不到，必须报错
        assert!(
            mgr.apply_scope_entries(
                &Scope::Project,
                &[entry("run", "t0.cmd", "cargo run")],
                None
            )
            .is_err()
        );
    }

    /// 项目作用域必须有 project_root
    #[test]
    fn project_scope_requires_project_root() {
        let mut mgr = ConfigManager::new_with_dir(temp_dir("proj"));
        assert!(mgr.scan_scope_entries(&Scope::Project, None).is_err());
        assert!(
            mgr.save_scope_entries(&Scope::Project, &[entry("ai", "api_key", "x")], None)
                .is_err()
        );
        assert!(
            mgr.apply_scope_entries(&Scope::Project, &[entry("ai", "api_key", "x")], None)
                .is_err()
        );
    }

    /// 旧版配置（纯路径列表）能解析为带默认 name/lang 的条目
    #[test]
    fn legacy_path_list_parses_to_entries() {
        let legacy = format!("[projects]\ncurrent = '{PATH_A}'\nlist = ['{PATH_A}', '{PATH_B}']\n");
        let cfg: ProjectsConfig = toml::from_str::<File>(&legacy).unwrap().projects;
        assert_eq!(cfg.current.as_deref(), Some(PATH_A));
        assert_eq!(cfg.list.len(), 2);
        assert_eq!(cfg.list[0].name, "bar");
        assert_eq!(cfg.list[0].path, PATH_A);
        assert_eq!(cfg.list[0].lang, "unknown");
        assert_eq!(cfg.list[1].name, "qux");
        assert_eq!(cfg.list[1].lang, "unknown");
    }

    /// 新版配置（name/path/lang 条目）正常解析，缺省 lang 补默认值
    #[test]
    fn new_format_parses_with_default_lang() {
        let new_fmt = r#"
[projects]
current = 'C:\foo\bar'

[[projects.list]]
name = 'bar'
path = 'C:\foo\bar'
lang = 'python'

[[projects.list]]
name = 'qux'
path = 'C:\baz\qux'
"#;
        let cfg: ProjectsConfig = toml::from_str::<File>(new_fmt).unwrap().projects;
        assert_eq!(cfg.list.len(), 2);
        assert_eq!(cfg.list[0].lang, "python");
        assert_eq!(cfg.list[1].lang, "unknown");
    }

    /// 新格式序列化后能读回（round-trip）
    #[test]
    fn roundtrip() {
        let cfg = ProjectsConfig {
            current: Some(r"C:\foo\bar".to_string()),
            list: vec![ProjectEntry {
                name: "bar".to_string(),
                path: r"C:\foo\bar".to_string(),
                lang: "python".to_string(),
            }],
        };
        let toml_str = toml::to_string_pretty(&File { projects: cfg }).unwrap();
        let parsed: ProjectsConfig = toml::from_str::<File>(&toml_str).unwrap().projects;
        assert_eq!(parsed.current.as_deref(), Some(r"C:\foo\bar"));
        assert_eq!(parsed.list.len(), 1);
        assert_eq!(parsed.list[0].name, "bar");
        assert_eq!(parsed.list[0].lang, "python");
    }

    /// 语言可选值：10 种，第一种为 unknown（默认）
    #[test]
    fn langs_list_has_ten_entries() {
        assert_eq!(PROJECT_LANGS.len(), 10);
        assert_eq!(PROJECT_LANGS[0], "unknown");
    }

    /// set_project_lang：设置语言并持久化；非法语言被拒绝
    #[test]
    fn set_project_lang_persists_and_validates() {
        let dir = std::env::temp_dir().join(format!("dhc-config-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let legacy = format!("[projects]\ncurrent = '{PATH_A}'\nlist = ['{PATH_A}', '{PATH_B}']\n");
        fs::write(dir.join("projects.toml"), legacy).unwrap();

        let mgr = ConfigManager::new_with_dir(dir.clone());

        // 设置语言并持久化
        mgr.set_project_lang(PATH_B, "python").unwrap();
        let cfg2 = mgr.load_projects();
        assert_eq!(cfg2.list[1].lang, "python");

        // 非法语言被拒绝
        assert!(mgr.set_project_lang(PATH_B, "bogus").is_err());

        let _ = fs::remove_dir_all(&dir);
    }

    /// update_project：改名称与语言；delete_project：删除条目并清空 current
    #[test]
    fn update_and_delete_project() {
        let dir = std::env::temp_dir().join(format!("dhc-config-test2-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let new_fmt = r#"
[projects]
current = 'C:\foo\bar'

[[projects.list]]
name = 'bar'
path = 'C:\foo\bar'
lang = 'unknown'

[[projects.list]]
name = 'qux'
path = 'C:\baz\qux'
lang = 'unknown'
"#;
        fs::write(dir.join("projects.toml"), new_fmt).unwrap();

        let mgr = ConfigManager::new_with_dir(dir.clone());

        // 修改名称与语言
        mgr.update_project(r"C:\foo\bar", " 我的项目 ", "rust")
            .unwrap();
        let cfg = mgr.load_projects();
        assert_eq!(cfg.list[0].name, "我的项目"); // trim 生效
        assert_eq!(cfg.list[0].lang, "rust");
        assert_eq!(cfg.list[0].path, r"C:\foo\bar"); // 路径不变

        // 空名称被拒绝
        assert!(mgr.update_project(r"C:\foo\bar", "  ", "rust").is_err());
        // 非法语言被拒绝
        assert!(mgr.update_project(r"C:\foo\bar", "ok", "bogus").is_err());
        // 不存在的项目被拒绝
        assert!(mgr.update_project(r"C:\nope", "x", "rust").is_err());

        // 删除当前项目：条目移除且 current 清空
        mgr.delete_project(r"C:\foo\bar").unwrap();
        let cfg = mgr.load_projects();
        assert_eq!(cfg.list.len(), 1);
        assert_eq!(cfg.list[0].path, r"C:\baz\qux");
        assert!(cfg.current.is_none());

        // 删除不存在的项目报错
        assert!(mgr.delete_project(r"C:\foo\bar").is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
