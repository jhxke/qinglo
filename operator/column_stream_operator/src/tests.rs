use super::*;
use operator_runtime::ColumnData;

/// 构造一个含 4 种类型列（各列带 null）的测试 DataFrame。
///
/// - `price`  (Float64): [10.0, null, 1.5]
/// - `code`   (String):  ["a", "b", null]
/// - `qty`    (Int64):   [1, null, 3]
/// - `flag`   (Bool):    [true, false, null]
fn build_typed_df() -> DataFrame {
    let mut df = DataFrame::new();
    df.add_column(DataFrame::new_float64_column(
        "price",
        vec![Some(10.0), None, Some(1.5)],
    ));
    df.add_column(DataFrame::new_string_column(
        "code",
        vec![Some("a"), Some("b"), None],
    ));
    df.add_column(DataFrame::new_int64_column(
        "qty",
        vec![Some(1), None, Some(3)],
    ));
    df.add_column(DataFrame::new_bool_column(
        "flag",
        vec![Some(true), Some(false), None],
    ));
    df
}

#[test]
fn float_column_to_strings() {
    let df = build_typed_df();
    let col = df.column("price").unwrap();
    assert_eq!(column_value_to_string(col, 0), Some("10".to_string()));
    assert_eq!(column_value_to_string(col, 1), None);
    assert_eq!(column_value_to_string(col, 2), Some("1.5".to_string()));
}

#[test]
fn int_column_to_strings() {
    let df = build_typed_df();
    let col = df.column("qty").unwrap();
    assert_eq!(column_value_to_string(col, 0), Some("1".to_string()));
    assert_eq!(column_value_to_string(col, 1), None);
    assert_eq!(column_value_to_string(col, 2), Some("3".to_string()));
}

#[test]
fn string_column_passthrough() {
    let df = build_typed_df();
    let col = df.column("code").unwrap();
    assert_eq!(column_value_to_string(col, 0), Some("a".to_string()));
    assert_eq!(column_value_to_string(col, 1), Some("b".to_string()));
    assert_eq!(column_value_to_string(col, 2), None);
}

#[test]
fn bool_column_to_strings() {
    let df = build_typed_df();
    let col = df.column("flag").unwrap();
    assert_eq!(column_value_to_string(col, 0), Some("true".to_string()));
    assert_eq!(column_value_to_string(col, 1), Some("false".to_string()));
    assert_eq!(column_value_to_string(col, 2), None);
}

#[test]
fn pull_skips_nulls_and_exhausts() {
    let df = build_typed_df();
    let col_idx = find_column_index(&df, "price").unwrap();
    let mut state = ColumnStream {
        df,
        col_idx,
        cursor: 0,
        null_skipped: 0,
        done_logged: false,
    };

    // price = [10.0, null, 1.5] → 只产出 2 个字符串，中间 null 被跳过
    assert_eq!(state.pull(), Some("10".to_string()));
    assert_eq!(state.null_skipped, 0);
    assert_eq!(state.pull(), Some("1.5".to_string()));
    assert_eq!(state.null_skipped, 1);
    assert_eq!(state.pull(), None);
    assert_eq!(state.pull(), None);
}

#[test]
fn stringify_column_preserves_nulls() {
    let df = build_typed_df();
    let col_idx = find_column_index(&df, "code").unwrap();
    let values = stringify_column(&df, col_idx);
    assert_eq!(
        values,
        vec![Some("a".to_string()), Some("b".to_string()), None]
    );
}

#[test]
fn find_missing_column_reports_existing() {
    let df = build_typed_df();
    let err = find_column_index(&df, "missing").unwrap_err();
    assert!(err.contains("missing"));
    assert!(err.contains("price"));
    assert!(err.contains("code"));
}

#[test]
fn resolve_column_name_trims_and_rejects_empty() {
    assert_eq!(resolve_column_name(" close "), Some("close"));
    assert_eq!(resolve_column_name("   "), None);
    assert_eq!(resolve_column_name(""), None);
}

#[test]
fn null_type_column_always_none() {
    let col = ColumnData::new("nothing".to_string(), DataType::Null);
    assert_eq!(column_value_to_string(&col, 0), None);
}
