use super::*;
use operator_runtime::DataFrame;

// ============ 参数解析 ============

#[test]
fn test_parse_params_empty() {
    let p = parse_params("");
    assert_eq!(p.n, "");
    assert_eq!(p.column_name, "");
    assert_eq!(p.source_column, "");
}

#[test]
fn test_parse_params_valid() {
    let json = r#"{"n":"20","column_name":"hhv20","source_column":"high"}"#;
    let p = parse_params(json);
    assert_eq!(p.n, "20");
    assert_eq!(p.column_name, "hhv20");
    assert_eq!(p.source_column, "high");
}

#[test]
fn test_parse_params_invalid_json() {
    let p = parse_params("not json");
    assert_eq!(p.n, "");
}

#[test]
fn test_parse_n_empty_defaults_to_twenty() {
    assert_eq!(parse_n(""), Some(20));
    assert_eq!(parse_n("   "), Some(20));
}

#[test]
fn test_parse_n_valid_positive() {
    assert_eq!(parse_n("5"), Some(5));
    assert_eq!(parse_n("  30  "), Some(30));
    assert_eq!(parse_n("1"), Some(1));
}

#[test]
fn test_parse_n_rejects_invalid() {
    assert_eq!(parse_n("0"), None);
    assert_eq!(parse_n("-1"), None);
    assert_eq!(parse_n("abc"), None);
    assert_eq!(parse_n("3.5"), None);
}

#[test]
fn test_resolve_column_defaults() {
    assert_eq!(resolve_column("", "high"), "high");
    assert_eq!(resolve_column("   ", "high"), "high");
    assert_eq!(resolve_column("  close  ", "high"), "close");
}

// ============ 滚动最大值 ============

#[test]
fn test_rolling_max_basic() {
    // [10, 12, 11, 13, 9], n=3
    // i=0,1: 窗口不足 → None
    // i=2: max(10,12,11)=12
    // i=3: max(12,11,13)=13
    // i=4: max(11,13,9)=13
    let v = vec![
        Some(10.0),
        Some(12.0),
        Some(11.0),
        Some(13.0),
        Some(9.0),
    ];
    let out = compute_rolling_max(&v, 3);
    assert_eq!(out.len(), 5);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert_eq!(out[2].unwrap(), 12.0);
    assert_eq!(out[3].unwrap(), 13.0);
    assert_eq!(out[4].unwrap(), 13.0);
}

#[test]
fn test_rolling_max_monotone_decreasing() {
    // 递减序列 [5,4,3,2,1], n=3
    // i=2: max(5,4,3)=5；i=3: max(4,3,2)=4；i=4: max(3,2,1)=3
    let v = vec![Some(5.0), Some(4.0), Some(3.0), Some(2.0), Some(1.0)];
    let out = compute_rolling_max(&v, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert_eq!(out[2].unwrap(), 5.0);
    assert_eq!(out[3].unwrap(), 4.0);
    assert_eq!(out[4].unwrap(), 3.0);
}

#[test]
fn test_rolling_max_monotone_increasing() {
    // 递增序列 [1,2,3,4,5], n=3
    // i=2: max(1,2,3)=3；i=3: max(2,3,4)=4；i=4: max(3,4,5)=5
    let v = vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)];
    let out = compute_rolling_max(&v, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert_eq!(out[2].unwrap(), 3.0);
    assert_eq!(out[3].unwrap(), 4.0);
    assert_eq!(out[4].unwrap(), 5.0);
}

#[test]
fn test_rolling_max_constant() {
    // 常数序列 [7,7,7,7], n=2
    let v = vec![Some(7.0), Some(7.0), Some(7.0), Some(7.0)];
    let out = compute_rolling_max(&v, 2);
    assert!(out[0].is_none());
    assert_eq!(out[1].unwrap(), 7.0);
    assert_eq!(out[2].unwrap(), 7.0);
    assert_eq!(out[3].unwrap(), 7.0);
}

#[test]
fn test_rolling_max_n_one_each_is_self() {
    // n=1：每行等于自身
    let v = vec![Some(5.0), Some(10.0), Some(3.0)];
    let out = compute_rolling_max(&v, 1);
    assert_eq!(out[0].unwrap(), 5.0);
    assert_eq!(out[1].unwrap(), 10.0);
    assert_eq!(out[2].unwrap(), 3.0);
}

