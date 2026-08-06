use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
    /// Rust 工具链安装路径（指向包含 rustc 的目录，或其父目录）。
    /// 为 None 或空字符串时使用系统 PATH 中的 rustc。
    #[serde(default)]
    pub rust_toolchain_path: Option<String>,
    /// 自定义算法编译目录。
    /// 为 None 或空字符串时使用系统临时目录。
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

/// 平台相关的 rustc 二进制文件名。
fn rustc_binary_name() -> &'static str {
    if cfg!(windows) { "rustc.exe" } else { "rustc" }
}

/// 解析配置路径下 rustc 的实际可执行文件路径。
///
/// `configured` 可以是：
/// - 包含 rustc 的 bin 目录（如 `~/.cargo/bin`）
/// - 其父目录（如 `~/.cargo`）
/// - rustc 可执行文件的完整路径
/// - None / 空字符串 → 返回 None，调用方应回退到 PATH 查找。
pub fn resolve_rustc_path(configured: Option<&str>) -> Option<PathBuf> {
    let raw = configured?.trim();
    if raw.is_empty() {
        return None;
    }

    let binary = rustc_binary_name();
    let configured_path = Path::new(raw);

    // 情况 1: 用户直接给了 rustc 可执行文件路径
    if configured_path.file_name().map(|n| n == binary).unwrap_or(false) && configured_path.exists() {
        return Some(configured_path.to_path_buf());
    }

    // 情况 2: 用户给了 bin 目录（rustc 直接在里面）
    let direct = configured_path.join(binary);
    if direct.exists() {
        return Some(direct);
    }

    // 情况 3: 用户给了父目录（rustc 在 bin/ 子目录里）
    let nested = configured_path.join("bin").join(binary);
    if nested.exists() {
        return Some(nested);
    }

    None
}

/// 构造一个准备执行 rustc 的 Command。
/// 优先使用配置的路径，否则回退到系统 PATH 中的 rustc。
pub fn get_rustc_command() -> Command {
    let configured = load_config()
        .ok()
        .and_then(|c| c.rust_toolchain_path);

    match resolve_rustc_path(configured.as_deref()) {
        Some(path) => Command::new(path),
        None => Command::new("rustc"), // 回退到 PATH
    }
}

/// 通过 `where`/`which` 自动检测系统 rustc 的安装位置，
/// 返回 rustc 所在的 bin 目录（用户应将该目录填入配置）。
pub fn detect_rust_installation() -> Option<PathBuf> {
    let output = if cfg!(windows) {
        Command::new("where").arg("rustc").output().ok()?
    } else {
        Command::new("which").arg("rustc").output().ok()?
    };

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next()?;
    let rustc_path = PathBuf::from(first_line.trim());

    // rustc 通常位于 <cargo_home>/bin/rustc，返回其父目录（bin 目录）
    rustc_path.parent().map(|p| p.to_path_buf())
}

/// 测试给定路径下 rustc 是否可用，返回版本字符串。
pub fn test_rust_toolchain(path: Option<&str>) -> Result<String, String> {
    let rustc = resolve_rustc_path(path)
        .ok_or_else(|| "在指定路径下未找到 rustc，请检查路径或使用系统 PATH".to_string())?;

    let output = Command::new(&rustc)
        .arg("--version")
        .output()
        .map_err(|e| format!("无法执行 rustc ({}): {}", rustc.display(), e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("rustc 执行失败: {}", stderr.trim()));
    }

    let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(version)
}

/// 仅更新配置中的 rust_toolchain_path 字段，保留其它字段。
///
/// 采用「加载-修改-保存」模式。
pub fn save_rust_toolchain_path(path: Option<String>) -> Result<(), ConfigError> {
    let mut config = load_config().unwrap_or_default();
    // 空字符串视为 None
    let normalized = path.and_then(|p| {
        let trimmed = p.trim().to_string();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    });
    config.rust_toolchain_path = normalized;
    save_config(&config)
}

/// 获取配置的编译目录，若未配置则返回程序运行目录下的 target/compile。
pub fn get_compile_directory() -> PathBuf {
    let configured = load_config()
        .ok()
        .and_then(|c| c.compile_directory);
    
    if let Some(path_str) = configured {
        let path = PathBuf::from(path_str.trim());
        if !path.exists() {
            // 如果目录不存在，尝试创建
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

/// 获取默认编译目录：程序运行目录下的 target/compile。
pub fn get_default_compile_directory() -> PathBuf {
    // 优先使用程序运行目录下的 target/compile
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
    
    // 回退到系统临时目录
    std::env::temp_dir()
}

/// 获取数据预览缓存目录：工作区根目录下的 cache。
///
/// 解析方式基于本 crate 的 `CARGO_MANIFEST_DIR`（即 `mining-app/`）取父目录，
/// 得到工作区根（与 `mining-app`、`operator_runtime` 等同级），再拼接 `cache`。
/// 这样无论编译产物从何处启动，都稳定指向工作区级 cache 目录。
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
///
/// 点击「执行 DAG」时，编排好的流程会以 JSON 形式落盘到此目录，
/// 随后下发到服务端解析执行。路径解析方式与 [`get_cache_directory`] 一致，
/// 基于本 crate 的 `CARGO_MANIFEST_DIR` 取父目录得到工作区根。
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
///
/// 挖掘分析视图中的每个「建模」（一张可编辑 DAG）以 JSON 形式落盘到此目录，
/// 文件名 `<id>.json`，供历史列表跨会话保留与重新打开。路径解析方式与
/// [`get_dag_directory`] 一致，基于本 crate 的 `CARGO_MANIFEST_DIR` 取父目录。
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
/// 已启用的自定义算子会被放置在此目录下的子目录中。
pub fn get_operator_directory() -> PathBuf {
    // 优先使用程序运行目录下的 operator
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(exe_dir) = exe_path.parent() {
            let op_dir = exe_dir.join("operator");
            if let Err(e) = fs::create_dir_all(&op_dir) {
                eprintln!("创建算子目录失败: {}", e);
            }
            return op_dir;
        }
    }
    
    // 回退到当前工作目录下的 operator
    std::env::current_dir().unwrap_or_default().join("operator")
}

/// 仅更新配置中的 compile_directory 字段，保留其它字段。
pub fn save_compile_directory(path: Option<String>) -> Result<(), ConfigError> {
    let mut config = load_config().unwrap_or_default();
    // 空字符串视为 None
    let normalized = path.and_then(|p| {
        let trimmed = p.trim().to_string();
        if trimmed.is_empty() { None } else { Some(trimmed) }
    });
    config.compile_directory = normalized;
    save_config(&config)
}
