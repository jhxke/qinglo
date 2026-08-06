//! 建模（可编辑 DAG）的磁盘持久化。
//!
//! 挖掘分析视图中的每个「建模」对应一张 [`DagGraph`]，以 JSON 形式落盘到
//! [`crate::config::get_models_directory`] 下的 `<id>.json`。本模块提供
//! 列表 / 加载 / 保存 / 删除四个原子操作，供 UI 层在新建、打开、切换、
//! 关闭建模时调用。
//!
//! 设计上保持无状态：每次调用直接读写磁盘，不在内存缓存，避免 UI 层与
//! 磁盘状态不一致。列表元数据 [`DagModelMeta`] 轻量，可在每帧或按需重扫。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::get_models_directory;
use crate::dag::DagGraph;

/// 建模的轻量元数据，用于左侧历史列表展示与按更新时间排序。
///
/// 不含 `graph` 数据，避免列表时加载全部图。打开建模时再调 [`load_model`]
/// 取完整记录。
#[derive(Debug, Clone)]
pub struct DagModelMeta {
    pub id: String,
    pub name: String,
    /// 最近更新时间（UTC 毫秒时间戳），用于排序与展示。
    pub updated_at: u64,
}

/// 建模的完整磁盘记录（`<id>.json` 的反序列化结构）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DagModelRecord {
    pub id: String,
    pub name: String,
    pub graph: DagGraph,
    pub updated_at: u64,
}

/// 生成一个新的建模 id（UUID v4 字符串）。
pub fn new_model_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 当前 UTC 毫秒时间戳。
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 单个建模文件路径：`<models_dir>/<id>.json`。
fn model_path(id: &str) -> PathBuf {
    get_models_directory().join(format!("{}.json", id))
}

/// 软删除（回收）后的文件扩展名：`<id>.json` → `<id>.deleted`。
///
/// `list_models` 仅扫描 `.json` 文件，故 `.deleted` 文件不会出现在历史列表中，
/// 等价于从 UI 中移除；但磁盘文件仍保留，可手动改回 `.json` 恢复。
const DELETED_EXTENSION: &str = "deleted";

/// 扫描建模目录，返回所有建模的元数据，按 `updated_at` 倒序（最新在前）。
///
/// 解析失败的文件会被跳过并 `eprintln` 提示，不影响其余建模列出。
pub fn list_models() -> Vec<DagModelMeta> {
    let dir = get_models_directory();
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let mut metas = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        match fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<DagModelRecord>(&content) {
                Ok(rec) => metas.push(DagModelMeta {
                    id: rec.id,
                    name: rec.name,
                    updated_at: rec.updated_at,
                }),
                Err(e) => {
                    eprintln!("解析建模文件失败 ({}): {}", path.display(), e);
                }
            },
            Err(e) => {
                eprintln!("读取建模文件失败 ({}): {}", path.display(), e);
            }
        }
    }

    metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    metas
}

/// 加载指定 id 的建模完整记录。文件不存在或解析失败时返回 `None`。
pub fn load_model(id: &str) -> Option<DagModelRecord> {
    let path = model_path(id);
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// 保存（或覆盖）指定 id 的建模：写 `<id>.json`，刷新 `updated_at`。
///
/// 失败仅 `eprintln` 提示，不中断 UI 流程（调用方在内存中已持有最新 graph）。
pub fn save_model(id: &str, name: &str, graph: &DagGraph) {
    let record = DagModelRecord {
        id: id.to_string(),
        name: name.to_string(),
        graph: graph.clone(),
        updated_at: now_millis(),
    };
    let path = model_path(id);
    match serde_json::to_string_pretty(&record) {
        Ok(json) => {
            if let Err(e) = fs::write(&path, &json) {
                eprintln!("写入建模文件失败: {} (路径: {})", e, path.display());
            }
        }
        Err(e) => eprintln!("序列化建模失败 (id={}): {}", id, e),
    }
}

/// 软删除指定 id 的建模：将 `<id>.json` 重命名为 `<id>.deleted`。
///
/// 不是真正删除磁盘文件——`list_models` 仅扫描 `.json`，故 `.deleted` 文件
/// 不会出现在历史列表中，等价于从 UI 移除；需要时手动改回 `.json` 即可恢复。
/// 源文件不存在视为成功；若已存在同名 `.deleted` 残留则先清理再重命名，
/// 避免 Windows 上 `rename` 因目标已存在而失败。
pub fn delete_model(id: &str) {
    let path = model_path(id);
    if !path.exists() {
        return;
    }
    let deleted_path = path.with_extension(DELETED_EXTENSION);
    if deleted_path.exists() {
        let _ = fs::remove_file(&deleted_path);
    }
    if let Err(e) = fs::rename(&path, &deleted_path) {
        eprintln!(
            "软删除建模文件失败 (重命名出错): {} (路径: {} → {})",
            e,
            path.display(),
            deleted_path.display()
        );
    }
}

/// 恢复软删除的建模：将 `<id>.deleted` 重命名回 `<id>.json`。
///
/// 成功后该建模会重新出现在历史列表中。源文件不存在或恢复失败时返回 `false`。
pub fn restore_model(id: &str) -> bool {
    let deleted_path = model_path(id).with_extension(DELETED_EXTENSION);
    let path = model_path(id);
    if !deleted_path.exists() {
        return false;
    }
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    match fs::rename(&deleted_path, &path) {
        Ok(()) => true,
        Err(e) => {
            eprintln!(
                "恢复建模文件失败: {} (路径: {} → {})",
                e,
                deleted_path.display(),
                path.display()
            );
            false
        }
    }
}

/// 将 UTC 毫秒时间戳格式化为 `YYYY-MM-DD HH:MM:SS`（UTC+8）便于列表展示。
pub fn format_timestamp(millis: u64) -> String {
    let total = (millis / 1000) as i64 + 8 * 3600;
    let days = total.div_euclid(86400);
    let secs_of_day = total.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let hh = secs_of_day / 3600;
    let mm = (secs_of_day / 60) % 60;
    let ss = secs_of_day % 60;
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, hh, mm, ss)
}

/// epoch 起算的第 `z` 天 → 公历 (年, 月, 日)。Howard Hinnant civil_from_days 算法。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}
