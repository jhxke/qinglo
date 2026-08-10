use super::*;
use operator_runtime::c_abi::{portdata_from_c, portdata_to_c, CPortData, CPortValue, TYPE_NULL};
use operator_runtime::{DataFrame, DataType, PortData};
use std::ffi::CString;
use std::ptr;

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
    let json = r#"{"n":"10","price_column":"close","volume_column":"vol","result_column":"fvc"}"#;
    let p = parse_params(json);
    assert_eq!(p.n, "10");
    assert_eq!(p.price_column, "close");
    assert_eq!(p.volume_column, "vol");
    assert_eq!(p.result_column, "fvc");
}

#[test]
fn test_parse_params_invalid_json() {
    let p = parse_params("not json");
    assert_eq!(p.n, "");
    assert_eq!(p.price_column, "");
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

// ============ compute_daily_return ============

#[test]
fn test_daily_return_basic() {
    let v = vec![Some(10.0), Some(11.0), Some(12.0), Some(9.0)];
    let out = compute_daily_return(&v);
    assert_eq!(out.len(), 4);
    assert!(out[0].is_none());
    assert!((out[1].unwrap() - 0.1).abs() < 1e-12);
    assert!((out[2].unwrap() - (1.0 / 11.0)).abs() < 1e-12);
    assert!((out[3].unwrap() - (-3.0 / 12.0)).abs() < 1e-12);
}

#[test]
fn test_daily_return_first_row_none() {
    let v = vec![Some(10.0), Some(11.0)];
    let out = compute_daily_return(&v);
    assert!(out[0].is_none());
    assert!((out[1].unwrap() - 0.1).abs() < 1e-12);
}

#[test]
fn test_daily_return_null_propagation() {
    let v = vec![Some(10.0), None, Some(12.0), Some(13.0)];
    let out = compute_daily_return(&v);
    assert!(out[0].is_none());
    // i=1: cur=None → None
    assert!(out[1].is_none());
    // i=2: prev=None → None
    assert!(out[2].is_none());
    // i=3: (13-12)/12
    assert!((out[3].unwrap() - (1.0 / 12.0)).abs() < 1e-12);
}

#[test]
fn test_daily_return_zero_prev_is_none() {
    let v = vec![Some(0.0), Some(12.0)];
    let out = compute_daily_return(&v);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
}

#[test]
fn test_daily_return_single_row() {
    let v = vec![Some(10.0)];
    let out = compute_daily_return(&v);
    assert_eq!(out.len(), 1);
    assert!(out[0].is_none());
}

// ============ 滚动均值 ============

#[test]
fn test_rolling_mean_basic() {
    let v = vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)];
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
    let v = vec![Some(10.0), None, Some(12.0), Some(13.0), Some(14.0)];
    let out = compute_rolling_mean(&v, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none()); // [10,None,12]
    assert!(out[3].is_none()); // [None,12,13]
    assert!((out[4].unwrap() - 13.0).abs() < 1e-12); // [12,13,14]
}

// ============ 滚动最大值 ============

