use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::{DataFrame, DataType, PortData};
use operator_runtime::c_abi::{
    CPortData, CPortValue, portdata_from_c,
    c_set_last_error, TYPE_NULL,
};
use std::ffi::{CStr, CString, c_char};
use serde::{Deserialize, Serialize};

/// K线可视化算子参数（全部 String，与前端字符串输入一致）。
///
/// `indices` 为 0 基逗号分隔下标，空表示不选（直接返回空 DSL）；
/// 各 `*_col` 指定列名，空字符串表示使用默认列名。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct KlineParams {
    /// 选取 DataFrameArray 的 0 基下标，逗号分隔，如 "1,2"；空 = 不选（返回空 DSL）
    #[serde(default)]
    pub indices: String,
    /// 开盘价列名（默认 "open"）
    #[serde(default)]
    pub open_col: String,
    /// 最高价列名（默认 "high"）
    #[serde(default)]
    pub high_col: String,
    /// 最低价列名（默认 "low"）
    #[serde(default)]
    pub low_col: String,
    /// 收盘价列名（默认 "close"）
    #[serde(default)]
    pub close_col: String,
    /// 日期列名（默认 "date"）
    #[serde(default)]
    pub date_col: String,
    /// MA5 列名（默认 "ma5"）
    #[serde(default)]
    pub ma5_col: String,
    /// MA10 列名（默认 "ma10"）
    #[serde(default)]
    pub ma10_col: String,
}

impl KlineParams {
    /// 用默认值回退空字段，得到完整的列名配置。
    fn with_defaults(&self) -> KlineParams {
        KlineParams {
            indices: self.indices.clone(),
            open_col: if self.open_col.is_empty() { "open".to_string() } else { self.open_col.clone() },
            high_col: if self.high_col.is_empty() { "high".to_string() } else { self.high_col.clone() },
            low_col: if self.low_col.is_empty() { "low".to_string() } else { self.low_col.clone() },
            close_col: if self.close_col.is_empty() { "close".to_string() } else { self.close_col.clone() },
            date_col: if self.date_col.is_empty() { "date".to_string() } else { self.date_col.clone() },
            ma5_col: if self.ma5_col.is_empty() { "ma5".to_string() } else { self.ma5_col.clone() },
            ma10_col: if self.ma10_col.is_empty() { "ma10".to_string() } else { self.ma10_col.clone() },
        }
    }
}

/// 解析参数 JSON 为 KlineParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> KlineParams {
    if params_json.is_empty() {
        return KlineParams::default();
    }
    match serde_json::from_str::<KlineParams>(params_json) {
        Ok(params) => params,
        Err(e) => {
            eprintln!("K线算子: 解析参数 JSON 失败: {}", e);
            KlineParams::default()
        }
    }
}

/// 解析逗号分隔的 0 基下标字符串，如 "1,2" -> [1, 2]；空串 -> 空 Vec（表示全选）。
/// 非法片段会被跳过并告警。
fn parse_indices(s: &str) -> Vec<usize> {
    s.split(',')
        .filter_map(|tok| {
            let t = tok.trim();
            if t.is_empty() {
                None
            } else {
                match t.parse::<usize>() {
                    Ok(i) => Some(i),
                    Err(_) => {
                        eprintln!("K线算子: 忽略非法下标 '{}'", t);
                        None
                    }
                }
            }
        })
        .collect()
}

/// 从 DataFrame 提取指定 Float64 列为 `Vec<Option<f64>>`；列不存在或非 Float64 返回 None。
fn extract_f64_col(df: &DataFrame, name: &str) -> Option<Vec<Option<f64>>> {
    let col = df.column(name)?;
    if !matches!(col.data_type, DataType::Float64) {
        return None;
    }
    Some(col.to_f64_vec())
}

