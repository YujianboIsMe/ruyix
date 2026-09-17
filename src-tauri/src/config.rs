use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// 配置键前缀
pub const PREFIX: &str = "darkhorse.code";

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
                .join(".darkhorse")
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

    /// 读取配置值。key 格式: darkhorse.code.<section>.<path>
    /// scope=global → ~/.darkhorse/code/<section>.toml
    /// scope=project → <project_root>/.darkhorse/code/<section>.toml
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
            .join(".darkhorse")
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
            .join(".darkhorse")
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
    /// "darkhorse.code.run.target0.cmd" → ("run", "target0.cmd")
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
            Some(root) if !root.is_empty() => {
                Ok(PathBuf::from(root).join(".darkhorse").join("code"))
            }
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
