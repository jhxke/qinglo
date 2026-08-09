use super::*;
use operator_runtime::c_abi::{portdata_to_c, CPortData, CPortValue, TYPE_NULL};
use operator_runtime::{DataFrame, PortData};
use std::ffi::CString;
use std::ptr;

// ============ 参数解析 ============

#[test]
fn test_parse_params_empty() {
    let p = parse_params("");
    assert_eq!(p.factor_column, "");
    assert_eq!(p.return_column, "");
    assert_eq!(p.bins, "");
    assert_eq!(p.min_val, "");
    assert_eq!(p.max_val, "");
    assert_eq!(p.result_column, "");
}

#[test]
fn test_parse_params_valid() {
    let json = r#"{"factor_column":"f1","return_column":"ret","bins":"10","min_val":"-1","max_val":"1","result_column":"avg_ret"}"#;
    let p = parse_params(json);
    assert_eq!(p.factor_column, "f1");
    assert_eq!(p.return_column, "ret");
    assert_eq!(p.bins, "10");
    assert_eq!(p.min_val, "-1");
    assert_eq!(p.max_val, "1");
    assert_eq!(p.result_column, "avg_ret");
}

#[test]
fn test_parse_params_invalid_json() {
    let p = parse_params("not json");
    assert_eq!(p.factor_column, "");
}

#[test]
fn test_parse_bins_empty_defaults_to_twenty() {
    assert_eq!(parse_bins(""), Some(20));
    assert_eq!(parse_bins("   "), Some(20));
}

#[test]
fn test_parse_bins_valid_positive() {
    assert_eq!(parse_bins("5"), Some(5));
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
fn test_parse_f64_opt_empty() {
    assert_eq!(parse_f64_opt(""), None);
    assert_eq!(parse_f64_opt("   "), None);
}

#[test]
fn test_parse_f64_opt_valid() {
    assert_eq!(parse_f64_opt("-1.5"), Some(-1.5));
    assert_eq!(parse_f64_opt("  2.0  "), Some(2.0));
}

#[test]
fn test_parse_f64_opt_invalid() {
    assert_eq!(parse_f64_opt("abc"), None);
}

#[test]
fn test_resolve_column_defaults() {
    assert_eq!(resolve_column("", "mean_return"), "mean_return");
    assert_eq!(resolve_column("   ", "mean_return"), "mean_return");
    assert_eq!(resolve_column("  avg  ", "mean_return"), "avg");
}

// ============ 样本收集 ============

fn build_factor_df(factor: &[Option<f64>], ret: &[Option<f64>]) -> DataFrame {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column("factor", factor.to_vec()));
    df.add_column(DataFrame::new_float64_column("return", ret.to_vec()));
    df
}

#[test]
fn test_collect_samples_basic() {
    let df = build_factor_df(
        &[Some(1.0), Some(2.0), Some(3.0)],
        &[Some(0.1), Some(0.2), Some(0.3)],
    );
    let s = collect_samples(&[df], "factor", "return").unwrap();
    assert_eq!(s.len(), 3);
    assert_eq!(s[0], (1.0, 0.1));
    assert_eq!(s[2], (3.0, 0.3));
}

#[test]
fn test_collect_samples_skips_nulls() {
    let df = build_factor_df(
        &[Some(1.0), None, Some(3.0)],
        &[Some(0.1), Some(0.2), None],
    );
    let s = collect_samples(&[df], "factor", "return").unwrap();
    // row0: both Some -> kept; row1: factor None -> skip; row2: return None -> skip
    assert_eq!(s.len(), 1);
    assert_eq!(s[0], (1.0, 0.1));
}

#[test]
fn test_collect_samples_skips_non_finite() {
    // 每行恰好一个非有限值，确保该行被跳过：
    //   row0: factor=1.0, return=0.1   -> 全有限，保留
    //   row1: factor=2.0, return=NaN  -> return 非有限，跳过
    //   row2: factor=Inf, return=0.3  -> factor 非有限，跳过
    let df = build_factor_df(
        &[Some(1.0), Some(2.0), Some(f64::INFINITY)],
        &[Some(0.1), Some(f64::NAN), Some(0.3)],
    );
    let s = collect_samples(&[df], "factor", "return").unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s[0], (1.0, 0.1));
}

