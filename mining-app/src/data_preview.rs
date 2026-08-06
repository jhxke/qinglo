//! 算子输出数据预览缓存。
//!
//! 算子执行完成后，服务端只回传每个输出端口的前 [`MAX_PREVIEW_ROWS`] 行预览数据
//! （已截断）+ 真实行数。客户端将这些预览以 JSON 形式写入
//! [`crate::config::get_cache_directory`] 指向的工作区 `cache/` 目录，
//! 供右键「数据预览」读取展示。
//!
//! 设计要点：
//! - 截断在服务端执行阶段完成，客户端不再触碰完整的内存输出；
//! - 完整数据始终保留在服务端内存中由算子间传递（指针语义），不出服务端；
//! - 写入失败不影响算子执行结果（best-effort）。

use operator_executor_client::PortData;
use serde::{Deserialize, Serialize};

/// 单个节点预览时最多保存/展示的行数。
pub const MAX_PREVIEW_ROWS: usize = 1000;

/// 节点预览缓存结构（序列化为 JSON 落盘）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreviewData {
    /// 节点 ID（缓存文件以此命名）。
    pub node_id: String,
    /// 算子名称（仅用于展示，缺失时回退到节点 ID）。
    pub node_name: String,
    /// 各输出端口的数据（已由服务端截断到前 1000 行）。
    pub outputs: Vec<PortData>,
    /// 原始首个 DataFrame 的行数，用于提示「仅展示前 N 行」。
    pub original_row_count: usize,
    /// 保存时间（UTC+8 格式字符串）。
    pub saved_at: String,
}

/// 将服务端已截断的预览数据写入 cache 目录。
///
/// `previews` 应为服务端回传的 `DagNodeResult.outputs`（每个端口已截断到
/// [`MAX_PREVIEW_ROWS`] 行）；`original_row_count` 为服务端回传的真实行数
/// （`DagNodeResult.output_row_count`）。本函数不再二次截断。
///
/// 文件名形如 `{node_id}.json`。`node_id` 一般是 UUID，但仍做文件名安全过滤。
/// 任何 I/O 或序列化错误都向上返回，由调用方决定是否记录日志。
pub fn save_preview_from_truncated(
    node_id: &str,
    node_name: &str,
    previews: &[PortData],
    original_row_count: usize,
) -> Result<(), String> {
    let cache_dir = crate::config::get_cache_directory();

    let data = PreviewData {
        node_id: node_id.to_string(),
        node_name: node_name.to_string(),
        outputs: previews.to_vec(),
        original_row_count,
        saved_at: format_local_timestamp(),
    };

    let file_name = format!("{}.json", sanitize_filename(node_id));
    let path = cache_dir.join(&file_name);
    let content = serde_json::to_string_pretty(&data).map_err(|e| format!("序列化预览数据失败: {}", e))?;
    std::fs::write(&path, content).map_err(|e| format!("写入预览缓存失败 ({}): {}", path.display(), e))?;
    Ok(())
}

/// 读取节点的预览缓存。文件不存在或解析失败时返回 `None`。
pub fn load_preview_cache(node_id: &str) -> Option<PreviewData> {
    let cache_dir = crate::config::get_cache_directory();
    let path = cache_dir.join(format!("{}.json", sanitize_filename(node_id)));
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// 过滤文件名中的非法字符，仅保留字母、数字、`-`、`_`。
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 格式化当前时间为 UTC+8 的 `YYYY-MM-DD HH:MM:SS`。
fn format_local_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            let total_seconds = d.as_secs() + 8 * 3600;
            let secs = total_seconds % 60;
            let mins = (total_seconds / 60) % 60;
            let hours = (total_seconds / 3600) % 24;
            let days = total_seconds / 86400;
            // 简化日期：从 1970-01-01 起按天累加得到年月日（避免引入 chrono）。
            let (year, month, day) = days_to_ymd(days);
            format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", year, month, day, hours, mins, secs)
        })
        .unwrap_or_else(|_| "unknown".to_string())
}

/// 将「1970-01-01 至今的天数」转换为 `(年, 月, 日)`（公历）。
fn days_to_ymd(days_since_epoch: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    let mut days = days_since_epoch;

    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }

    let month_lens = if is_leap(year) {
        [31u64, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31u64, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1u64;
    for &mlen in &month_lens {
        if days < mlen {
            break;
        }
        days -= mlen;
        month += 1;
    }

    (year, month, days + 1)
}

fn is_leap(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}