#[test]
fn test_rolling_max_n_equals_len() {
    // n 等于序列长度：仅最后一行有值
    let v = vec![Some(3.0), Some(7.0), Some(2.0), Some(9.0)];
    let out = compute_rolling_max(&v, 4);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none());
    assert_eq!(out[3].unwrap(), 9.0);
}

#[test]
fn test_rolling_max_null_propagation() {
    // 窗口含空 → 整窗 None；空离开窗口后恢复
    let v = vec![
        Some(10.0),
        None,
        Some(12.0),
        Some(13.0),
        Some(9.0),
    ];
    let out = compute_rolling_max(&v, 3);
    assert_eq!(out.len(), 5);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    // i=2: 窗口[10,None,12] 含空 → None
    assert!(out[2].is_none());
    // i=3: 窗口[None,12,13] 含空 → None
    assert!(out[3].is_none());
    // i=4: 窗口[12,13,9] 全有效 → 13
    assert_eq!(out[4].unwrap(), 13.0);
}

#[test]
fn test_rolling_max_leading_none_then_recovers() {
    // 开头多个 None，之后恢复
    let v = vec![None, None, Some(5.0), Some(8.0), Some(3.0)];
    let out = compute_rolling_max(&v, 2);
    // i=0: 窗口不足 → None
    // i=1: 窗口[None,None] 全空 → None
    // i=2: 窗口[None,5] 含空 → None
    // i=3: 窗口[5,8] → 8
    // i=4: 窗口[8,3] → 8
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none());
    assert_eq!(out[3].unwrap(), 8.0);
    assert_eq!(out[4].unwrap(), 8.0);
}

#[test]
fn test_rolling_max_n_zero_defensive() {
    let v = vec![Some(1.0), Some(2.0), Some(3.0)];
    let out = compute_rolling_max(&v, 0);
    assert_eq!(out.len(), 3);
    assert!(out.iter().all(|x| x.is_none()));
}

#[test]
fn test_rolling_max_empty_input() {
    let v: Vec<Option<f64>> = vec![];
    let out = compute_rolling_max(&v, 5);
    assert!(out.is_empty());
}

#[test]
fn test_rolling_max_large_window_all_none() {
    // 窗口大于序列长度 → 全 None
    let v = vec![Some(1.0), Some(2.0), Some(3.0)];
    let out = compute_rolling_max(&v, 10);
    assert_eq!(out.len(), 3);
    assert!(out.iter().all(|x| x.is_none()));
}

#[test]
fn test_rolling_max_negative_values() {
    // 含负数
    let v = vec![Some(-5.0), Some(-2.0), Some(-10.0), Some(-1.0)];
    let out = compute_rolling_max(&v, 3);
    // i=0,1: None
    // i=2: max(-5,-2,-10) = -2
    // i=3: max(-2,-10,-1) = -1
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert_eq!(out[2].unwrap(), -2.0);
    assert_eq!(out[3].unwrap(), -1.0);
}

#[test]
fn test_rolling_max_duplicated_max() {
    // 重复最大值：[5, 9, 9, 3, 9], n=3
    let v = vec![Some(5.0), Some(9.0), Some(9.0), Some(3.0), Some(9.0)];
    let out = compute_rolling_max(&v, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert_eq!(out[2].unwrap(), 9.0);
    assert_eq!(out[3].unwrap(), 9.0);
    assert_eq!(out[4].unwrap(), 9.0);
}

#[test]
fn test_rolling_max_long_series_correctness() {
    // 长序列随机性验证（与朴素 O(n*k) 对照）
    let values: Vec<Option<f64>> = (0..100)
        .map(|i| {
            // 伪随机但有规律：i*7%13 + i%5
            let v = ((i * 7) % 13) as f64 + (i % 5) as f64 * 0.5;
            Some(v)
        })
        .collect();
    let n = 7;
    let fast = compute_rolling_max(&values, n);
    // 朴素实现对照
    let naive: Vec<Option<f64>> = (0..values.len())
        .map(|i| {
            if i + 1 < n {
                None
            } else {
                let mut m = f64::MIN;
                for k in (i + 1 - n)..=i {
                    if let Some(v) = values[k] {
                        if v > m {
                            m = v;
                        }
                    }
                }
                if m == f64::MIN {
                    None
                } else {
                    Some(m)
                }
            }
        })
        .collect();
    assert_eq!(fast.len(), naive.len());
    for (i, (a, b)) in fast.iter().zip(naive.iter()).enumerate() {
        assert_eq!(a, b, "第 {} 行不一致: fast={:?} naive={:?}", i, a, b);
    }
}

// ============ 辅助 DataFrame 构造 ============

fn df_with_f64(col: &str, vals: Vec<Option<f64>>) -> DataFrame {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(col, vals));
    df
}

