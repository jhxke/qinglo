use super::*;
use operator_runtime::DataFrame;

/// 测试参数解析
#[test]
fn test_parse_params() {
    // 空参数
    let params = parse_params("");
    assert_eq!(params.ma_periods, "");
    assert_eq!(params.rsi_period, "");
    assert_eq!(params.macd_fast, "");

    // 有效参数（MA + RSI 同时配置）
    let json = r#"{"ma_periods":"5,10","rsi_period":"14"}"#;
    let params = parse_params(json);
    assert_eq!(params.ma_periods, "5,10");
    assert_eq!(params.rsi_period, "14");

    // 有效参数（MACD）
    let json = r#"{"macd_fast":"12","macd_slow":"26","macd_signal":"9"}"#;
    let params = parse_params(json);
    assert_eq!(params.macd_fast, "12");
    assert_eq!(params.macd_slow, "26");
    assert_eq!(params.macd_signal, "9");

    // 无效 JSON
    let params = parse_params("not valid json");
    assert_eq!(params.ma_periods, "");

    // 旧版 periods 兼容字段
    let json = r#"{"periods":"5,10,20"}"#;
    let params = parse_params(json);
    assert_eq!(params.periods, "5,10,20");

    // 旧 DAG 残留 indicator_type/column_name 字段应被忽略（不报错）
    let json = r#"{"indicator_type":"ma","column_name":"close","ma_periods":"5"}"#;
    let params = parse_params(json);
    assert_eq!(params.ma_periods, "5");
}

/// 测试单周期解析
#[test]
fn test_parse_single_period() {
    assert_eq!(parse_single_period("14", 14), 14);
    assert_eq!(parse_single_period("20", 14), 20);
    assert_eq!(parse_single_period("", 14), 14);
    assert_eq!(parse_single_period("0", 14), 14); // 非正被过滤
    assert_eq!(parse_single_period("abc", 14), 14); // 非法回退默认
    assert_eq!(parse_single_period("  7  ", 14), 7); // 容忍空格
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

/// 测试仅配置 MA
#[test]
fn test_apply_indicators_ma() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![
            Some(10.0), Some(10.5), Some(11.0), Some(10.8), Some(11.2),
            Some(11.5), Some(12.0), Some(11.8), Some(12.2), Some(12.5),
        ],
    );
    df.add_column(close_col);

    let params = IndicatorParams {
        ma_periods: "3,5".to_string(),
        ..Default::default()
    };
    apply_indicators_inplace(&mut df, &params);
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

    // 仅配置 MA，不应出现 rsi/macd 列
    assert!(output_df.column("rsi_5").is_none());
    assert!(output_df.column("macd").is_none());
}

/// 测试仅配置 RSI
#[test]
fn test_apply_indicators_rsi() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (1..=10).map(|i| Some(i as f64)).collect(),
    );
    df.add_column(close_col);

    let params = IndicatorParams {
        rsi_period: "5".to_string(),
        ..Default::default()
    };
    apply_indicators_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 2); // close, rsi_5
    let rsi = output_df.column("rsi_5").unwrap();
    for i in 0..5 {
        assert!(rsi.is_null(i));
    }
    for i in 5..10 {
        assert!((rsi.get_f64(i).unwrap() - 100.0).abs() < 0.001);
    }
    // 仅配置 RSI，不应出现 ma/macd 列
    assert!(output_df.column("ma_5").is_none());
    assert!(output_df.column("macd").is_none());
}

/// 测试仅配置 MACD
#[test]
fn test_apply_indicators_macd() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (0..20)
            .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
            .collect(),
    );
    df.add_column(close_col);

    let params = IndicatorParams {
        macd_fast: "3".to_string(),
        macd_slow: "5".to_string(),
        macd_signal: "3".to_string(),
        ..Default::default()
    };
    apply_indicators_inplace(&mut df, &params);
    let output_df = &df;

    // 应有 4 列: close, macd, macd_signal, macd_hist
    assert_eq!(output_df.col_count(), 4);
    assert!(output_df.column("macd").is_some());
    assert!(output_df.column("macd_signal").is_some());
    assert!(output_df.column("macd_hist").is_some());
    assert_eq!(output_df.row_count, 20);
    // 仅配置 MACD，不应出现 ma/rsi 列
    assert!(output_df.column("ma_3").is_none());
    assert!(output_df.column("rsi_14").is_none());
}