/// 从 DataFrame 提取日期列为字符串向量。按列类型分派；列缺失时用行号兜底。
fn extract_date_col(df: &DataFrame, name: &str, n: usize) -> Vec<String> {
    if let Some(col) = df.column(name) {
        match col.data_type {
            DataType::String => (0..n).map(|i| col.get_string(i).unwrap_or("").to_string()).collect(),
            DataType::Int64 => (0..n).map(|i| col.get_i64(i).map(|v| v.to_string()).unwrap_or_default()).collect(),
            DataType::Float64 => (0..n).map(|i| col.get_f64(i).map(|v| format_float(v)).unwrap_or_default()).collect(),
            _ => (0..n).map(|i| format!("#{}", i)).collect(),
        }
    } else {
        (0..n).map(|i| format!("#{}", i)).collect()
    }
}

/// 浮点格式化：保持往返精度（与渲染端 f64::from_str 兼容），去掉无意义尾零。
fn format_float(v: f64) -> String {
    if v.is_nan() || v.is_infinite() {
        format!("{:?}", v)
    } else {
        // 用 Debug 格式产生最短往返表示，渲染端能精确还原
        format!("{:?}", v)
    }
}

/// 对字符串字面量做转义：反转义后用 Debug 风格的转义输出，保证 DSL 解析端能还原。
/// 这里采用简单的双引号包裹 + 反斜杠转义内部双引号与反斜杠。
fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// 为单个 DataFrame 生成一个 `kline "标题" { ... }` 块的 DSL 文本。
///
/// - OHLC 四列任一缺失或非 Float64 → 返回 Err（调用方跳过该图表）。
/// - MA5/MA10 列缺失或非 Float64 → 跳过该 `line`。
/// - 价格为 NaN/Inf 的行跳过该 candle（不输出）。
/// - MA 值为 None → 输出 `_`；NaN/Inf MA 值按 None 处理（输出 `_`）。
fn emit_chart(df: &DataFrame, params: &KlineParams, chart_idx: usize) -> Result<String, String> {
    let n = df.row_count;
    if n == 0 {
        return Ok(format!("kline \"图表 {}\" {{\n}}\n", chart_idx + 1));
    }

    let open = extract_f64_col(df, &params.open_col)
        .ok_or_else(|| format!("缺少开盘价列 '{}' (Float64)", params.open_col))?;
    let high = extract_f64_col(df, &params.high_col)
        .ok_or_else(|| format!("缺少最高价列 '{}' (Float64)", params.high_col))?;
    let low = extract_f64_col(df, &params.low_col)
        .ok_or_else(|| format!("缺少最低价列 '{}' (Float64)", params.low_col))?;
    let close = extract_f64_col(df, &params.close_col)
        .ok_or_else(|| format!("缺少收盘价列 '{}' (Float64)", params.close_col))?;

    let dates = extract_date_col(df, &params.date_col, n);

    // MA 列可选
    let ma5 = extract_f64_col(df, &params.ma5_col);
    let ma10 = extract_f64_col(df, &params.ma10_col);

    let mut out = String::new();
    out.push_str(&format!("kline \"图表 {}\" {{\n", chart_idx + 1));

    // ---- candle 语句 ----
    for i in 0..n {
        let o = open[i];
        let h = high[i];
        let l = low[i];
        let c = close[i];
        // 任一为空或非有限值则跳过该 candle
        let (o, h, l, c) = match (o, h, l, c) {
            (Some(o), Some(h), Some(l), Some(c))
                if o.is_finite() && h.is_finite() && l.is_finite() && c.is_finite() =>
            {
                (o, h, l, c)
            }
            _ => continue,
        };
        out.push_str(&format!(
            "  candle {} {} {} {} {}\n",
            escape_str(&dates[i]),
            format_float(o),
            format_float(h),
            format_float(l),
            format_float(c),
        ));
    }

    // ---- line 语句（MA5 / MA10）----
    if let Some(ma) = ma5 {
        out.push_str(&format!("  line {} \"#FFD700\" [", escape_str("MA5")));
        push_ma_values(&mut out, ma, n);
        out.push_str("]\n");
    } else {
        eprintln!("K线算子: MA5 列 '{}' 缺失或非 Float64，跳过 MA5 线", params.ma5_col);
    }
    if let Some(ma) = ma10 {
        out.push_str(&format!("  line {} \"#9370DB\" [", escape_str("MA10")));
        push_ma_values(&mut out, ma, n);
        out.push_str("]\n");
    } else {
        eprintln!("K线算子: MA10 列 '{}' 缺失或非 Float64，跳过 MA10 线", params.ma10_col);
    }

    out.push_str("}\n");
    Ok(out)
}

