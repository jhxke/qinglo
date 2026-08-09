use super::*;
use operator_runtime::DataFrame;

// ============ 参数解析 ============

#[test]
fn test_parse_params_empty() {
    let p = parse_params("");
    assert_eq!(p.n, "");
    assert_eq!(p.price_column, "");
    assert_eq!(p.volume_column, "");
    assert_eq!(p.result_column, "");
}

#[test]
fn test_parse_params_valid() {
    let json =
        r#"{"n":"20","price_column":"close","volume_column":"vol","result_column":"pv_factor"}"#;
    let p = parse_params(json);
    assert_eq!(p.n, "20");
    assert_eq!(p.price_column, "close");
    assert_eq!(p.volume_column, "vol");
    assert_eq!(p.result_column, "pv_factor");
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
    assert_eq!(resolve_column("", "close"), "close");
    assert_eq!(resolve_column("   ", "volume"), "volume");
    assert_eq!(resolve_column("  price  ", "close"), "price");
}

// ============ 滚动均值 ============

#[test]
fn test_rolling_mean_basic() {
    // [10, 11, 12, 13, 14], n=3
    // 前 2 行 None；窗口:
    // i=2: avg([10,11,12])=11
    // i=3: avg([11,12,13])=12
    // i=4: avg([12,13,14])=13
    let v = vec![
        Some(10.0),
        Some(11.0),
        Some(12.0),
        Some(13.0),
        Some(14.0),
    ];
    let out = compute_rolling_mean(&v, 3);
    assert_eq!(out.len(), 5);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!((out[2].unwrap() - 11.0).abs() < 1e-12);
    assert!((out[3].unwrap() - 12.0).abs() < 1e-12);
    assert!((out[4].unwrap() - 13.0).abs() < 1e-12);
}

#[test]
fn test_rolling_mean_null_propagation() {
    let v = vec![
        Some(10.0),
        None,
        Some(12.0),
        Some(13.0),
        Some(14.0),
    ];
    let out = compute_rolling_mean(&v, 3);
    assert_eq!(out.len(), 5);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    // i=2: 窗口[10,None,12] 含空 → None
    assert!(out[2].is_none());
    // i=3: 窗口[None,12,13] 含空 → None
    assert!(out[3].is_none());
    // i=4: 窗口[12,13,14]=13
    assert!((out[4].unwrap() - 13.0).abs() < 1e-12);
}

#[test]
fn test_rolling_mean_n_zero_defensive() {
    let v = vec![Some(1.0), Some(2.0), Some(3.0)];
    let out = compute_rolling_mean(&v, 0);
    assert_eq!(out.len(), 3);
    assert!(out.iter().all(|x| x.is_none()));
}

#[test]
fn test_rolling_mean_n_one_each_is_self() {
    // n=1：每行等于自身
    let v = vec![Some(5.0), Some(10.0), None];
    let out = compute_rolling_mean(&v, 1);
    assert!((out[0].unwrap() - 5.0).abs() < 1e-12);
    assert!((out[1].unwrap() - 10.0).abs() < 1e-12);
    assert!(out[2].is_none());
}

// ============ 因子计算 ============

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
fn test_pv_factor_basic() {
    // 静态横盘：close=[10,10,10,10], volume=[100,100,100,100], n=3
    // 窗口满时 Pavg=10, Vavg=100
    // (Pt-Pavg)/Pavg=0, Vt/Vavg=1 → 因子=0
    let price = vec![
        Some(10.0),
        Some(10.0),
        Some(10.0),
        Some(10.0),
    ];
    let volume = vec![
        Some(100.0),
        Some(100.0),
        Some(100.0),
        Some(100.0),
    ];
    let out = compute_pv_factor(&price, &volume, 3);
    assert_approx_vec(&out, &[None, None, Some(0.0), Some(0.0)]);
}