/// 测试 close 列缺失时所有指标跳过、原样返回
#[test]
fn test_apply_indicators_missing_close() {
    let mut df = DataFrame::new();
    // 只有 id 列，没有 close
    let id_col = DataFrame::new_int64_column("id", vec![Some(1), Some(2), Some(3)]);
    df.add_column(id_col);

    let params = IndicatorParams {
        ma_periods: "5".to_string(),
        rsi_period: "14".to_string(),
        macd_fast: "12".to_string(),
        macd_slow: "26".to_string(),
        macd_signal: "9".to_string(),
        ..Default::default()
    };
    apply_indicators_inplace(&mut df, &params);
    let output_df = &df;

    // close 缺失，所有指标跳过，输出保持原样（只有 id 列）
    assert_eq!(output_df.col_count(), 1);
    assert_eq!(output_df.row_count, 3);
    assert!(output_df.column("ma_5").is_none());
    assert!(output_df.column("rsi_14").is_none());
    assert!(output_df.column("macd").is_none());
}

/// 测试 ma_periods 为空时回退旧版 periods 字段
#[test]
fn test_apply_indicators_legacy_periods_fallback() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)],
    );
    df.add_column(close_col);

    // ma_periods 为空，使用旧版 periods
    let params = IndicatorParams {
        periods: "3".to_string(),
        ..Default::default()
    };
    apply_indicators_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 2); // close, ma_3
    assert!(output_df.column("ma_3").is_some());
}

/// 测试多指标同时配置：追加顺序 MA → RSI → MACD
#[test]
fn test_apply_indicators_multiple_at_once() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (0..30)
            .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
            .collect(),
    );
    df.add_column(close_col);

    let params = IndicatorParams {
        ma_periods: "3,5".to_string(),
        rsi_period: "5".to_string(),
        macd_fast: "3".to_string(),
        macd_slow: "5".to_string(),
        macd_signal: "3".to_string(),
        ..Default::default()
    };
    apply_indicators_inplace(&mut df, &params);
    let output_df = &df;

    // 期望列顺序: close, ma_3, ma_5, rsi_5, macd, macd_signal, macd_hist
    assert_eq!(output_df.col_count(), 7);
    let names: Vec<&str> = output_df
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["close", "ma_3", "ma_5", "rsi_5", "macd", "macd_signal", "macd_hist"]
    );
}

/// 测试所有参数为空时原样返回输入
#[test]
fn test_apply_indicators_all_empty_returns_input() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)],
    );
    df.add_column(close_col);

    // 全空参数：原样返回，不追加列
    let params = IndicatorParams::default();
    apply_indicators_inplace(&mut df, &params);
    let output_df = &df;

    assert_eq!(output_df.col_count(), 1); // 仅 close
    assert_eq!(output_df.row_count, 5);
    assert!(output_df.column("ma_5").is_none());
    assert!(output_df.column("rsi_14").is_none());
    assert!(output_df.column("macd").is_none());
}

/// 测试 MACD 仅配置部分参数时缺失项回退默认值
#[test]
fn test_apply_indicators_macd_partial_params() {
    let mut df = DataFrame::new();
    let close_col = DataFrame::new_float64_column(
        "close",
        (0..30)
            .map(|i| Some(10.0 + i as f64 + if i % 4 == 0 { -2.0 } else { 1.0 }))
            .collect(),
    );
    df.add_column(close_col);

    // 仅配置 macd_fast，slow/signal 留空，应回退默认 26/9
    let params = IndicatorParams {
        macd_fast: "12".to_string(),
        ..Default::default()
    };
    apply_indicators_inplace(&mut df, &params);
    let output_df = &df;

    // MACD 触发，slow=26/signal=9 默认
    assert_eq!(output_df.col_count(), 4); // close, macd, macd_signal, macd_hist
    assert!(output_df.column("macd").is_some());
    assert!(output_df.column("macd_signal").is_some());
    assert!(output_df.column("macd_hist").is_some());
}

/// 测试版本函数
#[test]
fn test_version_function() {
    let version_ptr = indicator_operator_version();
    let version_str = unsafe { std::ffi::CStr::from_ptr(version_ptr) }
        .to_str()
        .unwrap();
    assert_eq!(version_str, "0.3.0");
}