#[test]
fn test_rolling_max_basic() {
    let v = vec![Some(10.0), Some(14.0), Some(12.0), Some(13.0), Some(11.0)];
    let out = compute_rolling_max(&v, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!((out[2].unwrap() - 14.0).abs() < 1e-12); // [10,14,12]
    assert!((out[3].unwrap() - 14.0).abs() < 1e-12); // [14,12,13]
    assert!((out[4].unwrap() - 13.0).abs() < 1e-12); // [12,13,11]
}

#[test]
fn test_rolling_max_null_propagation() {
    let v = vec![Some(10.0), None, Some(12.0), Some(13.0), Some(14.0)];
    let out = compute_rolling_max(&v, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none()); // 含 None
    assert!(out[3].is_none()); // 含 None
    assert!((out[4].unwrap() - 14.0).abs() < 1e-12); // [12,13,14]
}

// ============ 滚动最小值 ============

#[test]
fn test_rolling_min_basic() {
    let v = vec![Some(10.0), Some(14.0), Some(12.0), Some(13.0), Some(11.0)];
    let out = compute_rolling_min(&v, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!((out[2].unwrap() - 10.0).abs() < 1e-12); // [10,14,12]
    assert!((out[3].unwrap() - 12.0).abs() < 1e-12); // [14,12,13]
    assert!((out[4].unwrap() - 11.0).abs() < 1e-12); // [12,13,11]
}

#[test]
fn test_rolling_min_monotone_increasing_deque() {
    // 单调递增序列：min 始终为窗口首元素
    let v = vec![Some(1.0), Some(2.0), Some(3.0), Some(4.0), Some(5.0)];
    let out = compute_rolling_min(&v, 3);
    assert!((out[2].unwrap() - 1.0).abs() < 1e-12);
    assert!((out[3].unwrap() - 2.0).abs() < 1e-12);
    assert!((out[4].unwrap() - 3.0).abs() < 1e-12);
}

#[test]
fn test_rolling_min_null_propagation() {
    let v = vec![Some(10.0), None, Some(12.0), Some(13.0), Some(14.0)];
    let out = compute_rolling_min(&v, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none()); // 含 None
    assert!(out[3].is_none()); // 含 None
    assert!((out[4].unwrap() - 12.0).abs() < 1e-12); // [12,13,14]
}

#[test]
fn test_rolling_min_constant_series() {
    let v = vec![Some(5.0), Some(5.0), Some(5.0), Some(5.0)];
    let out = compute_rolling_min(&v, 2);
    assert!((out[1].unwrap() - 5.0).abs() < 1e-12);
    assert!((out[2].unwrap() - 5.0).abs() < 1e-12);
    assert!((out[3].unwrap() - 5.0).abs() < 1e-12);
}

// ============ 因子计算（手工锚点） ============

fn approx(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
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
fn test_factor_vc_basic_w_zero() {
    // close 单调递增 → 每日 close 都是窗口最高 → pos=1 → W=0 → factor_vc = M
    // close=[10,11,12,13,14], volume=[100,110,120,130,140], n=3
    // i=2: ret[0]=None → None
    // i=3: M=(13-12)/12=1/12, W=0 → factor_vc=1/12
    // i=4: M=(14-13)/13=1/13, W=0 → factor_vc=1/13
    let price = vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)];
    let volume = vec![Some(100.0), Some(110.0), Some(120.0), Some(130.0), Some(140.0)];
    let out = compute_factor_vc(&price, &volume, 3);
    assert_eq!(out.len(), 5);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none()); // ret[0]=None → S 窗口含空
    assert!((out[3].unwrap() - (1.0_f64 / 12.0)).abs() < 1e-9);
    assert!((out[4].unwrap() - (1.0_f64 / 13.0)).abs() < 1e-9);
}

#[test]
fn test_factor_vc_with_w_mid_position() {
    // close=[10,14,12,13,11], volume=[100,200,150,180,160], n=3
    // i=3: pos=0.5, W=1-0.5^1.5, M=0, S 手算
    let price = vec![Some(10.0), Some(14.0), Some(12.0), Some(13.0), Some(11.0)];
    let volume = vec![Some(100.0), Some(200.0), Some(150.0), Some(180.0), Some(160.0)];
    let out = compute_factor_vc(&price, &volume, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none()); // ret[0]=None

    // i=3 手算期望
    let vma_v = 530.0_f64 / 3.0; // (200+150+180)/3
    let s3 = ((1.0 + 200.0 / vma_v).ln() - (1.0 + 150.0 / vma_v).ln()
        + (1.0 + 180.0 / vma_v).ln())
        / 3.0;
    let w3 = 1.0 - 0.5_f64.powf(1.5); // pos=0.5
    let expected_3 = w3 * s3; // M=0
    assert!(
        (out[3].unwrap() - expected_3).abs() < 1e-9,
        "i=3: actual={} expected={}",
        out[3].unwrap(),
        expected_3
    );

    // i=4 手算期望：pos=0 → W=1, M=-1/12
    let vma_v4 = 490.0_f64 / 3.0; // (150+180+160)/3
    let s4 = (-(1.0 + 150.0 / vma_v4).ln() + (1.0 + 180.0 / vma_v4).ln()
        - (1.0 + 160.0 / vma_v4).ln())
        / 3.0;
    let expected_4 = -1.0_f64 / 12.0 + s4; // M + W*S, W=1
    assert!(
        (out[4].unwrap() - expected_4).abs() < 1e-9,
        "i=4: actual={} expected={}",
        out[4].unwrap(),
        expected_4
    );
}

