use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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

/// 配置编辑器的整个 scope 视图（配置菜单 → 编辑标签）
#[derive(Debug, Clone, Serialize)]
pub struct ScopeConfigDump {
    pub scope: String,
    /// 配置目录（runtime 为内存占位说明）
    pub dir: String,
    /// 合并渲染的 TOML 文本（可直接编辑、Ctrl+S 存回）
    pub content: String,
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
    pub fn new() -> Self {
        Self::new_with_dir(
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".ruyix")
                .join("code"),
        )
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
    // 配置编辑器（配置菜单）：整个 scope 的合并视图读写
    // ============================================

    /// 结构化文件：由专门功能管理，平铺编辑器写回会破坏格式，读时排除、写时拒绝
    const SCOPE_EXCLUDED_FILES: [&str; 3] = ["projects.toml", "execute.toml", "rag.toml"];

    /// 合并视图的渲染：各 section 按名字排序，`[section]` 表内平铺字符串键值。
    /// 渲染结果本身是合法 TOML，编辑器里直接改、保存时原样解析回来。
    fn render_sections_toml(
        sections: &mut Vec<(String, HashMap<String, String>)>,
        header: &str,
    ) -> String {
        sections.sort_by(|a, b| a.0.cmp(&b.0));
        let mut root = toml::map::Map::new();
        for (section, map) in sections {
            let mut table = toml::map::Map::new();
            for (k, v) in map {
                table.insert(k.clone(), toml::Value::String(v.clone()));
            }
            root.insert(section.clone(), toml::Value::Table(table));
        }
        let body =
            toml::to_string_pretty(&toml::Value::Table(root)).unwrap_or_else(|_| String::new());
        format!("{header}\n\n{body}")
    }

    /// 标量值 → 配置系统的字符串值。字符串原样；数字/布尔取字面量；
    /// 嵌套表/数组拒绝（配置系统没有这类值）。
    fn scalar_to_string(v: &toml::Value) -> Result<String, String> {
        match v {
            toml::Value::String(s) => Ok(s.clone()),
            toml::Value::Integer(_) | toml::Value::Float(_) | toml::Value::Boolean(_) => {
                Ok(v.to_string())
            }
            other => Err(format!("不支持嵌套值（配置只有平铺的键=字符串）: {other}")),
        }
    }