#[test]
fn test_pv_factor_surge_amplified() {
    // 涨价 + 放量：close=[10,10,10,11], volume=[100,100,100,200], n=3
    // i=2: Pavg=avg(10,10,10)=10, Vavg=avg(100,100,100)=100, pt=10, vt=100
    //      → (10-10)/10 * 100/100 = 0
    // i=3: Pavg=avg(10,10,11)=10.333..., Vavg=avg(100,100,200)=133.333...
    //      pt=11, vt=200
    //      (11-10.333)/10.333 = 0.064516
    //      200/133.333 = 1.5
    //      F1 ≈ 0.096774
    let price = vec![
        Some(10.0),
        Some(10.0),
        Some(10.0),
        Some(11.0),
    ];
    let volume = vec![
        Some(100.0),
        Some(100.0),
        Some(100.0),
        Some(200.0),
    ];
    let out = compute_pv_factor(&price, &volume, 3);
    assert_eq!(out.len(), 4);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    // i=2 窗口[10,10,10]有效，因子=0
    assert!((out[2].unwrap() - 0.0).abs() < 1e-9);
    // i=3 涨价放量
    assert!((out[3].unwrap() - 0.09677419354838711).abs() < 1e-9);
}

#[test]
fn test_pv_factor_price_drop_becomes_negative() {
    // 下跌缩量：close=[10,10,10,9], volume=[100,100,100,50], n=3
    // i=3: Pavg=avg(10,10,9)=9.666..., Vavg=avg(100,100,50)=83.333...
    //      pt=9, vt=50
    //      (9-9.666)/9.666 ≈ -0.068966
    //      50/83.333 = 0.6
    //      F1 ≈ -0.041379
    let price = vec![
        Some(10.0),
        Some(10.0),
        Some(10.0),
        Some(9.0),
    ];
    let volume = vec![
        Some(100.0),
        Some(100.0),
        Some(100.0),
        Some(50.0),
    ];
    let out = compute_pv_factor(&price, &volume, 3);
    assert_eq!(out.len(), 4);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!((out[2].unwrap() - 0.0).abs() < 1e-9);
    // 下跌时因子自动为负
    let f1 = out[3].unwrap();
    assert!(f1 < 0.0, "下跌因子应为负，实际 {}", f1);
    assert!((f1 - (-0.0413793103448276)).abs() < 1e-9);
}

#[test]
fn test_pv_factor_null_propagation() {
    // 价格窗口内含空 → 对应行 None；空离开窗口后恢复
    let price = vec![Some(10.0), None, Some(12.0), Some(13.0), Some(14.0)];
    let volume = vec![Some(100.0); 5];
    let out = compute_pv_factor(&price, &volume, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    // i=2: price窗口[10,None,12] 含空 → None
    assert!(out[2].is_none());
    // i=3: price窗口[None,12,13] 含空 → None
    assert!(out[3].is_none());
    // i=4: price窗口[12,13,14] 全有效 → Pavg=13, pt=14
    //   price_component = 1/13, volume_component = 100/100 = 1
    assert!((out[4].unwrap() - 1.0_f64 / 13.0).abs() < 1e-9);
}

#[test]
fn test_pv_factor_volume_null_propagation() {
    // volume 窗口含空；空离开后恢复
    let price = vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)];
    let volume = vec![Some(100.0), None, Some(100.0), Some(100.0), Some(100.0)];
    let out = compute_pv_factor(&price, &volume, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    // i=2,3 volume 窗口含空 → None
    assert!(out[2].is_none());
    assert!(out[3].is_none());
    // i=4 volume窗口[100,100,100] 全有效
    assert!(out[4].is_some());
}

