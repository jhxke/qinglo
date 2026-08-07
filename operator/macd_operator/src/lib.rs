use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::{DataFrame, DataType, PortData};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_from_c,
    c_set_last_error, TYPE_NULL,
};
use std::ffi::{CStr, CString, c_char};
use serde::{Deserialize, Serialize};

/// MACD 算子参数结构体
///
/// 三个周期字段统一使用 String 类型（与前端字符串输入一致），内部再解析为数值。
/// 任一非空即触发 MACD 计算，未填项回退默认 12/26/9；全空表示不计算。
/// `source_column` 指定计算所用源列名，留空回退默认 "close"。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct MacdParams {
    /// MACD 快线周期，如 "12"；未填时回退默认 12
    #[serde(default)]
    pub macd_fast: String,
    /// MACD 慢线周期，如 "26"
    #[serde(default)]
    pub macd_slow: String,
    /// MACD 信号线周期，如 "9"
    #[serde(default)]
    pub macd_signal: String,
    /// 指标计算所用的源列名；空则回退默认 "close"
    #[serde(default)]
    pub source_column: String,
}

/// 解析参数 JSON 为 MacdParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> MacdParams {
    if params_json.is_empty() {
        return MacdParams::default();
    }
    match serde_json::from_str::<MacdParams>(params_json) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("解析参数 JSON 失败: {}", e);
            MacdParams::default()
        }
    }
}

/// 解析单个正整数参数；解析失败或非正时返回 default_val
fn parse_single_period(raw: &str, default_val: usize) -> usize {
    raw.trim()
        .parse::<usize>()
        .ok()
        .filter(|&p| p > 0)
        .unwrap_or(default_val)
}

/// 将 Option<f64> 序列前向填充为 f64 序列（None 用上一个有效值代替，开头全空则用 0.0）
///
/// EMA/MACD 需要连续数值序列；价格列通常无空值，前向填充仅作兜底。
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

/// 指数移动平均 (EMA)，使用前 `period` 个值的 SMA 作为种子（与 TA-Lib 一致）
///
/// - 前 `period - 1` 个位置返回 None
/// - 第 `period - 1` 个位置为 SMA 种子
/// - 之后位置 `EMA[i] = α * x[i] + (1 - α) * EMA[i-1]`，其中 α = 2 / (period + 1)
fn ema_series(values: &[f64], period: usize) -> Vec<Option<f64>> {
    let n = values.len();
    let mut out = vec![None; n];
    if period == 0 || n < period {
        return out;
    }
    let alpha = 2.0 / (period as f64 + 1.0);

    // 种子：前 period 个值的 SMA，放在 index = period - 1
    let seed: f64 = values[..period].iter().sum::<f64>() / period as f64;
    out[period - 1] = Some(seed);
    let mut prev = seed;

    for i in period..n {
        let v = alpha * values[i] + (1.0 - alpha) * prev;
        out[i] = Some(v);
        prev = v;
    }
    out
}

/// 计算 MACD (指数平滑异同移动平均)
///
/// 返回 (macd_line, signal_line, histogram)，长度均与输入一致：
/// - `macd_line[i] = EMA_fast[i] - EMA_slow[i]`（两 EMA 均有效后才有值）
/// - `signal_line` = macd_line 有效段的 EMA(signal)
/// - `histogram[i] = macd_line[i] - signal_line[i]`
fn compute_macd(
    values: &[Option<f64>],
    fast: usize,
    slow: usize,
    signal: usize,
) -> (Vec<Option<f64>>, Vec<Option<f64>>, Vec<Option<f64>>) {
    let n = values.len();
    let mut macd_line = vec![None; n];
    let mut signal_line = vec![None; n];
    let mut hist = vec![None; n];

    if slow == 0 || n < slow {
        return (macd_line, signal_line, hist);
    }

    let filled = to_f64_filled(values);
    let ema_fast = ema_series(&filled, fast);
    let ema_slow = ema_series(&filled, slow);

    // macd_line 在两 EMA 均有效的首段连续区间内取值；收集该段用于计算信号线
    let mut macd_start: Option<usize> = None;
    let mut macd_values: Vec<f64> = Vec::new();
    for i in 0..n {
        match (ema_fast[i], ema_slow[i]) {
            (Some(ef), Some(es)) => {
                if macd_start.is_none() {
                    macd_start = Some(i);
                }
                let m = ef - es;
                macd_line[i] = Some(m);
                macd_values.push(m);
            }
            _ => {}
        }
    }

    // 信号线 = macd 有效段的 EMA(signal)，再映射回原索引
    if let Some(start) = macd_start {
        let sig = ema_series(&macd_values, signal);
        for (k, idx) in (start..n).enumerate() {
            if let Some(s) = sig[k] {
                signal_line[idx] = Some(s);
                if let Some(m) = macd_line[idx] {
                    hist[idx] = Some(m - s);
                }
            }
        }
    }

    (macd_line, signal_line, hist)
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

/// 对单个 DataFrame **就地**追加 MACD 三列，避免全量 clone。
///
/// 优化点：
/// - **就地修改**：直接在输入 DataFrame 上 `add_column`，不再 `clone()` 整个表
/// - 源列不存在或类型不匹配时仅跳过并打印告警，不影响其他 DataFrame
/// - 参数全空时，原样返回（不追加列）
fn apply_macd_inplace(df: &mut DataFrame, params: &MacdParams) {
    let want_macd = !params.macd_fast.is_empty()
        || !params.macd_slow.is_empty()
        || !params.macd_signal.is_empty();
    if !want_macd {
        println!("MACD算子: 未配置任何 MACD 周期，原样返回输入");
        return;
    }

    let source_col = resolve_source_column(&params.source_column);
    let source_values = match extract_column_values(df, source_col) {
        Some(v) => v,
        None => {
            let existing_cols: Vec<&str> = df.columns.iter().map(|c| c.name.as_str()).collect();
            eprintln!(
                "MACD算子: 源列 '{}' 不存在或类型不匹配 (现有列: {:?})，跳过 MACD",
                source_col, existing_cols
            );
            return;
        }
    };

    let fast = parse_single_period(&params.macd_fast, 12);
    let slow = parse_single_period(&params.macd_slow, 26);
    let signal = parse_single_period(&params.macd_signal, 9);
    if slow <= fast {
        eprintln!(
            "MACD算子: MACD slow({}) <= fast({})，无意义，跳过 MACD",
            slow, fast
        );
        return;
    }

    let (macd_line, signal_line, hist) = compute_macd(&source_values, fast, slow, signal);
    df.add_column(DataFrame::new_float64_column("macd", macd_line));
    df.add_column(DataFrame::new_float64_column("macd_signal", signal_line));
    df.add_column(DataFrame::new_float64_column("macd_hist", hist));
    // println!(
    //     "  已添加列: macd, macd_signal, macd_hist ({} 行)",
    //     df.row_count
    // );
}

/// MACD 算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立做 MACD 计算，
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

    let macd_configured = !params.macd_fast.is_empty()
        || !params.macd_slow.is_empty()
        || !params.macd_signal.is_empty();
    println!(
        "MACD算子: MACD={}, 输入 DataFrame 数量={}, 首个行数={}",
        if macd_configured { "已配置" } else { "未配置" },
        input_dfs.len(),
        input_dfs[0].row_count
    );

    // 逐个 DataFrame 就地做 MACD 计算（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("MACD算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_macd_inplace(df, &params);
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

/// 获取 MACD 算子版本
#[no_mangle]
pub extern "C" fn macd_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
