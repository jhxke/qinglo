use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c_owned, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};

// =============================================================================
// 参数解析
// =============================================================================

/// 收益率直方图算子参数结构体
///
/// - `expression`: 筛选表达式，如 `ma5 > ma10`；只有表达式为真的行才会被统计
/// - `value_column`: 要统计的值列名，通常是未来收益率列（如 `future_return_5`）
/// - `bins`: 直方图分箱数量（字符串形式），空串回退默认 20；必须为正整数
/// - `min_val`: 可选的最小值边界（字符串形式），空串表示自动取数据最小值
/// - `max_val`: 可选的最大值边界（字符串形式），空串表示自动取数据最大值
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ReturnHistogramParams {
    #[serde(default)]
    pub expression: String,
    #[serde(default)]
    pub value_column: String,
    #[serde(default)]
    pub bins: String,
    #[serde(default)]
    pub min_val: String,
    #[serde(default)]
    pub max_val: String,
}

fn parse_params(params_json: &str) -> ReturnHistogramParams {
    if params_json.is_empty() {
        return ReturnHistogramParams::default();
    }
    match serde_json::from_str::<ReturnHistogramParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("收益率直方图算子: 解析参数 JSON 失败: {}", e);
            ReturnHistogramParams::default()
        }
    }
}

fn parse_bins(raw: &str) -> Option<usize> {
    let t = raw.trim();
    if t.is_empty() {
        return Some(20);
    }
    match t.parse::<usize>() {
        Ok(v) if v >= 1 => Some(v),
        _ => None,
    }
}

fn parse_f64_opt(raw: &str) -> Option<f64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok()
}

// =============================================================================
// 词法分析 (Lexer) — 复用 expression_operator 的实现
// =============================================================================

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Gt,
    Lt,
    Ge,
    Le,
    Eq,
    Ne,
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    let n = chars.len();
    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '+' => { tokens.push(Token::Plus); i += 1; }
            '-' => { tokens.push(Token::Minus); i += 1; }
            '*' => { tokens.push(Token::Star); i += 1; }
            '/' => { tokens.push(Token::Slash); i += 1; }
            '(' => { tokens.push(Token::LParen); i += 1; }
            ')' => { tokens.push(Token::RParen); i += 1; }
            '>' => {
                if i + 1 < n && chars[i + 1] == '=' { tokens.push(Token::Ge); i += 2; }
                else { tokens.push(Token::Gt); i += 1; }
            }
            '<' => {
                if i + 1 < n && chars[i + 1] == '=' { tokens.push(Token::Le); i += 2; }
                else { tokens.push(Token::Lt); i += 1; }
            }
            '=' => {
                if i + 1 < n && chars[i + 1] == '=' { tokens.push(Token::Eq); i += 2; }
                else { return Err(format!("非法字符 '=' (应为 '==') 于位置 {}", i)); }
            }
            '!' => {
                if i + 1 < n && chars[i + 1] == '=' { tokens.push(Token::Ne); i += 2; }
                else { tokens.push(Token::Not); i += 1; }
            }
            '&' => {
                if i + 1 < n && chars[i + 1] == '&' { tokens.push(Token::And); i += 2; }
                else { return Err(format!("非法字符 '&' (应为 '&&') 于位置 {}", i)); }
            }
            '|' => {
                if i + 1 < n && chars[i + 1] == '|' { tokens.push(Token::Or); i += 2; }
                else { return Err(format!("非法字符 '|' (应为 '||') 于位置 {}", i)); }
            }
            d if d.is_ascii_digit() || d == '.' => {
                let start = i;
                while i < n && (chars[i].is_ascii_digit() || chars[i] == '.') { i += 1; }
                if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                    i += 1;
                    if i < n && (chars[i] == '+' || chars[i] == '-') { i += 1; }
                    while i < n && chars[i].is_ascii_digit() { i += 1; }
                }
                let s: String = chars[start..i].iter().collect();
                match s.parse::<f64>() {
                    Ok(v) => tokens.push(Token::Number(v)),
                    Err(_) => return Err(format!("无法解析数字 '{}' 于位置 {}", s, start)),
                }
            }
            a if a.is_alphabetic() || a == '_' => {
                let start = i;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') { i += 1; }
                let s: String = chars[start..i].iter().collect();
                tokens.push(Token::Ident(s));
            }
            _ => return Err(format!("非法字符 '{}' 于位置 {}", c, i)),
        }
    }
    Ok(tokens)
}

