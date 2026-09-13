//! 建模（可编辑 DAG）的磁盘持久化。
//!
//! 挖掘分析视图中的每个「建模」对应一张 [`DagGraph`]，以 JSON 形式落盘到
//! [`crate::config::get_models_directory`] 下，支持多级目录分类：
//! - 根目录建模：`<models>/<id>.json`
//! - 目录内建模：`<models>/<目录>/<子目录>/<id>.json`（目录数不限）
//!
//! 模型 id 即相对路径（`/` 分隔，根目录模型为纯 uuid，目录内模型为
//! `目录/uuid`），磁盘布局与算子的 `lib/<组>/<算子>/` 分类树一致。
//! 本模块提供建模的列表 / 加载 / 保存 / 删除，以及目录的列出 / 新建 /
//! 重命名 / 删除原子操作，供 UI 层调用。
//!
//! 设计上保持无状态：每次调用直接读写磁盘，不在内存缓存，避免 UI 层与
//! 磁盘状态不一致。列表元数据 [`DagModelMeta`] 轻量，可在每帧或按需重扫。

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::get_models_directory;
use crate::mining::dag::DagGraph;

/// 建模的轻量元数据，用于左侧历史列表展示与按更新时间排序。
///
/// 不含 `graph` 数据，避免列表时加载全部图。打开建模时再调 [`load_model`]
/// 取完整记录。
#[derive(Debug, Clone)]
pub struct DagModelMeta {
    /// 相对 models 根目录的 id（`/` 分隔）：根目录模型为纯 uuid，
    /// 目录内模型为 `目录/uuid`；同时也是磁盘相对路径（加 `.json`）。
    pub id: String,
    pub name: String,
    /// 最近更新时间（UTC 毫秒时间戳），用于排序与展示。
    pub updated_at: u64,
}

/// 建模目录的轻量元数据（目录名即磁盘目录名，无额外清单文件，与算子分类一致）。
#[derive(Debug, Clone)]
pub struct ModelFolderMeta {
    /// 相对 models 根目录的路径（`/` 分隔），作为目录的唯一标识。
    pub id: String,
    /// 目录显示名（路径末段）。
    pub name: String,
    /// 目录内（递归所有层级）建模文件数量，用于卡片副标题展示。
    pub model_count: usize,
}

/// 建模列表面板中的一个条目：目录 或 建模。
#[derive(Debug, Clone)]
pub enum ModelEntry {
    Folder(ModelFolderMeta),
    Model(DagModelMeta),
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

/// 拼接相对路径 id：`prefix` 为空时直接取 `name`，否则 `prefix/name`。
fn join_id(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", prefix, name)
    }
}

/// 把 `/` 分隔的相对路径安全地拼到 `base` 下。
///
/// 任何一段为空、为 `.`/`..`、含反斜杠都视为非法（防止路径逃逸 models 根），
/// 返回 `None`。
fn resolve_relative(base: &PathBuf, rel: &str) -> Option<PathBuf> {
    let mut p = base.clone();
    if !rel.is_empty() {
        for seg in rel.split('/') {
            if seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\') {
                return None;
            }
            p.push(seg);
        }
    }
    Some(p)
}

/// 目录 id → 磁盘目录路径；根目录（`""`）为 models 目录本身。
fn folder_dir(folder: &str) -> Option<PathBuf> {
    resolve_relative(&get_models_directory(), folder)
}

/// 单个建模文件路径：`<models_dir>/<id 中的 '/' 转为目录分隔>.json`。
fn model_path(id: &str) -> Option<PathBuf> {
    Some(
        resolve_relative(&get_models_directory(), id)?
            .with_extension("json"),
    )
}

/// 软删除（回收）后的文件扩展名：`<id>.json` → `<id>.deleted`。
///
/// 列表扫描仅收录 `.json` 文件 / 跳过名字以 `.deleted` 结尾的目录，故软删除的
/// 文件与目录不会出现在历史列表中，等价于从 UI 移除；但磁盘内容仍保留，
/// 可手动改回 `.json` / 去掉 `.deleted` 后缀恢复。
const DELETED_EXTENSION: &str = "deleted";

