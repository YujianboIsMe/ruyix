use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// 配置键前缀
pub const PREFIX: &str = "darkhorse.code";

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
                let map = self.read_toml_file(&path)?;
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
        // 运行目标不能保存为全局
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
                let mut map = self.read_toml_file(&path)?;
                map.insert(sub_key, value.to_string());
                self.write_toml_file(&path, &map)?;
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
                let mut map = self.read_toml_file(&path)?;
                if map.remove(&sub_key).is_none() {
                    return Err(format!("配置键不存在: {}", key));
                }
                self.write_toml_file(&path, &map)?;
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
            Some(root) => Ok(PathBuf::from(root).join(".darkhorse").join("code")),
            None => Err("未打开项目，无法使用项目配置 (-p)".to_string()),
        }
    }

    /// 读取扁平的 key-value TOML 文件
    fn read_toml_file(&self, path: &PathBuf) -> Result<HashMap<String, String>, String> {
        let content = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(_) => return Ok(HashMap::new()), // 文件不存在 → 空 map
        };

        // 解析为通用 toml::Value，然后展平
        let value: toml::Value = toml::from_str(&content).map_err(|e| format!("TOML 解析错误: {}", e))?;

        let mut map = HashMap::new();
        if let toml::Value::Table(table) = value {
            for (k, v) in table {
                if let Some(s) = v.as_str() {
                    map.insert(k, s.to_string());
                } else {
                    // 非字符串值序列化为字符串
                    map.insert(k, v.to_string());
                }
            }
        }
        Ok(map)
    }

    /// 写入扁平的 key-value TOML 文件
    fn write_toml_file(&self, path: &PathBuf, map: &HashMap<String, String>) -> Result<(), String> {
        let mut table = toml::map::Map::new();
        for (k, v) in map {
            table.insert(k.clone(), toml::Value::String(v.clone()));
        }
        let toml_str = toml::to_string_pretty(&table).map_err(|e| e.to_string())?;
        fs::write(path, toml_str).map_err(|e| format!("写入配置失败: {}", e))
    }
}
