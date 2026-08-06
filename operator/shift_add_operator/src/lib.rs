use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use serde::{Deserialize, Serialize};
use std::ffi::{c_char, CStr, CString};

/// 前移加算子参数结构体
///
/// - `source_column`：要前移（shift）的源列名。源列需为 Float64 或 Int64。
/// - `shift_n`：前移行数（非负整数，字符串形式，与前端字符串输入一致）。
///   前移 n 行后：前 n 行为空值，末尾 n 个原值被丢弃，列长度不变
///   （等价于 pandas `Series.shift(n)`）。
/// - `target_columns`：逗号分隔的目标列名列表，前移后的源列会逐列相加。
///   任一目标列可等于源列（结果 = 原值 + 前移值）。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ShiftAddParams {
    #[serde(default)]
    pub source_column: String,
    #[serde(default)]
    pub shift_n: String,
    #[serde(default)]
    pub target_columns: String,
}

/// 解析参数 JSON 为 ShiftAddParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> ShiftAddParams {
    if params_json.is_empty() {
        return ShiftAddParams::default();
    }
    match serde_json::from_str::<ShiftAddParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("前移加算子: 解析参数 JSON 失败: {}", e);
            ShiftAddParams::default()
        }
    }
}

/// 解析逗号分隔的列名：去空格、去空串、去重（保持首次出现顺序）
fn parse_columns(columns_str: &str) -> Vec<String> {
    let mut out = Vec::new();
    for s in columns_str.split(',') {
        let t = s.trim();
        if !t.is_empty() && !out.iter().any(|c: &String| c == t) {
            out.push(t.to_string());
        }
    }
    out
}

/// 解析 shift_n：空串或非法值返回 0（前移 0 行 = 原值，相当于直接把源列加到目标列）
fn parse_shift_n(raw: &str) -> usize {
    let t = raw.trim();
    if t.is_empty() {
        return 0;
    }
    match t.parse::<usize>() {
        Ok(v) => v,
        Err(_) => {
            eprintln!("前移加算子: shift_n='{}' 非法，按 0 处理", t);
            0
        }
    }
}

/// 前移后的源列值（保留源列类型，避免无谓类型提升）
enum ShiftedValues {
    F64(Vec<Option<f64>>),
    I64(Vec<Option<i64>>),
}

/// 将 f64 序列前移 n 行：前 n 行为 None，末尾 n 个原值被丢弃，长度不变
///
/// 即 `out[i] = values[i - n]`（i >= n），`out[i] = None`（i < n）。
fn shift_forward_f64(values: &[Option<f64>], n: usize) -> Vec<Option<f64>> {
    let len = values.len();
    if n == 0 {
        return values.to_vec();
    }
    let mut out = vec![None; len];
    if n < len {
        // out[n..len] <- values[0..len-n]
        out[n..len].copy_from_slice(&values[0..len - n]);
    }
    out
}

/// 将 i64 序列前移 n 行：语义同 `shift_forward_f64`
fn shift_forward_i64(values: &[Option<i64>], n: usize) -> Vec<Option<i64>> {
    let len = values.len();
    if n == 0 {
        return values.to_vec();
    }
    let mut out = vec![None; len];
    if n < len {
        out[n..len].copy_from_slice(&values[0..len - n]);
    }
    out
}

/// 从源列提取并前移，返回前移后的值；源列不存在或非数值时返回 None（已打印告警）
fn extract_shifted(df: &DataFrame, source_col: &str, n: usize) -> Option<ShiftedValues> {
    let col = df.column(source_col)?;
    match &col.data_type {
        DataType::Float64 => {
            let vals = col.to_f64_vec();
            Some(ShiftedValues::F64(shift_forward_f64(&vals, n)))
        }
        DataType::Int64 => {
            let vals = col.to_i64_vec();
            Some(ShiftedValues::I64(shift_forward_i64(&vals, n)))
        }
        other => {
            eprintln!(
                "前移加算子: 源列 '{}' 类型 {:?} 不支持 (仅 Float64/Int64)，跳过",
                source_col, other
            );
            None
        }
    }
}