#[test]
fn test_factor_vc_flat_price_hn_eq_ln_is_none() {
    // 价格恒定 → Hn==Ln → pos 分母为 0 → None
    let price = vec![Some(10.0), Some(10.0), Some(10.0), Some(10.0)];
    let volume = vec![Some(100.0), Some(200.0), Some(150.0), Some(180.0)];
    let out = compute_factor_vc(&price, &volume, 3);
    assert!(out.iter().all(|x| x.is_none()));
}

#[test]
fn test_factor_vc_volume_zero_avg_is_none() {
    // 窗口内 volume 全 0 → vma=0 → None
    let price = vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)];
    let volume = vec![Some(0.0), Some(0.0), Some(0.0), Some(100.0), Some(100.0)];
    let out = compute_factor_vc(&price, &volume, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none()); // vma=0
    // i=3: 窗口 [1,3] volume=[0,0,100] → vma=100/3 非 0，但 ret 窗口 [1,3] 全有效
    assert!(out[3].is_some());
    assert!(out[4].is_some());
}

#[test]
fn test_factor_vc_price_null_propagation() {
    // price 窗口含空 → cma/hn/ln 为 None 且 ret 窗口含空 → None
    let price = vec![Some(10.0), None, Some(12.0), Some(13.0), Some(14.0), Some(15.0)];
    let volume = vec![Some(100.0); 6];
    let out = compute_factor_vc(&price, &volume, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none()); // price 窗口 [10,None,12] 含空
    assert!(out[3].is_none()); // price 窗口 [None,12,13] 含空；ret[1]=None
    // i=4: price 窗口 [12,13,14] 全有效；ret[2..4] 依赖 price[1..4]
    //   ret[2]=(12-None)→None → S 窗口含空 → None
    assert!(out[4].is_none());
    // i=5: price 窗口 [13,14,15] 全有效；ret[3..5]
    //   ret[3]=(13-12)/12, ret[4]=(14-13)/13, ret[5]=(15-14)/14 全有效
    assert!(out[5].is_some());
}

#[test]
fn test_factor_vc_volume_null_propagation() {
    // volume 窗口含空 → vma=None → None；空离开后恢复
    let price = vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0), Some(15.0)];
    let volume = vec![Some(100.0), None, Some(120.0), Some(130.0), Some(140.0), Some(150.0)];
    let out = compute_factor_vc(&price, &volume, 3);
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none()); // volume 窗口含空 + ret[0]=None
    assert!(out[3].is_none()); // volume 窗口 [None,120,130] 含空
    assert!(out[4].is_some()); // volume 窗口 [120,130,140] 全有效
    assert!(out[5].is_some());
}

#[test]
fn test_factor_vc_n_equals_len_single_valid() {
    // n == len：仅最后一行窗口刚好填满，但 ret[0]=None → None
    let price = vec![Some(10.0), Some(11.0), Some(12.0)];
    let volume = vec![Some(100.0), Some(110.0), Some(120.0)];
    let out = compute_factor_vc(&price, &volume, 3);
    assert_eq!(out.len(), 3);
    // i=2: 窗口 [0,2] 含 ret[0]=None → None
    assert!(out[0].is_none());
    assert!(out[1].is_none());
    assert!(out[2].is_none());
}