fn df_with_high_f64_and_int64(
    fvals: Vec<Option<f64>>,
    ivals: Vec<Option<i64>>,
) -> DataFrame {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column("high", fvals));
    df.add_column(DataFrame::new_int64_column("vol", ivals));
    df
}

// ============ apply_highest ============

#[test]
fn test_apply_basic() {
    let mut df = df_with_f64(
        "high",
        vec![Some(10.0), Some(12.0), Some(11.0), Some(13.0), Some(9.0)],
    );
    apply_highest(&mut df, "high", 3, "highest_3");
    let col = df.column("highest_3").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert!(col.get_f64(0).is_none());
    assert!(col.get_f64(1).is_none());
    assert_eq!(col.get_f64(2).unwrap(), 12.0);
    assert_eq!(col.get_f64(3).unwrap(), 13.0);
    assert_eq!(col.get_f64(4).unwrap(), 13.0);
    // 源列保留
    assert_eq!(df.col_count(), 2);
    assert_eq!(df.column("high").unwrap().get_f64(3), Some(13.0));
}

#[test]
fn test_apply_int64_source_promotion() {
    // 源列为 Int64，应提升为 f64 计算
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_int64_column(
        "high",
        vec![Some(10), Some(12), Some(11), Some(13), Some(9)],
    ));
    apply_highest(&mut df, "high", 3, "hhv");
    let col = df.column("hhv").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert!(col.get_f64(0).is_none());
    assert!(col.get_f64(1).is_none());
    assert_eq!(col.get_f64(2).unwrap(), 12.0);
    assert_eq!(col.get_f64(3).unwrap(), 13.0);
    assert_eq!(col.get_f64(4).unwrap(), 13.0);
    // 源列仍为 Int64
    assert_eq!(df.column("high").unwrap().data_type, DataType::Int64);
}

#[test]
fn test_apply_custom_source_column() {
    // 用 close 列计算 N 日收盘价最高
    let mut df = df_with_f64(
        "close",
        vec![Some(5.0), Some(8.0), Some(6.0), Some(7.0)],
    );
    apply_highest(&mut df, "close", 2, "close_high_2");
    let col = df.column("close_high_2").unwrap();
    // i=0: None；i=1: max(5,8)=8；i=2: max(8,6)=8；i=3: max(6,7)=7
    assert!(col.get_f64(0).is_none());
    assert_eq!(col.get_f64(1).unwrap(), 8.0);
    assert_eq!(col.get_f64(2).unwrap(), 8.0);
    assert_eq!(col.get_f64(3).unwrap(), 7.0);
}

#[test]
fn test_apply_overwrite_existing_result_column() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(
        "high",
        vec![Some(10.0), Some(12.0), Some(11.0), Some(13.0)],
    ));
    df.add_column(DataFrame::new_float64_column("hhv", vec![Some(999.0); 4]));
    apply_highest(&mut df, "high", 3, "hhv");
    // 列数仍为 2
    assert_eq!(df.col_count(), 2);
    let col = df.column("hhv").unwrap();
    assert!(col.get_f64(0).is_none());
    assert!(col.get_f64(1).is_none());
    assert_eq!(col.get_f64(2).unwrap(), 12.0);
    assert_eq!(col.get_f64(3).unwrap(), 13.0);
    // 源列保留
    assert_eq!(df.column("high").unwrap().get_f64(3), Some(13.0));
}

#[test]
fn test_apply_result_equals_source_column_overwrites() {
    let mut df = df_with_f64(
        "high",
        vec![Some(10.0), Some(12.0), Some(11.0), Some(13.0)],
    );
    apply_highest(&mut df, "high", 3, "high");
    // 列数仍为 1（覆盖了 high）
    assert_eq!(df.col_count(), 1);
    let col = df.column("high").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert!(col.get_f64(0).is_none());
    assert!(col.get_f64(1).is_none());
    assert_eq!(col.get_f64(2).unwrap(), 12.0);
    assert_eq!(col.get_f64(3).unwrap(), 13.0);
}

#[test]
fn test_apply_missing_source_column_skipped() {
    let mut df = df_with_f64("close", vec![Some(10.0), Some(12.0)]);
    apply_highest(&mut df, "high", 3, "highest_3");
    assert!(df.column("highest_3").is_none());
    assert_eq!(df.col_count(), 1); // 原表不变
}

