//! 列转字符串流算子
//!
//! 输入一个 **DataFrame**，参数指定一列，把该列逐行转成字符串（list<string>），
//! 以每行一个 `PortData::String` chunk 的形式流式输出。导出 5 个流式 C ABI 符号
//! （`execute_operator_stream_start/push/push_end/next/end`），同时导出批量
//! `execute_operator` 作为 `stream=false` 时的兜底（输出单列 String DataFrame）。
//!
//! ## 设计要点
//!
//! - **流式源算子（head）**：物化的 DataFrame 在 `stream_start` 时被消费进 handle，
//!   之后 `next` 仅推进游标取一行，零额外批量分配；不支持流式上游（`push` 返回错误）。
//! - **类型统一转字符串**：Float64/Int64/String/Bool 各自按规则转成 `String`，
//!   下游收到的 chunk 类型恒为 `PortData::String`。
//! - **null 跳过**：null 行不产出 chunk，跳过数在流结束时打印到 stderr。
//! - **三态 `next`**：`0`=有 chunk；`1`=已到列尾（EOF）；`<0`=错误。

use std::ffi::{c_char, c_void, CStr, CString};

use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::{DataFrame, DataType, PortData};
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use serde::{Deserialize, Serialize};

/// 算子参数（字段名与 `operator.json` 的 Param 项一致）。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ColumnStreamParams {
    /// 要转成字符串流的列名（必填）
    #[serde(default)]
    pub source_column: String,
}

/// 解析参数 JSON；空串或非法 JSON 返回默认值。
fn parse_params(params_json: &str) -> ColumnStreamParams {
    if params_json.is_empty() {
        return ColumnStreamParams::default();
    }
    match serde_json::from_str::<ColumnStreamParams>(params_json) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("[column_stream_operator] 解析参数 JSON 失败: {}，使用默认值", e);
            ColumnStreamParams::default()
        }
    }
}

