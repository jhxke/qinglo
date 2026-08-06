use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::{DataFrame, DataType, PortData};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_from_c,
    c_set_last_error, TYPE_NULL,
};
use std::ffi::{CStr, CString, c_char};
use serde::{Deserialize, Serialize};

/// 指标算子参数结构体
///
/// 不再有 `indicator_type` 选择单一指标：各周期字段非空即触发对应指标计算，
/// 多个指标可同时计算、各自追加列（追加顺序 MA → RSI → MACD）。
/// 所有数值字段统一使用 String 类型（与前端字符串输入一致），内部再解析为数值，
/// 这样既兼容前端 String 参数渲染，又能复用 MA 多周期 "5,10,20" 的逗号分隔写法。
/// 字段留空表示不计算该指标（不再有默认值）。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct IndicatorParams {
    /// MA 均线周期列表，逗号分隔，如 "5,10,20"；空则不计算 MA
    #[serde(default)]
    pub ma_periods: String,
    /// RSI 周期，如 "14"；空则不计算 RSI
    #[serde(default)]
    pub rsi_period: String,
    /// MACD 快线周期，如 "12"；macd_* 任一非空即触发 MACD，缺失项回退默认 12/26/9
    #[serde(default)]
    pub macd_fast: String,
    /// MACD 慢线周期，如 "26"
    #[serde(default)]
    pub macd_slow: String,
    /// MACD 信号线周期，如 "9"
    #[serde(default)]
    pub macd_signal: String,
    /// 旧版参数别名 (兼容历史 DAG：当 ma_periods 为空时回退使用)
    #[serde(default)]
    pub periods: String,
}

/// 解析参数 JSON 为 IndicatorParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> IndicatorParams {
    if params_json.is_empty() {
        return IndicatorParams::default();
    }
    match serde_json::from_str::<IndicatorParams>(params_json) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("解析参数 JSON 失败: {}", e);
            IndicatorParams::default()
        }
    }
}

/// 解析逗号分隔的周期字符串，如 "5,10,20" -> vec![5, 10, 20]；非正数自动过滤
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

/// 解析单个正整数参数；解析失败或非正时返回 default_val
fn parse_single_period(raw: &str, default_val: usize) -> usize {
    raw.trim()
        .parse::<usize>()
        .ok()
        .filter(|&p| p > 0)
        .unwrap_or(default_val)
}

/// 计算简单移动平均线 (SMA) —— 滚动求和实现
///
/// 窗口内忽略空值，按有效值求平均；窗口不足 N 行时返回 None。
/// 使用滚动求和（窗口滑出时减去离开元素、滑入时加上进入元素），
/// 将时间复杂度从 O(n×period) 降低到 O(n)，对长周期（如 MA250）提升显著。
/// 浮点累积误差在金融数据量级下可忽略（period ≤ 1000 时相对误差 < 1e-12）。
fn compute_sma(values: &[Option<f64>], period: usize) -> Vec<Option<f64>> {
    let n = values.len();
    if period == 0 || n == 0 {
        return vec![None; n];
    }

    let mut result = Vec::with_capacity(n);
    let mut sum = 0.0f64;
    let mut valid_count = 0usize;

    for i in 0..n {
        // 窗口右端进入
        if let Some(val) = values[i] {
            sum += val;
            valid_count += 1;
        }
        // 窗口左端离开（窗口大小 = period）
        if i >= period {
            if let Some(val) = values[i - period] {
                sum -= val;
                valid_count -= 1;
            }
        }
        // 窗口填满后输出
        if i + 1 >= period {
            if valid_count == 0 {
                result.push(None);
            } else {
                result.push(Some(sum / valid_count as f64));
            }
        } else {
            result.push(None);
        }
    }

    result
}

/// 将 Option<f64> 序列前向填充为 f64 序列（None 用上一个有效值代替，开头全空则用 0.0）
///
/// EMA/RSI/MACD 需要连续数值序列；价格列通常无空值，前向填充仅作兜底。
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