// =============================================================================
// 语法分析 (Parser) — 递归下降
// =============================================================================

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Number(f64),
    Column(String),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BinOp {
    Add, Sub, Mul, Div,
    Gt, Lt, Ge, Le, Eq, Ne,
    And, Or,
}

struct Parser { tokens: Vec<Token>, pos: usize }

impl Parser {
    fn new(tokens: Vec<Token>) -> Self { Parser { tokens, pos: 0 } }
    fn peek(&self) -> Option<&Token> { self.tokens.get(self.pos) }
    fn advance(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() { self.pos += 1; }
        t
    }
    fn parse(&mut self) -> Result<Expr, String> {
        let e = self.parse_or()?;
        if self.pos < self.tokens.len() {
            return Err(format!("表达式末尾存在多余 Token: {:?}", self.tokens[self.pos]));
        }
        Ok(e)
    }
    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_cmp()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.advance();
            let right = self.parse_cmp()?;
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }
    fn parse_cmp(&mut self) -> Result<Expr, String> {
        let left = self.parse_add()?;
        let op = match self.peek() {
            Some(Token::Gt) => Some(BinOp::Gt),
            Some(Token::Lt) => Some(BinOp::Lt),
            Some(Token::Ge) => Some(BinOp::Ge),
            Some(Token::Le) => Some(BinOp::Le),
            Some(Token::Eq) => Some(BinOp::Eq),
            Some(Token::Ne) => Some(BinOp::Ne),
            _ => None,
        };
        if let Some(op) = op {
            self.advance();
            let right = self.parse_add()?;
            if matches!(self.peek(),
                Some(Token::Gt) | Some(Token::Lt) | Some(Token::Ge) |
                Some(Token::Le) | Some(Token::Eq) | Some(Token::Ne))
            {
                return Err("比较运算符不可连续使用，请用括号或逻辑运算符组合".to_string());
            }
            return Ok(Expr::Binary(op, Box::new(left), Box::new(right)));
        }
        Ok(left)
    }
    fn parse_add(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_mul()?;
        loop {
            match self.peek() {
                Some(Token::Plus) => {
                    self.advance();
                    let r = self.parse_mul()?;
                    left = Expr::Binary(BinOp::Add, Box::new(left), Box::new(r));
                }
                Some(Token::Minus) => {
                    self.advance();
                    let r = self.parse_mul()?;
                    left = Expr::Binary(BinOp::Sub, Box::new(left), Box::new(r));
                }
                _ => break,
            }
        }
        Ok(left)
    }
    fn parse_mul(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Token::Star) => {
                    self.advance();
                    let r = self.parse_unary()?;
                    left = Expr::Binary(BinOp::Mul, Box::new(left), Box::new(r));
                }
                Some(Token::Slash) => {
                    self.advance();
                    let r = self.parse_unary()?;
                    left = Expr::Binary(BinOp::Div, Box::new(left), Box::new(r));
                }
                _ => break,
            }
        }
        Ok(left)
    }
    fn parse_unary(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Some(Token::Minus) => {
                self.advance();
                let e = self.parse_unary()?;
                Ok(Expr::Neg(Box::new(e)))
            }
            Some(Token::Not) => {
                self.advance();
                let e = self.parse_unary()?;
                Ok(Expr::Not(Box::new(e)))
            }
            _ => self.parse_primary(),
        }
    }
    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.advance() {
            Some(Token::Number(n)) => Ok(Expr::Number(n)),
            Some(Token::Ident(s)) => Ok(Expr::Column(s)),
            Some(Token::LParen) => {
                let e = self.parse_or()?;
                match self.advance() {
                    Some(Token::RParen) => Ok(e),
                    other => Err(format!("期望 ')' 但得到 {:?}", other)),
                }
            }
            other => Err(format!("期望 数字 / 列名 / '(' 但得到 {:?}", other)),
        }
    }
}

fn parse_expression(expr: &str) -> Result<Expr, String> {
    let tokens = tokenize(expr)?;
    if tokens.is_empty() { return Err("表达式为空".to_string()); }
    Parser::new(tokens).parse()
}