/// 目录/文件名是否为软删除残留（名字以 `.deleted` 结尾），扫描时跳过。
fn is_deleted_name(name: &str) -> bool {
    name.ends_with(".deleted")
}

/// 递归收集 `dir` 下全部建模元数据；`prefix` 是 `dir` 相对 models 根的 id。
///
/// 解析失败的文件会被跳过并 `eprintln` 提示，不影响其余建模列出。
fn collect_models(dir: &PathBuf, prefix: &str, out: &mut Vec<DagModelMeta>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if is_deleted_name(name) {
            continue;
        }
        if path.is_dir() {
            collect_models(&path, &join_id(prefix, name), out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                match fs::read_to_string(&path) {
                    Ok(content) => {
                        match serde_json::from_str::<DagModelRecord>(&content) {
                            Ok(rec) => out.push(DagModelMeta {
                                id: join_id(prefix, stem),
                                name: rec.name,
                                updated_at: rec.updated_at,
                            }),
                            Err(e) => {
                                eprintln!("解析建模文件失败 ({}): {}", path.display(), e);
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("读取建模文件失败 ({}): {}", path.display(), e);
                    }
                }
            }
        }
    }
}

/// 扫描 models 根目录下（递归所有子目录）的全部建模，按 `updated_at` 倒序。
///
/// 用于需要"全部建模"扁平列表的场景；侧栏按目录浏览请用 [`list_entries_in`]。
pub fn list_models() -> Vec<DagModelMeta> {
    let mut metas = Vec::new();
    collect_models(&get_models_directory(), "", &mut metas);
    metas.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    metas
}

/// 列出指定目录（`folder` 为 `/` 分隔的相对 id，`""` 表示根目录）下一层的
/// 子目录与建模：目录在前（按名称排序），建模在后（按更新时间倒序）。
///
/// 目录不存在或 id 非法时返回空列表。
pub fn list_entries_in(folder: &str) -> Vec<ModelEntry> {
    let Some(dir) = folder_dir(folder) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut folders = Vec::new();
    let mut models = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if is_deleted_name(name) {
            continue;
        }
        if path.is_dir() {
            // 递归统计目录内建模数量（解析失败/软删除目录自动跳过）
            let mut nested = Vec::new();
            collect_models(&path, "", &mut nested);
            folders.push(ModelFolderMeta {
                id: join_id(folder, name),
                name: name.to_string(),
                model_count: nested.len(),
            });
        } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(content) = fs::read_to_string(&path) {
                    if let Ok(rec) = serde_json::from_str::<DagModelRecord>(&content) {
                        models.push(DagModelMeta {
                            id: join_id(folder, stem),
                            name: rec.name,
                            updated_at: rec.updated_at,
                        });
                    } else {
                        eprintln!("解析建模文件失败: {}", path.display());
                    }
                }
            }
        }
    }

    folders.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    models.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let mut entries_out: Vec<ModelEntry> = Vec::new();
    entries_out.extend(folders.into_iter().map(ModelEntry::Folder));
    entries_out.extend(models.into_iter().map(ModelEntry::Model));
    entries_out
}

/// 校验用户输入的目录名（单段，不含路径分隔），非法时返回中文错误说明。
fn validate_folder_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("目录名称不能为空".to_string());
    }
    if name.len() > 100 {
        return Err("目录名称过长（最多 100 个字符）".to_string());
    }
    if name == "." || name == ".." {
        return Err("目录名称不合法".to_string());
    }
    const FORBIDDEN: &[char] = &['\\', '/', ':', '*', '?', '"', '<', '>', '|'];
    if let Some(c) = name
        .chars()
        .find(|c| FORBIDDEN.contains(c) || c.is_control())
    {
        return Err(format!("目录名称包含非法字符：{}", c));
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err("目录名称不能以点号或空格结尾".to_string());
    }
    if is_deleted_name(name) {
        return Err("目录名称不能以 .deleted 结尾".to_string());
    }
    Ok(())
}

