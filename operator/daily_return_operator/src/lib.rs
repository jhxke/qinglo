use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, CStr, CString};

/// 当日收益率算子参数结构体
///
/// - `source_column`：计算收益率所用的价格源列名，空串回退默认 `close`。
///   支持 Float64 / Int64（Int64 会提升为 f64）。
/// - `result_column`：结果列名。为空时回退为 `daily_return`；
///   与 `source_column` 同名则就地覆盖源列，否则新增列（已存在则覆盖，源列保留）。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct DailyReturnParams {
    #[serde(default)]
    pub source_column: String,
    #[serde(default)]
    pub result_column: String,
}

/// 解析参数 JSON 为 DailyReturnParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> DailyReturnParams {
    if params_json.is_empty() {
        return DailyReturnParams::default();
    }
    match serde_json::from_str::<DailyReturnParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("当日收益率算子: 解析参数 JSON 失败: {}", e);
            DailyReturnParams::default()
        }
    }
}

/// 解析源列名；空串或纯空格回退默认 "close"
fn resolve_source_column(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        "close".to_string()
    } else {
        t.to_string()
    }
}

/// 解析结果列名；空串或纯空格回退默认 "daily_return"
fn resolve_result_column(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        "daily_return".to_string()
    } else {
        t.to_string()
    }
}

/// 计算当日收益率（环比变化率，周期=1）：
///   `out[i] = (values[i] - values[i-1]) / values[i-1]`
///
/// - 首行（i=0）无前值 → `None`。
/// - 当日价或前一日价为空 → `None`（空值传播）。
/// - 前一日价 == 0 → `None`（避免除零）。
fn compute_daily_return(values: &[Option<f64>]) -> Vec<Option<f64>> {
    let len = values.len();
    let mut result = Vec::with_capacity(len);
    for i in 0..len {
        if i == 0 {
            // 首行：无前值
            result.push(None);
            continue;
        }
        match (values[i], values[i - 1]) {
            (Some(cur), Some(prev)) => {
                if prev == 0.0 {
                    result.push(None); // 避免除零
                } else {
                    result.push(Some((cur - prev) / prev));
                }
            }
            _ => result.push(None), // 当日价或前一日价为空
        }
    }
    result
}

/// 对单个 DataFrame 就地写入当日收益率列。
///
/// - 源列需为 Float64 或 Int64（Int64 提升为 f64 计算，收益率恒为 Float64）。
/// - 源列不存在或非数值类型 → 跳过并告警，DataFrame 原样保留。
/// - `result_col == source` → 就地覆盖源列；否则覆盖同名列或新增列（源列保留）。
fn apply_daily_return(df: &mut DataFrame, source: &str, result_col: &str) {
    // 先定位源列索引与类型，结束借用后再写入，避免借用冲突
    let (src_pos, data_type) = match df.columns.iter().position(|c| c.name == source) {
        Some(p) => (p, df.columns[p].data_type.clone()),
        None => {
            eprintln!(
                "当日收益率算子: 源列 '{}' 不存在，跳过 (现有列: {:?})",
                source,
                df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
            return;
        }
    };

    // 提取 f64 序列：Float64 直接取，Int64 提升为 f64（收益率必为小数）
    let values: Vec<Option<f64>> = match data_type {
        DataType::Float64 => df.columns[src_pos].to_f64_vec(),
        DataType::Int64 => df
            .columns[src_pos]
            .to_i64_vec()
            .into_iter()
            .map(|v| v.map(|x| x as f64))
            .collect(),
        other => {
            eprintln!(
                "当日收益率算子: 源列 '{}' 类型 {:?} 不支持 (仅 Float64/Int64)，跳过",
                source, other
            );
            return;
        }
    };

    let out = compute_daily_return(&values);
    let new_col = DataFrame::new_float64_column(result_col, out);

    // 写入结果列：同名就地覆盖；异名则覆盖已有列或新增列（保留源列）
    if result_col == source {
        df.columns[src_pos] = new_col;
    } else {
        match df.columns.iter().position(|c| c.name == result_col) {
            Some(p) => df.columns[p] = new_col,
            None => df.add_column(new_col),
        }
    }
}

/// 当日收益率算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立计算当日收益率，
/// 输出同样为 DataFrameArray（顺序与输入一致）。
/// 单个 DataFrame 输入会被包装为单元素数组处理，输出仍为 DataFrameArray。
///
/// 返回值:
/// - 0:  成功
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入 DataFrame 数组为空
#[no_mangle]
pub extern "C" fn execute_operator(
    inputs: *const CPortData,
    input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    params_json: *const c_char,
) -> i32 {
    if let Err(e) = ensure_runtime_loaded() {
        let err_msg = format!("{}", e);
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("当日收益率算子: {}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_json_str);

    // 源列名：空串回退 "close"
    let source = resolve_source_column(&params.source_column);

    // 结果列名：空串回退 "daily_return"
    let result_col = resolve_result_column(&params.result_column);

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("当日收益率算子: {}", err);
        return -3;
    }

    // 从输入中提取 DataFrame 数组（兼容单个 DataFrame）
    let input_pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
    let input_dfs: Vec<DataFrame> = match input_pd {
        PortData::DataFrame(df) => vec![df],
        PortData::DataFrameArray(dfs) => dfs,
        _ => {
            let err = "输入不是 DataFrame / DataFrameArray 类型".to_string();
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("当日收益率算子: {}", err);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("当日收益率算子: {}", err);
        return -5;
    }

    println!(
        "当日收益率算子: 源列='{}', 结果列='{}', 输入 DataFrame 数量={}, 首个行数={}",
        source, result_col, input_dfs.len(), input_dfs[0].row_count
    );

    // 逐个 DataFrame 计算当日收益率（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("当日收益率算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_daily_return(df, &source, &result_col);
    }

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    // 输出统一为 DataFrameArray（与端口声明一致）
    let port_data = PortData::DataFrameArray(out_dfs);
    if !outputs.is_null() && output_cap > 0 {
        // 使用 owned 变体，避免每个 DataFrame 被 clone
        let c_pd = portdata_to_c_owned(port_data);
        unsafe {
            *outputs = c_pd;
            if output_cap > 1 {
                *outputs.add(1) = CPortData {
                    type_tag: TYPE_NULL,
                    value: CPortValue { str_ptr: std::ptr::null_mut() },
                };
            }
        }
    }

    0
}

/// 释放 C ABI PortData 内存（由调用方调用）
#[no_mangle]
pub extern "C" fn release_port_data(data_ptr: *mut CPortData) {
    if !data_ptr.is_null() {
        operator_runtime::c_abi::c_pd_free(data_ptr);
    }
}

/// 获取当日收益率算子版本
#[no_mangle]
pub extern "C" fn daily_return_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
