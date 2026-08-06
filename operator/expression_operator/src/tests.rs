use super::*;
use operator_runtime::DataFrame;
use std::collections::HashMap;

// ============ 词法分析 ============

#[test]
fn test_tokenize_basic() {
    let tokens = tokenize("ma5 > ma10").unwrap();
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0], Token::Ident("ma5".to_string()));
    assert_eq!(tokens[1], Token::Gt);
    assert_eq!(tokens[2], Token::Ident("ma10".to_string()));
}

#[test]
fn test_tokenize_numbers() {
    let tokens = tokenize("3.14 + 1e-5 - 2").unwrap();
    assert_eq!(tokens.len(), 5);
    assert_eq!(tokens[0], Token::Number(3.14));
    assert_eq!(tokens[1], Token::Plus);
    assert_eq!(tokens[2], Token::Number(1e-5));
    assert_eq!(tokens[3], Token::Minus);
    assert_eq!(tokens[4], Token::Number(2.0));
}

#[test]
fn test_tokenize_multi_char_ops() {
    let tokens = tokenize("a >= b && c <= d || e != f").unwrap();
    assert_eq!(tokens.iter().filter(|t| matches!(t, Token::Ge)).count(), 1);
    assert_eq!(tokens.iter().filter(|t| matches!(t, Token::Le)).count(), 1);
    assert_eq!(tokens.iter().filter(|t| matches!(t, Token::And)).count(), 1);
    assert_eq!(tokens.iter().filter(|t| matches!(t, Token::Or)).count(), 1);
    assert_eq!(tokens.iter().filter(|t| matches!(t, Token::Ne)).count(), 1);
}

#[test]
fn test_tokenize_whitespace_ignored() {
    let tokens = tokenize("  close   >  open  ").unwrap();
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0], Token::Ident("close".to_string()));
    assert_eq!(tokens[2], Token::Ident("open".to_string()));
}

#[test]
fn test_tokenize_illegal_single_eq() {
    assert!(tokenize("a = b").is_err());
}

#[test]
fn test_tokenize_illegal_char() {
    assert!(tokenize("a @ b").is_err());
}

// ============ 语法分析 ============

