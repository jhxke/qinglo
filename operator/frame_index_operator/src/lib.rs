//! 数组取帧算子
//!
//! 输入一个 **DataFrameArray**，按参数 `frame_index` 指定的下标取出其中一帧，
//! 输出单个 **DataFrame**。支持 Python 风格的负下标（`-1` = 最后一帧）。
//!
//! ## 设计要点
//!
//! - **零拷贝取帧**：消费输入数组的所有权后用 `into_iter().nth(idx)` 移动
//!   目标帧，不 clone 任何列数据；其余帧随 Vec drop 释放。
//! - **负下标**：`-k` 解析为 `len - k`，越界（含正向）统一返回错误并附带
//!   数组长度，便于排查。
//! - **严格输入类型**：仅接受 DataFrameArray；单个 DataFrame 无需取帧，
//!   传入时返回类型错误。

use std::ffi::{c_char, CStr, CString};

use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::PortData;

/// 设置最近一次错误（同时打印到 stderr 便于诊断）。
fn set_err(msg: &str) {
    eprintln!("[frame_index_operator] {}", msg);
    let c = CString::new(msg).unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

/// 清空最近一次错误（成功路径调用）。
fn clear_err() {
    let c = CString::new("").unwrap_or_default();
    c_set_last_error(c.as_ptr());
}

/// 从参数 JSON 解析帧下标 `frame_index`。
///
/// 前端对 Int 类型参数序列化为 JSON 数字；同时兼容字符串形式与字段缺失
/// （缺失/Null/空 JSON 时取默认值 0）。非法值返回错误说明。
fn parse_frame_index(params_json: &str) -> Result<i64, String> {
    let trimmed = params_json.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }
    let value = serde_json::from_str::<serde_json::Value>(trimmed)
        .map_err(|e| format!("参数 JSON 非法: {}", e))?;
    match value.get("frame_index") {
        None | Some(serde_json::Value::Null) => Ok(0),
        Some(serde_json::Value::Number(n)) => n
            .as_i64()
            .ok_or_else(|| format!("frame_index 超出 64 位整数范围: {}", n)),
        Some(serde_json::Value::String(s)) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("frame_index 不是合法整数: '{}'", s)),
        Some(other) => Err(format!("frame_index 必须是整数，实际为: {}", other)),
    }
}

/// 把可能为负的下标解析成长度为 `len` 的数组中的实际位置。
///
/// - `k >= 0`：要求 `k < len`
/// - `-k`：解析为 `len - k`，要求 `k <= len`
fn resolve_index(raw: i64, len: usize) -> Result<usize, String> {
    if raw < 0 {
        let resolved = len as i64 + raw;
        if resolved < 0 {
            return Err(format!(
                "帧下标 {} 越界（数组共 {} 帧，负下标绝对值不能超过数组长度）",
                raw, len
            ));
        }
        Ok(resolved as usize)
    } else {
        let idx = raw as usize;
        if idx >= len {
            return Err(format!(
                "帧下标 {} 越界（数组共 {} 帧，有效下标为 0..{} 或负下标 -1..-{}）",
                raw,
                len,
                len as i64 - 1,
                len
            ));
        }
        Ok(idx)
    }
}

/// 数组取帧算子的执行函数（C ABI）。
///
/// 返回值:
/// - 0:  成功
/// - -1: runtime 加载失败
/// - -2: 参数 frame_index 非法
/// - -3: 缺少输入数据（端口未连接 / TYPE_NULL）
/// - -4: 输入不是 DataFrameArray 类型
/// - -5: 输入 DataFrameArray 为空（0 帧）
/// - -6: 帧下标越界
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
    let raw_index = match parse_frame_index(params_str) {
        Ok(v) => v,
        Err(e) => {
            set_err(&e);
            return -2;
        }
    };

    if input_count == 0 || inputs.is_null() {
        set_err("缺少输入数据（需要 1 个 DataFrameArray 输入）");
        return -3;
    }

    // TYPE_NULL 经 portdata_from_c 会退化为 Float(0.0)，需提前拦截
    let input_ptr = unsafe { inputs.add(0) };
    if input_ptr.is_null() || unsafe { (*input_ptr).type_tag } == TYPE_NULL {
        set_err("输入端口未连接或无数据（TYPE_NULL）");
        return -3;
    }

    let input_pd = unsafe { portdata_from_c(input_ptr as *mut CPortData) };
    let frames = match input_pd {
        PortData::DataFrameArray(dfs) => dfs,
        other => {
            set_err(&format!(
                "输入类型错误：需要 DataFrameArray，实际为 {}（单个 DataFrame 无需取帧，可直连下游）",
                other.type_name()
            ));
            return -4;
        }
    };

    let frame_count = frames.len();
    if frame_count == 0 {
        set_err("输入 DataFrameArray 为空（0 帧），无法取帧");
        return -5;
    }

    let index = match resolve_index(raw_index, frame_count) {
        Ok(idx) => idx,
        Err(e) => {
            set_err(&e);
            return -6;
        }
    };

    // 移动目标帧的所有权，零拷贝；其余帧随 frames drop
    let df = frames
        .into_iter()
        .nth(index)
        .expect("下标已通过 resolve_index 边界校验");

    println!(
        "[frame_index_operator] 取第 {} 帧（参数 frame_index={}，数组共 {} 帧，{} 行 × {} 列）",
        index,
        raw_index,
        frame_count,
        df.row_count,
        df.columns.len()
    );

    clear_err();
    if !outputs.is_null() && output_cap > 0 {
        let c_pd = portdata_to_c_owned(PortData::DataFrame(df));
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

/// 获取数组取帧算子版本。
#[no_mangle]
pub extern "C" fn frame_index_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