// =============================================================================
// 求值 (Evaluator)
// =============================================================================

fn collect_columns(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Number(_) => {}
        Expr::Column(name) => {
            if !out.contains(name) { out.push(name.clone()); }
        }
        Expr::Neg(e) | Expr::Not(e) => collect_columns(e, out),
        Expr::Binary(_, a, b) => {
            collect_columns(a, out);
            collect_columns(b, out);
        }
    }
}

fn apply_binop(op: BinOp, a: Option<f64>, b: Option<f64>) -> Option<f64> {
    use BinOp::*;
    match op {
        Add => Some(a? + b?),
        Sub => Some(a? - b?),
        Mul => Some(a? * b?),
        Div => {
            let bv = b?;
            if bv == 0.0 { None } else { Some(a? / bv) }
        }
        Gt => match (a, b) { (Some(x), Some(y)) => Some(if x > y { 1.0 } else { 0.0 }), _ => None },
        Lt => match (a, b) { (Some(x), Some(y)) => Some(if x < y { 1.0 } else { 0.0 }), _ => None },
        Ge => match (a, b) { (Some(x), Some(y)) => Some(if x >= y { 1.0 } else { 0.0 }), _ => None },
        Le => match (a, b) { (Some(x), Some(y)) => Some(if x <= y { 1.0 } else { 0.0 }), _ => None },
        Eq => match (a, b) { (Some(x), Some(y)) => Some(if x == y { 1.0 } else { 0.0 }), _ => None },
        Ne => match (a, b) { (Some(x), Some(y)) => Some(if x != y { 1.0 } else { 0.0 }), _ => None },
        And => match (a, b) {
            (Some(0.0), _) | (_, Some(0.0)) => Some(0.0),
            (Some(_), Some(_)) => Some(1.0),
            _ => None,
        },
        Or => match (a, b) {
            (Some(x), _) if x != 0.0 => Some(1.0),
            (_, Some(y)) if y != 0.0 => Some(1.0),
            (Some(0.0), Some(0.0)) => Some(0.0),
            _ => None,
        },
    }
}

fn evaluate(expr: &Expr, row: usize, columns: &HashMap<String, Vec<Option<f64>>>) -> Option<f64> {
    match expr {
        Expr::Number(n) => Some(*n),
        Expr::Column(name) => columns
            .get(name)
            .and_then(|v| v.get(row))
            .copied()
            .flatten(),
        Expr::Neg(e) => evaluate(e, row, columns).map(|v| -v),
        Expr::Not(e) => match evaluate(e, row, columns) {
            None => None,
            Some(v) => Some(if v == 0.0 { 1.0 } else { 0.0 }),
        },
        Expr::Binary(op, a, b) => {
            let va = evaluate(a, row, columns);
            let vb = evaluate(b, row, columns);
            apply_binop(*op, va, vb)
        }
    }
}

// =============================================================================
// DataFrame 适配
// =============================================================================

fn extract_column_as_f64(df: &DataFrame, name: &str) -> Option<Vec<Option<f64>>> {
    let col = df.column(name)?;
    match col.data_type {
        DataType::Float64 => Some(col.to_f64_vec()),
        DataType::Int64 => Some(
            col.to_i64_vec()
                .into_iter()
                .map(|v| v.map(|x| x as f64))
                .collect(),
        ),
        _ => None,
    }
}

// =============================================================================
// 直方图统计
// =============================================================================

