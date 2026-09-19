use super::*;

/// 参数解析：空串/非法 JSON 回落到默认值，缺字段走 serde 默认值。
#[test]
fn test_parse_params_defaults() {
    let p = parse_params("");
    assert_eq!(p.host, "localhost");
    assert_eq!(p.port, 5432);
    assert_eq!(p.database, "whatigo");
    assert!(p.query.is_empty());
    assert!(!p.sort_descending);

    let p = parse_params("{ not json");
    assert_eq!(p.port, 5432);

    let p = parse_params(r#"{"host": "10.0.0.1", "query": "SELECT 1"}"#);
    assert_eq!(p.host, "10.0.0.1");
    assert_eq!(p.port, 5432); // 缺省字段
    assert_eq!(p.query, "SELECT 1");
}

/// `${input}` 被原文替换为上游字符串值。
#[test]
fn test_render_sql_basic_substitution() {
    let mut unk = HashSet::new();
    let sql = render_sql(
        "SELECT * FROM t WHERE code = '${input}' LIMIT 10",
        "000001.SZ",
        &mut unk,
    );
    assert_eq!(sql, "SELECT * FROM t WHERE code = '000001.SZ' LIMIT 10");

    // 同一条模板可反复渲染（流式中每个 chunk 渲染一次）
    let sql2 = render_sql(&sql_before(), "600519.SH", &mut unk);
    assert_eq!(sql2, "SELECT * FROM t WHERE code = '600519.SH'");
}

fn sql_before() -> String {
    "SELECT * FROM t WHERE code = '${input}'".to_string()
}

/// 多个 `${input}` 占位符全部替换；无占位符时原样返回。
#[test]
fn test_render_sql_multiple_and_none() {
    let mut unk = HashSet::new();
    let sql = render_sql(
        "SELECT * FROM t WHERE a = '${input}' OR b = '${input}'",
        "x",
        &mut unk,
    );
    assert_eq!(sql, "SELECT * FROM t WHERE a = 'x' OR b = 'x'");

    let plain = "SELECT 1";
    assert_eq!(render_sql(plain, "x", &mut unk), plain);
}

/// 值为空字符串时替换为空串（模板引号保留）。
#[test]
fn test_render_sql_empty_value() {
    let mut unk = HashSet::new();
    let sql = render_sql("code='${input}'", "", &mut unk);
    assert_eq!(sql, "code=''");
}

/// 未知变量 `${xxx}` 原样保留，且不影响 `${input}` 替换。
#[test]
fn test_render_sql_unknown_var_kept() {
    let mut unk = HashSet::new();
    let sql = render_sql(
        "SELECT * FROM t WHERE a = '${input}' AND b = '${other}'",
        "v",
        &mut unk,
    );
    assert_eq!(
        sql,
        "SELECT * FROM t WHERE a = 'v' AND b = '${other}'"
    );
    assert!(unk.contains("other"));
}

/// 语法不完整/变量名非法的 `${...}` 按普通文本保留，不 panic。
#[test]
fn test_render_sql_malformed_kept_literal() {
    let mut unk = HashSet::new();
    assert_eq!(render_sql("a $ b", "v", &mut unk), "a $ b");
    assert_eq!(render_sql("a ${x", "v", &mut unk), "a ${x");
    assert_eq!(
        render_sql("WHERE c = '${input}' AND x = ${a b}", "v", &mut unk),
        "WHERE c = 'v' AND x = ${a b}"
    );
    // 变量名不能为空：${} 原样保留
    assert_eq!(render_sql("${}", "v", &mut unk), "${}");
}