#[test]
fn test_factor_vc_n_one_all_none() {
    // n=1：窗口只有当日；但 ret[0]=None → i=0 None；i>=1 时窗口 [i,i] 只含 ret[i]
    // ret[i] 依赖 price[i-1]，i>=1 时有效（若 price 全有效）
    // pos=(pt-Ln)/(Hn-Ln)=(pt-pt)/(pt-pt)=0/0 → Hn==Ln → None
    let price = vec![Some(10.0), Some(11.0), Some(12.0)];
    let volume = vec![Some(100.0), Some(110.0), Some(120.0)];
    let out = compute_factor_vc(&price, &volume, 1);
    // n=1 时窗口单点，Hn==Ln==close → pos 分母 0 → 全 None
    assert!(out.iter().all(|x| x.is_none()));
}

#[test]
fn test_factor_vc_empty_input() {
    let out = compute_factor_vc(&[], &[], 3);
    assert!(out.is_empty());
}

#[test]
fn test_factor_vc_n_zero_defensive() {
    let price = vec![Some(10.0), Some(11.0)];
    let volume = vec![Some(100.0), Some(110.0)];
    let out = compute_factor_vc(&price, &volume, 0);
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|x| x.is_none()));
}

// ============ naive 参考实现对比（长序列 + 含空值） ============

