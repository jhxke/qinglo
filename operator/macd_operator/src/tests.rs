use super::*;
use operator_runtime::DataFrame;

/// 测试参数解析
#[test]
fn test_parse_params() {
    // 空参数
    let params = parse_params("");
    assert_eq!(params.macd_fast, "");
    assert_eq!(params.macd_slow, "");
    assert_eq!(params.macd_signal, "");
    assert_eq!(params.source_column, "");

    // 有效参数
    let json = r#"{"macd_fast":"12","macd_slow":"26","macd_signal":"9"}"#;
    let params = parse_params(json);
    assert_eq!(params.macd_fast, "12");
    assert_eq!(params.macd_slow, "26");
    assert_eq!(params.macd_signal, "9");
    assert_eq!(params.source_column, "");

    // 含 source_column
    let json = r#"{"macd_fast":"12","source_column":"price"}"#;
    let params = parse_params(json);
    assert_eq!(params.macd_fast, "12");
    assert_eq!(params.source_column, "price");

    // 无效 JSON
    let params = parse_params("not valid json");
    assert_eq!(params.macd_fast, "");

    // 旧 DAG 残留字段应被忽略（不报错）
    let json = r#"{"indicator_type":"macd","ma_periods":"5","macd_fast":"12"}"#;
    let params = parse_params(json);
    assert_eq!(params.macd_fast, "12");
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

/// 测试单周期解析
#[test]
fn test_parse_single_period() {
    assert_eq!(parse_single_period("12", 12), 12);
    assert_eq!(parse_single_period("26", 12), 26);
    assert_eq!(parse_single_period("", 12), 12);
    assert_eq!(parse_single_period("0", 12), 12); // 非正被过滤
    assert_eq!(parse_single_period("abc", 12), 12); // 非法回退默认
    assert_eq!(parse_single_period("  7  ", 12), 7); // 容忍空格
}

/// 测试 EMA 计算（SMA 种子 + 递推）
#[test]
fn test_ema_series() {
    let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
    let ema3 = ema_series(&values, 3);
    assert_eq!(ema3.len(), 5);
    // 前 2 个为 None
    assert!(ema3[0].is_none());
    assert!(ema3[1].is_none());
    // 种子 = (1+2+3)/3 = 2.0
    assert!((ema3[2].unwrap() - 2.0).abs() < 0.001);
    // alpha = 2/(3+1) = 0.5
    // i=3: 0.5*4 + 0.5*2 = 3.0
    assert!((ema3[3].unwrap() - 3.0).abs() < 0.001);
    // i=4: 0.5*5 + 0.5*3 = 4.0
    assert!((ema3[4].unwrap() - 4.0).abs() < 0.001);
}

/// 测试 EMA 周期大于数据长度
#[test]
fn test_ema_series_period_too_large() {
    let values = vec![1.0, 2.0, 3.0];
    let ema = ema_series(&values, 5);
    assert_eq!(ema.len(), 3);
    for v in &ema {
        assert!(v.is_none());
    }
}

/// 测试 MACD 结构与长度
#[test]
fn test_compute_macd_structure() {
    // 20 个递增带波动的值
    let values: Vec<Option<f64>> = (0..20)
        .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
        .collect();

    let (macd_line, signal_line, hist) = compute_macd(&values, 3, 5, 3);
    assert_eq!(macd_line.len(), 20);
    assert_eq!(signal_line.len(), 20);
    assert_eq!(hist.len(), 20);

    // macd_line 在两 EMA 均有效后才有值：max(fast-1, slow-1) = max(2, 4) = 4
    for i in 0..4 {
        assert!(macd_line[i].is_none(), "macd_line[{}] 应为 None", i);
    }
    for i in 4..20 {
        assert!(macd_line[i].is_some(), "macd_line[{}] 应有值", i);
    }

    // 信号线 = macd 有效段(从 idx=4 起，共 16 个)的 EMA(3)，Some 从段内 index 2 起
    // -> 原索引 4 + 2 = 6 起有值
    for i in 0..6 {
        assert!(signal_line[i].is_none(), "signal_line[{}] 应为 None", i);
    }
    for i in 6..20 {
        assert!(signal_line[i].is_some(), "signal_line[{}] 应有值", i);
    }

    // hist = macd - signal，仅在两者均有值时存在
    for i in 0..6 {
        assert!(hist[i].is_none());
    }
    for i in 6..20 {
        let h = hist[i].unwrap();
        let m = macd_line[i].unwrap();
        let s = signal_line[i].unwrap();
        assert!((h - (m - s)).abs() < 1e-9, "hist[{}] 应等于 macd - signal", i);
    }
}

/// 测试 MACD 数据不足（小于 slow 周期）
#[test]
fn test_compute_macd_insufficient_data() {
    let values: Vec<Option<f64>> = vec![Some(1.0), Some(2.0), Some(3.0)];
    let (macd_line, signal_line, hist) = compute_macd(&values, 12, 26, 9);
    assert_eq!(macd_line.len(), 3);
    assert_eq!(signal_line.len(), 3);
    assert_eq!(hist.len(), 3);
    for v in macd_line.iter().chain(signal_line.iter()).chain(hist.iter()) {
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

/// 测试配置 MACD
#[test]
fn test_apply_macd() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (0..20)
            .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
            .collect(),
    );
    df.add_column(close_col);

    let params = MacdParams {
        macd_fast: "3".to_string(),
        macd_slow: "5".to_string(),
        macd_signal: "3".to_string(),
        ..Default::default()
    };
    apply_macd_inplace(&mut df, &params);
    let output_df = &df;

    // 应有 4 列: close, macd, macd_signal, macd_hist
    assert_eq!(output_df.col_count(), 4);
    assert!(output_df.column("macd").is_some());
    assert!(output_df.column("macd_signal").is_some());
    assert!(output_df.column("macd_hist").is_some());
    assert_eq!(output_df.row_count, 20);
    // 不应出现 ma/rsi 列
    assert!(output_df.column("ma_3").is_none());
    assert!(output_df.column("rsi_14").is_none());
}

/// 测试 close 列缺失时跳过、原样返回
#[test]
fn test_apply_macd_missing_close() {
    let mut df = DataFrame::new();
    // 只有 id 列，没有 close
    let id_col = DataFrame::new_int64_column("id", vec![Some(1), Some(2), Some(3)]);
    df.add_column(id_col);

    let params = MacdParams {
        macd_fast: "12".to_string(),
        macd_slow: "26".to_string(),
        macd_signal: "9".to_string(),
        ..Default::default()
    };
    apply_macd_inplace(&mut df, &params);
    let output_df = &df;

    // close 缺失，跳过，输出保持原样（只有 id 列）
    assert_eq!(output_df.col_count(), 1);
    assert_eq!(output_df.row_count, 3);
    assert!(output_df.column("macd").is_none());
}

/// 测试所有参数为空时原样返回输入
#[test]
fn test_apply_macd_all_empty_returns_input() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)],
    );
    df.add_column(close_col);

    // 全空参数：原样返回，不追加列
    let params = MacdParams::default();
    apply_macd_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 1); // 仅 close
    assert_eq!(output_df.row_count, 5);
    assert!(output_df.column("macd").is_none());
}

