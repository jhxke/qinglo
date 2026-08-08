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
fn test_build_histogram_symmetric_pos_and_neg() {
    // 正负混合样本，bins=4：neg_bins=2, pos_bins=2
    // 数据: [-6, -4, -3, -1, 0, 1, 2, 4, 5, 7]
    // data_min=-6, data_max=7 → abs_bound = max(6,7)=7
    // side_bins_max = max(2,2) = 2 → bin_width = 7/2 = 3.5
    // 负值侧（2 箱）: [-7, -3.5), [-3.5, 0)
    //   idx 0: [-7, -3.5) → -6, -4 → count=2
    //   idx 1: [-3.5, 0)  → -3, -1 → count=2
    // 正值侧（2 箱）: [0, 3.5), [3.5, 7]
    //   idx 2: [0, 3.5)  → 0,1,2 → count=3
    //   idx 3: [3.5, 7]  → 4,5,7 → count=3
    let values = vec![-6.0, -4.0, -3.0, -1.0, 0.0, 1.0, 2.0, 4.0, 5.0, 7.0];
    let df = build_histogram_dataframe(&values, 4, None, None);
    assert_eq!(df.row_count, 4);
    // 必须存在新增的 sign 列，总共 7 列
    assert_eq!(df.col_count(), 7);
    assert!(df.column("sign").is_some());

    let count_col = df.column("count").unwrap();
    // 负值 2 箱
    assert_eq!(count_col.get_i64(0), Some(2)); // -6, -4
    assert_eq!(count_col.get_i64(1), Some(2)); // -3, -1
    // 正值 2 箱
    assert_eq!(count_col.get_i64(2), Some(3)); // 0,1,2
    assert_eq!(count_col.get_i64(3), Some(3)); // 4,5,7

    // 总数 = 10
    let total: i64 = (0..4).map(|i| count_col.get_i64(i).unwrap_or(0)).sum();
    assert_eq!(total, 10);

    // sign 列：负值侧 -1，正值侧 1
    let sign_col = df.column("sign").unwrap();
    assert_eq!(sign_col.get_i64(0), Some(-1));
    assert_eq!(sign_col.get_i64(1), Some(-1));
    assert_eq!(sign_col.get_i64(2), Some(1));
    assert_eq!(sign_col.get_i64(3), Some(1));

    // bin_center：负值箱中心应为负，正值箱中心应为正
    let center_col = df.column("bin_center").unwrap();
    assert!(center_col.get_f64(0).unwrap() < 0.0);
    assert!(center_col.get_f64(1).unwrap() < 0.0);
    assert!(center_col.get_f64(2).unwrap() >= 0.0);
    assert!(center_col.get_f64(3).unwrap() > 0.0);

    // 正值第一个箱 left = 0
    let left_col = df.column("bin_left").unwrap();
    assert!((left_col.get_f64(2).unwrap() - 0.0).abs() < 1e-9);

    // 频率之和 ≈ 1.0
    let freq_col = df.column("frequency").unwrap();
    let total_freq: f64 = (0..4).map(|i| freq_col.get_f64(i).unwrap_or(0.0)).sum();
    assert!((total_freq - 1.0).abs() < 1e-9);
}

#[test]
fn test_build_histogram_all_positive_still_symmetric() {
    // 全正值数据：也应保留负值侧分箱（虽然都是 0 count）
    // 数据: [1,2,3,4,5,6,7,8,9,10], bins=4
    // abs_bound=10, side_bins_max=2 → bin_width=5
    // 负值 2 箱: [-10,-5), [-5,0) → count=0
    // 正值 2 箱: [0,5), [5,10]
    //   idx2: 1,2,3,4 → 4
    //   idx3: 5,6,7,8,9,10 → 6  (10 越界修正到最后一箱)
    let values: Vec<f64> = (1..=10).map(|i| i as f64).collect();
    let df = build_histogram_dataframe(&values, 4, None, None);
    assert_eq!(df.row_count, 4);

    let count_col = df.column("count").unwrap();
    assert_eq!(count_col.get_i64(0), Some(0)); // 负值箱
    assert_eq!(count_col.get_i64(1), Some(0)); // 负值箱
    assert_eq!(count_col.get_i64(2), Some(4)); // [0,5) → 1,2,3,4
    assert_eq!(count_col.get_i64(3), Some(6)); // [5,10] → 5..10

    let sign_col = df.column("sign").unwrap();
    assert_eq!(sign_col.get_i64(0), Some(-1));
    assert_eq!(sign_col.get_i64(1), Some(-1));
    assert_eq!(sign_col.get_i64(2), Some(1));
    assert_eq!(sign_col.get_i64(3), Some(1));
}

