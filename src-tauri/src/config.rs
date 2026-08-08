use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// 配置键前缀，所有配置在内存中以完整前缀存储
#[allow(dead_code)]
pub const PREFIX: &str = "darkhorse.code";

// ============================================
// 配置数据结构
// ============================================

/// TOML 文件中 projects 节的表示（可省略前缀）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectsToml {
    pub current: Option<String>,
    pub list: Option<Vec<String>>,
}

/// 运行时 projects 配置，带完整前缀
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProjectsConfig {
    pub current: Option<String>,
    pub list: Vec<String>,
}

// ============================================
// ConfigManager
// ============================================

#[derive(Debug, Clone)]
pub struct ConfigManager {
    /// ~/.darkhorse/code
    global_dir: PathBuf,
}

impl ConfigManager {
    /// 创建配置管理器，确保全局配置目录存在
    pub fn new() -> Self {
        let global_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".darkhorse")
            .join("code");

        // 确保目录存在
        let _ = fs::create_dir_all(&global_dir);

        Self { global_dir }
    }

    /// 全局配置目录
    pub fn global_dir(&self) -> &PathBuf {
        &self.global_dir
    }

    // ============================================
    // Projects 配置
    // ============================================

    fn projects_path(&self) -> PathBuf {
        self.global_dir.join("projects.toml")
    }

    /// 加载 projects 配置
    pub fn load_projects(&self) -> ProjectsConfig {
        let path = self.projects_path();

        let toml_str = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return ProjectsConfig::default(),
        };

        // 尝试解析：先尝试带前缀的完整 key，再尝试缩略 key
        // toml crate 支持直接从字符串解析
        match self.parse_projects(&toml_str) {
            Ok(cfg) => cfg,
            Err(_) => ProjectsConfig::default(),
        }
    }

    /// 保存 projects 配置
    pub fn save_projects(&self, cfg: &ProjectsConfig) -> Result<(), String> {
        let path = self.projects_path();

        // 序列化为 TOML，使用完整前缀格式
        let toml_str = self.serialize_projects(cfg)?;

        fs::write(&path, toml_str).map_err(|e| format!("写入配置失败: {}", e))?;
        Ok(())
    }

    /// 添加项目到列表，并设为当前项目
    pub fn set_current_project(&self, project_path: &str) -> Result<(), String> {
        let mut cfg = self.load_projects();

        // 去重：如果已存在，移到列表末尾（最近使用）
        cfg.list.retain(|p| p != project_path);
        cfg.list.push(project_path.to_string());
        cfg.current = Some(project_path.to_string());

        self.save_projects(&cfg)
    }

    /// 从 projects 列表中移除
    pub fn remove_project(&self, project_path: &str) -> Result<(), String> {
        let mut cfg = self.load_projects();
        cfg.list.retain(|p| p != project_path);
        if cfg.current.as_deref() == Some(project_path) {
            cfg.current = cfg.list.last().cloned();
        }
        self.save_projects(&cfg)
    }

    // ============================================
    // TOML 解析（支持省略 darkhorse.code 前缀）
    // ============================================

    fn parse_projects(&self, toml_str: &str) -> Result<ProjectsConfig, String> {
        // 尝试方式1：带完整前缀 darkhorse.code.projects
        #[derive(Deserialize)]
        struct FullConfig {
            #[serde(rename = "darkhorse.code")]
            darkhorse_code: Option<DarkhorseSection>,
        }

        #[derive(Deserialize)]
        struct DarkhorseSection {
            projects: Option<ProjectsToml>,
        }

        if let Ok(full) = toml::from_str::<FullConfig>(toml_str) {
            if let Some(section) = full.darkhorse_code {
                if let Some(p) = section.projects {
                    return Ok(self.build_projects_config(p));
                }
            }
        }

        // 尝试方式2：缩略 key，省略前缀 [projects]
        #[derive(Deserialize)]
        struct ShortConfig {
            projects: Option<ProjectsToml>,
        }

        if let Ok(short) = toml::from_str::<ShortConfig>(toml_str) {
            if let Some(p) = short.projects {
                return Ok(self.build_projects_config(p));
            }
        }

        Ok(ProjectsConfig::default())
    }

    fn build_projects_config(&self, toml: ProjectsToml) -> ProjectsConfig {
        ProjectsConfig {
            current: toml.current,
            list: toml.list.unwrap_or_default(),
        }
    }

    fn serialize_projects(&self, cfg: &ProjectsConfig) -> Result<String, String> {
        use std::fmt::Write;

        let mut out = String::new();

        // 使用缩略格式（省略 darkhorse.code 前缀），更易读
        writeln!(&mut out, "[projects]").map_err(|e| e.to_string())?;

        if let Some(ref current) = cfg.current {
            writeln!(&mut out, "current = {:?}", current).map_err(|e| e.to_string())?;
        }

        // 序列化 list
        let list_str = cfg
            .list
            .iter()
            .map(|p| format!("  {:?}", p))
            .collect::<Vec<_>>()
            .join(",\n");
        writeln!(&mut out, "list = [\n{}\n]", list_str).map_err(|e| e.to_string())?;

        Ok(out)
    }
}