/// 测试仅配置部分参数时缺失项回退默认值
#[test]
fn test_apply_macd_partial_params() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (0..30)
            .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
            .collect(),
    );
    df.add_column(close_col);

    // 仅配置 macd_fast，slow/signal 留空，应回退默认 26/9
    let params = MacdParams {
        macd_fast: "12".to_string(),
        ..Default::default()
    };
    apply_macd_inplace(&mut df, &params);
    let output_df = &df;

    // MACD 触发，slow=26/signal=9 默认
    assert_eq!(output_df.col_count(), 4); // close, macd, macd_signal, macd_hist
    assert!(output_df.column("macd").is_some());
    assert!(output_df.column("macd_signal").is_some());
    assert!(output_df.column("macd_hist").is_some());
}

/// 测试 slow <= fast 时跳过 MACD
#[test]
fn test_apply_macd_slow_le_fast_skipped() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (0..30)
            .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
            .collect(),
    );
    df.add_column(close_col);

    // slow(5) <= fast(12)，无意义，应跳过
    let params = MacdParams {
        macd_fast: "12".to_string(),
        macd_slow: "5".to_string(),
        macd_signal: "9".to_string(),
        ..Default::default()
    };
    apply_macd_inplace(&mut df, &params);
    let output_df = &df;

    // 跳过 MACD，不追加列
    assert_eq!(output_df.col_count(), 1); // 仅 close
    assert!(output_df.column("macd").is_none());
}

/// 测试自定义源列名（非标准 close 列）
#[test]
fn test_apply_macd_custom_source_column() {
    let mut df = DataFrame::new();
    // 用 "price" 而非 "close" 作为源列
    let price_col = DataFrame::new_float64_column(
        "price",
        (0..20)
            .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
            .collect(),
    );
    df.add_column(price_col);

    let params = MacdParams {
        macd_fast: "3".to_string(),
        macd_slow: "5".to_string(),
        macd_signal: "3".to_string(),
        source_column: "price".to_string(),
    };
    apply_macd_inplace(&mut df, &params);
    let output_df = &df;

    // 应基于 price 列计算并追加 macd 三列
    assert_eq!(output_df.col_count(), 4); // price, macd, macd_signal, macd_hist
    assert!(output_df.column("macd").is_some());
    assert!(output_df.column("macd_signal").is_some());
    assert!(output_df.column("macd_hist").is_some());
}

/// 测试 source_column 为空时回退默认 "close"
#[test]
fn test_apply_macd_source_column_defaults_to_close() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (0..20)
            .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
            .collect(),
    );
    df.add_column(close_col);

    // source_column 留空，应回退 "close"
    let params = MacdParams {
        macd_fast: "3".to_string(),
        macd_slow: "5".to_string(),
        macd_signal: "3".to_string(),
        source_column: "".to_string(),
    };
    apply_macd_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 4); // close, macd, macd_signal, macd_hist
    assert!(output_df.column("macd").is_some());
}

/// 测试自定义源列不存在时跳过、原样返回
#[test]
fn test_apply_macd_custom_source_column_missing() {
    let mut df = DataFrame::new();
    // 只有 close 列，但配置 source_column=price
    let close_col = DataFrame::new_float64_column(
        "close",
        (0..20)
            .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
            .collect(),
    );
    df.add_column(close_col);

    let params = MacdParams {
        macd_fast: "3".to_string(),
        macd_slow: "5".to_string(),
        macd_signal: "3".to_string(),
        source_column: "price".to_string(),
    };
    apply_macd_inplace(&mut df, &params);
    let output_df = &df;

    // price 列缺失，跳过，输出保持原样（只有 close 列）
    assert_eq!(output_df.col_count(), 1);
    assert!(output_df.column("macd").is_none());
}

/// 测试版本函数
#[test]
fn test_version_function() {
    let version_ptr = macd_operator_version();
    let version_str = unsafe { std::ffi::CStr::from_ptr(version_ptr) }
        .to_str()
        .unwrap();
    assert_eq!(version_str, "0.1.0");
}