#[test]
fn test_pv_factor_price_zero_avg_is_none() {
    // Pavg=0 保护：价格全为 0（退化情形）
    let price = vec![Some(0.0), Some(0.0), Some(0.0), Some(1.0)];
    let volume = vec![Some(100.0), Some(100.0), Some(100.0), Some(200.0)];
    let out = compute_pv_factor(&price, &volume, 3);
    // i=3: Pavg=avg(0,0,1)=0.333... 非 0，正常计算
    // Pavg==0 仅在窗口全为 0 时触发；前 3 行窗口全 0
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    // i=2: Pavg=0 → None
    assert!(out[2].is_none());
    // i=3: Pavg=0.333... 非 0，因子有效
    assert!(out[3].is_some());
}

#[test]
fn test_pv_factor_volume_zero_avg_is_none() {
    let price = vec![Some(10.0), Some(10.0), Some(10.0), Some(11.0)];
    let volume = vec![Some(0.0), Some(0.0), Some(0.0), Some(100.0)];
    let out = compute_pv_factor(&price, &volume, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    // i=2: Vavg=0 → None
    assert!(out[2].is_none());
    // i=3: Vavg=33.33 非 0 → 有效
    assert!(out[3].is_some());
}

#[test]
fn test_pv_factor_small_window_stable() {
    // 横盘且量稳定，因子应恒定为 0
    let price: Vec<Option<f64>> = vec![Some(10.0); 6];
    let volume: Vec<Option<f64>> = vec![Some(100.0); 6];
    let out = compute_pv_factor(&price, &volume, 3);
    for i in 0..out.len() {
        if i < 2 {
            assert!(out[i].is_none(), "前 2 行应为空");
        } else {
            assert!((out[i].unwrap() - 0.0).abs() < 1e-12, "横盘因子应为 0");
        }
    }
}

// ============ 辅助 DataFrame 构造 ============

fn df_with_two_f64(
    pcol: &str,
    pvals: Vec<Option<f64>>,
    vcol: &str,
    vvals: Vec<Option<f64>>,
) -> DataFrame {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(pcol, pvals));
    df.add_column(DataFrame::new_float64_column(vcol, vvals));
    df
}

fn df_with_price_f64_volume_i64(
    pvals: Vec<Option<f64>>,
    vvals: Vec<Option<i64>>,
) -> DataFrame {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column("close", pvals));
    df.add_column(DataFrame::new_int64_column("volume", vvals));
    df
}

// ============ apply_pv_factor ============

#[test]
fn test_apply_basic() {
    let mut df = df_with_two_f64(
        "close",
        vec![
            Some(10.0),
            Some(10.0),
            Some(10.0),
            Some(11.0),
        ],
        "volume",
        vec![
            Some(100.0),
            Some(100.0),
            Some(100.0),
            Some(200.0),
        ],
    );
    apply_pv_factor(&mut df, "close", "volume", 3, "pv");
    let col = df.column("pv").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert!(col.get_f64(0).is_none());
    assert!(col.get_f64(1).is_none());
    // i=2 窗口[10,10,10] 横盘，因子=0
    assert!((col.get_f64(2).unwrap() - 0.0).abs() < 1e-9);
    // i=3 涨价放量
    assert!((col.get_f64(3).unwrap() - 0.09677419354838711).abs() < 1e-9);
    // 源列保留
    assert_eq!(df.col_count(), 3);
}

#[test]
fn test_apply_int64_volume_promotion() {
    let mut df = df_with_price_f64_volume_i64(
        vec![
            Some(10.0),
            Some(10.0),
            Some(10.0),
            Some(11.0),
        ],
        vec![Some(100), Some(100), Some(100), Some(200)],
    );
    apply_pv_factor(&mut df, "close", "volume", 3, "pv_f");
    let col = df.column("pv_f").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert!((col.get_f64(3).unwrap() - 0.09677419354838711).abs() < 1e-9);
    // 源列 volume 仍为 Int64
    assert_eq!(df.column("volume").unwrap().data_type, DataType::Int64);
    assert_eq!(df.column("volume").unwrap().to_i64_vec(), vec![Some(100), Some(100), Some(100), Some(200)]);
}