/// 由平均涨幅 / 平均跌幅计算 RSI 值
fn rsi_from(avg_gain: f64, avg_loss: f64) -> f64 {
    if avg_loss == 0.0 {
        100.0
    } else {
        let rs = avg_gain / avg_loss;
        100.0 - 100.0 / (1.0 + rs)
    }
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

/// 从 DataFrame 中提取指定列的 f64 数据；列不存在或非 Float64 类型时返回 None
fn extract_column_values(df: &DataFrame, column_name: &str) -> Option<Vec<Option<f64>>> {
    let col = df.column(column_name)?;
    if !matches!(col.data_type, DataType::Float64) {
        return None;
    }
    Some(col.to_f64_vec())
}

/// 对单个 DataFrame **就地**追加指标列，避免全量 clone。
///
/// 不再通过 `indicator_type` 选择单一指标：根据 `params` 中各周期字段是否非空，
/// 触发对应指标计算，多个指标可同时追加列。追加顺序固定为 MA → RSI → MACD。
///
/// 优化点：
/// - **就地修改**：直接在输入 DataFrame 上 `add_column`，不再 `clone()` 整个表
/// - **close 列只提取一次**：MA/RSI/MACD 共享同一份 `Vec<Option<f64>>`，避免 3 次重复提取
/// - 源列不存在或类型不匹配时仅跳过该指标并打印告警，不影响其他指标
/// - 所有指标参数都为空时，原样返回（不追加列）
fn apply_indicators_inplace(df: &mut DataFrame, params: &IndicatorParams) {
    let want_ma = !params.ma_periods.is_empty() || !params.periods.is_empty();
    let want_rsi = !params.rsi_period.is_empty();
    let want_macd = !params.macd_fast.is_empty()
        || !params.macd_slow.is_empty()
        || !params.macd_signal.is_empty();
    if !want_ma && !want_rsi && !want_macd {
        println!("指标算子: 未配置任何指标周期，原样返回输入");
        return;
    }

    // close 列只提取一次，MA/RSI/MACD 共用
    let close_values = if want_ma || want_rsi || want_macd {
        extract_column_values(df, "close")
    } else {
        None
    };

    if close_values.is_none() {
        let existing_cols: Vec<&str> = df.columns.iter().map(|c| c.name.as_str()).collect();
        eprintln!(
            "指标算子: 源列 'close' 不存在或类型不匹配 (现有列: {:?})，跳过所有指标",
            existing_cols
        );
        return;
    }
    let close_values = close_values.unwrap();

    // ---------- MA ----------
    if want_ma {
        let periods_str = if !params.ma_periods.is_empty() {
            params.ma_periods.as_str()
        } else {
            params.periods.as_str()
        };
        let periods = parse_periods(periods_str);
        if periods.is_empty() {
            eprintln!(
                "指标算子: MA 周期串 '{}' 未解析出有效周期，跳过 MA",
                periods_str
            );
        } else {
            for &period in &periods {
                let ma_values = compute_sma(&close_values, period);
                let col_name = format!("ma_{}", period);
                df.add_column(DataFrame::new_float64_column(&col_name, ma_values));
                // println!("  已添加列: {} ({} 行)", col_name, df.row_count);
            }
        }
    }

    // ---------- RSI ----------
    if want_rsi {
        let period = parse_single_period(&params.rsi_period, 14);
        let rsi_values = compute_rsi(&close_values, period);
        let col_name = format!("rsi_{}", period);
        df.add_column(DataFrame::new_float64_column(&col_name, rsi_values));
        // println!("  已添加列: {} ({} 行)", col_name, df.row_count);
    }

    // ---------- MACD ----------
    if want_macd {
        let fast = parse_single_period(&params.macd_fast, 12);
        let slow = parse_single_period(&params.macd_slow, 26);
        let signal = parse_single_period(&params.macd_signal, 9);
        if slow <= fast {
            eprintln!(
                "指标算子: MACD slow({}) <= fast({})，无意义，跳过 MACD",
                slow, fast
            );
        } else {
            let (macd_line, signal_line, hist) = compute_macd(&close_values, fast, slow, signal);
            df.add_column(DataFrame::new_float64_column("macd", macd_line));
            df.add_column(DataFrame::new_float64_column("macd_signal", signal_line));
            df.add_column(DataFrame::new_float64_column("macd_hist", hist));
            println!(
                "  已添加列: macd, macd_signal, macd_hist ({} 行)",
                df.row_count
            );
        }
    }
}

/// 指标算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立做指标计算，
/// 输出同样为 DataFrameArray（顺序与输入一致）。
/// 为兼容旧 DAG，单个 DataFrame 输入会被包装为单元素数组处理，
/// 输出仍为 DataFrameArray（单元素）。
///
/// 返回值:
/// - 0: 成功（包括所有参数为空、源列缺失时静默原样返回的情形）
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

    let ma_configured = !params.ma_periods.is_empty() || !params.periods.is_empty();
    let rsi_configured = !params.rsi_period.is_empty();
    let macd_configured = !params.macd_fast.is_empty()
        || !params.macd_slow.is_empty()
        || !params.macd_signal.is_empty();
    println!(
        "指标算子: MA={}, RSI={}, MACD={}, 输入 DataFrame 数量={}, 首个行数={}",
        if ma_configured { "已配置" } else { "未配置" },
        if rsi_configured { "已配置" } else { "未配置" },
        if macd_configured { "已配置" } else { "未配置" },
        input_dfs.len(),
        input_dfs[0].row_count
    );

    // 逐个 DataFrame 就地做指标计算（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("指标算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_indicators_inplace(df, &params);
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

/// 获取指标算子版本
#[no_mangle]
pub extern "C" fn indicator_operator_version() -> *const c_char {
    b"0.3.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