/// 从所有 DataFrame 中收集满足表达式条件的值
fn collect_filtered_values(
    input_dfs: &[DataFrame],
    ast: &Expr,
    value_col_name: &str,
) -> Result<Vec<f64>, String> {
    let mut all_values: Vec<f64> = Vec::new();

    // 收集表达式引用的列名
    let mut referenced_cols = Vec::new();
    collect_columns(ast, &mut referenced_cols);
    // 值列也需要提取
    let mut all_needed_cols = referenced_cols.clone();
    if !all_needed_cols.iter().any(|c| c == value_col_name) {
        all_needed_cols.push(value_col_name.to_string());
    }

    for (df_idx, df) in input_dfs.iter().enumerate() {
        if df.row_count == 0 {
            continue;
        }

        // 提取所有需要的列
        let mut col_data: HashMap<String, Vec<Option<f64>>> = HashMap::new();
        for name in &all_needed_cols {
            match extract_column_as_f64(df, name) {
                Some(vals) => {
                    col_data.insert(name.clone(), vals);
                }
                None => {
                    return Err(format!(
                        "第 {} 个 DataFrame 中引用列 '{}' 不存在或类型不支持 (现有列: {:?})",
                        df_idx,
                        name,
                        df.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
                    ));
                }
            }
        }

        // 逐行判断表达式是否为 true，若是则收集值列对应的值
        let row_count = df.row_count;
        for row in 0..row_count {
            // 判断表达式是否为真
            let expr_true = match evaluate(ast, row, &col_data) {
                None => false,
                Some(v) => v != 0.0,
            };
            if !expr_true {
                continue;
            }
            // 取值列同一行的值
            if let Some(val_col) = col_data.get(value_col_name) {
                if let Some(Some(v)) = val_col.get(row) {
                    if v.is_finite() {
                        all_values.push(*v);
                    }
                }
            }
        }
    }

    Ok(all_values)
}

