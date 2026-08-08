use super::*;
use operator_runtime::DataFrame;

// ============ 参数解析 ============

#[test]
fn test_parse_params_empty() {
    let p = parse_params("");
    assert_eq!(p.expression, "");
    assert_eq!(p.value_column, "");
    assert_eq!(p.bins, "");
}

#[test]
fn test_parse_params_valid() {
    let json = r#"{"expression":"ma5 > ma10","value_column":"future_return_5","bins":"30","min_val":"-0.2","max_val":"0.5"}"#;
    let p = parse_params(json);
    assert_eq!(p.expression, "ma5 > ma10");
    assert_eq!(p.value_column, "future_return_5");
    assert_eq!(p.bins, "30");
    assert_eq!(p.min_val, "-0.2");
    assert_eq!(p.max_val, "0.5");
}

#[test]
fn test_parse_params_invalid_json() {
    let p = parse_params("not json");
    assert_eq!(p.expression, "");
}

#[test]
fn test_parse_bins_defaults_to_twenty() {
    assert_eq!(parse_bins(""), Some(20));
    assert_eq!(parse_bins("   "), Some(20));
}

#[test]
fn test_parse_bins_valid() {
    assert_eq!(parse_bins("10"), Some(10));
    assert_eq!(parse_bins("  30  "), Some(30));
    assert_eq!(parse_bins("1"), Some(1));
}

#[test]
fn test_parse_bins_rejects_invalid() {
    assert_eq!(parse_bins("0"), None);
    assert_eq!(parse_bins("-1"), None);
    assert_eq!(parse_bins("abc"), None);
    assert_eq!(parse_bins("3.5"), None);
}

#[test]
fn test_parse_f64_opt() {
    assert_eq!(parse_f64_opt(""), None);
    assert_eq!(parse_f64_opt("   "), None);
    assert_eq!(parse_f64_opt("3.14"), Some(3.14));
    assert_eq!(parse_f64_opt("-0.2"), Some(-0.2));
    assert_eq!(parse_f64_opt("abc"), None);
}

// ============ 表达式语法（复用自 expression_operator，关键用例抽测） ============

#[test]
fn test_parse_simple_expression() {
    let ast = parse_expression("ma5 > ma10").unwrap();
    match ast {
        Expr::Binary(BinOp::Gt, a, b) => {
            assert_eq!(*a, Expr::Column("ma5".to_string()));
            assert_eq!(*b, Expr::Column("ma10".to_string()));
        }
        other => panic!("期望 Gt Binary，得到 {:?}", other),
    }
}

#[test]
fn test_evaluate_gt_true_and_false() {
    let ast = parse_expression("ma5 > ma10").unwrap();
    let mut cols: HashMap<String, Vec<Option<f64>>> = HashMap::new();
    cols.insert("ma5".to_string(), vec![Some(10.0), Some(5.0)]);
    cols.insert("ma10".to_string(), vec![Some(5.0), Some(10.0)]);
    assert_eq!(evaluate(&ast, 0, &cols), Some(1.0));
    assert_eq!(evaluate(&ast, 1, &cols), Some(0.0));
}

// ============ 直方图统计 ============

fn build_test_df_a() -> DataFrame {
    let mut df = DataFrame::new();
    // signal 条件: ma5 > ma10 → 行1(15>10):真, 行3(20>10):真
    df.add_column(DataFrame::new_float64_column(
        "ma5",
        vec![Some(5.0), Some(15.0), Some(8.0), Some(20.0)],
    ));
    df.add_column(DataFrame::new_float64_column(
        "ma10",
        vec![Some(10.0), Some(10.0), Some(10.0), Some(10.0)],
    ));
    // 未来 5 日收益率列：行1=0.1, 行3=-0.05
    df.add_column(DataFrame::new_float64_column(
        "future_return_5",
        vec![Some(0.05), Some(0.1), Some(-0.02), Some(-0.05)],
    ));
    df
}

fn build_test_df_b() -> DataFrame {
    let mut df = DataFrame::new();
    // ma5 > ma10 → 行0:真(30>20), 行2:真(25>20)
    df.add_column(DataFrame::new_float64_column(
        "ma5",
        vec![Some(30.0), Some(15.0), Some(25.0)],
    ));
    df.add_column(DataFrame::new_float64_column(
        "ma10",
        vec![Some(20.0), Some(20.0), Some(20.0)],
    ));
    // 收益率: 行0=0.15, 行2=0.2
    df.add_column(DataFrame::new_float64_column(
        "future_return_5",
        vec![Some(0.15), Some(-0.01), Some(0.2)],
    ));
    df
}