/// 把前移后的源列值加到单个目标列上（就地替换该列）。
///
/// 类型规则：
/// - 源 Int64 + 目标 Int64 → Int64（饱和加法，保留 Int64 类型）
/// - 任一方为 Float64 → Float64（Int64 方提升为 f64）
/// - 目标非数值类型 → 跳过并告警
///
/// 空值规则：与 pandas 逐元素加法一致——任一加数为空则结果为空。
fn add_shifted_to_target(df: &mut DataFrame, target_col: &str, shifted: &ShiftedValues) {
    let (pos, target_type) = match df.columns.iter().position(|c| c.name == target_col) {
        Some(p) => (p, df.columns[p].data_type.clone()),
        None => {
            eprintln!(
                "前移加算子: 目标列 '{}' 不存在，跳过 (现有列: {:?})",
                target_col,
                df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
            );
            return;
        }
    };

    match (shifted, target_type) {
        (ShiftedValues::I64(sv), DataType::Int64) => {
            let tv = df.columns[pos].to_i64_vec();
            let out: Vec<Option<i64>> = tv
                .iter()
                .zip(sv.iter())
                .map(|(t, s)| match (t, s) {
                    (Some(t), Some(s)) => Some(t.saturating_add(*s)),
                    _ => None,
                })
                .collect();
            df.columns[pos] = DataFrame::new_int64_column(target_col, out);
        }
        (ShiftedValues::F64(sv), DataType::Float64) => {
            let tv = df.columns[pos].to_f64_vec();
            let out: Vec<Option<f64>> = tv
                .iter()
                .zip(sv.iter())
                .map(|(t, s)| match (t, s) {
                    (Some(t), Some(s)) => Some(t + s),
                    _ => None,
                })
                .collect();
            df.columns[pos] = DataFrame::new_float64_column(target_col, out);
        }
        (ShiftedValues::F64(sv), DataType::Int64) => {
            // 源 Float64 + 目标 Int64 → 提升为 Float64
            let tv = df.columns[pos].to_i64_vec();
            let out: Vec<Option<f64>> = tv
                .iter()
                .zip(sv.iter())
                .map(|(t, s)| match (t, s) {
                    (Some(t), Some(s)) => Some(*t as f64 + s),
                    _ => None,
                })
                .collect();
            df.columns[pos] = DataFrame::new_float64_column(target_col, out);
        }
        (ShiftedValues::I64(sv), DataType::Float64) => {
            let tv = df.columns[pos].to_f64_vec();
            let out: Vec<Option<f64>> = tv
                .iter()
                .zip(sv.iter())
                .map(|(t, s)| match (t, s) {
                    (Some(t), Some(s)) => Some(t + *s as f64),
                    _ => None,
                })
                .collect();
            df.columns[pos] = DataFrame::new_float64_column(target_col, out);
        }
        (_, other) => {
            eprintln!(
                "前移加算子: 目标列 '{}' 类型 {:?} 不支持加法 (仅 Float64/Int64)，跳过",
                target_col, other
            );
        }
    }
}

/// 对单个 DataFrame 就地执行：源列前移 n 行后，逐列加到目标列上。
///
/// 前移值只计算一次（来自原始源列），因此源列自身也可作为目标列之一。
fn apply_shift_add_inplace(df: &mut DataFrame, source_col: &str, n: usize, targets: &[String]) {
    if df.row_count == 0 {
        return;
    }
    let shifted = match extract_shifted(df, source_col, n) {
        Some(v) => v,
        None => return, // 已打印告警
    };
    for t in targets {
        add_shifted_to_target(df, t, &shifted);
    }
}

