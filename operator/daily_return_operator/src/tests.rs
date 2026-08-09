use super::*;
use operator_runtime::c_abi::{portdata_to_c, CPortData, CPortValue, TYPE_NULL};
use operator_runtime::{DataFrame, PortData};
use std::ffi::CString;
use std::ptr;

// ============ 参数解析 ============

#[test]
fn test_parse_params_empty() {
    let p = parse_params("");
    assert_eq!(p.source_column, "");
    assert_eq!(p.result_column, "");
}

#[test]
fn test_parse_params_valid() {
    let json = r#"{"source_column":"close","result_column":"ret_1d"}"#;
    let p = parse_params(json);
    assert_eq!(p.source_column, "close");
    assert_eq!(p.result_column, "ret_1d");
}

#[test]
fn test_parse_params_partial() {
    // 只提供部分字段，其余走 serde default
    let p = parse_params(r#"{"source_column":"adj_close"}"#);
    assert_eq!(p.source_column, "adj_close");
    assert_eq!(p.result_column, "");
}

#[test]
fn test_parse_params_invalid_json() {
    let p = parse_params("not json");
    assert_eq!(p.source_column, "");
    assert_eq!(p.result_column, "");
}

#[test]
fn test_parse_params_empty_json_object() {
    let p = parse_params(r#"{}"#);
    assert_eq!(p.source_column, "");
    assert_eq!(p.result_column, "");
}

#[test]
fn test_resolve_source_column_defaults() {
    assert_eq!(resolve_source_column(""), "close");
    assert_eq!(resolve_source_column("   "), "close");
    assert_eq!(resolve_source_column("  price  "), "price");
}

#[test]
fn test_resolve_result_column_defaults() {
    assert_eq!(resolve_result_column(""), "daily_return");
    assert_eq!(resolve_result_column("   "), "daily_return");
    assert_eq!(resolve_result_column("  ret_1d  "), "ret_1d");
}

// ============ compute_daily_return 纯函数 ============

fn approx(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-12,
        (None, None) => true,
        _ => false,
    }
}

fn assert_approx_vec(actual: &[Option<f64>], expected: &[Option<f64>]) {
    assert_eq!(
        actual.len(),
        expected.len(),
        "长度不一致: actual={} expected={}",
        actual.len(),
        expected.len()
    );
    for (i, (a, b)) in actual.iter().zip(expected.iter()).enumerate() {
        assert!(approx(*a, *b), "第 {} 行不一致: actual={:?} expected={:?}", i, a, b);
    }
}

#[test]
fn test_compute_basic_float64() {
    // close = [10, 11, 12, 13, 14]
    // i=0: None（首行无前值）
    // i=1: (11-10)/10 = 0.1
    // i=2: (12-11)/11 ≈ 0.090909
    // i=3: (13-12)/12 ≈ 0.083333
    // i=4: (14-13)/13 ≈ 0.076923
    let v = vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)];
    let out = compute_daily_return(&v);
    assert_eq!(out.len(), 5);
    assert!(out[0].is_none());
    assert_approx_vec(
        &out,
        &[
            None,
            Some((11.0 - 10.0) / 10.0),
            Some((12.0 - 11.0) / 11.0),
            Some((13.0 - 12.0) / 12.0),
            Some((14.0 - 13.0) / 13.0),
        ],
    );
}

#[test]
fn test_compute_first_row_always_none() {
    // 不论首值是否有效，首行恒为 None
    let v = vec![Some(100.0), Some(200.0)];
    let out = compute_daily_return(&v);
    assert!(out[0].is_none());
    assert!((out[1].unwrap() - 1.0).abs() < 1e-12);

    let v2 = vec![None, Some(200.0)];
    let out2 = compute_daily_return(&v2);
    assert!(out2[0].is_none());
}

#[test]
fn test_compute_flat_zero_return() {
    // 价格持平 → 收益率恒为 0
    let v = vec![Some(10.0), Some(10.0), Some(10.0), Some(10.0)];
    let out = compute_daily_return(&v);
    assert!(out[0].is_none());
    for i in 1..out.len() {
        assert!((out[i].unwrap() - 0.0).abs() < 1e-12, "持平收益率应为 0");
    }
}

#[test]
fn test_compute_negative_return_on_drop() {
    // 下跌 → 负收益率
    // [10, 9] → (9-10)/10 = -0.1
    let v = vec![Some(10.0), Some(9.0)];
    let out = compute_daily_return(&v);
    assert!(out[0].is_none());
    assert!((out[1].unwrap() - (-0.1)).abs() < 1e-12);
    assert!(out[1].unwrap() < 0.0);
}