#[test]
fn test_build_histogram_odd_bins_pos_gets_one_more() {
    // bins=5 (奇数) → neg_bins=2, pos_bins=3
    // 数据: [-3, -1, 0, 2, 4, 6, 8]
    // data 绝对值: min=0→1e-9, max=8 → abs_bound=8
    // side_bins_max = max(2,3) = 3 → bin_width = 8/3 ≈ 2.666...
    // 负值侧 2 箱: [-16/3, -8/3), [-8/3, 0)
    //   idx 0: [-5.33, -2.67) → -3 → count=1
    //   idx 1: [-2.67, 0)    → -1 → count=1
    // 正值侧 3 箱: [0, 8/3), [8/3, 16/3), [16/3, 8]
    //   idx 2: [0, 2.67)   → 0, 2 → count=2
    //   idx 3: [2.67, 5.33) → 4 → count=1
    //   idx 4: [5.33, 8]   → 6, 8 → count=2
    let values = vec![-3.0, -1.0, 0.0, 2.0, 4.0, 6.0, 8.0];
    let df = build_histogram_dataframe(&values, 5, None, None);
    assert_eq!(df.row_count, 5);

    let count_col = df.column("count").unwrap();
    // 负值侧合计 2
    assert_eq!(
        count_col.get_i64(0).unwrap_or(0) + count_col.get_i64(1).unwrap_or(0),
        2
    );
    // 正值侧合计 5
    let pos_total: i64 = (2..5).map(|i| count_col.get_i64(i).unwrap_or(0)).sum();
    assert_eq!(pos_total, 5);
    // 总数 7
    let total: i64 = (0..5).map(|i| count_col.get_i64(i).unwrap_or(0)).sum();
    assert_eq!(total, 7);

    // sign 列 2 个 -1 + 3 个 1
    let sign_col = df.column("sign").unwrap();
    assert_eq!(sign_col.get_i64(0), Some(-1));
    assert_eq!(sign_col.get_i64(1), Some(-1));
    assert_eq!(sign_col.get_i64(2), Some(1));
    assert_eq!(sign_col.get_i64(3), Some(1));
    assert_eq!(sign_col.get_i64(4), Some(1));
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
    // 7 列含 sign
    assert_eq!(df.col_count(), 7);
    assert!(df.column("sign").is_some());
}

#[test]
fn test_build_histogram_single_value() {
    // 单值全为 5.0 → bins=5 → neg_bins=2, pos_bins=3
    // abs_bound=5, side_bins_max=3 → bin_width=5/3≈1.6667
    // 10 个样本都等于 5.0，应该落入正值侧最后一箱
    let values = vec![5.0; 10];
    let df = build_histogram_dataframe(&values, 5, None, None);
    // 10 个样本全部落入若干正值箱，count 之和应为 10
    let count_col = df.column("count").unwrap();
    let total: i64 = (0..5).map(|i| count_col.get_i64(i).unwrap_or(0)).sum();
    assert_eq!(total, 10);
}

#[test]
fn test_build_histogram_bin_columns_exist() {
    let values = vec![-1.0, 0.0, 1.0];
    let df = build_histogram_dataframe(&values, 2, None, None);
    // 必须存在这 7 列（含 sign）
    assert!(df.column("bin_index").is_some());
    assert!(df.column("bin_left").is_some());
    assert!(df.column("bin_right").is_some());
    assert!(df.column("bin_center").is_some());
    assert!(df.column("count").is_some());
    assert!(df.column("frequency").is_some());
    assert!(df.column("sign").is_some());
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