    /// 读整个 scope 的合并视图（配置菜单 → 编辑标签）。
    /// - global/project：目录下各 `<section>.toml` 合并渲染；结构化文件排除；
    ///   只收字符串键值（`read_toml_file` 同款规则），空 section 跳过
    /// - runtime：内存键按 section 分组渲染；**保存时以文本为准整体替换**
    pub fn dump_scope_toml(
        &self,
        scope: &Scope,
        project_root: Option<&str>,
    ) -> Result<ScopeConfigDump, String> {
        if *scope == Scope::Runtime {
            let mut sections: Vec<(String, HashMap<String, String>)> = Vec::new();
            let mut keys: Vec<&String> = self.runtime.keys().collect();
            keys.sort();
            for key in keys {
                let rest = key
                    .strip_prefix(PREFIX)
                    .and_then(|s| s.strip_prefix('.'))
                    .ok_or_else(|| format!("运行时键缺少前缀: {key}"))?;
                let (section, sub) = rest
                    .split_once('.')
                    .ok_or_else(|| format!("运行时键缺少 section: {key}"))?;
                let value = self.runtime.get(key).cloned().unwrap_or_default();
                if let Some((_, m)) = sections.iter_mut().find(|(s, _)| s == section) {
                    m.insert(sub.to_string(), value);
                } else {
                    let mut m = HashMap::new();
                    m.insert(sub.to_string(), value);
                    sections.push((section.to_string(), m));
                }
            }
            return Ok(ScopeConfigDump {
                scope: "runtime".into(),
                dir: "(内存 · 不落盘)".into(),
                content: Self::render_sections_toml(
                    &mut sections,
                    "# ruyix 运行时配置（内存，不落盘）\n# 以本文件内容为准：保存会清掉未列出的运行时键",
                ),
            });
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
        let mut sections: Vec<(String, HashMap<String, String>)> = Vec::new();
        for name in names {
            let section = name.trim_end_matches(".toml").to_string();
            let map = self.read_toml_file(&dir.join(&name), &section)?;
            if !map.is_empty() {
                sections.push((section, map));
            }
        }
        let title = match scope {
            Scope::Global => "全局",
            Scope::Project => "项目",
            Scope::Runtime => "",
        };
        let header = format!(
            "# ruyix {title}配置 — {}\n# Ctrl+S 保存；projects/execute/rag 由专门功能管理，不在此编辑",
            dir.display()
        );
        Ok(ScopeConfigDump {
            scope: match scope {
                Scope::Global => "global".into(),
                Scope::Project => "project".into(),
                Scope::Runtime => "runtime".into(),
            },
            dir: dir.to_string_lossy().to_string(),
            content: Self::render_sections_toml(&mut sections, &header),
        })
    }

    /// 保存合并视图（配置标签 Ctrl+S）。
    /// - global/project：按文本里的 `[section]` 逐个写 `<section>.toml`（合并语义：
    ///   未在文本中出现的既有 section 文件不受影响）
    /// - runtime：整体替换内存键
    ///
    /// 返回写入的键数量。
    pub fn save_scope_toml(
        &mut self,
        scope: &Scope,
        content: &str,
        project_root: Option<&str>,
    ) -> Result<usize, String> {
        let root: toml::Value =
            toml::from_str(content).map_err(|e| format!("TOML 解析错误: {e}"))?;
        let table = root
            .as_table()
            .ok_or_else(|| "顶层必须是 [section] 表".to_string())?;

        let mut count = 0usize;
        for (section, value) in table {
            let section_table = value
                .as_table()
                .ok_or_else(|| format!("[{section}] 必须是键值表"))?;
            if Self::SCOPE_EXCLUDED_FILES.contains(&format!("{section}.toml").as_str()) {
                return Err(format!(
                    "[{section}] 由专门功能管理，不能通过配置编辑器写入"
                ));
            }
            match scope {
                Scope::Runtime => {
                    // 运行时整体替换：首个 section 写入前清空内存键
                    if count == 0 {
                        self.runtime.clear();
                    }
                    for (k, v) in section_table {
                        let key = format!("{}.{}.{}", PREFIX, section, k);
                        self.runtime.insert(key, Self::scalar_to_string(v)?);
                        count += 1;
                    }
                }
                _ => {
                    let dir = self.scope_dir_for(scope, project_root)?;
                    fs::create_dir_all(&dir).map_err(|e| format!("创建目录失败: {e}"))?;
                    let mut map = HashMap::new();
                    for (k, v) in section_table {
                        map.insert(k.clone(), Self::scalar_to_string(v)?);
                        count += 1;
                    }
                    self.write_toml_file(&dir.join(format!("{section}.toml")), section, &map)?;
                }
            }
        }
        Ok(count)
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

    /// 迁移旧版配置：把 projects.list 里的纯路径字符串转换为带 name/lang 的条目，
    /// 并写回新格式。返回迁移的旧条目数量（0 = 无需迁移）。
    pub fn migrate_projects(&self) -> Result<usize, String> {
        let content = match fs::read_to_string(self.projects_path()) {
            Ok(s) => s,
            Err(_) => return Ok(0), // 文件不存在，无需迁移
        };
        // 检查 list 中是否还有纯字符串条目
        let legacy_count = toml::from_str::<toml::Value>(&content)
            .ok()
            .and_then(|v| {
                let list = v.get("projects")?.get("list")?.as_array()?;
                Some(list.iter().filter(|x| x.is_str()).count())
            })
            .unwrap_or(0);
        if legacy_count == 0 {
            return Ok(0); // 已是新格式
        }
        let cfg = self.load_projects(); // load_projects 会把旧字符串转成新条目
        self.save_projects(&cfg)?;
        Ok(legacy_count)
    }

    // ============================================
    // 运行目标
    // ============================================

    /// 加载项目运行目标，按 target key 分组。
    /// 从 `run.toml` 中读取所有 `target<N>.cmd` 和 `target<N>.name` 键值对。
    pub fn load_run_targets(&self, project_root: Option<&str>) -> Result<Vec<RunTarget>, String> {
        let dir = self.resolve_project_dir(project_root)?;
        let path = dir.join("run.toml");
        let map = self.read_toml_file(&path, "run")?;

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
        let path = std::path::Path::new(project_root)
            .join(".ruyix")
            .join("code")
            .join("learn.lua");
        if !path.exists() {
            return Ok(String::new());
        }
        std::fs::read_to_string(&path).map_err(|e| format!("读取 learn.lua 失败: {}", e))
    }

    /// 追加 Lua 代码到 learn.lua
    pub fn append_lua_script(&self, project_root: &str, lua_code: &str) -> Result<(), String> {
        let dir = std::path::Path::new(project_root)
            .join(".ruyix")
            .join("code");
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
            Some(root) if !root.is_empty() => Ok(PathBuf::from(root).join(".ruyix").join("code")),
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

    // ===== 配置编辑器（配置菜单）=====

    #[test]
    fn dump_merges_sections_and_excludes_structured_files() {
        let dir = temp_dir("dump");
        let mut mgr = ConfigManager::new_with_dir(dir.clone());
        mgr.config_write(&Scope::Global, "ruyix.code.ai.api_key", "sk-1", None)
            .unwrap();
        mgr.config_write(&Scope::Global, "ruyix.code.ui.lang", "zh-CN", None)
            .unwrap();
        // 结构化文件必须被排除（平铺写回会破坏格式）
        fs::write(dir.join("projects.toml"), "[projects]\ncurrent = '/x'\n").unwrap();
        fs::write(dir.join("rag.toml"), "enabled = true\n").unwrap();

        let dump = mgr.dump_scope_toml(&Scope::Global, None).unwrap();
        assert!(dump.content.contains("[ai]"));
        assert!(dump.content.contains("api_key"));
        assert!(dump.content.contains("[ui]"));
        // 头部注释会提到 projects/execute/rag，这里只断言没有它们的 section
        assert!(!dump.content.contains("[projects]"));
        assert!(!dump.content.contains("[rag]"));
    }

    #[test]
    fn save_round_trips_and_rejects_reserved_sections() {
        let dir = temp_dir("save");
        let mut mgr = ConfigManager::new_with_dir(dir.clone());
        let text = "[ai]\napi_key = \"sk-2\"\nmax_tokens = 2000\n\n[ui]\nlang = \"en\"\n";
        let n = mgr.save_scope_toml(&Scope::Global, text, None).unwrap();
        assert_eq!(n, 3);

        // 读回：写盘格式与 config_read 兼容；数字标量转为字符串
        assert_eq!(
            mgr.config_read(&Scope::Global, "ruyix.code.ai.api_key", None)
                .unwrap()
                .as_deref(),
            Some("sk-2")
        );
        assert_eq!(
            mgr.config_read(&Scope::Global, "ruyix.code.ai.max_tokens", None)
                .unwrap()
                .as_deref(),
            Some("2000")
        );

        // 保留 section：写回会破坏结构化文件，必须拒绝
        let bad = "[projects]\ncurrent = '/x'\n";
        assert!(mgr.save_scope_toml(&Scope::Global, bad, None).is_err());

        // 再 dump：内容回到编辑器形状（round-trip）
        let dump = mgr.dump_scope_toml(&Scope::Global, None).unwrap();
        assert!(dump.content.contains("api_key"));
    }

    #[test]
    fn runtime_save_replaces_all_keys() {
        let mut mgr = ConfigManager::new_with_dir(temp_dir("rt"));
        mgr.config_write(&Scope::Runtime, "ruyix.code.ai.api_key", "old", None)
            .unwrap();
        mgr.config_write(&Scope::Runtime, "ruyix.code.ui.emoji", "true", None)
            .unwrap();

        let dump = mgr.dump_scope_toml(&Scope::Runtime, None).unwrap();
        assert!(dump.content.contains("[ai]") && dump.content.contains("[ui]"));

        let n = mgr
            .save_scope_toml(&Scope::Runtime, "[ai]\nmodel = \"deepseek-chat\"\n", None)
            .unwrap();
        assert_eq!(n, 1);
        // 整体替换语义：未列出的 ui.emoji 被清掉
        assert_eq!(
            mgr.config_read(&Scope::Runtime, "ruyix.code.ai.model", None)
                .unwrap(),
            Some("deepseek-chat".into())
        );
        assert_eq!(
            mgr.config_read(&Scope::Runtime, "ruyix.code.ui.emoji", None)
                .unwrap(),
            None
        );
    }

    #[test]
    fn project_scope_requires_project_root() {
        let mut mgr = ConfigManager::new_with_dir(temp_dir("proj"));
        assert!(mgr.dump_scope_toml(&Scope::Project, None).is_err());
        assert!(
            mgr.save_scope_toml(&Scope::Project, "[ai]\napi_key = 'x'\n", None)
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

    /// migrate_projects：旧版纯路径列表 → 新格式，返回迁移数量；再次迁移返回 0
    #[test]
    fn migrate_projects_rewrites_legacy_file() {
        let dir = std::env::temp_dir().join(format!("dhc-config-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let legacy = format!("[projects]\ncurrent = '{PATH_A}'\nlist = ['{PATH_A}', '{PATH_B}']\n");
        fs::write(dir.join("projects.toml"), legacy).unwrap();

        let mgr = ConfigManager::new_with_dir(dir.clone());
        // 第一次迁移：2 个旧条目
        assert_eq!(mgr.migrate_projects().unwrap(), 2);

        let cfg = mgr.load_projects();
        assert_eq!(cfg.list.len(), 2);
        assert_eq!(cfg.list[0].name, "bar");
        assert_eq!(cfg.list[0].lang, "unknown");
        assert_eq!(cfg.list[1].name, "qux");
        assert_eq!(cfg.current.as_deref(), Some(PATH_A));

        // 已是新格式：无需迁移
        assert_eq!(mgr.migrate_projects().unwrap(), 0);

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