#[test]
fn test_compute_null_current_propagates() {
    // 当日价为空 → 该行 None
    let v = vec![Some(10.0), None, Some(12.0), Some(13.0)];
    let out = compute_daily_return(&v);
    // i=0: None（首行）
    // i=1: cur=None → None
    // i=2: prev=close[1]=None → None
    // i=3: (13-12)/12 ≈ 0.083333
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none());
    assert!((out[3].unwrap() - (13.0 - 12.0) / 12.0).abs() < 1e-12);
}

#[test]
fn test_compute_null_previous_propagates() {
    // 前一日价为空 → 当日 None；空值离开后恢复
    let v = vec![Some(10.0), None, Some(12.0), Some(13.0), Some(14.0)];
    let out = compute_daily_return(&v);
    // i=0: None（首行）
    // i=1: prev=10, cur=None → None
    // i=2: prev=None → None
    // i=3: (13-12)/12
    // i=4: (14-13)/13
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none());
    assert!((out[3].unwrap() - (13.0 - 12.0) / 12.0).abs() < 1e-12);
    assert!((out[4].unwrap() - (14.0 - 13.0) / 13.0).abs() < 1e-12);
}

#[test]
fn test_compute_division_by_zero_is_none() {
    // 前一日价 == 0 → 避免除零，结果 None
    // [10, 0, 12] → i=1: (0-10)/10=-1.0, i=2: prev=0 → None
    let v = vec![Some(10.0), Some(0.0), Some(12.0)];
    let out = compute_daily_return(&v);
    assert!(out[0].is_none());
    assert!((out[1].unwrap() - (-1.0)).abs() < 1e-12);
    assert!(out[2].is_none(), "前值为 0 应返回 None 避免除零");
}

#[test]
fn test_compute_full_drop_to_zero() {
    // 从 10 跌到 0 → 收益率 -1.0（跌 100%）
    let v = vec![Some(10.0), Some(0.0)];
    let out = compute_daily_return(&v);
    assert!(out[0].is_none());
    assert!((out[1].unwrap() - (-1.0)).abs() < 1e-12);
}

#[test]
fn test_compute_empty_input() {
    let out = compute_daily_return(&[]);
    assert!(out.is_empty());
}

#[test]
fn test_compute_single_element() {
    // 仅 1 个值 → 首行 None，无后续
    let out = compute_daily_return(&[Some(42.0)]);
    assert_eq!(out.len(), 1);
    assert!(out[0].is_none());
}

#[test]
fn test_compute_all_none() {
    let v = vec![None, None, None];
    let out = compute_daily_return(&v);
    assert_approx_vec(&out, &[None, None, None]);
}

#[test]
fn test_compute_long_series_vs_naive() {
    // 与朴素实现逐行比对，验证 O(n) 实现正确性
    let n = 1000;
    let v: Vec<Option<f64>> = (0..n)
        .map(|i| {
            if i % 37 == 0 {
                None // 周期性注入空值
            } else {
                Some(100.0 + (i as f64).sin() * 10.0 + i as f64 * 0.01)
            }
        })
        .collect();

    let fast = compute_daily_return(&v);
    // 朴素实现
    let naive: Vec<Option<f64>> = (0..n)
        .map(|i| {
            if i == 0 {
                None
            } else {
                match (v[i], v[i - 1]) {
                    (Some(cur), Some(prev)) if prev != 0.0 => Some((cur - prev) / prev),
                    _ => None,
                }
            }
        })
        .collect();
    assert_approx_vec(&fast, &naive);
}

// ============ 辅助 DataFrame 构造 ============

fn df_f64(name: &str, vals: Vec<Option<f64>>) -> DataFrame {
    let col = DataFrame::new_float64_column(name, vals);
    let mut df = DataFrame::new();
    df.add_column(col);
    df
}

fn df_i64(name: &str, vals: Vec<Option<i64>>) -> DataFrame {
    let col = DataFrame::new_int64_column(name, vals);
    let mut df = DataFrame::new();
    df.add_column(col);
    df
}

// ============ apply_daily_return ============