/// 构建直方图 DataFrame（以 0 为中心向两边对称分箱）
///
/// 分箱策略：
/// - 负值部分：从 0 向左分 `neg_bins` 个箱，范围 `[-neg_bins*bin_width, 0)`
/// - 正值部分：从 0 向右分 `pos_bins` 个箱，范围 `[0, pos_bins*bin_width)`
/// - 两侧使用相同的 `bin_width`，保证刻度对称
/// - bin_index 顺序：先负值箱（最靠左开始），后正值箱（从 0 向右）
/// - 新增列 `sign` (Int64)：-1 表示负值侧，1 表示正值侧（0 值归到正值侧第一个箱，sign=1）
///
/// 列: bin_index(Int64), bin_left(Float64), bin_right(Float64),
///     bin_center(Float64), count(Int64), frequency(Float64), sign(Int64)
fn build_histogram_dataframe(
    values: &[f64],
    bins: usize,
    min_opt: Option<f64>,
    max_opt: Option<f64>,
) -> DataFrame {
    let mut df = DataFrame::new();

    // ---------- Step 1: 决定负值/正值各自的分箱数 ----------
    // bins 拆分：偶数则平分；奇数则正值侧多一个（0 归到正值侧）
    let neg_bins = bins / 2;
    let pos_bins = bins - neg_bins;
    let total_bins = neg_bins + pos_bins; // 等于 bins

    // ---------- Step 2: 决定分箱宽度 bin_width（以 0 为中心对称） ----------
    // 如果用户显式指定了 min/max，取两者绝对值的较大者作为半区间宽度的参考
    let (abs_data_min, abs_data_max) = if values.is_empty() {
        (1.0, 1.0)
    } else {
        let mn = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let mx = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        (mn.abs().max(1e-9), mx.abs().max(1e-9))
    };

    // 用户显式边界的绝对值覆盖
    let user_abs_min = min_opt.map(|v| v.abs().max(1e-9));
    let user_abs_max = max_opt.map(|v| v.abs().max(1e-9));
    let abs_lo = user_abs_min.unwrap_or(abs_data_min);
    let abs_hi = user_abs_max.unwrap_or(abs_data_max);
    let abs_bound = abs_lo.max(abs_hi);

    // 各侧需要覆盖的绝对值范围 = abs_bound（若数据全正/全负，仍然对称留空另一侧）
    let side_range = if abs_bound <= 0.0 { 1.0 } else { abs_bound };

    // 计算 bin_width：使用单侧分箱数多的那一方（保证刚好覆盖 abs_bound）
    let side_bins_max = neg_bins.max(pos_bins).max(1);
    let bin_width = side_range / side_bins_max as f64;
    let bin_width = if bin_width <= 0.0 { 1e-9 } else { bin_width };

    // 实际正负范围（以 0 为中心对称）
    let neg_total = neg_bins as f64 * bin_width; // 负值侧总宽度
    let pos_total = pos_bins as f64 * bin_width; // 正值侧总宽度
    let range_min = -neg_total;
    let range_max = pos_total;

    let total_count = values.len() as f64;

    // ---------- Step 3: 计数 ----------
    let mut counts: Vec<i64> = vec![0; total_bins];

    if !values.is_empty() && bin_width > 0.0 {
        for &v in values {
            if v < range_min || v > range_max {
                continue;
            }
            let idx = if v < 0.0 {
                // 负值侧：[range_min, 0) → idx 0..neg_bins
                // 第 i 个负值箱 (0-indexed): [ -neg_total + i*bin_width, -neg_total + (i+1)*bin_width )
                // 简化：从 0 向左看，v 在 [-k*bin_width, -(k-1)*bin_width) → idx = neg_bins - k
                let from_zero = (-v).min(neg_total);
                let k = (from_zero / bin_width).floor() as usize;
                // k=0 → 最接近 0 的负值箱 idx = neg_bins - 1
                // k=neg_bins-1 → 最左侧负值箱 idx = 0
                let k = k.min(neg_bins.saturating_sub(1));
                neg_bins - 1 - k
            } else {
                // 正值侧：[0, range_max] → idx neg_bins..total_bins-1
                let from_zero = v.min(pos_total);
                let mut k = (from_zero / bin_width).floor() as usize;
                if k >= pos_bins {
                    k = pos_bins - 1;
                }
                neg_bins + k
            };
            if idx < total_bins {
                counts[idx] += 1;
            }
        }
    }

    // ---------- Step 4: 构造各列 ----------
    let mut bin_index_col: Vec<Option<i64>> = Vec::with_capacity(total_bins);
    let mut bin_left_col: Vec<Option<f64>> = Vec::with_capacity(total_bins);
    let mut bin_right_col: Vec<Option<f64>> = Vec::with_capacity(total_bins);
    let mut bin_center_col: Vec<Option<f64>> = Vec::with_capacity(total_bins);
    let mut count_col: Vec<Option<i64>> = Vec::with_capacity(total_bins);
    let mut freq_col: Vec<Option<f64>> = Vec::with_capacity(total_bins);
    let mut sign_col: Vec<Option<i64>> = Vec::with_capacity(total_bins);

    // 负值箱：先从最左 (range_min) 开始，idx 0..neg_bins
    for i in 0..neg_bins {
        let left = range_min + (i as f64) * bin_width;
        let right = left + bin_width;
        let center = (left + right) / 2.0;
        let cnt = counts[i];
        let freq = if total_count > 0.0 { cnt as f64 / total_count } else { 0.0 };

        bin_index_col.push(Some(i as i64));
        bin_left_col.push(Some(left));
        bin_right_col.push(Some(right));
        bin_center_col.push(Some(center));
        count_col.push(Some(cnt));
        freq_col.push(Some(freq));
        sign_col.push(Some(-1));
    }
    // 正值箱：从 0 开始，idx neg_bins..total_bins-1
    for j in 0..pos_bins {
        let i = neg_bins + j;
        let left = 0.0 + (j as f64) * bin_width;
        let right = left + bin_width;
        let center = (left + right) / 2.0;
        let cnt = counts[i];
        let freq = if total_count > 0.0 { cnt as f64 / total_count } else { 0.0 };

        bin_index_col.push(Some(i as i64));
        bin_left_col.push(Some(left));
        bin_right_col.push(Some(right));
        bin_center_col.push(Some(center));
        count_col.push(Some(cnt));
        freq_col.push(Some(freq));
        sign_col.push(Some(1));
    }

    df.add_column(DataFrame::new_int64_column("bin_index", bin_index_col));
    df.add_column(DataFrame::new_float64_column("bin_left", bin_left_col));
    df.add_column(DataFrame::new_float64_column("bin_right", bin_right_col));
    df.add_column(DataFrame::new_float64_column("bin_center", bin_center_col));
    df.add_column(DataFrame::new_int64_column("count", count_col));
    df.add_column(DataFrame::new_float64_column("frequency", freq_col));
    df.add_column(DataFrame::new_int64_column("sign", sign_col));

    // 用于 debug：打印分箱概览
    println!(
        "收益率直方图算子: 分箱统计(以0为中心) bins={}(neg={},pos={}), bin_width={}, 范围=[{}, {}]",
        bins, neg_bins, pos_bins, bin_width, range_min, range_max
    );

    df
}

// =============================================================================
// C ABI 入口
// =============================================================================