#[test]
fn test_collect_filtered_values_two_dfs() {
    // 表达式 ma5 > ma10 为真的行:
    // DF-A: 行1(ret=0.1), 行3(ret=-0.05)
    // DF-B: 行0(ret=0.15), 行2(ret=0.2)
    let dfs = vec![build_test_df_a(), build_test_df_b()];
    let ast = parse_expression("ma5 > ma10").unwrap();
    let values = collect_filtered_values(&dfs, &ast, "future_return_5").unwrap();
    // 4 个样本，顺序 DF-A 先，然后 DF-B
    assert_eq!(values.len(), 4);
    assert!((values[0] - 0.1).abs() < 1e-12);
    assert!((values[1] - (-0.05)).abs() < 1e-12);
    assert!((values[2] - 0.15).abs() < 1e-12);
    assert!((values[3] - 0.2).abs() < 1e-12);
}

#[test]
fn test_collect_filtered_values_null_expr_row_skipped() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(
        "ma5",
        vec![None, Some(15.0)], // 行0 ma5=None → 表达式为空 → 视为不成立
    ));
    df.add_column(DataFrame::new_float64_column(
        "ma10",
        vec![Some(10.0), Some(10.0)],
    ));
    df.add_column(DataFrame::new_float64_column(
        "ret",
        vec![Some(0.99), Some(0.1)],
    ));
    let ast = parse_expression("ma5 > ma10").unwrap();
    let values = collect_filtered_values(&[df], &ast, "ret").unwrap();
    assert_eq!(values.len(), 1);
    assert!((values[0] - 0.1).abs() < 1e-12);
}

#[test]
fn test_collect_filtered_values_null_value_skipped() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(
        "ma5",
        vec![Some(20.0), Some(20.0)], // 两行表达式均为真
    ));
    df.add_column(DataFrame::new_float64_column(
        "ma10",
        vec![Some(10.0), Some(10.0)],
    ));
    df.add_column(DataFrame::new_float64_column(
        "ret",
        vec![None, Some(0.1)], // 行0值为空 → 跳过
    ));
    let ast = parse_expression("ma5 > ma10").unwrap();
    let values = collect_filtered_values(&[df], &ast, "ret").unwrap();
    assert_eq!(values.len(), 1);
    assert!((values[0] - 0.1).abs() < 1e-12);
}

#[test]
fn test_build_histogram_simple() {
    // 10 个样本：[0,1,2,3,4,5,6,7,8,9]，bins=5
    // 期望分箱: [0,2) [2,4) [4,6) [6,8) [8,10]
    // counts: 2, 2, 2, 2, 2
    let values: Vec<f64> = (0..10).map(|i| i as f64).collect();
    let df = build_histogram_dataframe(&values, 5, None, None);
    assert_eq!(df.row_count, 5);
    assert_eq!(df.col_count(), 6);

    let count_col = df.column("count").unwrap();
    assert_eq!(count_col.get_i64(0), Some(2));
    assert_eq!(count_col.get_i64(1), Some(2));
    assert_eq!(count_col.get_i64(2), Some(2));
    assert_eq!(count_col.get_i64(3), Some(2));
    assert_eq!(count_col.get_i64(4), Some(2));

    // 验证频率 = 2/10 = 0.2
    let freq_col = df.column("frequency").unwrap();
    for i in 0..5 {
        let f = freq_col.get_f64(i).unwrap();
        assert!((f - 0.2).abs() < 1e-12, "第{}箱频率={}，期望0.2", i, f);
    }

    // 验证边界
    let left_col = df.column("bin_left").unwrap();
    let right_col = df.column("bin_right").unwrap();
    assert!((left_col.get_f64(0).unwrap() - 0.0).abs() < 1e-12);
    assert!((right_col.get_f64(4).unwrap() - 9.0).abs() < 1e-12);
}

#[test]
fn test_build_histogram_empty_values() {
    let values: Vec<f64> = vec![];
    let df = build_histogram_dataframe(&values, 10, None, None);
    assert_eq!(df.row_count, 10);
    // 所有 count=0, frequency=0
    let count_col = df.column("count").unwrap();
    for i in 0..10 {
        assert_eq!(count_col.get_i64(i), Some(0));
    }
}

#[test]
fn test_build_histogram_single_value() {
    // min==max，会自动扩展边界
    let values = vec![5.0; 10];
    let df = build_histogram_dataframe(&values, 5, None, None);
    // 10 个样本全部落入中间某个箱，count 之和应为 10
    let count_col = df.column("count").unwrap();
    let total: i64 = (0..5).map(|i| count_col.get_i64(i).unwrap_or(0)).sum();
    assert_eq!(total, 10);
}

#[test]
fn test_build_histogram_with_custom_range() {
    // 样本值 0..10 = [0,1,2,3,4,5,6,7,8,9]
    // 自定义范围 [2, 8), bins=3, bin_width=2
    // 箱0: [2,4)  → 值 2,3   → count=2
    // 箱1: [4,6)  → 值 4,5   → count=2
    // 箱2: [6,8]  → 值 6,7,8 → count=3  (v=8 <= 8 且 (8-6)/2=1 → floor=1 越界→最后一箱)
    // 值 0,1,9  越界被丢弃
    let values: Vec<f64> = (0..10).map(|i| i as f64).collect();
    let df = build_histogram_dataframe(&values, 3, Some(2.0), Some(8.0));
    assert_eq!(df.row_count, 3);
    let count_col = df.column("count").unwrap();
    assert_eq!(count_col.get_i64(0), Some(2));
    assert_eq!(count_col.get_i64(1), Some(2));
    assert_eq!(count_col.get_i64(2), Some(3));
}