/// 朴素参考实现：完全按 Python 公式逐行计算，不依赖 rolling 辅助函数。
/// 用于与 O(n) 主实现交叉验证。
fn naive_factor_vc(price: &[Option<f64>], volume: &[Option<f64>], n: usize) -> Vec<Option<f64>> {
    let len = price.len();
    let mut result = Vec::with_capacity(len);
    for i in 0..len {
        if i + 1 < n {
            result.push(None);
            continue;
        }
        // 收集窗口 price / volume
        let mut p_win: Vec<f64> = Vec::with_capacity(n);
        let mut v_win: Vec<f64> = Vec::with_capacity(n);
        let mut valid = true;
        for j in (i + 1 - n)..=i {
            match (price[j], volume[j]) {
                (Some(p), Some(v)) => {
                    p_win.push(p);
                    v_win.push(v);
                }
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if !valid {
            result.push(None);
            continue;
        }
        // 收集窗口 ret（ret[j] 依赖 price[j-1]；j==0 → None）
        let mut r_win: Vec<f64> = Vec::with_capacity(n);
        for j in (i + 1 - n)..=i {
            if j == 0 {
                valid = false;
                break;
            }
            match (price[j - 1], price[j]) {
                (Some(prev), Some(_)) if prev != 0.0 => {
                    r_win.push((price[j].unwrap() - prev) / prev);
                }
                _ => {
                    valid = false;
                    break;
                }
            }
        }
        if !valid {
            result.push(None);
            continue;
        }
        // vma_
        let vma_v: f64 = v_win.iter().sum::<f64>() / n as f64;
        if vma_v == 0.0 {
            result.push(None);
            continue;
        }
        // S
        let mut s = 0.0f64;
        for (r, v) in r_win.iter().zip(v_win.iter()) {
            let lv = (1.0 + v / vma_v).ln();
            if *r > 0.0 {
                s += lv;
            } else if *r < 0.0 {
                s -= lv;
            }
        }
        let s_val = s / n as f64;
        // M
        let cma_v: f64 = p_win.iter().sum::<f64>() / n as f64;
        if cma_v == 0.0 {
            result.push(None);
            continue;
        }
        let pt = p_win[n - 1];
        let m = (pt - cma_v) / cma_v;
        // W
        let hn_v = p_win.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let ln_v = p_win.iter().cloned().fold(f64::INFINITY, f64::min);
        if hn_v == ln_v {
            result.push(None);
            continue;
        }
        let pos = (pt - ln_v) / (hn_v - ln_v);
        let w = 1.0 - pos.powf(1.5);
        result.push(Some(m + w * s_val));
    }
    result
}

#[test]
fn test_naive_vs_impl_monotone() {
    let price: Vec<Option<f64>> = (0..30).map(|i| Some(10.0 + i as f64 * 0.3)).collect();
    let volume: Vec<Option<f64>> = (0..30).map(|i| Some(1000.0 + i as f64 * 10.0)).collect();
    for n in [2, 3, 5, 10, 20] {
        let a = compute_factor_vc(&price, &volume, n);
        let b = naive_factor_vc(&price, &volume, n);
        assert_approx_vec(&a, &b);
    }
}

#[test]
fn test_naive_vs_impl_oscillating() {
    let price: Vec<Option<f64>> = (0..40)
        .map(|i| Some(10.0 + (i as f64 * 0.7).sin() * 2.0 + i as f64 * 0.05))
        .collect();
    let volume: Vec<Option<f64>> = (0..40)
        .map(|i| Some(500.0 + (i as f64 * 0.5).cos().abs() * 800.0 + 100.0))
        .collect();
    for n in [3, 5, 8, 15] {
        let a = compute_factor_vc(&price, &volume, n);
        let b = naive_factor_vc(&price, &volume, n);
        assert_approx_vec(&a, &b);
    }
}

#[test]
fn test_naive_vs_impl_with_nulls() {
    // 含空值的序列：每隔几行插入 None
    let price: Vec<Option<f64>> = (0..50)
        .map(|i| if i % 11 == 3 { None } else { Some(10.0 + (i as f64 * 0.4).sin() * 3.0 + i as f64 * 0.02) })
        .collect();
    let volume: Vec<Option<f64>> = (0..50)
        .map(|i| if i % 7 == 2 { None } else { Some(800.0 + (i as f64).fract() * 500.0 + (i as f64 * 0.3).cos().abs() * 400.0) })
        .collect();
    for n in [3, 5, 10] {
        let a = compute_factor_vc(&price, &volume, n);
        let b = naive_factor_vc(&price, &volume, n);
        assert_approx_vec(&a, &b);
    }
}

#[test]
fn test_naive_vs_impl_declining() {
    // 单调下跌序列：ret 全为负，S 为负
    let price: Vec<Option<f64>> = (0..25).map(|i| Some(50.0 - i as f64 * 0.5)).collect();
    let volume: Vec<Option<f64>> = (0..25).map(|i| Some(1000.0 - i as f64 * 5.0)).collect();
    for n in [3, 5, 10] {
        let a = compute_factor_vc(&price, &volume, n);
        let b = naive_factor_vc(&price, &volume, n);
        assert_approx_vec(&a, &b);
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

// ============ apply_factor_vc ============

#[test]
fn test_apply_basic() {
    let mut df = df_with_two_f64(
        "close",
        vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)],
        "volume",
        vec![Some(100.0), Some(110.0), Some(120.0), Some(130.0), Some(140.0)],
    );
    apply_factor_vc(&mut df, "close", "volume", 3, "fvc");
    let col = df.column("fvc").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert!(col.get_f64(0).is_none());
    assert!(col.get_f64(1).is_none());
    assert!(col.get_f64(2).is_none()); // ret[0]=None
    assert!((col.get_f64(3).unwrap() - (1.0_f64 / 12.0)).abs() < 1e-9);
    assert!((col.get_f64(4).unwrap() - (1.0_f64 / 13.0)).abs() < 1e-9);
    // 源列保留
    assert_eq!(df.col_count(), 3);
}

#[test]
fn test_apply_int64_volume_promotion() {
    let mut df = df_with_price_f64_volume_i64(
        vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)],
        vec![Some(100), Some(110), Some(120), Some(130), Some(140)],
    );
    apply_factor_vc(&mut df, "close", "volume", 3, "fvc");
    let col = df.column("fvc").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert!((col.get_f64(3).unwrap() - (1.0_f64 / 12.0)).abs() < 1e-9);
    assert!((col.get_f64(4).unwrap() - (1.0_f64 / 13.0)).abs() < 1e-9);
    // 源列 volume 仍为 Int64
    assert_eq!(df.column("volume").unwrap().data_type, DataType::Int64);
}