/// 在 `parent` 目录（`""` 为根目录）下新建名为 `name` 的子目录。
///
/// 成功返回新目录的完整 id。重名、非法名或磁盘错误返回中文错误说明。
pub fn create_folder(parent: &str, name: &str) -> Result<String, String> {
    let name = name.trim();
    validate_folder_name(name)?;
    let parent_dir = folder_dir(parent).ok_or("父目录路径非法")?;
    let target = parent_dir.join(name);
    if target.exists() {
        return Err(format!("已存在同名目录「{}」", name));
    }
    fs::create_dir_all(&target).map_err(|e| format!("创建目录失败: {}", e))?;
    Ok(join_id(parent, name))
}

/// 重命名目录（仅改路径末段，不移动层级）。
///
/// 成功返回重命名后的新 id；非法 id / 名称冲突 / 磁盘错误返回中文错误说明。
pub fn rename_folder(id: &str, new_name: &str) -> Result<String, String> {
    let new_name = new_name.trim();
    validate_folder_name(new_name)?;
    let Some(path) = folder_dir(id) else {
        return Err("目录路径非法".to_string());
    };
    if !path.is_dir() {
        return Err("目录不存在".to_string());
    }
    // 名字未变化：视为成功的空操作（避免与自身冲突报"重名"）
    if path
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|cur| cur == new_name)
    {
        return Ok(id.to_string());
    }
    let target = path.with_file_name(new_name);
    if target.exists() {
        return Err(format!("已存在同名目录「{}」", new_name));
    }
    fs::rename(&path, &target).map_err(|e| format!("重命名目录失败: {}", e))?;
    let parent = id.rfind('/').map(|i| &id[..i]).unwrap_or("");
    Ok(join_id(parent, new_name))
}

/// 软删除目录：将目录整体重命名为同级的 `<name>.deleted`。
///
/// 目录内所有建模随目录一并从列表消失但磁盘保留；已存在同名
/// `.deleted` 残留时先永久清理再重命名（与建模软删除策略一致）。
/// 根目录（id 为空）不允许删除。
pub fn delete_folder(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("不能删除根目录".to_string());
    }
    let Some(path) = folder_dir(id) else {
        return Err("目录路径非法".to_string());
    };
    if !path.exists() {
        return Ok(());
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("目录路径非法")?;
    let deleted_path = path.with_file_name(format!("{}.{}", name, DELETED_EXTENSION));
    if deleted_path.exists() {
        let _ = fs::remove_dir_all(&deleted_path);
    }
    fs::rename(&path, &deleted_path).map_err(|e| format!("删除目录失败: {}", e))
}

/// 生成在指定目录（`""` 为根目录）下的新建模 id：根目录为纯 uuid，
/// 目录内为 `目录/<uuid>`。
pub fn new_model_id_in(folder: &str) -> String {
    join_id(folder, &new_model_id())
}

/// 递归列出 models 根目录下所有目录（不含根目录本身）。
///
/// 返回的 `ModelFolderMeta.id` 为完整相对路径（`/` 分隔），按字典序排序，
/// 便于「移动到目录」对话框展示整棵目录树。软删除目录（`.deleted` 结尾）跳过。
pub fn list_all_folders() -> Vec<ModelFolderMeta> {
    let root = get_models_directory();
    let mut out = Vec::new();
    collect_folders(&root, "", &mut out);
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// 递归收集 `dir` 下的目录元数据；`prefix` 为 `dir` 相对 models 根的 id。
fn collect_folders(dir: &PathBuf, prefix: &str, out: &mut Vec<ModelFolderMeta>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if is_deleted_name(name) || !path.is_dir() {
            continue;
        }
        let id = join_id(prefix, name);
        let mut nested = Vec::new();
        collect_models(&path, "", &mut nested);
        out.push(ModelFolderMeta {
            id: id.clone(),
            name: name.to_string(),
            model_count: nested.len(),
        });
        collect_folders(&path, &id, out);
    }
}