#[test]
fn test_apply_empty_df_noop() {
    let mut df = DataFrame::new();
    apply_highest(&mut df, "high", 5, "highest_5");
    assert_eq!(df.row_count, 0);
    assert_eq!(df.col_count(), 0);
}

#[test]
fn test_apply_string_column_not_supported() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_string_column("high", vec![Some("a"), Some("b")]));
    apply_highest(&mut df, "high", 2, "highest_2");
    assert!(df.column("highest_2").is_none());
    assert_eq!(df.col_count(), 1);
}

#[test]
fn test_apply_preserves_other_columns() {
    // 多列场景：结果列追加，不影响其他列
    let mut df = df_with_high_f64_and_int64(
        vec![Some(10.0), Some(12.0), Some(11.0), Some(13.0)],
        vec![Some(100), Some(200), Some(150), Some(180)],
    );
    apply_highest(&mut df, "high", 2, "hhv2");
    assert_eq!(df.col_count(), 3);
    assert_eq!(df.column("vol").unwrap().data_type, DataType::Int64);
    assert_eq!(df.column("vol").unwrap().get_i64(2), Some(150));
    let col = df.column("hhv2").unwrap();
    // i=0: None；i=1: max(10,12)=12；i=2: max(12,11)=12；i=3: max(11,13)=13
    assert!(col.get_f64(0).is_none());
    assert_eq!(col.get_f64(1).unwrap(), 12.0);
    assert_eq!(col.get_f64(2).unwrap(), 12.0);
    assert_eq!(col.get_f64(3).unwrap(), 13.0);
}

// ============ execute_operator 端到端 ============

fn build_stock_df(n: usize) -> DataFrame {
    let mut df = DataFrame::new();
    // 构造 high 序列：基准 10 + 递增 + 末尾冲高
    let high: Vec<Option<f64>> = (0..n)
        .map(|i| Some(10.0 + (i as f64) * 0.5 + (if i >= n - 1 { 5.0 } else { 0.0 })))
        .collect();
    df.add_column(DataFrame::new_float64_column("high", high));
    df
}

#[test]
fn test_execute_default_params() {
    // 默认 n=20, source=high, result=highest_20
    let df = build_stock_df(25);
    let input = PortData::DataFrameArray(vec![df]);
    let mut c_in = [operator_runtime::c_abi::portdata_to_c(&input)];
    let params = CString::new("{}").unwrap();
    let mut out_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];

    let rc = execute_operator(
        c_in.as_ptr(),
        c_in.len(),
        out_slots.as_mut_ptr(),
        out_slots.len(),
        params.as_ptr(),
    );
    assert_eq!(rc, 0);

    if c_in[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_in[0] as *mut CPortData);
    }

    let pd = unsafe { portdata_from_c(&mut out_slots[0] as *mut CPortData) };
    match pd {
        PortData::DataFrameArray(dfs) => {
            assert_eq!(dfs.len(), 1);
            let df = &dfs[0];
            assert_eq!(df.row_count, 25);
            // 应有 high + highest_20 两列
            assert!(df.column("high").is_some());
            let col = df.column("highest_20").unwrap();
            assert_eq!(col.data_type, DataType::Float64);
            // 前 19 行 None
            for i in 0..19 {
                assert!(col.get_f64(i).is_none(), "前 19 行应为空，第 {} 行非空", i);
            }
            // 第 20 行（i=19）开始有值
            assert!(col.get_f64(19).is_some());
        }
        _ => panic!("期望 DataFrameArray 输出"),
    }
}

