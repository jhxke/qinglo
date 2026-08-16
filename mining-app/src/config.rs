use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("文件读取失败: {0}")]
    ReadError(#[from] std::io::Error),
    #[error("JSON 解析失败: {0}")]
    JsonError(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Rust 工具链安装路径（预留字段，当前不再在设置页暴露）。
    #[serde(default)]
    pub rust_toolchain_path: Option<String>,
    /// 自定义算法编译目录（预留字段，当前不再在设置页暴露）。
    #[serde(default)]
    pub compile_directory: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            rust_toolchain_path: None,
            compile_directory: None,
        }
    }
}

pub fn get_config_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("stock-factor-miner");
    path.push("config.json");
    path
}

pub fn load_config() -> Result<AppConfig, ConfigError> {
    let path = get_config_path();
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    
    let content = fs::read_to_string(&path)?;
    let config = serde_json::from_str(&content)?;
    Ok(config)
}

pub fn save_config(config: &AppConfig) -> Result<(), ConfigError> {
    let path = get_config_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_string_pretty(config)?;
    fs::write(&path, content)?;
    Ok(())
}

/// 获取配置的编译目录，若未配置则返回程序运行目录下的 target/compile。
pub fn get_compile_directory() -> PathBuf {
    let configured = load_config()
        .ok()
        .and_then(|c| c.compile_directory);
    
    if let Some(path_str) = configured {
        let path = PathBuf::from(path_str.trim());
        if !path.exists() {
            if let Err(e) = fs::create_dir_all(&path) {
                eprintln!("创建编译目录失败: {}，将使用默认目录", e);
                return get_default_compile_directory();
            }
        }
        path
    } else {
        get_default_compile_directory()
    }
}

/// 获取默认编译目录：程序运行目录下的 compile。
pub fn get_default_compile_directory() -> PathBuf {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let default_path = exe_dir.join("compile");
            if let Err(e) = fs::create_dir_all(&default_path) {
                eprintln!("创建默认编译目录失败: {}，将使用系统临时目录", e);
            } else {
                return default_path;
            }
        }
    }
    
    std::env::temp_dir()
}

/// 获取数据预览缓存目录：工作区根目录下的 cache。
pub fn get_cache_directory() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("cache");
    if let Err(e) = fs::create_dir_all(&path) {
        eprintln!("创建缓存目录失败: {} (路径: {})", e, path.display());
    }
    path
}

/// 获取 DAG 流程文件目录：工作区根目录下的 dag。
pub fn get_dag_directory() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("dag");
    if let Err(e) = fs::create_dir_all(&path) {
        eprintln!("创建 DAG 目录失败: {} (路径: {})", e, path.display());
    }
    path
}

/// 获取建模文件目录：工作区根目录下的 models。
pub fn get_models_directory() -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("models");
    if let Err(e) = fs::create_dir_all(&path) {
        eprintln!("创建建模目录失败: {} (路径: {})", e, path.display());
    }
    path
}

/// 获取算子目录：程序运行目录下的 operator。
pub fn get_operator_directory() -> PathBuf {
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let op_dir = exe_dir.join("operator");
            if let Err(e) = fs::create_dir_all(&op_dir) {
                eprintln!("创建算子目录失败: {}", e);
            }
            return op_dir;
        }
    }
    
    std::env::current_dir().unwrap_or_default().join("operator")
}
