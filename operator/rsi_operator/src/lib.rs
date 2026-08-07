use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::{DataFrame, DataType, PortData};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_from_c,
    c_set_last_error, TYPE_NULL,
};
use std::ffi::{CStr, CString, c_char};
use serde::{Deserialize, Serialize};

/// RSI 算子参数结构体
///
/// `rsi_periods` 为逗号分隔的周期串（如 "5,10,14"），与前端 String 参数渲染一致；
/// 留空表示不计算（原样返回输入）。
/// `source_column` 指定计算所用源列名，留空回退默认 "close"。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct RsiParams {
    /// RSI 周期列表，逗号分隔，如 "5,10,14"；空则不计算
    #[serde(default)]
    pub rsi_periods: String,
    /// 指标计算所用的源列名；空则回退默认 "close"
    #[serde(default)]
    pub source_column: String,
}

/// 解析参数 JSON 为 RsiParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> RsiParams {
    if params_json.is_empty() {
        return RsiParams::default();
    }
    match serde_json::from_str::<RsiParams>(params_json) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("解析参数 JSON 失败: {}", e);
            RsiParams::default()
        }
    }
}

/// 解析逗号分隔的周期字符串，如 "5,10,14" -> vec![5, 10, 14]；非正数自动过滤
fn parse_periods(periods_str: &str) -> Vec<usize> {
    periods_str
        .split(',')
        .filter_map(|s| {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                trimmed.parse::<usize>().ok().filter(|&p| p > 0)
            }
        })
        .collect()
}

/// 将 Option<f64> 序列前向填充为 f64 序列（None 用上一个有效值代替，开头全空则用 0.0）
///
/// RSI 需要连续数值序列；价格列通常无空值，前向填充仅作兜底。
fn to_f64_filled(values: &[Option<f64>]) -> Vec<f64> {
    let mut out = Vec::with_capacity(values.len());
    let mut last = 0.0f64;
    for v in values {
        match v {
            Some(x) => {
                last = *x;
                out.push(*x);
            }
            None => out.push(last),
        }
    }
    out
}

/// 由平均涨幅 / 平均跌幅计算 RSI 值
fn rsi_from(avg_gain: f64, avg_loss: f64) -> f64 {
    if avg_loss == 0.0 {
        100.0
    } else {
        let rs = avg_gain / avg_loss;
        100.0 - 100.0 / (1.0 + rs)
    }
}

/// 计算 RSI (相对强弱指数)，采用 Wilder 平滑
///
/// - 前 `period` 个位置返回 None（无足够的变化数据）
/// - 第 `period` 个位置为首根 RSI（基于前 period 个变化的 SMA）
/// - 之后用 Wilder 平滑：`avg[i] = (avg[i-1] * (N-1) + diff[i]) / N`
/// - 当 avg_loss == 0 时 RSI = 100（无下跌）
fn compute_rsi(values: &[Option<f64>], period: usize) -> Vec<Option<f64>> {
    let n = values.len();
    let mut result = vec![None; n];
    if period == 0 || n <= period {
        return result;
    }

    let filled = to_f64_filled(values);

    // 逐日涨跌幅
    let mut gains = vec![0.0f64; n];
    let mut losses = vec![0.0f64; n];
    for i in 1..n {
        let diff = filled[i] - filled[i - 1];
        if diff >= 0.0 {
            gains[i] = diff;
        } else {
            losses[i] = -diff;
        }
    }

    // 首个平均涨跌：changes[1..=period] 的 SMA
    let mut avg_gain: f64 = gains[1..=period].iter().sum::<f64>() / period as f64;
    let mut avg_loss: f64 = losses[1..=period].iter().sum::<f64>() / period as f64;

    result[period] = Some(rsi_from(avg_gain, avg_loss));

    // Wilder 平滑递推
    let p = period as f64;
    for i in (period + 1)..n {
        avg_gain = (avg_gain * (p - 1.0) + gains[i]) / p;
        avg_loss = (avg_loss * (p - 1.0) + losses[i]) / p;
        result[i] = Some(rsi_from(avg_gain, avg_loss));
    }

    result
}