#[test]
fn test_apply_custom_columns() {
    let mut df = df_with_two_f64(
        "price",
        vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)],
        "vol",
        vec![Some(100.0), Some(110.0), Some(120.0), Some(130.0), Some(140.0)],
    );
    apply_factor_vc(&mut df, "price", "vol", 3, "my_fvc");
    let col = df.column("my_fvc").unwrap();
    assert!((col.get_f64(3).unwrap() - (1.0_f64 / 12.0)).abs() < 1e-9);
}

#[test]
fn test_apply_overwrite_existing_result_column() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(
        "close",
        vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)],
    ));
    df.add_column(DataFrame::new_float64_column(
        "volume",
        vec![Some(100.0), Some(110.0), Some(120.0), Some(130.0), Some(140.0)],
    ));
    df.add_column(DataFrame::new_float64_column("fvc", vec![Some(999.0); 5]));
    apply_factor_vc(&mut df, "close", "volume", 3, "fvc");
    assert_eq!(df.col_count(), 3);
    assert!((df.column("fvc").unwrap().get_f64(3).unwrap() - (1.0_f64 / 12.0)).abs() < 1e-9);
    assert_eq!(df.column("close").unwrap().get_f64(3), Some(13.0));
}

#[test]
fn test_apply_result_equals_price_column_overwrites() {
    let mut df = df_with_two_f64(
        "close",
        vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)],
        "volume",
        vec![Some(100.0), Some(110.0), Some(120.0), Some(130.0), Some(140.0)],
    );
    apply_factor_vc(&mut df, "close", "volume", 3, "close");
    assert_eq!(df.col_count(), 2);
    let col = df.column("close").unwrap();
    assert_eq!(col.data_type, DataType::Float64);
    assert!((col.get_f64(3).unwrap() - (1.0_f64 / 12.0)).abs() < 1e-9);
}

#[test]
fn test_apply_result_equals_volume_column_overwrites() {
    let mut df = df_with_two_f64(
        "close",
        vec![Some(10.0), Some(11.0), Some(12.0), Some(13.0), Some(14.0)],
        "volume",
        vec![Some(100.0), Some(110.0), Some(120.0), Some(130.0), Some(140.0)],
    );
    apply_factor_vc(&mut df, "close", "volume", 3, "volume");
    assert_eq!(df.col_count(), 2);
    assert!((df.column("volume").unwrap().get_f64(3).unwrap() - (1.0_f64 / 12.0)).abs() < 1e-9);
}

#[test]
fn test_apply_missing_price_column_skipped() {
    let mut df = df_with_two_f64(
        "close",
        vec![Some(10.0), Some(11.0)],
        "volume",
        vec![Some(100.0), Some(110.0)],
    );
    apply_factor_vc(&mut df, "missing", "volume", 3, "fvc");
    assert!(df.column("fvc").is_none());
    assert_eq!(df.col_count(), 2);
}

#[test]
fn test_apply_missing_volume_column_skipped() {
    let mut df = df_with_two_f64(
        "close",
        vec![Some(10.0), Some(11.0)],
        "volume",
        vec![Some(100.0), Some(110.0)],
    );
    apply_factor_vc(&mut df, "close", "missing", 3, "fvc");
    assert!(df.column("fvc").is_none());
    assert_eq!(df.col_count(), 2);
}

#[test]
fn test_apply_empty_df_noop() {
    let mut df = DataFrame::new();
    apply_factor_vc(&mut df, "close", "volume", 5, "fvc");
    assert_eq!(df.row_count, 0);
    assert_eq!(df.col_count(), 0);
}