#[test]
fn test_parse_simple_comparison() {
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
fn test_parse_arithmetic_precedence() {
    // a + b * c  →  a + (b * c)
    let ast = parse_expression("a + b * c").unwrap();
    match ast {
        Expr::Binary(BinOp::Add, _, rhs) => match &*rhs {
            Expr::Binary(BinOp::Mul, _, _) => {}
            other => panic!("乘法应在加法之下，得到 {:?}", other),
        },
        other => panic!("期望 Add 根节点，得到 {:?}", other),
    }
}

#[test]
fn test_parse_paren_override_precedence() {
    // (a + b) * c
    let ast = parse_expression("(a + b) * c").unwrap();
    match ast {
        Expr::Binary(BinOp::Mul, lhs, _) => match &*lhs {
            Expr::Binary(BinOp::Add, _, _) => {}
            other => panic!("括号内应为 Add，得到 {:?}", other),
        },
        other => panic!("期望 Mul 根节点，得到 {:?}", other),
    }
}

#[test]
fn test_parse_logical_and_comparison() {
    // ma5 > ma10 && rsi < 30
    let ast = parse_expression("ma5 > ma10 && rsi < 30").unwrap();
    match ast {
        Expr::Binary(BinOp::And, _, _) => {}
        other => panic!("期望 And 根节点，得到 {:?}", other),
    }
}

#[test]
fn test_parse_unary_negation() {
    let ast = parse_expression("-a").unwrap();
    match ast {
        Expr::Neg(inner) => match &*inner {
            Expr::Column(name) => assert_eq!(name, "a"),
            other => panic!("期望 Column，得到 {:?}", other),
        },
        other => panic!("期望 Neg，得到 {:?}", other),
    }
}

#[test]
fn test_parse_logical_not() {
    let ast = parse_expression("!(a > b)").unwrap();
    match ast {
        Expr::Not(inner) => match &*inner {
            Expr::Binary(BinOp::Gt, _, _) => {}
            other => panic!("期望 Gt，得到 {:?}", other),
        },
        other => panic!("期望 Not，得到 {:?}", other),
    }
}

#[test]
fn test_parse_non_associative_comparison_rejected() {
    // a < b < c 应被拒绝
    assert!(parse_expression("a < b < c").is_err());
}

#[test]
fn test_parse_trailing_token_rejected() {
    assert!(parse_expression("a b").is_err());
}

#[test]
fn test_parse_unclosed_paren_rejected() {
    assert!(parse_expression("(a + b").is_err());
}

#[test]
fn test_parse_empty_rejected() {
    assert!(parse_expression("").is_err());
    assert!(parse_expression("   ").is_err());
}

// ============ 求值 ============

/// 辅助：构造列数据 map
fn make_columns(pairs: &[(&str, Vec<Option<f64>>)]) -> HashMap<String, Vec<Option<f64>>> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn test_evaluate_gt_true() {
    let ast = parse_expression("ma5 > ma10").unwrap();
    let cols = make_columns(&[("ma5", vec![Some(10.0)]), ("ma10", vec![Some(5.0)])]);
    assert_eq!(evaluate(&ast, 0, &cols), Some(1.0));
}

#[test]
fn test_evaluate_gt_false() {
    let ast = parse_expression("ma5 > ma10").unwrap();
    let cols = make_columns(&[("ma5", vec![Some(5.0)]), ("ma10", vec![Some(10.0)])]);
    assert_eq!(evaluate(&ast, 0, &cols), Some(0.0));
}

#[test]
fn test_evaluate_null_propagates() {
    let ast = parse_expression("ma5 > ma10").unwrap();
    let cols = make_columns(&[("ma5", vec![None]), ("ma10", vec![Some(5.0)])]);
    assert_eq!(evaluate(&ast, 0, &cols), None);
}

#[test]
fn test_evaluate_arithmetic() {
    let ast = parse_expression("close * 2 + 1").unwrap();
    let cols = make_columns(&[("close", vec![Some(3.0)])]);
    assert_eq!(evaluate(&ast, 0, &cols), Some(7.0));
}

#[test]
fn test_evaluate_division_by_zero() {
    let ast = parse_expression("a / b").unwrap();
    let cols = make_columns(&[("a", vec![Some(5.0)]), ("b", vec![Some(0.0)])]);
    assert_eq!(evaluate(&ast, 0, &cols), None);
}

#[test]
fn test_evaluate_and_logic() {
    let ast = parse_expression("a > 0 && b > 0").unwrap();
    let cols = make_columns(&[
        ("a", vec![Some(1.0), Some(1.0), Some(0.0), Some(-1.0)]),
        ("b", vec![Some(1.0), Some(0.0), Some(1.0), Some(1.0)]),
    ]);
    // 行0: 1 && 1 = 1
    assert_eq!(evaluate(&ast, 0, &cols), Some(1.0));
    // 行1: 1 && 0 = 0
    assert_eq!(evaluate(&ast, 1, &cols), Some(0.0));
    // 行2: 0 && 1 = 0
    assert_eq!(evaluate(&ast, 2, &cols), Some(0.0));
    // 行3: 0 && 1 = 0
    assert_eq!(evaluate(&ast, 3, &cols), Some(0.0));
}

#[test]
fn test_evaluate_or_logic() {
    let ast = parse_expression("a > 0 || b > 0").unwrap();
    let cols = make_columns(&[
        ("a", vec![Some(1.0), Some(0.0)]),
        ("b", vec![Some(0.0), Some(1.0)]),
    ]);
    assert_eq!(evaluate(&ast, 0, &cols), Some(1.0));
    assert_eq!(evaluate(&ast, 1, &cols), Some(1.0));
}

#[test]
fn test_evaluate_not() {
    let ast = parse_expression("!(a > 0)").unwrap();
    let cols = make_columns(&[("a", vec![Some(5.0), Some(-3.0)])]);
    // a=5: a>0=1, !1=0
    assert_eq!(evaluate(&ast, 0, &cols), Some(0.0));
    // a=-3: a>0=0, !0=1
    assert_eq!(evaluate(&ast, 1, &cols), Some(1.0));
}

#[test]
fn test_evaluate_compound_expression() {
    // (ma5 > ma10) && (rsi < 30)
    let ast = parse_expression("ma5 > ma10 && rsi < 30").unwrap();
    let cols = make_columns(&[
        ("ma5", vec![Some(15.0), Some(15.0), Some(5.0)]),
        ("ma10", vec![Some(10.0), Some(10.0), Some(10.0)]),
        ("rsi", vec![Some(25.0), Some(50.0), Some(25.0)]),
    ]);
    // 行0: 1 && 1 = 1
    assert_eq!(evaluate(&ast, 0, &cols), Some(1.0));
    // 行1: 1 && 0 = 0
    assert_eq!(evaluate(&ast, 1, &cols), Some(0.0));
    // 行2: 0 && 1 = 0
    assert_eq!(evaluate(&ast, 2, &cols), Some(0.0));
}

// ============ collect_columns ============

#[test]
fn test_collect_columns_dedup() {
    let ast = parse_expression("ma5 > ma10 && ma5 < ma20").unwrap();
    let mut cols = Vec::new();
    collect_columns(&ast, &mut cols);
    assert_eq!(cols, vec!["ma5", "ma10", "ma20"]);
}

// ============ apply_expression ============

/// 构造测试 DataFrame：含 ma5 / ma10 两列（Float64）
fn build_test_df() -> DataFrame {
    let mut df = DataFrame::new();
    let ma5 =
        DataFrame::new_float64_column("ma5", vec![Some(5.0), Some(15.0), Some(8.0), Some(20.0)]);
    let ma10 =
        DataFrame::new_float64_column("ma10", vec![Some(10.0), Some(10.0), Some(10.0), Some(10.0)]);
    df.add_column(ma5);
    df.add_column(ma10);
    df
}

#[test]
fn test_apply_expression_new_column() {
    let df = build_test_df();
    let ast = parse_expression("ma5 > ma10").unwrap();
    let out = apply_expression(&df, "signal", &ast).unwrap();

    // 应新增 signal 列，原列保留
    assert_eq!(out.col_count(), 3);
    assert!(out.column("signal").is_some());
    assert_eq!(out.row_count, 4);

    let signal = out.column("signal").unwrap();
    // ma5=[5,15,8,20], ma10=[10,10,10,10] → [0,1,0,1]
    assert_eq!(signal.get_f64(0), Some(0.0));
    assert_eq!(signal.get_f64(1), Some(1.0));
    assert_eq!(signal.get_f64(2), Some(0.0));
    assert_eq!(signal.get_f64(3), Some(1.0));
}

#[test]
fn test_apply_expression_overwrite_existing_column() {
    let df = build_test_df();
    let ast = parse_expression("ma5 > ma10").unwrap();
    // 用 ma5 作为输出列名 → 应覆盖 ma5（原 Float64）
    let out = apply_expression(&df, "ma5", &ast).unwrap();
    assert_eq!(out.col_count(), 2); // 列数不变
    let ma5 = out.column("ma5").unwrap();
    assert_eq!(ma5.get_f64(1), Some(1.0));
    assert_eq!(ma5.get_f64(0), Some(0.0));
}

#[test]
fn test_apply_expression_with_nulls() {
    // 前 2 行 ma5 为空
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(
        "ma5",
        vec![None, None, Some(15.0), Some(8.0)],
    ));
    df.add_column(DataFrame::new_float64_column(
        "ma10",
        vec![Some(10.0), Some(10.0), Some(10.0), Some(10.0)],
    ));

    let ast = parse_expression("ma5 > ma10").unwrap();
    let out = apply_expression(&df, "signal", &ast).unwrap();
    let signal = out.column("signal").unwrap();
    // 空 → 0
    assert_eq!(signal.get_f64(0), Some(0.0));
    assert_eq!(signal.get_f64(1), Some(0.0));
    // 15>10 → 1, 8>10 → 0
    assert_eq!(signal.get_f64(2), Some(1.0));
    assert_eq!(signal.get_f64(3), Some(0.0));
}