/// 将指定建模移动到目标目录下：把 `<id>.json` 重命名到 `target_folder/<stem>.json`，
/// 并以磁盘路径覆写记录内 `id`（保持与 [`load_model`] 一致的"路径即 id"约定）。
///
/// - `target_folder` 为 `""` 表示移回根目录。
/// - 若目标目录下已存在同 stem 的建模（UUID 碰撞，理论上不可能），返回错误。
/// - 源文件不存在或 id 非法返回错误说明。
///
/// 成功返回移动后的新 id。
pub fn move_model(id: &str, target_folder: &str) -> Result<String, String> {
    let src = model_path(id).ok_or("建模路径非法")?;
    if !src.exists() {
        return Err("建模不存在".to_string());
    }
    let stem = src
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or("建模路径非法")?
        .to_string();
    // 目标目录必须已存在（根目录除外，根目录即 models 目录本身必然存在）
    if !target_folder.is_empty() {
        let Some(t_dir) = folder_dir(target_folder) else {
            return Err("目标目录路径非法".to_string());
        };
        if !t_dir.is_dir() {
            return Err("目标目录不存在".to_string());
        }
    }
    let new_id = join_id(target_folder, &stem);
    // 同目录移动（目标 = 源）视为成功空操作
    if new_id == id {
        return Ok(new_id);
    }
    let Some(dst) = model_path(&new_id) else {
        return Err("目标路径非法".to_string());
    };
    if dst.exists() {
        return Err("目标目录已存在同名建模".to_string());
    }
    fs::rename(&src, &dst).map_err(|e| format!("移动建模失败: {}", e))?;
    // 覆写记录内 id，保证 load_model / save_model 一致
    if let Ok(content) = fs::read_to_string(&dst) {
        if let Ok(mut rec) = serde_json::from_str::<DagModelRecord>(&content) {
            rec.id = new_id.clone();
            if let Ok(json) = serde_json::to_string_pretty(&rec) {
                let _ = fs::write(&dst, &json);
            }
        }
    }
    Ok(new_id)
}

/// 加载指定 id 的建模完整记录。文件不存在或解析失败时返回 `None`。
///
/// 返回记录的 `id` 以磁盘相对路径为准（防止 JSON 内 id 与文件位置不一致）。
pub fn load_model(id: &str) -> Option<DagModelRecord> {
    let path = model_path(id)?;
    let content = fs::read_to_string(&path).ok()?;
    let mut rec: DagModelRecord = serde_json::from_str(&content).ok()?;
    rec.id = id.to_string();
    Some(rec)
}

/// 保存（或覆盖）指定 id 的建模：写 `<id>.json`，刷新 `updated_at`。
///
/// id 含目录层级时自动 `create_dir_all` 建齐父目录（新建模落在目录内的场景）。
/// 失败仅 `eprintln` 提示，不中断 UI 流程（调用方在内存中已持有最新 graph）。
pub fn save_model(id: &str, name: &str, graph: &DagGraph) {
    let Some(path) = model_path(id) else {
        eprintln!("建模 id 非法，放弃保存: {}", id);
        return;
    };
    let record = DagModelRecord {
        id: id.to_string(),
        name: name.to_string(),
        graph: graph.clone(),
        updated_at: now_millis(),
    };
    match serde_json::to_string_pretty(&record) {
        Ok(json) => {
            if let Some(parent) = path.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    eprintln!("创建建模目录失败: {} (路径: {})", e, parent.display());
                    return;
                }
            }
            if let Err(e) = fs::write(&path, &json) {
                eprintln!("写入建模文件失败: {} (路径: {})", e, path.display());
            }
        }
        Err(e) => eprintln!("序列化建模失败 (id={}): {}", id, e),
    }
}

/// 软删除指定 id 的建模：将 `<id>.json` 重命名为 `<id>.deleted`。
///
/// 不是真正删除磁盘文件——列表扫描仅收录 `.json`，故 `.deleted` 文件
/// 不会出现在历史列表中，等价于从 UI 移除；需要时手动改回 `.json` 即可恢复。
/// 源文件不存在视为成功；若已存在同名 `.deleted` 残留则先清理再重命名，
/// 避免 Windows 上 `rename` 因目标已存在而失败。
pub fn delete_model(id: &str) {
    let Some(path) = model_path(id) else {
        eprintln!("建模 id 非法，放弃删除: {}", id);
        return;
    };
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
    let (Some(path), Some(deleted_path)) = (model_path(id), model_path(id).map(|p| p.with_extension(DELETED_EXTENSION))) else {
        return false;
    };
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