#[test]
fn test_apply_string_column_not_supported() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_string_column("close", vec![Some("a"), Some("b")]));
    df.add_column(DataFrame::new_float64_column("volume", vec![Some(100.0), Some(200.0)]));
    apply_factor_vc(&mut df, "close", "volume", 2, "fvc");
    assert!(df.column("fvc").is_none());
    assert_eq!(df.col_count(), 2);
}

// ============ execute_operator 端到端 ============

fn run_factor_vc(input_dfs: Vec<DataFrame>, params_json: &str) -> (i32, Option<PortData>) {
    let input_port = PortData::DataFrameArray(input_dfs);
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

    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }

    let output = if output_slots[0].type_tag != TYPE_NULL {
        Some(unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) })
    } else {
        None
    };
    (result, output)
}

fn build_stock_df(n: usize) -> DataFrame {
    let mut df = DataFrame::new();
    let close: Vec<Option<f64>> = (0..n)
        .map(|i| Some(10.0 + (i as f64) * 0.5 + (if i >= n - 1 { 2.0 } else { 0.0 })))
        .collect();
    let volume: Vec<Option<f64>> = (0..n)
        .map(|i| Some(1000.0 + (i as f64) * 50.0 + (if i >= n - 1 { 1000.0 } else { 0.0 })))
        .collect();
    df.add_column(DataFrame::new_float64_column("close", close));
    df.add_column(DataFrame::new_float64_column("volume", volume));
    df
}