#[test]
fn test_apply_expression_missing_column_errors() {
    let df = build_test_df();
    let ast = parse_expression("ma5 > close").unwrap(); // close 不存在
    let res = apply_expression(&df, "signal", &ast);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(err.contains("close"));
}

#[test]
fn test_apply_expression_supports_int64_column() {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_int64_column(
        "volume",
        vec![Some(100i64), Some(200), Some(50)],
    ));
    df.add_column(DataFrame::new_int64_column(
        "threshold",
        vec![Some(150i64), Some(150), Some(150)],
    ));

    let ast = parse_expression("volume > threshold").unwrap();
    let out = apply_expression(&df, "flag", &ast).unwrap();
    let flag = out.column("flag").unwrap();
    assert_eq!(flag.get_f64(0), Some(0.0)); // 100 > 150 = false
    assert_eq!(flag.get_f64(1), Some(1.0)); // 200 > 150 = true
    assert_eq!(flag.get_f64(2), Some(0.0)); // 50 > 150 = false
}

#[test]
fn test_apply_expression_numeric_literal_expression() {
    // 纯数值表达式：5 > 3 → 1
    let df = DataFrame::new();
    // 空 DataFrame 行数为 0，先加一列制造 1 行
    let mut df = df;
    df.add_column(DataFrame::new_float64_column("x", vec![Some(0.0)]));
    let ast = parse_expression("5 > 3").unwrap();
    let out = apply_expression(&df, "signal", &ast).unwrap();
    assert_eq!(out.column("signal").unwrap().get_f64(0), Some(1.0));
}

// ============ parse_params ============

#[test]
fn test_parse_params_empty() {
    let p = parse_params("");
    assert_eq!(p.column_name, "");
    assert_eq!(p.expression, "");
}

#[test]
fn test_parse_params_valid() {
    let json = r#"{"column_name":"flag","expression":"ma5 > ma10"}"#;
    let p = parse_params(json);
    assert_eq!(p.column_name, "flag");
    assert_eq!(p.expression, "ma5 > ma10");
}

#[test]
fn test_parse_params_invalid_json() {
    let p = parse_params("not json");
    assert_eq!(p.column_name, "");
}

#[test]
fn test_parse_params_partial() {
    // 只给 expression，column_name 缺省
    let json = r#"{"expression":"close > 0"}"#;
    let p = parse_params(json);
    assert_eq!(p.column_name, "");
    assert_eq!(p.expression, "close > 0");
}
