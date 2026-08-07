use super::*;
use operator_runtime::DataFrame;

/// 测试参数解析
#[test]
fn test_parse_params() {
    // 空参数
    let params = parse_params("");
    assert_eq!(params.rsi_periods, "");
    assert_eq!(params.source_column, "");

    // 有效参数（多周期）
    let json = r#"{"rsi_periods":"5,10,14"}"#;
    let params = parse_params(json);
    assert_eq!(params.rsi_periods, "5,10,14");
    assert_eq!(params.source_column, "");

    // 含 source_column
    let json = r#"{"rsi_periods":"14","source_column":"price"}"#;
    let params = parse_params(json);
    assert_eq!(params.rsi_periods, "14");
    assert_eq!(params.source_column, "price");

    // 无效 JSON
    let params = parse_params("not valid json");
    assert_eq!(params.rsi_periods, "");

    // 旧 DAG 残留字段应被忽略（不报错）
    let json = r#"{"indicator_type":"rsi","ma_periods":"5","rsi_periods":"14"}"#;
    let params = parse_params(json);
    assert_eq!(params.rsi_periods, "14");
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

/// 测试多周期解析
#[test]
fn test_parse_periods() {
    // 基本多周期
    assert_eq!(parse_periods("5,10,14"), vec![5, 10, 14]);
    // 带空格
    assert_eq!(parse_periods(" 5 , 10 , 14 "), vec![5, 10, 14]);
    // 单周期
    assert_eq!(parse_periods("14"), vec![14]);
    // 空串返回空
    assert_eq!(parse_periods(""), Vec::<usize>::new());
    // 非正被过滤
    assert_eq!(parse_periods("0,5,-3,10"), vec![5, 10]);
    // 非法被过滤
    assert_eq!(parse_periods("abc,5,xyz,10"), vec![5, 10]);
    // 全非法返回空
    assert_eq!(parse_periods("abc,0,-1"), Vec::<usize>::new());
    // 空元素被跳过
    assert_eq!(parse_periods(",5,,10,"), vec![5, 10]);
}

/// 测试 RSI：纯上涨序列应全为 100
#[test]
fn test_compute_rsi_uptrend() {
    let values: Vec<Option<f64>> = (1..=10).map(|i| Some(i as f64)).collect();
    let rsi = compute_rsi(&values, 5);
    assert_eq!(rsi.len(), 10);
    // 前 5 个为 None
    for i in 0..5 {
        assert!(rsi[i].is_none(), "位置 {} 应为 None", i);
    }
    // 之后全是 100（无下跌）
    for i in 5..10 {
        assert!((rsi[i].unwrap() - 100.0).abs() < 0.001, "位置 {} 应为 100", i);
    }
}

/// 测试 RSI：纯下跌序列应全为 0
#[test]
fn test_compute_rsi_downtrend() {
    let values: Vec<Option<f64>> = (1..=10).rev().map(|i| Some(i as f64)).collect();
    let rsi = compute_rsi(&values, 5);
    assert_eq!(rsi.len(), 10);
    for i in 0..5 {
        assert!(rsi[i].is_none());
    }
    for i in 5..10 {
        assert!((rsi[i].unwrap() - 0.0).abs() < 0.001, "位置 {} 应为 0", i);
    }
}

/// 测试 RSI 数据不足
#[test]
fn test_compute_rsi_insufficient_data() {
    let values: Vec<Option<f64>> = vec![Some(1.0), Some(2.0), Some(3.0)];
    let rsi = compute_rsi(&values, 5);
    assert_eq!(rsi.len(), 3);
    for v in &rsi {
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

/// 测试配置单周期 RSI
#[test]
fn test_apply_rsi_single_period() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (1..=10).map(|i| Some(i as f64)).collect(),
    );
    df.add_column(close_col);

    let params = RsiParams {
        rsi_periods: "5".to_string(),
        ..Default::default()
    };
    apply_rsi_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 2); // close, rsi_5
    let rsi = output_df.column("rsi_5").unwrap();
    for i in 0..5 {
        assert!(rsi.is_null(i));
    }
    for i in 5..10 {
        assert!((rsi.get_f64(i).unwrap() - 100.0).abs() < 0.001);
    }
    // 不应出现 ma/macd 列
    assert!(output_df.column("ma_5").is_none());
    assert!(output_df.column("macd").is_none());
}

/// 测试配置多周期 RSI（5,10）
#[test]
fn test_apply_rsi_multi_periods() {
    let mut df = DataFrame::new();
    // 需要 > 10 行以产生 rsi_10 的首根值
    let close_col = DataFrame::new_float64_column(
        "close",
        (1..=20).map(|i| Some(i as f64)).collect(),
    );
    df.add_column(close_col);

    let params = RsiParams {
        rsi_periods: "5,10".to_string(),
        ..Default::default()
    };
    apply_rsi_inplace(&mut df, &params);
    let output_df = &df;

    // close + rsi_5 + rsi_10 = 3 列
    assert_eq!(output_df.col_count(), 3);

    // 验证 rsi_5
    let rsi5 = output_df.column("rsi_5").unwrap();
    for i in 0..5 {
        assert!(rsi5.is_null(i), "rsi_5 位置 {} 应为 None", i);
    }
    for i in 5..20 {
        assert!(
            (rsi5.get_f64(i).unwrap() - 100.0).abs() < 0.001,
            "rsi_5 位置 {} 应为 100",
            i
        );
    }

    // 验证 rsi_10
    let rsi10 = output_df.column("rsi_10").unwrap();
    for i in 0..10 {
        assert!(rsi10.is_null(i), "rsi_10 位置 {} 应为 None", i);
    }
    for i in 10..20 {
        assert!(
            (rsi10.get_f64(i).unwrap() - 100.0).abs() < 0.001,
            "rsi_10 位置 {} 应为 100",
            i
        );
    }
}