/// 收益率直方图算子的执行函数（C ABI）
///
/// 输入: DataFrameArray
/// 流程: 对每个 DataFrame 逐行评估表达式；表达式为 true 的行，取 value_column
///       列的同一行值；将所有 DataFrame 中收集到的值汇总，生成直方图 DataFrame。
/// 输出: 单个 DataFrame（直方图），包含列: bin_index, bin_left, bin_right,
///       bin_center, count, frequency
///
/// 返回值:
/// - 0:  成功
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入 DataFrame 数组为空
/// - -6: 参数非法（expression 空 / bins 非正整数 / value_column 空）
/// - -7: 表达式解析失败（词法/语法错误）
/// - -8: 求值失败（引用列不存在或类型不支持）
#[no_mangle]
pub extern "C" fn execute_operator(
    inputs: *const CPortData,
    input_count: usize,
    outputs: *mut CPortData,
    output_cap: usize,
    params_json: *const c_char,
) -> i32 {
    if let Err(e) = ensure_runtime_loaded() {
        let err_msg = format!("{}", e);
        let c_msg = CString::new(err_msg.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("收益率直方图算子: {}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_json_str);

    // 校验参数
    let expr_trimmed = params.expression.trim();
    if expr_trimmed.is_empty() {
        let err = "expression 参数为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("收益率直方图算子: {}", err);
        return -6;
    }

    let value_col = params.value_column.trim();
    if value_col.is_empty() {
        let err = "value_column 参数为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("收益率直方图算子: {}", err);
        return -6;
    }

    let bins = match parse_bins(&params.bins) {
        Some(v) => v,
        None => {
            let err = format!("bins='{}' 非法 (需为正整数，空串默认 20)", params.bins);
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("收益率直方图算子: {}", err);
            return -6;
        }
    };

    let min_opt = parse_f64_opt(&params.min_val);
    let max_opt = parse_f64_opt(&params.max_val);

    let ast = match parse_expression(expr_trimmed) {
        Ok(a) => a,
        Err(e) => {
            let err = format!("表达式解析失败: {}", e);
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("收益率直方图算子: {}", err);
            return -7;
        }
    };

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("收益率直方图算子: {}", err);
        return -3;
    }

    let input_pd = unsafe { portdata_from_c(inputs as *mut CPortData) };
    let input_dfs: Vec<DataFrame> = match input_pd {
        PortData::DataFrame(df) => vec![df],
        PortData::DataFrameArray(dfs) => dfs,
        _ => {
            let err = "输入不是 DataFrame / DataFrameArray 类型".to_string();
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("收益率直方图算子: {}", err);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("收益率直方图算子: {}", err);
        return -5;
    }

    println!(
        "收益率直方图算子: expression='{}', value_column='{}', bins={}, min={:?}, max={:?}, 输入 DataFrame 数量={}",
        params.expression, value_col, bins, min_opt, max_opt, input_dfs.len()
    );

    // 收集满足条件的所有值
    let values = match collect_filtered_values(&input_dfs, &ast, value_col) {
        Ok(v) => v,
        Err(e) => {
            let err = format!("收集数据失败: {}", e);
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("收益率直方图算子: {}", err);
            return -8;
        }
    };

    println!(
        "收益率直方图算子: 收集到 {} 个有效样本值 (范围 {:?})",
        values.len(),
        if values.is_empty() {
            None
        } else {
            Some((
                values.iter().cloned().fold(f64::INFINITY, f64::min),
                values.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
            ))
        }
    );

    // 构建直方图 DataFrame
    let histogram_df = build_histogram_dataframe(&values, bins, min_opt, max_opt);

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    // 输出为单个 DataFrame
    let port_data = PortData::DataFrame(histogram_df);
    if !outputs.is_null() && output_cap > 0 {
        let c_pd = portdata_to_c_owned(port_data);
        unsafe {
            *outputs = c_pd;
            if output_cap > 1 {
                *outputs.add(1) = CPortData {
                    type_tag: TYPE_NULL,
                    value: CPortValue { str_ptr: std::ptr::null_mut() },
                };
            }
        }
    }

    0
}

#[no_mangle]
pub extern "C" fn release_port_data(data_ptr: *mut CPortData) {
    if !data_ptr.is_null() {
        operator_runtime::c_abi::c_pd_free(data_ptr);
    }
}

#[no_mangle]
pub extern "C" fn return_histogram_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