/// 把 MA 值序列追加到 DSL 输出缓冲：None 或非有限值 → `_`，逗号分隔。
fn push_ma_values(out: &mut String, values: Vec<Option<f64>>, n: usize) {
    let len = values.len().min(n);
    for i in 0..len {
        if i > 0 {
            out.push_str(", ");
        }
        match values[i] {
            Some(v) if v.is_finite() => out.push_str(&format_float(v)),
            _ => out.push('_'),
        }
    }
}

/// K线可视化算子的执行函数（C ABI）。
///
/// 支持 DataFrameArray 输入：
/// - 显式指定 `indices`（如 "0" 或 "0,1,5"）→ 仅对选中的下标逐个 DataFrame
///   生成 `kline` 块，拼成完整 DSL 文本，以 `PortData::String` 输出。
/// - `indices` 为空 → **直接返回空 DSL**（不再「全选」全部 DF）。
///   原因：大 DataFrameArray（成百上千个 DF）场景下全选会导致：
///     ① 算子 CPU/内存暴涨，执行时间过长（用户感知「卡、不出结果」）
///     ② 返回 DSL 字符串巨大、服务端 Debug 缓存膨胀
///     ③ 下游 DSL 解析 / 渲染端阻塞
///   对应解决方案：**Debug 模式 + 空 indices** 触发前端预览窗口走
///   「前端渲染分支」——从上游节点 Debug 会话中分页取 DataFrame，
///   根据算子参数在前端按需生成单个 kline 块 DSL，并提供 DF 切换导航
///   查看任意一个。
///
/// 返回值:
/// - 0: 成功（包括空 indices 返回空 DSL、所有 DF 都生成失败但没硬错）
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型，或全部选中 DF 无法生成有效图表
/// - -5: 输入 DataFrame 数组为空 / 显式 indices 未选中任何 DF
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

    let params = parse_params(params_json_str).with_defaults();

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

    // 按 indices 选取子集（空 = 不选）
    //
    // 空 indices 的语义：不再像过去那样「全选」，而是直接返回空 DSL。
    // 原因：
    //   1. DataFrameArray 可能有成百上千个 DF，一次性生成 DSL 会导致算子
    //      执行时间过长（CPU 耗死、返回 DSL 字符串巨大、服务端缓存膨胀），
    //      表现为用户感知的「算子一直在进行，不出结果」。
    //   2. 前端 Debug 模式下如果检测到 indices 为空，会走「前端渲染分支」
    //      ——从上游节点的 Debug 会话中按需分页取 DataFrame，再根据算子参数
    //      在前端生成单个 kline 块 DSL，并提供 DF 切换导航来查看任意一个，
    //      不需要算子本身生成完整大 DSL。
    //   3. 非 Debug 模式（或用户想让算子生成多图 DSL）时，必须显式指定
    //      indices，如 "0" 或 "0,1,5"，避免误触发大批量生成。
    let indices = parse_indices(&params.indices);
    let selected: Vec<&DataFrame> = if indices.is_empty() {
        println!(
            "K线算子: indices 为空，跳过 DSL 生成（共 {} 个 DataFrame）。\n\
             → Debug 模式下请使用前端预览窗口逐个查看 DataFrame。\n\
             → 非 Debug 模式下如需算子生成多图 DSL，请显式设置 indices，例如 indices=\"0\" 或 indices=\"0,1,5\"。",
            input_dfs.len()
        );
        // 清空错误信息：空 DSL 仍视为成功（算子正常结束）
        let c_msg = CString::new("").unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        let port_data = PortData::String(String::new());
        if !outputs.is_null() && output_cap > 0 {
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
        return 0;
    } else {
        indices
            .iter()
            .filter_map(|&i| {
                if i < input_dfs.len() {
                    Some(&input_dfs[i])
                } else {
                    eprintln!("K线算子: 下标 {} 越界（共 {} 个 DataFrame），跳过", i, input_dfs.len());
                    None
                }
            })
            .collect()
    };

    if selected.is_empty() {
        let err_msg = format!("indices={} 未选中任何 DataFrame（共 {} 个）", params.indices, input_dfs.len());
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("{}", err_msg);
        return -5;
    }

    println!(
        "K线算子: indices={:?}, 选中 {} 个 DataFrame (共 {}), 列配置 open={}/high={}/low={}/close={}/date={}/ma5={}/ma10={}",
        params.indices,
        selected.len(),
        input_dfs.len(),
        params.open_col, params.high_col, params.low_col, params.close_col,
        params.date_col, params.ma5_col, params.ma10_col,
    );

    // 逐个 DataFrame 生成 kline 块
    let mut dsl = String::new();
    let mut chart_idx = 0usize;
    for df in &selected {
        match emit_chart(df, &params, chart_idx) {
            Ok(block) => {
                dsl.push_str(&block);
                chart_idx += 1;
            }
            Err(e) => {
                eprintln!("K线算子: 第 {} 个 DataFrame 生成失败，跳过: {}", chart_idx + 1, e);
            }
        }
    }

    if chart_idx == 0 {
        let err_msg = "所有选中的 DataFrame 均无法生成 K线（请检查 OHLC 列名与类型）".to_string();
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("{}", err_msg);
        return -4;
    }

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    let port_data = PortData::String(dsl);

    if !outputs.is_null() && output_cap > 0 {
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

/// 获取 K线可视化算子版本
#[no_mangle]
pub extern "C" fn kline_visualization_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ohlc_df(n: usize) -> DataFrame {
        let mut df = DataFrame::new();
        // owned 字符串保持在栈上，引用其 as_str 构造列，避免内存泄漏
        let date_strings: Vec<String> = (0..n).map(|i| format!("2024-01-{:02}", i + 1)).collect();
        let dates: Vec<Option<&str>> = date_strings.iter().map(|s| Some(s.as_str())).collect();
        df.add_column(DataFrame::new_string_column("date", dates));
        let o: Vec<Option<f64>> = (0..n).map(|i| Some(10.0 + i as f64)).collect();
        let h: Vec<Option<f64>> = (0..n).map(|i| Some(11.0 + i as f64)).collect();
        let l: Vec<Option<f64>> = (0..n).map(|i| Some(9.0 + i as f64)).collect();
        let c: Vec<Option<f64>> = (0..n).map(|i| Some(10.5 + i as f64)).collect();
        df.add_column(DataFrame::new_float64_column("open", o));
        df.add_column(DataFrame::new_float64_column("high", h));
        df.add_column(DataFrame::new_float64_column("low", l));
        df.add_column(DataFrame::new_float64_column("close", c));
        // MA5 前 4 个为 None
        let ma5: Vec<Option<f64>> = (0..n).map(|i| if i >= 4 { Some(10.2 + i as f64) } else { None }).collect();
        df.add_column(DataFrame::new_float64_column("ma5", ma5));
        df
    }

    #[test]
    fn parse_indices_handles_cases() {
        assert_eq!(parse_indices(""), Vec::<usize>::new());
        assert_eq!(parse_indices("1,2"), vec![1, 2]);
        assert_eq!(parse_indices(" 0 , 2 ,"), vec![0, 2]);
        assert_eq!(parse_indices("x,3"), vec![3]);
    }

    #[test]
    fn emit_chart_produces_valid_dsl() {
        let df = make_ohlc_df(6);
        let params = KlineParams::default().with_defaults();
        let dsl = emit_chart(&df, &params, 0).unwrap();
        assert!(dsl.starts_with("kline \"图表 1\" {"));
        assert!(dsl.contains("candle \"2024-01-01\""));
        assert!(dsl.contains("line \"MA5\" \"#FFD700\" ["));
        assert!(dsl.contains("_, _, _, _,"));
    }

    #[test]
    fn emit_chart_missing_ohlc_errors() {
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_float64_column("open", vec![Some(1.0)]));
        // 缺 high/low/close
        let params = KlineParams::default().with_defaults();
        assert!(emit_chart(&df, &params, 0).is_err());
    }
}