#[test]
fn test_apply_basic_float64() {
    let mut df = df_f64("close", vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0)]);
    apply_daily_return(&mut df, "close", "daily_return");
    let col = df.column("daily_return").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert_approx_vec(
        &col.to_f64_vec(),
        &[
            None,
            Some((11.0 - 10.0) / 10.0),
            Some((12.0 - 11.0) / 11.0),
            Some((13.0 - 12.0) / 12.0),
        ],
    );
    // 源列保留
    assert_eq!(
        df.column("close").unwrap().to_f64_vec(),
        vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0)]
    );
    assert_eq!(df.col_count(), 2);
}

#[test]
fn test_apply_int64_source_promotes_to_float64() {
    // Int64 源列 → 结果提升为 Float64
    let mut df = df_i64("close", vec![Some(10), Some(20), Some(40)]);
    apply_daily_return(&mut df, "close", "ret");
    let col = df.column("ret").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    // i=0: None, i=1: (20-10)/10=1.0, i=2: (40-20)/20=1.0
    assert_approx_vec(&col.to_f64_vec(), &[None, Some(1.0), Some(1.0)]);
    // 源列保持 Int64 原值
    assert_eq!(df.column("close").unwrap().data_type, DataType::Int64);
    assert_eq!(
        df.column("close").unwrap().to_i64_vec(),
        vec![Some(10), Some(20), Some(40)]
    );
}

#[test]
fn test_apply_custom_source_column() {
    let mut df = df_f64("adj_close", vec![Some(100.0), Some(110.0), Some(99.0)]);
    apply_daily_return(&mut df, "adj_close", "ret_1d");
    let col = df.column("ret_1d").unwrap();
    // i=1: (110-100)/100=0.1, i=2: (99-110)/110≈-0.1
    assert!((col.get_f64(1).unwrap() - 0.1).abs() < 1e-12);
    assert!((col.get_f64(2).unwrap() - ((99.0 - 110.0) / 110.0)).abs() < 1e-12);
    assert_eq!(df.col_count(), 2);
}

#[test]
fn test_apply_default_result_column_name() {
    // apply 层不直接处理默认列名（由 execute_operator 解析），
    // 但可验证显式传入 "daily_return" 时正常写入
    let mut df = df_f64("close", vec![Some(10.0), Some(20.0)]);
    apply_daily_return(&mut df, "close", "daily_return");
    assert!(df.column("daily_return").is_some());
}

#[test]
fn test_apply_overwrite_existing_result_column() {
    // result_col 指向已存在的另一列：覆盖该列，源列保留
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(
        "close",
        vec![Some(10.0), Some(20.0), Some(40.0)],
    ));
    df.add_column(DataFrame::new_float64_column(
        "w",
        vec![Some(999.0), Some(999.0), Some(999.0)],
    ));
    apply_daily_return(&mut df, "close", "w");
    assert_approx_vec(
        &df.column("close").unwrap().to_f64_vec(),
        &[Some(10.0), Some(20.0), Some(40.0)],
    );
    assert_approx_vec(&df.column("w").unwrap().to_f64_vec(), &[None, Some(1.0), Some(1.0)]);
    assert_eq!(df.col_count(), 2);
}

#[test]
fn test_apply_result_equals_source_overwrites_source() {
    // result_col == source：就地覆盖源列
    let mut df = df_f64("close", vec![Some(10.0), Some(20.0), Some(40.0)]);
    apply_daily_return(&mut df, "close", "close");
    assert_approx_vec(
        &df.column("close").unwrap().to_f64_vec(),
        &[None, Some(1.0), Some(1.0)],
    );
    assert_eq!(df.col_count(), 1);
}

#[test]
fn test_apply_missing_source_column_skipped() {
    // 源列不存在：不 panic，不新增结果列
    let mut df = df_f64("close", vec![Some(1.0), Some(2.0)]);
    apply_daily_return(&mut df, "missing", "ret");
    assert_eq!(
        df.column("close").unwrap().to_f64_vec(),
        vec![Some(1.0), Some(2.0)]
    );
    assert!(df.column("ret").is_none());
    assert_eq!(df.col_count(), 1);
}

#[test]
fn test_apply_string_column_not_supported() {
    // 字符串列不支持：跳过，原值不变，不新增结果列
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_string_column(
        "s",
        vec![Some("a"), Some("b"), Some("c")],
    ));
    apply_daily_return(&mut df, "s", "ret");
    let s = df.column("s").unwrap();
    assert_eq!(s.get_string(0), Some("a"));
    assert_eq!(s.get_string(1), Some("b"));
    assert!(df.column("ret").is_none());
    assert_eq!(df.col_count(), 1);
}