#[test]
fn test_apply_custom_columns() {
    let mut df = df_with_two_f64(
        "price",
        vec![Some(10.0), Some(10.0), Some(10.0), Some(11.0)],
        "vol",
        vec![Some(100.0), Some(100.0), Some(100.0), Some(200.0)],
    );
    apply_pv_factor(&mut df, "price", "vol", 3, "f1");
    assert!((df.column("f1").unwrap().get_f64(3).unwrap() - 0.09677419354838711).abs() < 1e-9);
}

#[test]
fn test_apply_overwrite_existing_result_column() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column("close", vec![Some(10.0), Some(10.0), Some(10.0), Some(11.0)]));
    df.add_column(DataFrame::new_float64_column("volume", vec![Some(100.0), Some(100.0), Some(100.0), Some(200.0)]));
    df.add_column(DataFrame::new_float64_column("f1", vec![Some(999.0); 4]));
    apply_pv_factor(&mut df, "close", "volume", 3, "f1");
    // 列数仍为 3
    assert_eq!(df.col_count(), 3);
    assert!((df.column("f1").unwrap().get_f64(3).unwrap() - 0.09677419354838711).abs() < 1e-9);
    // 源列保留
    assert_eq!(df.column("close").unwrap().get_f64(3), Some(11.0));
}

#[test]
fn test_apply_result_equals_price_column_overwrites() {
    let mut df = df_with_two_f64(
        "close",
        vec![Some(10.0), Some(10.0), Some(10.0), Some(11.0)],
        "volume",
        vec![Some(100.0), Some(100.0), Some(100.0), Some(200.0)],
    );
    apply_pv_factor(&mut df, "close", "volume", 3, "close");
    // 列数仍为 2（覆盖了 close，保留 volume）
    assert_eq!(df.col_count(), 2);
    let col = df.column("close").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert!((col.get_f64(3).unwrap() - 0.09677419354838711).abs() < 1e-9);
}

#[test]
fn test_apply_result_equals_volume_column_overwrites() {
    let mut df = df_with_two_f64(
        "close",
        vec![Some(10.0), Some(10.0), Some(10.0), Some(11.0)],
        "volume",
        vec![Some(100.0), Some(100.0), Some(100.0), Some(200.0)],
    );
    apply_pv_factor(&mut df, "close", "volume", 3, "volume");
    assert_eq!(df.col_count(), 2);
    assert!((df.column("volume").unwrap().get_f64(3).unwrap() - 0.09677419354838711).abs() < 1e-9);
}

#[test]
fn test_apply_missing_price_column_skipped() {
    let mut df = df_with_two_f64(
        "close",
        vec![Some(10.0)],
        "volume",
        vec![Some(100.0)],
    );
    apply_pv_factor(&mut df, "missing", "volume", 3, "pv");
    assert!(df.column("pv").is_none());
    assert_eq!(df.col_count(), 2); // 原表不变
}

#[test]
fn test_apply_missing_volume_column_skipped() {
    let mut df = df_with_two_f64(
        "close",
        vec![Some(10.0)],
        "volume",
        vec![Some(100.0)],
    );
    apply_pv_factor(&mut df, "close", "missing", 3, "pv");
    assert!(df.column("pv").is_none());
    assert_eq!(df.col_count(), 2);
}

#[test]
fn test_apply_empty_df_noop() {
    let mut df = DataFrame::new();
    apply_pv_factor(&mut df, "close", "volume", 5, "pv");
    assert_eq!(df.row_count, 0);
    assert_eq!(df.col_count(), 0);
}

#[test]
fn test_apply_string_column_not_supported() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_string_column("close", vec![Some("a"), Some("b")]));
    df.add_column(DataFrame::new_float64_column("volume", vec![Some(100.0), Some(200.0)]));
    apply_pv_factor(&mut df, "close", "volume", 2, "pv");
    assert!(df.column("pv").is_none());
    assert_eq!(df.col_count(), 2);
}