#[test]
fn test_build_histogram_bin_columns_exist() {
    let values = vec![1.0, 2.0, 3.0];
    let df = build_histogram_dataframe(&values, 2, None, None);
    // 必须存在这 6 列
    assert!(df.column("bin_index").is_some());
    assert!(df.column("bin_left").is_some());
    assert!(df.column("bin_right").is_some());
    assert!(df.column("bin_center").is_some());
    assert!(df.column("count").is_some());
    assert!(df.column("frequency").is_some());
    // bin_index 是 0 基编号
    let idx_col = df.column("bin_index").unwrap();
    assert_eq!(idx_col.get_i64(0), Some(0));
    assert_eq!(idx_col.get_i64(1), Some(1));
}

#[test]
fn test_collect_missing_column_returns_error() {
    let df = build_test_df_a();
    let ast = parse_expression("ma5 > ma10").unwrap();
    let res = collect_filtered_values(&[df], &ast, "nonexistent_col");
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(err.contains("nonexistent_col"));
}

// ============ C ABI 端到端 ============

#[test]
fn test_execute_operator_e2e_success() {
    let dfs = vec![build_test_df_a(), build_test_df_b()];
    let input = PortData::DataFrameArray(dfs);
    let mut c_inputs = [operator_runtime::c_abi::portdata_to_c(&input)];

    let params_cstr = CString::new(
        r#"{"expression":"ma5 > ma10","value_column":"future_return_5","bins":"4"}"#
    ).unwrap();

    let mut out: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];

    let rc = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        out.as_mut_ptr(),
        out.len(),
        params_cstr.as_ptr(),
    );

    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }

    assert_eq!(rc, 0, "执行应成功，rc={}", rc);
    assert_ne!(out[0].type_tag, TYPE_NULL, "应有输出");

    let pd = unsafe { portdata_from_c(&mut out[0] as *mut CPortData) };
    match pd {
        PortData::DataFrame(df) => {
            assert_eq!(df.row_count, 4, "bins=4 应输出 4 行");
            // count 之和 = 4 个有效样本
            let count_col = df.column("count").unwrap();
            let total: i64 = (0..4).map(|i| count_col.get_i64(i).unwrap_or(0)).sum();
            assert_eq!(total, 4);
            // frequency 之和 ≈ 1.0
            let freq_col = df.column("frequency").unwrap();
            let total_freq: f64 = (0..4).map(|i| freq_col.get_f64(i).unwrap_or(0.0)).sum();
            assert!((total_freq - 1.0).abs() < 1e-9);
        }
        other => panic!("期望输出 DataFrame，实际 {:?}", other.type_name()),
    }
}

#[test]
fn test_execute_operator_missing_expression_error() {
    let params_cstr = CString::new(r#"{"value_column":"ret"}"#).unwrap();
    let mut out: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];
    let rc = execute_operator(
        std::ptr::null(), 0,
        out.as_mut_ptr(), out.len(),
        params_cstr.as_ptr(),
    );
    assert_eq!(rc, -6, "expression 空应返回 -6");
}

#[test]
fn test_execute_operator_missing_value_column_error() {
    let params_cstr = CString::new(r#"{"expression":"a > b"}"#).unwrap();
    let mut out: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];
    let rc = execute_operator(
        std::ptr::null(), 0,
        out.as_mut_ptr(), out.len(),
        params_cstr.as_ptr(),
    );
    assert_eq!(rc, -6, "value_column 空应返回 -6");
}

#[test]
fn test_execute_operator_invalid_bins_error() {
    let params_cstr = CString::new(
        r#"{"expression":"a > b","value_column":"ret","bins":"-1"}"#
    ).unwrap();
    let mut out: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];
    let rc = execute_operator(
        std::ptr::null(), 0,
        out.as_mut_ptr(), out.len(),
        params_cstr.as_ptr(),
    );
    assert_eq!(rc, -6, "bins 非法应返回 -6");
}

#[test]
fn test_execute_operator_expression_syntax_error() {
    let df = build_test_df_a();
    let input = PortData::DataFrameArray(vec![df]);
    let mut c_inputs = [operator_runtime::c_abi::portdata_to_c(&input)];
    let params_cstr = CString::new(
        r#"{"expression":"a < b < c","value_column":"ret"}"#
    ).unwrap();
    let mut out: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: std::ptr::null_mut() },
    }; 2];
    let rc = execute_operator(
        c_inputs.as_ptr(), c_inputs.len(),
        out.as_mut_ptr(), out.len(),
        params_cstr.as_ptr(),
    );
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
    assert_eq!(rc, -7, "表达式语法错误应返回 -7");
}