#[test]
fn test_apply_empty_df_noop() {
    // 空 DataFrame：函数不应 panic，直接返回
    let mut df = DataFrame::new();
    apply_daily_return(&mut df, "close", "ret");
    assert_eq!(df.row_count, 0);
    assert_eq!(df.col_count(), 0);
}

#[test]
fn test_apply_single_row_result_is_none() {
    // 仅 1 行：无前值，结果为 None
    let mut df = df_f64("close", vec![Some(42.0)]);
    apply_daily_return(&mut df, "close", "ret");
    let col = df.column("ret").unwrap();
    assert_eq!(col.to_f64_vec().len(), 1);
    assert!(col.get_f64(0).is_none());
}

#[test]
fn test_apply_preserves_other_columns() {
    // 多列场景：仅计算源列，其它列原样保留
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(
        "close",
        vec![Some(10.0), Some(11.0), Some(12.0)],
    ));
    df.add_column(DataFrame::new_int64_column(
        "volume",
        vec![Some(100), Some(200), Some(300)],
    ));
    df.add_column(DataFrame::new_string_column(
        "code",
        vec![Some("A"), Some("A"), Some("A")],
    ));
    apply_daily_return(&mut df, "close", "ret");
    assert_eq!(df.col_count(), 4);
    // volume 列保持 Int64 原值
    assert_eq!(df.column("volume").unwrap().data_type, DataType::Int64);
    assert_eq!(
        df.column("volume").unwrap().to_i64_vec(),
        vec![Some(100), Some(200), Some(300)]
    );
    // code 列保持 String 原值
    assert_eq!(df.column("code").unwrap().get_string(1), Some("A"));
    // ret 列正确
    assert!((df.column("ret").unwrap().get_f64(1).unwrap() - 0.1).abs() < 1e-12);
}

// ============ execute_operator 端到端（C ABI） ============

/// 从 PortData 提取 DataFrameArray，类型不匹配时 panic
fn unwrap_dfa(pd: PortData) -> Vec<DataFrame> {
    match pd {
        PortData::DataFrameArray(d) => d,
        other => panic!("期望 DataFrameArray 输出，实际得到 {}", other.type_name()),
    }
}

/// 以 DataFrameArray 输入运行一次当日收益率算子，返回 (返回码, 输出 PortData)
fn run_operator(input_dfs: Vec<DataFrame>, params_json: &str) -> (i32, Option<PortData>) {
    let input_port = PortData::DataFrameArray(input_dfs);
    // portdata_to_c 内部克隆 DataFrame 到独立 C 句柄，原 PortData 仍持有所有权
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(params_json).unwrap_or_default();

    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];

    let result = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );

    // 若输入未被消费（提前返回错误），释放输入句柄
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }

    // 提取输出（portdata_from_c 会消费 output_slots[0] 的句柄）
    let out = if output_slots[0].type_tag != TYPE_NULL {
        Some(unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) })
    } else {
        None
    };
    (result, out)
}