/// 解析源列名；空串或纯空格回退默认 "close"
///
/// DataFrameArray 不一定含有标准的 open/high/low/close 列，
/// 通过该函数把空配置统一回退到 "close"，保持向后兼容。
fn resolve_source_column(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "close"
    } else {
        trimmed
    }
}

/// 从 DataFrame 中提取指定列的 f64 数据；列不存在或非 Float64 类型时返回 None
fn extract_column_values(df: &DataFrame, column_name: &str) -> Option<Vec<Option<f64>>> {
    let col = df.column(column_name)?;
    if !matches!(col.data_type, DataType::Float64) {
        return None;
    }
    Some(col.to_f64_vec())
}

/// 对单个 DataFrame **就地**追加 RSI 列，避免全量 clone。
///
/// 优化点：
/// - **就地修改**：直接在输入 DataFrame 上 `add_column`，不再 `clone()` 整个表
/// - 源列不存在或类型不匹配时仅跳过并打印告警，不影响其他 DataFrame
/// - 参数为空时，原样返回（不追加列）
/// - 支持多周期：对 `rsi_periods` 中解析出的每个 period 依次追加 rsi_N 列
fn apply_rsi_inplace(df: &mut DataFrame, params: &RsiParams) {
    if params.rsi_periods.is_empty() {
        println!("RSI算子: 未配置任何 RSI 周期，原样返回输入");
        return;
    }

    let source_col = resolve_source_column(&params.source_column);
    let source_values = match extract_column_values(df, source_col) {
        Some(v) => v,
        None => {
            let existing_cols: Vec<&str> = df.columns.iter().map(|c| c.name.as_str()).collect();
            eprintln!(
                "RSI算子: 源列 '{}' 不存在或类型不匹配 (现有列: {:?})，跳过 RSI",
                source_col, existing_cols
            );
            return;
        }
    };

    let periods = parse_periods(&params.rsi_periods);
    if periods.is_empty() {
        eprintln!(
            "RSI算子: RSI 周期串 '{}' 未解析出有效周期，跳过 RSI",
            params.rsi_periods
        );
        return;
    }

    for &period in &periods {
        let rsi_values = compute_rsi(&source_values, period);
        let col_name = format!("rsi_{}", period);
        df.add_column(DataFrame::new_float64_column(&col_name, rsi_values));
    }
}

/// RSI 算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立做 RSI 计算，
/// 输出同样为 DataFrameArray（顺序与输入一致）。
/// 为兼容旧 DAG，单个 DataFrame 输入会被包装为单元素数组处理，
/// 输出仍为 DataFrameArray（单元素）。
///
/// 返回值:
/// - 0: 成功（包括参数为空、源列缺失时静默原样返回的情形）
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
        eprintln!("{}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };

    let params = parse_params(params_json_str);

    if input_count == 0 || inputs.is_null() {
        let err_msg = "缺少输入数据".to_string();
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("{}", err_msg);
        return -3;
    }

    // 从输入中提取 DataFrame 数组（兼容单个 DataFrame）
    let input_pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
    let input_dfs: Vec<DataFrame> = match input_pd {
        PortData::DataFrame(df) => vec![df],
        PortData::DataFrameArray(dfs) => dfs,
        _ => {
            let err_msg = "输入不是 DataFrame / DataFrameArray 类型".to_string();
            let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("{}", err_msg);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err_msg = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("{}", err_msg);
        return -5;
    }

    let rsi_configured = !params.rsi_periods.is_empty();
    println!(
        "RSI算子: RSI={}, 输入 DataFrame 数量={}, 首个行数={}",
        if rsi_configured { "已配置" } else { "未配置" },
        input_dfs.len(),
        input_dfs[0].row_count
    );

    // 逐个 DataFrame 就地做 RSI 计算（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("RSI算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_rsi_inplace(df, &params);
    }

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    // 输出统一为 DataFrameArray（与端口声明一致）
    let port_data = PortData::DataFrameArray(out_dfs);

    if !outputs.is_null() && output_cap > 0 {
        // 使用 owned 变体，避免每个 DataFrame 被 clone
        let c_pd = operator_runtime::c_abi::portdata_to_c_owned(port_data);
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

/// 获取 RSI 算子版本
#[no_mangle]
pub extern "C" fn rsi_operator_version() -> *const c_char {
    b"0.2.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