#[test]
fn test_execute_default_params() {
    // 默认 n=20，df 有 25 行 → i=20..24 有效（5 行有效因子）
    let df = build_stock_df(25);
    let (code, pd) = run_factor_vc(vec![df], r#"{}"#);
    assert_eq!(code, 0);
    let dfs = match pd {
        Some(PortData::DataFrameArray(d)) => d,
        _ => panic!("期望 DataFrameArray 输出"),
    };
    assert_eq!(dfs.len(), 1);
    let out_df = &dfs[0];
    assert_eq!(out_df.col_count(), 3); // close, volume, factor_vc_20
    // 默认结果列名 factor_vc_20
    assert!(out_df.column("factor_vc_20").is_some());
    let col = out_df.column("factor_vc_20").unwrap();
    // 前 N 行为 None（N=20）：i=0..18 窗口不足，i=19 窗口 [0,19] 含 ret[0]=None → None
    // i>=20 窗口 [1,..] 不含 ret[0] → 有效
    for i in 0..20 {
        assert!(col.get_f64(i).is_none(), "i={} 应为 None", i);
    }
    assert!(col.get_f64(20).is_some());
}

#[test]
fn test_execute_custom_n_and_columns() {
    let df = build_stock_df(15);
    let (code, pd) = run_factor_vc(
        vec![df],
        r#"{"n":"5","price_column":"close","volume_column":"volume","result_column":"my_vc"}"#,
    );
    assert_eq!(code, 0);
    let dfs = match pd {
        Some(PortData::DataFrameArray(d)) => d,
        _ => panic!("期望 DataFrameArray 输出"),
    };
    assert!(dfs[0].column("my_vc").is_some());
    // n=5 → i=0..3 窗口不足，i=4 窗口 [0,4] 含 ret[0]=None → None；i>=5 窗口 [1,..] 有效
    let col = dfs[0].column("my_vc").unwrap();
    for i in 0..5 {
        assert!(col.get_f64(i).is_none());
    }
    assert!(col.get_f64(5).is_some());
}

#[test]
fn test_execute_empty_result_column_fallback() {
    let df = build_stock_df(12);
    let (code, pd) = run_factor_vc(vec![df], r#"{"n":"3"}"#);
    assert_eq!(code, 0);
    let dfs = match pd {
        Some(PortData::DataFrameArray(d)) => d,
        _ => panic!("期望 DataFrameArray 输出"),
    };
    // 空结果列回退为 factor_vc_3
    assert!(dfs[0].column("factor_vc_3").is_some());
}

#[test]
fn test_execute_multiple_dataframes() {
    let df_a = build_stock_df(10);
    let df_b = build_stock_df(20);
    let (code, pd) = run_factor_vc(vec![df_a, df_b], r#"{"n":"3"}"#);
    assert_eq!(code, 0);
    let dfs = match pd {
        Some(PortData::DataFrameArray(d)) => d,
        _ => panic!("期望 DataFrameArray 输出"),
    };
    assert_eq!(dfs.len(), 2);
    assert!(dfs[0].column("factor_vc_3").is_some());
    assert!(dfs[1].column("factor_vc_3").is_some());
}

#[test]
fn test_execute_single_dataframe_wrap() {
    // 单个 DataFrame 输入也应被包装为单元素数组处理
    let input_port = PortData::DataFrame(build_stock_df(15));
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(r#"{"n":"5"}"#).unwrap_or_default();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let code = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
    assert_eq!(code, 0);
    assert_ne!(output_slots[0].type_tag, TYPE_NULL);
    let pd = unsafe { portdata_from_c(&mut output_slots[0] as *mut CPortData) };
    match pd {
        PortData::DataFrameArray(dfs) => assert_eq!(dfs.len(), 1),
        _ => panic!("单个 DataFrame 输入应输出 DataFrameArray"),
    }
}

#[test]
fn test_execute_missing_input_returns_minus_3() {
    let params_cstr = CString::new(r#"{"n":"5"}"#).unwrap_or_default();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let code = execute_operator(
        ptr::null(),
        0,
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    assert_eq!(code, -3);
}

#[test]
fn test_execute_empty_array_returns_minus_5() {
    let input_port = PortData::DataFrameArray(vec![]);
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(r#"{"n":"5"}"#).unwrap_or_default();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let code = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
    assert_eq!(code, -5);
}

#[test]
fn test_execute_wrong_input_type_returns_minus_4() {
    // 输入 Int 而非 DataFrame / DataFrameArray
    let input_port = PortData::Int(42);
    let mut c_inputs = [portdata_to_c(&input_port)];
    let params_cstr = CString::new(r#"{"n":"5"}"#).unwrap_or_default();
    let mut output_slots: [CPortData; 2] = [CPortData {
        type_tag: TYPE_NULL,
        value: CPortValue { str_ptr: ptr::null_mut() },
    }; 2];
    let code = execute_operator(
        c_inputs.as_ptr(),
        c_inputs.len(),
        output_slots.as_mut_ptr(),
        output_slots.len(),
        params_cstr.as_ptr(),
    );
    if c_inputs[0].type_tag != TYPE_NULL {
        release_port_data(&mut c_inputs[0] as *mut CPortData);
    }
    assert_eq!(code, -4);
}

#[test]
fn test_execute_invalid_n_returns_minus_6() {
    let df = build_stock_df(10);
    let (code, _) = run_factor_vc(vec![df], r#"{"n":"0"}"#);
    assert_eq!(code, -6);
}

#[test]
fn test_execute_invalid_n_non_numeric_returns_minus_6() {
    let df = build_stock_df(10);
    let (code, _) = run_factor_vc(vec![df], r#"{"n":"abc"}"#);
    assert_eq!(code, -6);
}

#[test]
fn test_execute_empty_df_preserved() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column("close", vec![]));
    df.add_column(DataFrame::new_float64_column("volume", vec![]));
    let (code, pd) = run_factor_vc(vec![df], r#"{"n":"3"}"#);
    assert_eq!(code, 0);
    let dfs = match pd {
        Some(PortData::DataFrameArray(d)) => d,
        _ => panic!("期望 DataFrameArray 输出"),
    };
    // 空 DataFrame 原样保留（行数为 0）
    assert_eq!(dfs[0].row_count, 0);
}

#[test]
fn test_version() {
    let v = unsafe { CStr::from_ptr(factor_vc_operator_version()) }
        .to_str()
        .unwrap();
    assert_eq!(v, "0.1.0");
}