/// 测试 close 列缺失时跳过、原样返回
#[test]
fn test_apply_rsi_missing_close() {
    let mut df = DataFrame::new();
    // 只有 id 列，没有 close
    let id_col = DataFrame::new_int64_column("id", vec![Some(1), Some(2), Some(3)]);
    df.add_column(id_col);

    let params = RsiParams {
        rsi_periods: "14".to_string(),
        ..Default::default()
    };
    apply_rsi_inplace(&mut df, &params);
    let output_df = &df;

    // close 缺失，跳过，输出保持原样（只有 id 列）
    assert_eq!(output_df.col_count(), 1);
    assert_eq!(output_df.row_count, 3);
    assert!(output_df.column("rsi_14").is_none());
}

/// 测试所有参数为空时原样返回输入
#[test]
fn test_apply_rsi_all_empty_returns_input() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)],
    );
    df.add_column(close_col);

    // 全空参数：原样返回，不追加列
    let params = RsiParams::default();
    apply_rsi_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 1); // 仅 close
    assert_eq!(output_df.row_count, 5);
    assert!(output_df.column("rsi_14").is_none());
}

/// 测试非法周期串（全部被过滤）时跳过、不追加列
#[test]
fn test_apply_rsi_all_invalid_periods_skipped() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (1..=20).map(|i| Some(i as f64)).collect(),
    );
    df.add_column(close_col);

    // 全部为非法/非正，parse_periods 返回空，跳过
    let params = RsiParams {
        rsi_periods: "abc,0,-1".to_string(),
        ..Default::default()
    };
    apply_rsi_inplace(&mut df, &params);
    let output_df = &df;

    // 只有 close 列，不追加任何 rsi 列
    assert_eq!(output_df.col_count(), 1);
    assert!(output_df.column("rsi_14").is_none());
}

/// 测试部分非法周期：非法的被过滤，合法的正常计算
#[test]
fn test_apply_rsi_partial_invalid_periods() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (1..=20).map(|i| Some(i as f64)).collect(),
    );
    df.add_column(close_col);

    // "abc" 和 "0" 被过滤，只保留 5
    let params = RsiParams {
        rsi_periods: "abc,5,0".to_string(),
        ..Default::default()
    };
    apply_rsi_inplace(&mut df, &params);
    let output_df = &df;

    // close + rsi_5 = 2 列
    assert_eq!(output_df.col_count(), 2);
    assert!(output_df.column("rsi_5").is_some());
}

/// 测试自定义源列名（非标准 close 列）
#[test]
fn test_apply_rsi_custom_source_column() {
    let mut df = DataFrame::new();
    // 用 "price" 而非 "close" 作为源列
    let price_col = DataFrame::new_float64_column(
        "price",
        (1..=10).map(|i| Some(i as f64)).collect(),
    );
    df.add_column(price_col);

    let params = RsiParams {
        rsi_periods: "5".to_string(),
        source_column: "price".to_string(),
    };
    apply_rsi_inplace(&mut df, &params);
    let output_df = &df;

    // 应基于 price 列计算并追加 rsi_5
    assert_eq!(output_df.col_count(), 2); // price, rsi_5
    let rsi = output_df.column("rsi_5").unwrap();
    // 纯上涨序列，RSI 应全为 100
    for i in 5..10 {
        assert!((rsi.get_f64(i).unwrap() - 100.0).abs() < 0.001);
    }
}

/// 测试 source_column 为空时回退默认 "close"
#[test]
fn test_apply_rsi_source_column_defaults_to_close() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (1..=10).map(|i| Some(i as f64)).collect(),
    );
    df.add_column(close_col);

    // source_column 留空，应回退 "close"
    let params = RsiParams {
        rsi_periods: "5".to_string(),
        source_column: "".to_string(),
    };
    apply_rsi_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 2); // close, rsi_5
    assert!(output_df.column("rsi_5").is_some());
}

/// 测试自定义源列不存在时跳过、原样返回
#[test]
fn test_apply_rsi_custom_source_column_missing() {
    let mut df = DataFrame::new();
    // 只有 close 列，但配置 source_column=price
    let close_col = DataFrame::new_float64_column(
        "close",
        (1..=20).map(|i| Some(i as f64)).collect(),
    );
    df.add_column(close_col);

    let params = RsiParams {
        rsi_periods: "14".to_string(),
        source_column: "price".to_string(),
    };
    apply_rsi_inplace(&mut df, &params);
    let output_df = &df;

    // price 列缺失，跳过，输出保持原样（只有 close 列）
    assert_eq!(output_df.col_count(), 1);
    assert!(output_df.column("rsi_14").is_none());
}

/// 测试版本函数
#[test]
fn test_version_function() {
    let version_ptr = rsi_operator_version();
    let version_str = unsafe { std::ffi::CStr::from_ptr(version_ptr) }
        .to_str()
        .unwrap();
    assert_eq!(version_str, "0.2.0");
}