#[test]
fn test_execute_default_params() {
    // 默认 source=close, result=daily_return
    let df = df_f64("close", vec![Some(10.0), Some(11.0), Some(12.0)]);
    let (code, pd) = run_operator(vec![df], r#"{}"#);
    assert_eq!(code, 0);
    let dfs = match pd {
        Some(PortData::DataFrameArray(d)) => d,
        _ => panic!("期望 DataFrameArray 输出"),
    };
    assert_eq!(dfs.len(), 1);
    let out = &dfs[0];
    assert_eq!(out.col_count(), 2);
    let col = out.column("daily_return").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert_approx_vec(
        &col.to_f64_vec(),
        &[
            None,
            Some((11.0 - 10.0) / 10.0),
            Some((12.0 - 11.0) / 11.0),
        ],
    );
}

#[test]
fn test_execute_custom_columns() {
    let df = df_f64("adj_close", vec![Some(100.0), Some(110.0)]);
    let (code, pd) = run_operator(
        vec![df],
        r#"{"source_column":"adj_close","result_column":"ret_1d"}"#,
    );
    assert_eq!(code, 0);
    let dfs = match pd {
        Some(PortData::DataFrameArray(d)) => d,
        _ => panic!("期望 DataFrameArray 输出"),
    };
    let col = dfs[0].column("ret_1d").unwrap();
    assert!((col.get_f64(1).unwrap() - 0.1).abs() < 1e-12);
}

#[test]
fn test_execute_empty_result_column_falls_back() {
    // result_column 为空 → 回退 daily_return
    let df = df_f64("close", vec![Some(10.0), Some(20.0)]);
    let (code, pd) = run_operator(vec![df], r#"{"result_column":""}"#);
    assert_eq!(code, 0);
    let dfs = unwrap_dfa(pd.unwrap());
    assert!(dfs[0].column("daily_return").is_some());
}

#[test]
fn test_execute_multiple_dataframes() {
    // 多个 DataFrame 顺序独立计算
    let df_a = df_f64("close", vec![Some(10.0), Some(11.0), Some(12.0)]);
    let df_b = df_f64("close", vec![Some(100.0), Some(200.0)]);
    let (code, pd) = run_operator(vec![df_a, df_b], r#"{}"#);
    assert_eq!(code, 0);
    let dfs = unwrap_dfa(pd.unwrap());
    assert_eq!(dfs.len(), 2);
    // df_a
    assert!((dfs[0].column("daily_return").unwrap().get_f64(1).unwrap() - 0.1).abs() < 1e-12);
    // df_b: (200-100)/100 = 1.0
    assert!((dfs[1].column("daily_return").unwrap().get_f64(1).unwrap() - 1.0).abs() < 1e-12);
}

#[test]
fn test_execute_single_dataframe_wrap() {
    // 单个 DataFrame 输入应被包装为单元素数组处理
    let df = df_f64("close", vec![Some(10.0), Some(20.0)]);
    let input_port = PortData::DataFrame(df);
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(r#"{}"#).unwrap();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let result = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    assert_eq!(result, 0);
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
    let pd = unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) };
    // 输出统一为 DataFrameArray
    let dfs = unwrap_dfa(pd);
    assert_eq!(dfs.len(), 1);
    assert!(dfs[0].column("daily_return").is_some());
}

#[test]
fn test_execute_missing_input_returns_minus_3() {
    // 无输入数据 → -3
    let params_cstr = CString::new(r#"{}"#).unwrap();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let result = execute_operator(
        ptr::null(),
        0,
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    assert_eq!(result, -3);
}

#[test]
fn test_execute_empty_dataframe_array_returns_minus_5() {
    // 空 DataFrameArray → -5
    let (code, _pd) = run_operator(vec![], r#"{}"#);
    assert_eq!(code, -5);
}

#[test]
fn test_execute_wrong_input_type_returns_minus_4() {
    // 输入非 DataFrame / DataFrameArray → -4（用 String 端口模拟）
    let input_port = PortData::String("not a dataframe".to_string());
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(r#"{}"#).unwrap();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let result = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    assert_eq!(result, -4);
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
}

#[test]
fn test_execute_empty_df_in_array_preserved() {
    // 数组中含空 DataFrame：原样保留，不 panic
    let df_empty = DataFrame::new();
    let df_ok = df_f64("close", vec![Some(10.0), Some(20.0)]);
    let (code, pd) = run_operator(vec![df_empty, df_ok], r#"{}"#);
    assert_eq!(code, 0);
    let dfs = unwrap_dfa(pd.unwrap());
    assert_eq!(dfs.len(), 2);
    // 空 DataFrame 原样保留（无新增列）
    assert_eq!(dfs[0].col_count(), 0);
    assert_eq!(dfs[0].row_count, 0);
    // 正常 DataFrame 已追加列
    assert!(dfs[1].column("daily_return").is_some());
}

#[test]
fn test_execute_null_value_handling_end_to_end() {
    // 含空值与除零的端到端验证
    let df = df_f64("close", vec![Some(10.0), None, Some(0.0), Some(5.0)]);
    let (code, pd) = run_operator(vec![df], r#"{}"#);
    assert_eq!(code, 0);
    let dfs = unwrap_dfa(pd.unwrap());
    let col = dfs[0].column("daily_return").unwrap();
    // i=0: None（首行）
    // i=1: cur=None → None
    // i=2: prev=None → None
    // i=3: prev=0 → None（除零保护）
    assert_approx_vec(
        &col.to_f64_vec(),
        &[None, None, None, None],
    );
}

#[test]
fn test_version_function() {
    let ptr = daily_return_operator_version();
    let bytes = unsafe { std::ffi::CStr::from_ptr(ptr).to_bytes() };
    let version = std::str::from_utf8(bytes).unwrap();
    assert_eq!(version, "0.1.0");
}
