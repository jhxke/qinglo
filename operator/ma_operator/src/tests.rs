use super::*;
use operator_runtime::DataFrame;

/// 测试参数解析
#[test]
fn test_parse_params() {
    // 空参数
    let params = parse_params("");
    assert_eq!(params.ma_periods, "");
    assert_eq!(params.source_column, "");

    // 有效参数
    let json = r#"{"ma_periods":"5,10,20"}"#;
    let params = parse_params(json);
    assert_eq!(params.ma_periods, "5,10,20");
    assert_eq!(params.source_column, "");

    // 含 source_column
    let json = r#"{"ma_periods":"5","source_column":"price"}"#;
    let params = parse_params(json);
    assert_eq!(params.ma_periods, "5");
    assert_eq!(params.source_column, "price");

    // 无效 JSON
    let params = parse_params("not valid json");
    assert_eq!(params.ma_periods, "");

    // 旧 DAG 残留字段应被忽略（不报错）
    let json = r#"{"indicator_type":"ma","rsi_period":"14","ma_periods":"5"}"#;
    let params = parse_params(json);
    assert_eq!(params.ma_periods, "5");
}

/// 测试源列名解析：空串回退默认 "close"
#[test]
fn test_resolve_source_column() {
    assert_eq!(resolve_source_column(""), "close");
    assert_eq!(resolve_source_column("   "), "close");
    assert_eq!(resolve_source_column("close"), "close");
    assert_eq!(resolve_source_column("price"), "price");
    assert_eq!(resolve_source_column("  price  "), "price"); // 容忍空格
}

/// 测试周期解析
#[test]
fn test_parse_periods() {
    assert_eq!(parse_periods("5,10"), vec![5, 10]);
    assert_eq!(parse_periods("5, 10, 20"), vec![5, 10, 20]);
    assert_eq!(parse_periods("5"), vec![5]);
    assert_eq!(parse_periods(""), Vec::<usize>::new());
    assert_eq!(parse_periods("0,5"), vec![5]); // 0 被过滤
    assert_eq!(parse_periods("-1,5"), vec![5]); // -1 解析失败被过滤
    assert_eq!(parse_periods("abc,5,def"), vec![5]);
}

/// 测试简单移动平均线计算
#[test]
fn test_compute_sma() {
    let values = vec![
        Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0),
        Some(6.0), Some(7.0), Some(8.0), Some(9.0), Some(10.0),
    ];

    let sma5 = compute_sma(&values, 5);
    assert_eq!(sma5.len(), 10);
    for i in 0..4 {
        assert!(sma5[i].is_none(), "位置 {} 应为 None", i);
    }
    assert!((sma5[4].unwrap() - 3.0).abs() < 0.001);
    assert!((sma5[5].unwrap() - 4.0).abs() < 0.001);
    assert!((sma5[9].unwrap() - 8.0).abs() < 0.001);

    let sma10 = compute_sma(&values, 10);
    for i in 0..9 {
        assert!(sma10[i].is_none(), "位置 {} 应为 None", i);
    }
    assert!((sma10[9].unwrap() - 5.5).abs() < 0.001);
}

/// 测试带 null 值的均线计算
#[test]
fn test_compute_sma_with_nulls() {
    let values = vec![
        Some(1.0), None, Some(3.0), Some(4.0), Some(5.0),
        Some(6.0), Some(7.0), None, Some(9.0), Some(10.0),
    ];

    let sma5 = compute_sma(&values, 5);
    // 位置 4: 值为 [1, null, 3, 4, 5] -> 有效 4 个 -> (1+3+4+5)/4 = 3.25
    assert!((sma5[4].unwrap() - 3.25).abs() < 0.001);
    // 位置 8: 窗口 values[4..9] = [5.0, 6.0, 7.0, null, 9.0] -> (5+6+7+9)/4 = 6.75
    assert!((sma5[8].unwrap() - 6.75).abs() < 0.001);
}

/// 测试空输入 DataFrame
#[test]
fn test_compute_sma_empty() {
    let values: Vec<Option<f64>> = vec![];
    let sma = compute_sma(&values, 5);
    assert!(sma.is_empty());
}

/// 测试周期大于数据长度
#[test]
fn test_compute_sma_period_larger_than_data() {
    let values = vec![Some(1.0), Some(2.0), Some(3.0)];
    let sma = compute_sma(&values, 5);
    assert_eq!(sma.len(), 3);
    for v in &sma {
        assert!(v.is_none());
    }
}

/// 测试从 DataFrame 提取列值
#[test]
fn test_extract_column_values() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![Some(1.0), Some(2.0), Some(3.0)],
    );
    df.add_column(close_col);

    // 存在的列
    let values = extract_column_values(&df, "close");
    assert!(values.is_some());
    assert_eq!(values.unwrap().len(), 3);

    // 不存在的列
    let values = extract_column_values(&df, "open");
    assert!(values.is_none());
}

/// 测试非 Float64 列应返回 None
#[test]
fn test_extract_column_values_wrong_type() {
    let mut df = DataFrame::new();
    let id_col = DataFrame::new_int64_column("id", vec![Some(1), Some(2)]);
    df.add_column(id_col);

    // Int64 列不是 Float64，应返回 None
    assert!(extract_column_values(&df, "id").is_none());
}