#[test]
fn test_collect_samples_multi_df_aggregation() {
    let df1 = build_factor_df(&[Some(1.0)], &[Some(0.1)]);
    let df2 = build_factor_df(&[Some(2.0)], &[Some(0.2)]);
    let s = collect_samples(&[df1, df2], "factor", "return").unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0], (1.0, 0.1));
    assert_eq!(s[1], (2.0, 0.2));
}

#[test]
fn test_collect_samples_missing_factor_col() {
    let df = build_factor_df(&[Some(1.0)], &[Some(0.1)]);
    let err = collect_samples(&[df], "xxx", "return").unwrap_err();
    assert!(err.contains("因子列"));
    assert!(err.contains("xxx"));
}

#[test]
fn test_collect_samples_missing_return_col() {
    let df = build_factor_df(&[Some(1.0)], &[Some(0.1)]);
    let err = collect_samples(&[df], "factor", "xxx").unwrap_err();
    assert!(err.contains("收益率列"));
    assert!(err.contains("xxx"));
}

#[test]
fn test_collect_samples_supports_int64() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_int64_column("factor", vec![Some(1), Some(2)]));
    df.add_column(DataFrame::new_int64_column("return", vec![Some(10), Some(20)]));
    let s = collect_samples(&[df], "factor", "return").unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(s[0], (1.0, 10.0));
    assert_eq!(s[1], (2.0, 20.0));
}

#[test]
fn test_collect_samples_empty_df_skipped() {
    let empty = DataFrame::new();
    let df = build_factor_df(&[Some(1.0)], &[Some(0.1)]);
    let s = collect_samples(&[empty, df], "factor", "return").unwrap();
    assert_eq!(s.len(), 1);
}

// ============ 直方图构建 ============

#[test]
fn test_build_histogram_basic_binning() {
    // factor in [1, 4], bins=2 -> bin0=[1, 2.5), bin1=[2.5, 4]
    let samples: Vec<Sample> = vec![
        (1.0, 0.10),
        (2.0, 0.20),
        (3.0, 0.60),
        (4.0, 0.80),
    ];
    let df = build_factor_histogram_dataframe(&samples, 2, None, None, "mean_return");
    assert_eq!(df.row_count, 2);
    let count = df.column("count").unwrap().to_i64_vec();
    assert_eq!(count, vec![Some(2), Some(2)]);
    let mean = df.column("mean_return").unwrap().to_f64_vec();
    assert!((mean[0].unwrap() - 0.15).abs() < 1e-12); // (0.10+0.20)/2
    assert!((mean[1].unwrap() - 0.70).abs() < 1e-12); // (0.60+0.80)/2
}

#[test]
fn test_build_histogram_bin_boundaries() {
    // factor in [0, 10], bins=2 -> bin0=[0,5), bin1=[5,10]
    let samples: Vec<Sample> = vec![(0.0, 0.0), (10.0, 1.0)];
    let df = build_factor_histogram_dataframe(&samples, 2, None, None, "mean_return");
    let left = df.column("bin_left").unwrap().to_f64_vec();
    let right = df.column("bin_right").unwrap().to_f64_vec();
    let center = df.column("bin_center").unwrap().to_f64_vec();
    assert_eq!(left, vec![Some(0.0), Some(5.0)]);
    assert_eq!(right, vec![Some(5.0), Some(10.0)]);
    assert_eq!(center, vec![Some(2.5), Some(7.5)]);
}

#[test]
fn test_build_histogram_max_value_in_last_bin() {
    // range [0, 10], bins=2; value 10 (==max) falls into last bin (right-closed)
    let samples: Vec<Sample> = vec![(10.0, 1.0)];
    let df = build_factor_histogram_dataframe(&samples, 2, None, None, "mean_return");
    let count = df.column("count").unwrap().to_i64_vec();
    assert_eq!(count, vec![Some(0), Some(1)]);
}