/// 前移加算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立做「源列前移 + 多列相加」，
/// 输出同样为 DataFrameArray（顺序与输入一致）。
/// 单个 DataFrame 输入会被包装为单元素数组处理，输出仍为 DataFrameArray。
///
/// 返回值:
/// - 0:  成功
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入 DataFrame 数组为空
/// - -6: 未指定源列（参数 source_column 为空）
/// - -7: 未指定目标列（参数 target_columns 为空）
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
        eprintln!("前移加算子: {}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_json_str);

    let source_col = params.source_column.trim().to_string();
    if source_col.is_empty() {
        let err = "未指定源列 (参数 source_column 为空)".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("前移加算子: {}", err);
        return -6;
    }

    let targets = parse_columns(&params.target_columns);
    if targets.is_empty() {
        let err = "未指定目标列 (参数 target_columns 为空)".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("前移加算子: {}", err);
        return -7;
    }

    let n = parse_shift_n(&params.shift_n);

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("前移加算子: {}", err);
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
            eprintln!("前移加算子: {}", err);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("前移加算子: {}", err);
        return -5;
    }

    println!(
        "前移加算子: 源列={}, 前移={}行, 目标列={:?}, 输入 DataFrame 数量={}, 首个行数={}",
        source_col, n, targets, input_dfs.len(), input_dfs[0].row_count
    );

    // 逐个 DataFrame 就地执行（消费 input_dfs，避免 clone）
    let mut out_dfs: Vec<DataFrame> = input_dfs;
    for (i, df) in out_dfs.iter_mut().enumerate() {
        if df.row_count == 0 {
            eprintln!("前移加算子: 第 {} 个 DataFrame 为空，原样保留", i);
            continue;
        }
        apply_shift_add_inplace(df, &source_col, n, &targets);
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

/// 获取前移加算子版本
#[no_mangle]
pub extern "C" fn shift_add_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造单列 Float64 DataFrame
    fn df_f64(name: &str, vals: Vec<Option<f64>>) -> DataFrame {
        let col = DataFrame::new_float64_column(name, vals);
        let mut df = DataFrame::new();
        df.add_column(col);
        df
    }

    #[test]
    fn shift_forward_basic() {
        let v = vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)];
        let out = shift_forward_f64(&v, 2);
        // [None, None, 1, 2, 3]  末尾 4,5 被丢弃
        assert_eq!(out, vec![None, None, Some(1.0), Some(2.0), Some(3.0)]);
    }

    #[test]
    fn shift_forward_zero_is_identity() {
        let v = vec![Some(1.0), None, Some(3.0)];
        assert_eq!(shift_forward_f64(&v, 0), v);
    }

    #[test]
    fn shift_forward_n_ge_len_all_none() {
        let v = vec![Some(1.0), Some(2.0)];
        let out = shift_forward_f64(&v, 2);
        assert_eq!(out, vec![None, None]);
        let out2 = shift_forward_f64(&v, 5);
        assert_eq!(out2, vec![None, None]);
    }

    #[test]
    fn shift_forward_preserves_none_in_middle() {
        let v = vec![Some(1.0), None, Some(3.0), Some(4.0)];
        let out = shift_forward_f64(&v, 1);
        // [None, 1, None, 3]
        assert_eq!(out, vec![None, Some(1.0), None, Some(3.0)]);
    }

    #[test]
    fn shift_forward_i64_basic() {
        let v = vec![Some(10), Some(20), Some(30), Some(40)];
        let out = shift_forward_i64(&v, 1);
        assert_eq!(out, vec![None, Some(10), Some(20), Some(30)]);
    }

    #[test]
    fn add_f64_to_f64_target() {
        // 源列 s = [1,2,3,4,5]，前移 2 -> [None,None,1,2,3]
        // 目标列 t = [10,20,30,40,50]
        // t + s_shifted = [None, None, 31, 42, 53]
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_float64_column("s", vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)]));
        df.add_column(DataFrame::new_float64_column("t", vec![Some(10.0), Some(20.0), Some(30.0), Some(40.0), Some(50.0)]));
        apply_shift_add_inplace(&mut df, "s", 2, &["t".to_string()]);
        assert_eq!(
            df.column("t").unwrap().to_f64_vec(),
            vec![None, None, Some(31.0), Some(42.0), Some(53.0)]
        );
        // 源列保持不变
        assert_eq!(df.column("s").unwrap().to_f64_vec(), vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)]);
    }

    #[test]
    fn add_i64_to_i64_preserves_type() {
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_int64_column("s", vec![Some(1), Some(2), Some(3), Some(4)]));
        df.add_column(DataFrame::new_int64_column("t", vec![Some(10), Some(20), Some(30), Some(40)]));
        apply_shift_add_inplace(&mut df, "s", 1, &["t".to_string()]);
        assert_eq!(df.column("t").unwrap().data_type, DataType::Int64);
        // t + s_shifted([None,1,2,3]) = [None, 21, 32, 43]
        assert_eq!(df.column("t").unwrap().to_i64_vec(), vec![None, Some(21), Some(32), Some(43)]);
    }

    #[test]
    fn add_f64_source_to_i64_target_promotes_to_f64() {
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_float64_column("s", vec![Some(0.5), Some(1.5), Some(2.5)]));
        df.add_column(DataFrame::new_int64_column("t", vec![Some(10), Some(20), Some(30)]));
        apply_shift_add_inplace(&mut df, "s", 1, &["t".to_string()]);
        // t + s_shifted([None,0.5,1.5]) = [None, 20.5, 31.5]，类型提升为 Float64
        assert_eq!(df.column("t").unwrap().data_type, DataType::Float64);
        assert_eq!(df.column("t").unwrap().to_f64_vec(), vec![None, Some(20.5), Some(31.5)]);
    }

    #[test]
    fn add_i64_source_to_f64_target() {
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_int64_column("s", vec![Some(1), Some(2), Some(3)]));
        df.add_column(DataFrame::new_float64_column("t", vec![Some(10.5), Some(20.5), Some(30.5)]));
        apply_shift_add_inplace(&mut df, "s", 1, &["t".to_string()]);
        // t + s_shifted([None,1,2]) = [None, 21.5, 32.5]
        assert_eq!(df.column("t").unwrap().to_f64_vec(), vec![None, Some(21.5), Some(32.5)]);
    }

    #[test]
    fn shift_zero_adds_source_directly() {
        // n=0：前移 0 行 = 源列原值，直接相加
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_float64_column("s", vec![Some(1.0), Some(2.0), Some(3.0)]));
        df.add_column(DataFrame::new_float64_column("t", vec![Some(10.0), Some(20.0), Some(30.0)]));
        apply_shift_add_inplace(&mut df, "s", 0, &["t".to_string()]);
        assert_eq!(df.column("t").unwrap().to_f64_vec(), vec![Some(11.0), Some(22.0), Some(33.0)]);
    }

    #[test]
    fn source_also_target_uses_original_shifted() {
        // 源列 s 同时作为目标：结果 = 原值 + 前移值
        // s = [1,2,3,4], 前移1 -> [None,1,2,3], s + shifted = [None,3,5,7]
        let mut df = df_f64("s", vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);
        apply_shift_add_inplace(&mut df, "s", 1, &["s".to_string()]);
        assert_eq!(df.column("s").unwrap().to_f64_vec(), vec![None, Some(3.0), Some(5.0), Some(7.0)]);
    }

    #[test]
    fn add_to_multiple_targets_independently() {
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_float64_column("s", vec![Some(1.0), Some(2.0), Some(3.0)]));
        df.add_column(DataFrame::new_float64_column("a", vec![Some(10.0), Some(20.0), Some(30.0)]));
        df.add_column(DataFrame::new_float64_column("b", vec![Some(100.0), Some(200.0), Some(300.0)]));
        apply_shift_add_inplace(&mut df, "s", 1, &["a".to_string(), "b".to_string()]);
        // s_shifted = [None, 1, 2]
        assert_eq!(df.column("a").unwrap().to_f64_vec(), vec![None, Some(21.0), Some(32.0)]);
        assert_eq!(df.column("b").unwrap().to_f64_vec(), vec![None, Some(201.0), Some(302.0)]);
    }

    #[test]
    fn none_propagates_in_addition() {
        // 目标列含空值时，相加结果为空
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_float64_column("s", vec![Some(1.0), Some(2.0), Some(3.0)]));
        df.add_column(DataFrame::new_float64_column("t", vec![Some(10.0), None, Some(30.0)]));
        apply_shift_add_inplace(&mut df, "s", 1, &["t".to_string()]);
        // s_shifted = [None, 1, 2]; t + shifted = [None, None, 32]
        assert_eq!(df.column("t").unwrap().to_f64_vec(), vec![None, None, Some(32.0)]);
    }

    #[test]
    fn missing_source_column_is_noop() {
        let mut df = df_f64("t", vec![Some(1.0), Some(2.0)]);
        apply_shift_add_inplace(&mut df, "missing", 1, &["t".to_string()]);
        assert_eq!(df.column("t").unwrap().to_f64_vec(), vec![Some(1.0), Some(2.0)]);
    }

    #[test]
    fn missing_target_column_skipped() {
        let mut df = df_f64("s", vec![Some(1.0), Some(2.0)]);
        // 目标列不存在：不应 panic，源列不变
        apply_shift_add_inplace(&mut df, "s", 1, &["nope".to_string()]);
        assert_eq!(df.column("s").unwrap().to_f64_vec(), vec![Some(1.0), Some(2.0)]);
    }

    #[test]
    fn non_numeric_source_skipped() {
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_string_column("s", vec![Some("a"), Some("b")]));
        df.add_column(DataFrame::new_float64_column("t", vec![Some(1.0), Some(2.0)]));
        apply_shift_add_inplace(&mut df, "s", 1, &["t".to_string()]);
        // 字符串源列不支持：跳过，目标列不变
        assert_eq!(df.column("t").unwrap().to_f64_vec(), vec![Some(1.0), Some(2.0)]);
    }

    #[test]
    fn non_numeric_target_skipped() {
        let mut df = DataFrame::new();
        df.add_column(DataFrame::new_float64_column("s", vec![Some(1.0), Some(2.0)]));
        df.add_column(DataFrame::new_string_column("t", vec![Some("x"), Some("y")]));
        apply_shift_add_inplace(&mut df, "s", 1, &["t".to_string()]);
        // 字符串目标列不支持加法：跳过，原值不变
        let t = df.column("t").unwrap();
        assert_eq!(t.get_string(0), Some("x"));
        assert_eq!(t.get_string(1), Some("y"));
    }

    #[test]
    fn parse_columns_dedup_and_trim() {
        assert_eq!(parse_columns("a, b ,a,, b"), vec!["a", "b"]);
        assert!(parse_columns(",,,").is_empty());
        assert!(parse_columns("").is_empty());
    }

    #[test]
    fn parse_shift_n_handles_invalid() {
        assert_eq!(parse_shift_n(""), 0);
        assert_eq!(parse_shift_n("3"), 3);
        assert_eq!(parse_shift_n("  5  "), 5);
        assert_eq!(parse_shift_n("abc"), 0);
        assert_eq!(parse_shift_n("-1"), 0);
    }
}
