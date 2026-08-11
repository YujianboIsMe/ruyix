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
}

/// 知名清单文件定义（预置为可运行）
#[derive(Debug, Clone, Copy)]
pub struct ManifestDef {
    pub name: &'static str, // 文件名（小写）
    pub cmd: &'static str,  // 建议运行命令
}

pub const MANIFEST_FILES: &[ManifestDef] = &[
    ManifestDef { name: "cargo.toml", cmd: "cargo run" },
    ManifestDef { name: "package.json", cmd: "npm start" },
    ManifestDef { name: "makefile", cmd: "make" },
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectsConfig {
    pub current: Option<String>,
    pub list: Vec<String>,
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
        let global_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".darkhorse")
            .join("code");

        let _ = fs::create_dir_all(&global_dir);

        Self {
            global_dir,
            runtime: HashMap::new(),
        }
    }

    pub fn global_dir(&self) -> &PathBuf {
        &self.global_dir
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
                struct File { projects: ProjectsConfig }
                toml::from_str::<File>(&s)
                    .map(|f| f.projects)
                    .unwrap_or_default()
            }
            Err(_) => ProjectsConfig::default(),
        }
    }

    pub fn save_projects(&self, cfg: &ProjectsConfig) -> Result<(), String> {
        #[derive(Serialize)]
        struct File { projects: ProjectsConfig }
        let file = File { projects: cfg.clone() };
        let toml_str = toml::to_string_pretty(&file).map_err(|e| e.to_string())?;
        fs::write(self.projects_path(), toml_str).map_err(|e| format!("写入配置失败: {}", e))
    }

    pub fn set_current_project(&self, project_path: &str) -> Result<(), String> {
        let mut cfg = self.load_projects();
        cfg.list.retain(|p| p != project_path);
        cfg.list.push(project_path.to_string());
        cfg.current = Some(project_path.to_string());
        self.save_projects(&cfg)
    }

    // ============================================
    // 运行目标
    // ============================================

    /// 加载项目运行目标，按 target key 分组。
    /// 从 `run.toml` 中读取所有 `target<N>.cmd` 和 `target<N>.name` 键值对。
    pub fn load_run_targets(
        &self,
        project_root: Option<&str>,
    ) -> Result<Vec<RunTarget>, String> {
        let dir = self.resolve_project_dir(project_root)?;
        let path = dir.join("run.toml");
        let map = self.read_toml_file(&path, "run")?;

        // 按 target key 分组：target0.cmd → target0, target0.name → target0
        let mut groups: HashMap<String, Option<String>> = HashMap::new();
        let mut names: HashMap<String, Option<String>> = HashMap::new();

        for (k, v) in &map {
            if let Some(rest) = k.strip_suffix(".cmd") {
                groups.entry(rest.to_string()).or_insert(None);
                // 用 entry 来持有，后面统一构建
            } else if let Some(rest) = k.strip_suffix(".name") {
                names.insert(rest.to_string(), Some(v.clone()));
            }
        }

        // 合并
        let mut targets: Vec<RunTarget> = Vec::new();
        for (key, _) in &groups {
            let cmd = map.get(&format!("{}.cmd", key)).cloned();
            let name = names.get(key).cloned().flatten();
            targets.push(RunTarget {
                key: key.clone(),
                name,
                cmd,
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
                if let toml::Value::Table(inner) = v {
                    if let Some(toml::Value::Boolean(can_run)) = inner.get("can_run") {
                        map.insert(ext, *can_run);
                    }
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
            format!("配置键格式错误 (需为 {0}.<section>.<key>): {1}", PREFIX, key)
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
    fn read_toml_file(&self, path: &PathBuf, section: &str) -> Result<HashMap<String, String>, String> {
        let content = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Ok(HashMap::new()),
        };

        let value: toml::Value = toml::from_str(&content).map_err(|e| format!("TOML 解析错误: {}", e))?;

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
    fn write_toml_file(&self, path: &PathBuf, section: &str, map: &HashMap<String, String>) -> Result<(), String> {
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