#[test]
fn test_build_histogram_empty_bin_mean_null() {
    // 用户指定宽范围 [1, 2]，bins=2 → bin0=[1,1.5), bin1=[1.5,2]；
    // 两个样本都落入 bin0，bin1 为空 → mean 为 None
    let samples: Vec<Sample> = vec![(1.0, 0.1), (1.1, 0.2)];
    let df = build_factor_histogram_dataframe(&samples, 2, Some(1.0), Some(2.0), "mean_return");
    let count = df.column("count").unwrap().to_i64_vec();
    let mean = df.column("mean_return").unwrap().to_f64_vec();
    assert_eq!(count, vec![Some(2), Some(0)]);
    assert!(mean[0].is_some());
    assert!(mean[1].is_none());
}

#[test]
fn test_build_histogram_empty_samples() {
    let samples: Vec<Sample> = vec![];
    let df = build_factor_histogram_dataframe(&samples, 3, None, None, "mean_return");
    assert_eq!(df.row_count, 3);
    let count = df.column("count").unwrap().to_i64_vec();
    assert_eq!(count, vec![Some(0), Some(0), Some(0)]);
    let mean = df.column("mean_return").unwrap().to_f64_vec();
    assert!(mean.iter().all(|m| m.is_none()));
}

#[test]
fn test_build_histogram_frequency() {
    let samples: Vec<Sample> = vec![
        (1.0, 0.0),
        (2.0, 0.0),
        (6.0, 0.0),
        (8.0, 0.0),
    ];
    let df = build_factor_histogram_dataframe(&samples, 2, None, None, "mean_return");
    let freq = df.column("frequency").unwrap().to_f64_vec();
    assert!((freq[0].unwrap() - 0.5).abs() < 1e-12);
    assert!((freq[1].unwrap() - 0.5).abs() < 1e-12);
}

#[test]
fn test_build_histogram_custom_result_column() {
    let samples: Vec<Sample> = vec![(1.0, 0.1)];
    let df = build_factor_histogram_dataframe(&samples, 2, None, None, "avg_ret");
    assert!(df.column("avg_ret").is_some());
    assert!(df.column("mean_return").is_none());
}

#[test]
fn test_build_histogram_user_bounds_filter() {
    // data range [1, 8], but user bounds [1, 4] -> values 6,8 out of range filtered
    let samples: Vec<Sample> = vec![
        (1.0, 0.1),
        (2.0, 0.2),
        (6.0, 0.6),
        (8.0, 0.8),
    ];
    let df = build_factor_histogram_dataframe(&samples, 2, Some(1.0), Some(4.0), "mean_return");
    let count = df.column("count").unwrap().to_i64_vec();
    assert_eq!(count, vec![Some(2), Some(0)]);
}

#[test]
fn test_build_histogram_min_max_swapped() {
    // user passes min>max; auto-swap -> range [0, 2], bins=2, bin0=[0,1), bin1=[1,2]
    let samples: Vec<Sample> = vec![(1.0, 0.1), (2.0, 0.2)];
    let df = build_factor_histogram_dataframe(&samples, 2, Some(2.0), Some(0.0), "mean_return");
    let left = df.column("bin_left").unwrap().to_f64_vec();
    assert_eq!(left, vec![Some(0.0), Some(1.0)]);
    // 1.0 -> bin1; 2.0 -> bin1 (last bin right-closed)
    let count = df.column("count").unwrap().to_i64_vec();
    assert_eq!(count, vec![Some(0), Some(2)]);
}

#[test]
fn test_build_histogram_min_equals_max_expanded() {
    // all factor values == 5.0; min==max -> expand by 0.5 -> [4.5, 5.5]
    let samples: Vec<Sample> = vec![(5.0, 0.1), (5.0, 0.2)];
    let df = build_factor_histogram_dataframe(&samples, 2, None, None, "mean_return");
    let left = df.column("bin_left").unwrap().to_f64_vec();
    let right = df.column("bin_right").unwrap().to_f64_vec();
    assert_eq!(left, vec![Some(4.5), Some(5.0)]);
    assert_eq!(right, vec![Some(5.0), Some(5.5)]);
    // both 5.0 fall into bin1=[5.0, 5.5]
    let count = df.column("count").unwrap().to_i64_vec();
    assert_eq!(count, vec![Some(0), Some(2)]);
}