/// 设置最近一次错误（同时打印到 stderr 便于诊断）。
fn set_err(msg: &str) {
    eprintln!("[column_stream_operator] {}", msg);
    let c = CString::new(msg).unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

/// 清空最近一次错误（成功路径调用）。
fn clear_err() {
    let c = CString::new("").unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

/// 去除列名两端空白；空串返回 None（参数未配置）。
fn resolve_column_name(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// 在 DataFrame 中查找列的下标；不存在时返回错误信息（附带现有列名便于排查）。
fn find_column_index(df: &DataFrame, name: &str) -> Result<usize, String> {
    df.columns
        .iter()
        .position(|c| c.name == name)
        .ok_or_else(|| {
            let existing: Vec<&str> = df.columns.iter().map(|c| c.name.as_str()).collect();
            format!("列 '{}' 不存在 (现有列: {:?})", name, existing)
        })
}

/// 把列中第 `i` 行的值转成字符串；null（或不支持的 Null 类型）返回 `None`。
///
/// 转换规则：
/// - Float64：Rust 默认 `Display`（`10.0` → `"10"`，`1.5` → `"1.5"`）
/// - Int64：十进制整数
/// - String：原样
/// - Bool：`"true"` / `"false"`
fn column_value_to_string(col: &operator_runtime::ColumnData, i: usize) -> Option<String> {
    match col.data_type {
        DataType::Float64 => col.get_f64(i).map(|v| v.to_string()),
        DataType::Int64 => col.get_i64(i).map(|v| v.to_string()),
        DataType::String => col.get_string(i).map(|s| s.to_string()),
        DataType::Bool => col.get_bool(i).map(|b| b.to_string()),
        DataType::Null => None,
    }
}

/// 把整列转成 `Vec<Option<String>>`（批量兜底使用，null 保留为 `None`）。
fn stringify_column(
    df: &DataFrame,
    col_idx: usize,
) -> Vec<Option<String>> {
    let col = &df.columns[col_idx];
    (0..df.row_count)
        .map(|i| column_value_to_string(col, i))
        .collect()
}

// ===== 流式 handle =====

/// 流式执行的运行时状态（`stream_start` 返回的 `*mut c_void` 指向它）。
///
/// 持有消费来的 DataFrame 所有权与目标列下标，`next` 时按游标逐行取值，
/// 不复制整列数据。
struct ColumnStream {
    /// 输入数据表（start 时消费取得所有权）
    df: DataFrame,
    /// 目标列在 `df.columns` 中的下标
    col_idx: usize,
    /// 下一个待读取的行下标
    cursor: usize,
    /// 已跳过的 null 行数
    null_skipped: usize,
    /// 结束统计日志是否已打印（避免 next 重复返回 EOF 时刷屏）
    done_logged: bool,
}

impl ColumnStream {
    /// 推进游标，返回下一个非 null 行的字符串；列耗尽返回 `None`。
    fn pull(&mut self) -> Option<String> {
        let row_count = self.df.row_count;
        let col_idx = self.col_idx;
        while self.cursor < row_count {
            let i = self.cursor;
            self.cursor += 1;
            match column_value_to_string(&self.df.columns[col_idx], i) {
                Some(s) => return Some(s),
                None => self.null_skipped += 1,
            }
        }
        None
    }
}

// ===== 流式 C ABI：5 个符号 =====

/// `stream_start`：消费物化 DataFrame 输入，定位目标列，返回不透明 handle。
///
/// 返回 null = 失败（用 `c_get_last_error` 取详情）。
#[no_mangle]
pub extern "C" fn execute_operator_stream_start(
    inputs: *const CPortData,
    input_count: usize,
    params_json: *const c_char,
) -> *mut c_void {
    if let Err(e) = ensure_runtime_loaded() {
        set_err(&format!("runtime 加载失败: {}", e));
        return std::ptr::null_mut();
    }

    let params_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_str);

    let column_name = match resolve_column_name(&params.source_column) {
        Some(name) => name,
        None => {
            set_err("缺少必需参数 source_column（要转为字符串流的列名）");
            return std::ptr::null_mut();
        }
    };

    if input_count == 0 || inputs.is_null() {
        set_err("缺少输入数据（需要 1 个 DataFrame 输入）");
        return std::ptr::null_mut();
    }

    // 消费（take）输入端口 0 的 owned 数据，与 SDK 释放契约一致
    let input_pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
    let df = match input_pd {
        PortData::DataFrame(df) => df,
        other => {
            set_err(&format!(
                "输入类型错误：需要 DataFrame，实际为 {}（本算子不接受 DataFrameArray）",
                other.type_name()
            ));
            return std::ptr::null_mut();
        }
    };

    let col_idx = match find_column_index(&df, column_name) {
        Ok(idx) => idx,
        Err(e) => {
            set_err(&e);
            return std::ptr::null_mut();
        }
    };

    let col_type = df.columns[col_idx].data_type.clone();
    println!(
        "[column_stream_operator] 开始流式输出列 '{}'（类型 {:?}，共 {} 行）",
        column_name, col_type, df.row_count
    );

    clear_err();
    let state = Box::new(ColumnStream {
        df,
        col_idx,
        cursor: 0,
        null_skipped: 0,
        done_logged: false,
    });
    Box::into_raw(state) as *mut c_void
}

/// `stream_push`：推入上游 chunk。本算子为源（head），不支持流式上游输入。
#[no_mangle]
pub extern "C" fn execute_operator_stream_push(
    _handle: *mut c_void,
    _chunk: *const CPortData,
) -> i32 {
    set_err("列转字符串流算子不支持流式上游输入（DataFrame 由 stream_start 物化传入）");
    -1
}

/// `stream_push_end`：通知上游 EOF。源算子无上游，no-op。
#[no_mangle]
pub extern "C" fn execute_operator_stream_push_end(_handle: *mut c_void) -> i32 {
    0
}

/// `stream_next`：拉下一行的字符串 chunk（三态返回）。
///
/// - `0`：有 chunk（已写入 `*out_chunk`，owned String）
/// - `1`：EOF（列已遍历完）
/// - `<0`：错误
#[no_mangle]
pub extern "C" fn execute_operator_stream_next(
    handle: *mut c_void,
    out_chunk: *mut CPortData,
) -> i32 {
    if handle.is_null() || out_chunk.is_null() {
        set_err("stream_next: handle 或 out_chunk 为 null");
        return -1;
    }
    let state: &mut ColumnStream = unsafe { &mut *(handle as *mut ColumnStream) };

    match state.pull() {
        Some(s) => {
            let c_pd = portdata_to_c_owned(PortData::String(s));
            unsafe { *out_chunk = c_pd };
            0
        }
        None => {
            if !state.done_logged {
                println!(
                    "[column_stream_operator] 流式输出结束：共 {} 行，跳过 null {} 行",
                    state.cursor, state.null_skipped
                );
                state.done_logged = true;
            }
            1
        }
    }
}

/// `stream_end`：释放 handle 及关联资源。
#[no_mangle]
pub extern "C" fn execute_operator_stream_end(handle: *mut c_void) {
    if !handle.is_null() {
        unsafe {
            drop(Box::from_raw(handle as *mut ColumnStream));
        }
    }
}

// ===== 批量兜底：execute_operator =====

/// 批量执行（`stream=false` 降级时调用）：把目标列转成 String 列，
/// 输出只含该列的单列 DataFrame（null 保留），即 list<string> 的物化形态。
///
/// 返回：`0`=成功；`-1`=runtime 加载失败；`-2`=参数/列错误；
/// `-3`=缺少输入；`-4`=输入类型错误。
#[no_mangle]
pub extern "C" fn execute_operator(
    inputs: *const CPortData,
    input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    params_json: *const c_char,
) -> i32 {
    if let Err(e) = ensure_runtime_loaded() {
        set_err(&format!("runtime 加载失败: {}", e));
        return -1;
    }

    let params_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_str);

    let column_name = match resolve_column_name(&params.source_column) {
        Some(name) => name,
        None => {
            set_err("缺少必需参数 source_column（要转为字符串的列名）");
            return -2;
        }
    };

    if input_count == 0 || inputs.is_null() {
        set_err("缺少输入数据（需要 1 个 DataFrame 输入）");
        return -3;
    }

    let input_pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
    let df = match input_pd {
        PortData::DataFrame(df) => df,
        other => {
            set_err(&format!(
                "输入类型错误：需要 DataFrame，实际为 {}（本算子不接受 DataFrameArray）",
                other.type_name()
            ));
            return -4;
        }
    };

    let col_idx = match find_column_index(&df, column_name) {
        Ok(idx) => idx,
        Err(e) => {
            set_err(&e);
            return -2;
        }
    };

    let col_name = df.columns[col_idx].name.clone();
    let string_values = stringify_column(&df, col_idx);
    let mut out_df = DataFrame::new();
    out_df.add_column(DataFrame::new_string_column(
        &col_name,
        string_values.iter().map(|v| v.as_deref()).collect(),
    ));

    clear_err();
    let port_data = PortData::DataFrame(out_df);

    if !outputs.is_null() && output_cap > 0 {
        let c_pd = portdata_to_c_owned(port_data);
        unsafe {
            *outputs = c_pd;
            if output_cap > 1 {
                *outputs.add(1) = CPortData {
                    type_tag: TYPE_NULL,
                    value: CPortValue {
                        str_ptr: std::ptr::null_mut(),
                    },
                };
            }
        }
    }

    0
}

/// 释放 C ABI PortData 内存（由调用方调用）。
#[no_mangle]
pub extern "C" fn release_port_data(data_ptr: *mut CPortData) {
    if !data_ptr.is_null() {
        operator_runtime::c_abi::c_pd_free(data_ptr);
    }
}

/// 获取算子版本。
#[no_mangle]
pub extern "C" fn column_stream_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