#[test]
fn test_execute_custom_n_and_column() {
    let df = build_stock_df(10);
    let input = PortData::DataFrameArray(vec![df]);
    let mut c_in = [operator_runtime::c_abi::portdata_to_c(&input)];
    let params = CString::new(r#"{"n":"3","column_name":"hhv3"}"#).unwrap();
    let mut out_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];

    let rc = execute_operator(
        c_in.as_ptr(),
        c_in.len(),
        out_slots.as_mut_ptr(),
        out_slots.len(),
        params.as_ptr(),
    );
    assert_eq!(rc, 0);

    if c_in[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_in[0] as *mut CPortData);
    }

    let pd = unsafe { portdata_from_c(&mut out_slots[0] as *mut CPortData) };
    match pd {
        PortData::DataFrameArray(dfs) => {
            let df = &dfs[0];
            let col = df.column("hhv3").unwrap();
            // high 序列: 10, 10.5, 11, 11.5, 12, 12.5, 13, 13.5, 14, 19.5 (末尾 i=9 冲高 +5)
            // n=3:
            // i=0,1: None
            // i=2: max(10,10.5,11)=11
            // i=3: max(10.5,11,11.5)=11.5
            // ...
            // i=9: max(13.5,14,19.5)=19.5（窗口[i-2,i-1,i]=[13.5,14,19.5]）
            assert!(col.get_f64(0).is_none());
            assert!(col.get_f64(1).is_none());
            assert_eq!(col.get_f64(2).unwrap(), 11.0);
            assert_eq!(col.get_f64(3).unwrap(), 11.5);
            assert_eq!(col.get_f64(9).unwrap(), 19.5);
        }
        _ => panic!("期望 DataFrameArray 输出"),
    }
}

#[test]
fn test_execute_invalid_n_returns_error() {
    let df = build_stock_df(5);
    let input = PortData::DataFrameArray(vec![df]);
    let mut c_in = [operator_runtime::c_abi::portdata_to_c(&input)];
    let params = CString::new(r#"{"n":"0"}"#).unwrap();
    let mut out_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];

    let rc = execute_operator(
        c_in.as_ptr(),
        c_in.len(),
        out_slots.as_mut_ptr(),
        out_slots.len(),
        params.as_ptr(),
    );
    assert_eq!(rc, -6);

    if c_in[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_in[0] as *mut CPortData);
    }
}

#[test]
fn test_execute_missing_input_returns_error() {
    let params = CString::new("{}").unwrap();
    let mut out_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];

    let rc = execute_operator(
        std::ptr::null(),
        0,
        out_slots.as_mut_ptr(),
        out_slots.len(),
        params.as_ptr(),
    );
    assert_eq!(rc, -3);
}

#[test]
fn test_execute_multiple_dataframes() {
    // 多个 DataFrame 输入，各自独立计算
    let df1 = build_stock_df(8);
    let df2 = build_stock_df(12);
    let input = PortData::DataFrameArray(vec![df1, df2]);
    let mut c_in = [operator_runtime::c_abi::portdata_to_c(&input)];
    let params = CString::new(r#"{"n":"5"}"#).unwrap();
    let mut out_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];

    let rc = execute_operator(
        c_in.as_ptr(),
        c_in.len(),
        out_slots.as_mut_ptr(),
        out_slots.len(),
        params.as_ptr(),
    );
    assert_eq!(rc, 0);

    if c_in[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_in[0] as *mut CPortData);
    }

    let pd = unsafe { portdata_from_c(&mut out_slots[0] as *mut CPortData) };
    match pd {
        PortData::DataFrameArray(dfs) => {
            assert_eq!(dfs.len(), 2);
            assert_eq!(dfs[0].row_count, 8);
            assert_eq!(dfs[1].row_count, 12);
            // 两个 DataFrame 都应有 highest_5 列
            assert!(dfs[0].column("highest_5").is_some());
            assert!(dfs[1].column("highest_5").is_some());
        }
        _ => panic!("期望 DataFrameArray 输出"),
    }
}

#[test]
fn test_execute_single_dataframe_input() {
    // 单个 DataFrame 输入应被包装为单元素数组处理
    let df = build_stock_df(8);
    let input = PortData::DataFrame(df);
    let mut c_in = [operator_runtime::c_abi::portdata_to_c(&input)];
    let params = CString::new(r#"{"n":"3"}"#).unwrap();
    let mut out_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];

    let rc = execute_operator(
        c_in.as_ptr(),
        c_in.len(),
        out_slots.as_mut_ptr(),
        out_slots.len(),
        params.as_ptr(),
    );
    assert_eq!(rc, 0);

    if c_in[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_in[0] as *mut CPortData);
    }

    let pd = unsafe { portdata_from_c(&mut out_slots[0] as *mut CPortData) };
    match pd {
        PortData::DataFrameArray(dfs) => {
            // 输出统一为 DataFrameArray
            assert_eq!(dfs.len(), 1);
            assert!(dfs[0].column("highest_3").is_some());
        }
        _ => panic!("期望 DataFrameArray 输出"),
    }
}

#[test]
fn test_execute_version() {
    let v = unsafe { CStr::from_ptr(highest_operator_version()) }
        .to_str()
        .unwrap();
    assert_eq!(v, "0.1.0");
}