/// 测试配置多周期 MA
#[test]
fn test_apply_ma_multiple_periods() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![
            Some(10.0), Some(10.5), Some(11.0), Some(10.8), Some(11.2),
            Some(11.5), Some(12.0), Some(11.8), Some(12.2), Some(12.5),
        ],
    );
    df.add_column(close_col);

    let params = MaParams {
        ma_periods: "3,5".to_string(),
        ..Default::default()
    };
    apply_ma_inplace(&mut df, &params);
    let output_df = &df;

    // 应有 3 列: close, ma_3, ma_5
    assert_eq!(output_df.col_count(), 3);
    assert_eq!(output_df.row_count, 10);

    let ma3 = output_df.column("ma_3").unwrap();
    assert!(ma3.is_null(0));
    assert!(ma3.is_null(1));
    // (10.0 + 10.5 + 11.0) / 3 = 10.5
    assert!((ma3.get_f64(2).unwrap() - 10.5).abs() < 0.001);

    let ma5 = output_df.column("ma_5").unwrap();
    assert!(ma5.is_null(3));
    // (10.0 + 10.5 + 11.0 + 10.8 + 11.2) / 5 = 10.7
    assert!((ma5.get_f64(4).unwrap() - 10.7).abs() < 0.001);

    // 不应出现 rsi/macd 列
    assert!(output_df.column("rsi_5").is_none());
    assert!(output_df.column("macd").is_none());
}

/// 测试 close 列缺失时跳过、原样返回
#[test]
fn test_apply_ma_missing_close() {
    let mut df = DataFrame::new();
    // 只有 id 列，没有 close
    let id_col = DataFrame::new_int64_column("id", vec![Some(1), Some(2), Some(3)]);
    df.add_column(id_col);

    let params = MaParams {
        ma_periods: "5".to_string(),
        ..Default::default()
    };
    apply_ma_inplace(&mut df, &params);
    let output_df = &df;

    // close 缺失，跳过，输出保持原样（只有 id 列）
    assert_eq!(output_df.col_count(), 1);
    assert_eq!(output_df.row_count, 3);
    assert!(output_df.column("ma_5").is_none());
}

/// 测试所有参数为空时原样返回输入
#[test]
fn test_apply_ma_all_empty_returns_input() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)],
    );
    df.add_column(close_col);

    // 全空参数：原样返回，不追加列
    let params = MaParams::default();
    apply_ma_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 1); // 仅 close
    assert_eq!(output_df.row_count, 5);
    assert!(output_df.column("ma_5").is_none());
}

/// 测试无效周期串被过滤
#[test]
fn test_apply_ma_invalid_periods_filtered() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)],
    );
    df.add_column(close_col);

    // 全部为非法周期，过滤后为空，不应追加列
    let params = MaParams {
        ma_periods: "0,abc,-1".to_string(),
        ..Default::default()
    };
    apply_ma_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 1); // 仅 close
    assert!(output_df.column("ma_0").is_none());
}

/// 测试自定义源列名（非标准 close 列）
#[test]
fn test_apply_ma_custom_source_column() {
    let mut df = DataFrame::new();
    // 用 "price" 而非 "close" 作为源列
    let price_col = DataFrame::new_float64_column(
        "price",
        vec![
            Some(10.0), Some(10.5), Some(11.0), Some(10.8), Some(11.2),
            Some(11.5), Some(12.0), Some(11.8), Some(12.2), Some(12.5),
        ],
    );
    df.add_column(price_col);

    let params = MaParams {
        ma_periods: "3".to_string(),
        source_column: "price".to_string(),
    };
    apply_ma_inplace(&mut df, &params);
    let output_df = &df;

    // 应基于 price 列计算并追加 ma_3
    assert_eq!(output_df.col_count(), 2); // price, ma_3
    let ma3 = output_df.column("ma_3").unwrap();
    // (10.0 + 10.5 + 11.0) / 3 = 10.5
    assert!((ma3.get_f64(2).unwrap() - 10.5).abs() < 0.001);
}

/// 测试 source_column 为空时回退默认 "close"
#[test]
fn test_apply_ma_source_column_defaults_to_close() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)],
    );
    df.add_column(close_col);

    // source_column 留空，应回退 "close"
    let params = MaParams {
        ma_periods: "3".to_string(),
        source_column: "".to_string(),
    };
    apply_ma_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 2); // close, ma_3
    assert!(output_df.column("ma_3").is_some());
    // (1+2+3)/3 = 2.0
    assert!((output_df.column("ma_3").unwrap().get_f64(2).unwrap() - 2.0).abs() < 0.001);
}

/// 测试自定义源列不存在时跳过、原样返回
#[test]
fn test_apply_ma_custom_source_column_missing() {
    let mut df = DataFrame::new();
    // 只有 close 列，但配置 source_column=price
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)],
    );
    df.add_column(close_col);

    let params = MaParams {
        ma_periods: "3".to_string(),
        source_column: "price".to_string(),
    };
    apply_ma_inplace(&mut df, &params);
    let output_df = &df;

    // price 列缺失，跳过，输出保持原样（只有 close 列）
    assert_eq!(output_df.col_count(), 1);
    assert!(output_df.column("ma_3").is_none());
}

/// 测试版本函数
#[test]
fn test_version_function() {
    let version_ptr = ma_operator_version();
    let version_str = unsafe { std::ffi::CStr::from_ptr(version_ptr) }
        .to_str()
        .unwrap();
    assert_eq!(version_str, "0.1.0");
}