#[test]
fn test_build_histogram_single_bin() {
    let samples: Vec<Sample> = vec![(1.0, 0.1), (2.0, 0.2), (3.0, 0.3)];
    let df = build_factor_histogram_dataframe(&samples, 1, None, None, "mean_return");
    assert_eq!(df.row_count, 1);
    let count = df.column("count").unwrap().to_i64_vec();
    assert_eq!(count, vec![Some(3)]);
    let mean = df.column("mean_return").unwrap().to_f64_vec();
    assert!((mean[0].unwrap() - 0.2).abs() < 1e-12); // (0.1+0.2+0.3)/3 = 0.2
}

// ============ execute_operator 端到端 ============

fn run_operator(input_pd: PortData, params_json: &str) -> (i32, Option<PortData>) {
    let mut c_inputs = [portdata_to_c(&input_pd)];
    let params_cstr = CString::new(params_json).unwrap_or_default();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];

    let rc = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
    let out = if output_slots[0].type_tag != TYPE_NULL {
        Some(unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) })
    } else {
        None
    };
    (rc, out)
}

#[test]
fn execute_missing_factor_column_param() {
    let df = build_factor_df(&[Some(1.0)], &[Some(0.1)]);
    let (rc, _) = run_operator(PortData::DataFrame(df), r#"{"return_column":"return"}"#);
    assert_eq!(rc, -6);
}

#[test]
fn execute_missing_return_column_param() {
    let df = build_factor_df(&[Some(1.0)], &[Some(0.1)]);
    let (rc, _) = run_operator(PortData::DataFrame(df), r#"{"factor_column":"factor"}"#);
    assert_eq!(rc, -6);
}

#[test]
fn execute_invalid_bins() {
    let df = build_factor_df(&[Some(1.0)], &[Some(0.1)]);
    let (rc, _) = run_operator(
        PortData::DataFrame(df),
        r#"{"factor_column":"factor","return_column":"return","bins":"0"}"#,
    );
    assert_eq!(rc, -6);
}

#[test]
fn execute_missing_input() {
    let params_cstr = CString::new(r#"{"factor_column":"f","return_column":"r"}"#).unwrap();
    let mut out: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let rc = execute_operator(
        ptr::null(),
        0,
        out.as_mut_ptr(),
        out.len(),
        params_cstr.as_ptr(),
    );
    assert_eq!(rc, -3);
}

#[test]
fn execute_dataframe_array_empty() {
    let (rc, _) = run_operator(
        PortData::DataFrameArray(vec![]),
        r#"{"factor_column":"factor","return_column":"return"}"#,
    );
    assert_eq!(rc, -5);
}

#[test]
fn execute_missing_column_error() {
    let df = build_factor_df(&[Some(1.0)], &[Some(0.1)]);
    let (rc, _) = run_operator(
        PortData::DataFrame(df),
        r#"{"factor_column":"xxx","return_column":"return"}"#,
    );
    assert_eq!(rc, -8);
}

#[test]
fn execute_return_column_missing_error() {
    let df = build_factor_df(&[Some(1.0)], &[Some(0.1)]);
    let (rc, _) = run_operator(
        PortData::DataFrame(df),
        r#"{"factor_column":"factor","return_column":"xxx"}"#,
    );
    assert_eq!(rc, -8);
}

#[test]
fn execute_dataframe_input_outputs_histogram() {
    let df = build_factor_df(
        &[Some(1.0), Some(2.0), Some(3.0), Some(4.0)],
        &[Some(0.1), Some(0.2), Some(0.3), Some(0.4)],
    );
    let (rc, out) = run_operator(
        PortData::DataFrame(df),
        r#"{"factor_column":"factor","return_column":"return","bins":"2"}"#,
    );
    assert_eq!(rc, 0);
    match out {
        Some(PortData::DataFrame(result)) => {
            assert_eq!(result.row_count, 2);
            let count = result.column("count").unwrap().to_i64_vec();
            assert_eq!(count, vec![Some(2), Some(2)]);
            let mean = result.column("mean_return").unwrap().to_f64_vec();
            assert!((mean[0].unwrap() - 0.15).abs() < 1e-12);
            assert!((mean[1].unwrap() - 0.35).abs() < 1e-12);
        }
        other => panic!("期望 Some(DataFrame)，实际 {:?}", other),
    }
}

#[test]
fn execute_dataframe_array_input_aggregates() {
    let df1 = build_factor_df(&[Some(1.0), Some(2.0)], &[Some(0.1), Some(0.2)]);
    let df2 = build_factor_df(&[Some(3.0), Some(4.0)], &[Some(0.3), Some(0.4)]);
    let (rc, out) = run_operator(
        PortData::DataFrameArray(vec![df1, df2]),
        r#"{"factor_column":"factor","return_column":"return","bins":"2"}"#,
    );
    assert_eq!(rc, 0);
    match out {
        Some(PortData::DataFrame(result)) => {
            let count = result.column("count").unwrap().to_i64_vec();
            assert_eq!(count, vec![Some(2), Some(2)]);
            let mean = result.column("mean_return").unwrap().to_f64_vec();
            assert!((mean[0].unwrap() - 0.15).abs() < 1e-12);
            assert!((mean[1].unwrap() - 0.35).abs() < 1e-12);
        }
        other => panic!("期望 Some(DataFrame)，实际 {:?}", other),
    }
}

#[test]
fn execute_output_compatible_with_viz_defaults() {
    // 输出列必须覆盖「直方图展示算子」默认配置 (bin_center/count/bin_left/bin_right)
    // 以及均值列 mean_return
    let df = build_factor_df(&[Some(1.0), Some(2.0)], &[Some(0.1), Some(0.2)]);
    let (rc, out) = run_operator(
        PortData::DataFrame(df),
        r#"{"factor_column":"factor","return_column":"return","bins":"2"}"#,
    );
    assert_eq!(rc, 0);
    if let Some(PortData::DataFrame(result)) = out {
        for col in [
            "bin_index",
            "bin_left",
            "bin_right",
            "bin_center",
            "count",
            "frequency",
            "mean_return",
        ] {
            assert!(result.column(col).is_some(), "缺少兼容列 {}", col);
        }
    } else {
        panic!("期望 DataFrame 输出");
    }
}

#[test]
fn execute_reserved_result_column_falls_back() {
    // result_column 与保留列名 'count' 冲突 -> 回退 mean_return，不产生重复 count 列
    let df = build_factor_df(&[Some(1.0), Some(2.0)], &[Some(0.1), Some(0.2)]);
    let (rc, out) = run_operator(
        PortData::DataFrame(df),
        r#"{"factor_column":"factor","return_column":"return","bins":"2","result_column":"count"}"#,
    );
    assert_eq!(rc, 0);
    if let Some(PortData::DataFrame(result)) = out {
        assert!(result.column("mean_return").is_some());
        // 仅应有一个 count 列（Int64 类型，而非冲突的 Float64 均值列）
        let count_cols: Vec<_> = result.columns.iter().filter(|c| c.name == "count").collect();
        assert_eq!(count_cols.len(), 1);
        assert_eq!(count_cols[0].data_type, DataType::Int64);
    } else {
        panic!("期望 DataFrame 输出");
    }
}

#[test]
fn execute_null_rows_skipped() {
    // 含空值行应被跳过，不影响有效样本统计
    let df = build_factor_df(
        &[Some(1.0), None, Some(3.0)],
        &[Some(0.1), Some(0.2), None],
    );
    let (rc, out) = run_operator(
        PortData::DataFrame(df),
        r#"{"factor_column":"factor","return_column":"return","bins":"1"}"#,
    );
    assert_eq!(rc, 0);
    if let Some(PortData::DataFrame(result)) = out {
        let count = result.column("count").unwrap().to_i64_vec();
        assert_eq!(count, vec![Some(1)]); // 仅 row0 有效
    } else {
        panic!("期望 DataFrame 输出");
    }
}
