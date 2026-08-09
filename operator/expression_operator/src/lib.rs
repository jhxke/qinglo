use operator_executor_client::ensure_runtime_loaded;
use operator_runtime::c_abi::{
    c_set_last_error, portdata_from_c, portdata_to_c, CPortData, CPortValue, TYPE_NULL,
};
use operator_runtime::{DataFrame, DataType, PortData};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::{c_char, CStr, CString};

/// 表达式算子参数结构体
///
/// - `column_name`：结果写入的列名；为空时回退为 `signal`。
///   若该列已存在则覆盖（无论原类型），否则追加为新的 Float64 列。
/// - `expression`：待计算的表达式，如 `ma5 > ma10`。为空时算子报错。
#[derive(Debug, Serialize, Deserialize, Default)]
pub struct ExpressionParams {
    #[serde(default)]
    pub column_name: String,
    #[serde(default)]
    pub expression: String,
}

/// 解析参数 JSON 为 ExpressionParams；空串或非法 JSON 返回默认值
fn parse_params(params_json: &str) -> ExpressionParams {
    if params_json.is_empty() {
        return ExpressionParams::default();
    }
    match serde_json::from_str::<ExpressionParams>(params_json) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("表达式算子: 解析参数 JSON 失败: {}", e);
            ExpressionParams::default()
        }
    }
}

// =============================================================================
// 词法分析 (Lexer)
// =============================================================================

/// 表达式 Token
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
    LBracket,
    RBracket,
}

/// 将表达式字符串切分为 Token 序列
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
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            '*' => {
                tokens.push(Token::Star);
                i += 1;
            }
            '/' => {
                tokens.push(Token::Slash);
                i += 1;
            }
            '(' => {
                tokens.push(Token::LParen);
                i += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                i += 1;
            }
            '[' => {
                tokens.push(Token::LBracket);
                i += 1;
            }
            ']' => {
                tokens.push(Token::RBracket);
                i += 1;
            }
            '>' => {
                if i + 1 < n && chars[i + 1] == '=' {
                    tokens.push(Token::Ge);
                    i += 2;
                } else {
                    tokens.push(Token::Gt);
                    i += 1;
                }
            }
            '<' => {
                if i + 1 < n && chars[i + 1] == '=' {
                    tokens.push(Token::Le);
                    i += 2;
                } else {
                    tokens.push(Token::Lt);
                    i += 1;
                }
            }
            '=' => {
                if i + 1 < n && chars[i + 1] == '=' {
                    tokens.push(Token::Eq);
                    i += 2;
                } else {
                    return Err(format!("非法字符 '=' (应为 '==') 于位置 {}", i));
                }
            }
            '!' => {
                if i + 1 < n && chars[i + 1] == '=' {
                    tokens.push(Token::Ne);
                    i += 2;
                } else {
                    tokens.push(Token::Not);
                    i += 1;
                }
            }
            '&' => {
                if i + 1 < n && chars[i + 1] == '&' {
                    tokens.push(Token::And);
                    i += 2;
                } else {
                    return Err(format!("非法字符 '&' (应为 '&&') 于位置 {}", i));
                }
            }
            '|' => {
                if i + 1 < n && chars[i + 1] == '|' {
                    tokens.push(Token::Or);
                    i += 2;
                } else {
                    return Err(format!("非法字符 '|' (应为 '||') 于位置 {}", i));
                }
            }
            d if d.is_ascii_digit() || d == '.' => {
                let start = i;
                // 整数 / 小数部分
                while i < n && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                // 科学计数法
                if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                    i += 1;
                    if i < n && (chars[i] == '+' || chars[i] == '-') {
                        i += 1;
                    }
                    while i < n && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                let s: String = chars[start..i].iter().collect();
                match s.parse::<f64>() {
                    Ok(v) => tokens.push(Token::Number(v)),
                    Err(_) => return Err(format!("无法解析数字 '{}' 于位置 {}", s, start)),
                }
            }
            a if a.is_alphabetic() || a == '_' => {
                let start = i;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
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

/// 表达式 AST
#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Number(f64),
    /// 列引用：(列名, 行偏移量)
    /// - offset = 0  → close 或 close[n]（当前行）
    /// - offset = -1 → close[n-1]（上一行）
    /// - offset = 1  → close[n+1]（下一行）
    Column(String, i32),
    Neg(Box<Expr>),
    Not(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Gt,
    Lt,
    Ge,
    Le,
    Eq,
    Ne,
    And,
    Or,
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn parse(&mut self) -> Result<Expr, String> {
        let e = self.parse_or()?;
        if self.pos < self.tokens.len() {
            return Err(format!(
                "表达式末尾存在多余 Token: {:?}",
                self.tokens[self.pos]
            ));
        }
        Ok(e)
    }

    /// or_expr := and_expr ( "||" and_expr )*
    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Or)) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::Binary(BinOp::Or, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// and_expr := cmp_expr ( "&&" cmp_expr )*
    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_cmp()?;
        while matches!(self.peek(), Some(Token::And)) {
            self.advance();
            let right = self.parse_cmp()?;
            left = Expr::Binary(BinOp::And, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// cmp_expr := add_expr ( ( > | < | >= | <= | == | != ) add_expr )?
    /// 比较为非结合：仅允许一次比较，避免 `a < b < c` 被误解为 `(a<b) < c`
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
            // 若再出现比较运算符则报错（非结合）
            if matches!(
                self.peek(),
                Some(Token::Gt)
                    | Some(Token::Lt)
                    | Some(Token::Ge)
                    | Some(Token::Le)
                    | Some(Token::Eq)
                    | Some(Token::Ne)
            ) {
                return Err(
                    "比较运算符不可连续使用（如 a < b < c），请用括号或逻辑运算符组合".to_string(),
                );
            }
            return Ok(Expr::Binary(op, Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    /// add_expr := mul_expr ( ( "+" | "-" ) mul_expr )*
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

    /// mul_expr := unary ( ( "*" | "/" ) unary )*
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

    /// unary := ( "-" | "!" ) unary | primary
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

    /// 解析列偏移表达式 [n] / [n-k] / [n+k]，返回偏移量 i32
    /// 前提：已消费 '[' Token，当前 pos 指向 'n'
    fn parse_column_offset(&mut self) -> Result<i32, String> {
        // 必须以 'n' 开头（严格匹配小写）
        match self.advance() {
            Some(Token::Ident(ref s)) if s == "n" => {}
            other => return Err(format!("偏移表达式必须以 'n' 开头，得到 {:?}", other)),
        }
        // 接下来可能是 ']'（[n]），或 '+/-' 数字后接 ']'（[n±k]）
        let offset: i32 = match self.peek() {
            Some(Token::RBracket) => 0,
            Some(Token::Minus) => {
                self.advance(); // 消费 '-'
                match self.advance() {
                    Some(Token::Number(v)) => {
                        if v.fract() != 0.0 || v < 0.0 {
                            return Err(format!(
                                "偏移表达式仅支持非负整数常数，得到 {}",
                                v
                            ));
                        }
                        -(v as i32)
                    }
                    other => {
                        return Err(format!(
                            "偏移表达式 '-' 后应为非负整数常数，得到 {:?}",
                            other
                        ))
                    }
                }
            }
            Some(Token::Plus) => {
                self.advance(); // 消费 '+'
                match self.advance() {
                    Some(Token::Number(v)) => {
                        if v.fract() != 0.0 || v < 0.0 {
                            return Err(format!(
                                "偏移表达式仅支持非负整数常数，得到 {}",
                                v
                            ));
                        }
                        v as i32
                    }
                    other => {
                        return Err(format!(
                            "偏移表达式 '+' 后应为非负整数常数，得到 {:?}",
                            other
                        ))
                    }
                }
            }
            other => {
                return Err(format!(
                    "偏移表达式 'n' 后应为 ']' 或 '+/-' 整数，得到 {:?}",
                    other
                ))
            }
        };
        // 消费结束的 ']'
        match self.advance() {
            Some(Token::RBracket) => Ok(offset),
            other => Err(format!("偏移表达式期望 ']'，得到 {:?}", other)),
        }
    }

    /// primary := number | ident ( '[' n (('+'|'-') number)? ']' )? | "(" expr ")"
    fn parse_primary(&mut self) -> Result<Expr, String> {
        match self.advance() {
            Some(Token::Number(n)) => Ok(Expr::Number(n)),
            Some(Token::Ident(s)) => {
                // 列名后可能紧跟 [n±k] 偏移表达式
                let offset = if matches!(self.peek(), Some(Token::LBracket)) {
                    self.advance(); // 消费 '['
                    self.parse_column_offset()?
                } else {
                    0
                };
                Ok(Expr::Column(s, offset))
            }
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

/// 解析表达式字符串为 AST
fn parse_expression(expr: &str) -> Result<Expr, String> {
    let tokens = tokenize(expr)?;
    if tokens.is_empty() {
        return Err("表达式为空".to_string());
    }
    Parser::new(tokens).parse()
}

// =============================================================================
// 求值 (Evaluator)
// =============================================================================

/// 遍历 AST 收集所有引用的列名（去重，保持首次出现顺序）
fn collect_columns(expr: &Expr, out: &mut Vec<String>) {
    match expr {
        Expr::Number(_) => {}
        Expr::Column(name, _offset) => {
            if !out.contains(name) {
                out.push(name.clone());
            }
        }
        Expr::Neg(e) | Expr::Not(e) => collect_columns(e, out),
        Expr::Binary(_, a, b) => {
            collect_columns(a, out);
            collect_columns(b, out);
        }
    }
}

/// 应用二元运算。None 表示操作数为空或结果不可计算（空值传播）。
fn apply_binop(op: BinOp, a: Option<f64>, b: Option<f64>) -> Option<f64> {
    use BinOp::*;
    match op {
        Add => Some(a? + b?),
        Sub => Some(a? - b?),
        Mul => Some(a? * b?),
        Div => {
            let bv = b?;
            if bv == 0.0 {
                None
            } else {
                Some(a? / bv)
            }
        }
        Gt => match (a, b) {
            (Some(x), Some(y)) => Some(if x > y { 1.0 } else { 0.0 }),
            _ => None,
        },
        Lt => match (a, b) {
            (Some(x), Some(y)) => Some(if x < y { 1.0 } else { 0.0 }),
            _ => None,
        },
        Ge => match (a, b) {
            (Some(x), Some(y)) => Some(if x >= y { 1.0 } else { 0.0 }),
            _ => None,
        },
        Le => match (a, b) {
            (Some(x), Some(y)) => Some(if x <= y { 1.0 } else { 0.0 }),
            _ => None,
        },
        Eq => match (a, b) {
            (Some(x), Some(y)) => Some(if x == y { 1.0 } else { 0.0 }),
            _ => None,
        },
        Ne => match (a, b) {
            (Some(x), Some(y)) => Some(if x != y { 1.0 } else { 0.0 }),
            _ => None,
        },
        // 三值逻辑：明确为 0 → 0；明确均非零 → 1；含空且无 0 → None
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

/// 在指定行号评估表达式的值，返回 Option<f64>（None 表示空值/不可计算）
fn evaluate(expr: &Expr, row: usize, columns: &HashMap<String, Vec<Option<f64>>>) -> Option<f64> {
    match expr {
        Expr::Number(n) => Some(*n),
        Expr::Column(name, offset) => {
            let col = columns.get(name)?;
            let len = col.len() as i64;
            // 按 offset 调整行号；越界返回 None（空值传播）
            let adjusted = (row as i64) + (*offset as i64);
            if adjusted < 0 || adjusted >= len {
                None
            } else {
                col.get(adjusted as usize).copied().flatten()
            }
        }
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

/// 从 DataFrame 中提取列的 f64 数据；支持 Float64 与 Int64，其他类型/不存在返回 None
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

/// 设置/替换 DataFrame 中的 Float64 列。列已存在则覆盖，否则追加。
fn set_float64_column(df: &mut DataFrame, name: &str, values: Vec<Option<f64>>) {
    let new_col = DataFrame::new_float64_column(name, values);
    if let Some(pos) = df.columns.iter().position(|c| c.name == name) {
        df.columns[pos] = new_col;
    } else {
        df.add_column(new_col);
    }
}

/// 对单个 DataFrame 应用表达式，返回写入结果列后的新 DataFrame
///
/// - 收集 AST 引用的所有列，从输入 DataFrame 提取为 f64 序列（保留 None 表示空值）
/// - 逐行评估；None（空值/不可计算）→ 0.0，Some(非零) → 1.0，Some(0) → 0.0
/// - 结果写入 `output_col_name`（已存在则覆盖，否则追加）
fn apply_expression(
    input_df: &DataFrame,
    output_col_name: &str,
    ast: &Expr,
) -> Result<DataFrame, String> {
    let mut output_df = input_df.clone();
    let row_count = output_df.row_count;

    // 收集并提取引用列
    let mut col_names = Vec::new();
    collect_columns(ast, &mut col_names);

    let mut col_data: HashMap<String, Vec<Option<f64>>> = HashMap::new();
    for name in &col_names {
        match extract_column_as_f64(input_df, name) {
            Some(vals) => {
                col_data.insert(name.clone(), vals);
            }
            None => {
                return Err(format!(
                    "引用列 '{}' 不存在或类型不支持 (现有列: {:?})",
                    name,
                    input_df
                        .columns
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                ));
            }
        }
    }

    // 逐行评估
    let mut result_values: Vec<Option<f64>> = Vec::with_capacity(row_count);
    for row in 0..row_count {
        let bit = match evaluate(ast, row, &col_data) {
            None => 0.0,
            Some(v) if v != 0.0 => 1.0,
            Some(_) => 0.0,
        };
        result_values.push(Some(bit));
    }

    set_float64_column(&mut output_df, output_col_name, result_values);
    Ok(output_df)
}

// =============================================================================
// C ABI 入口
// =============================================================================

/// 表达式算子的执行函数（C ABI）
///
/// 支持 DataFrameArray 输入：对数组中每一个 DataFrame 独立做表达式求值，
/// 输出同样为 DataFrameArray（顺序与输入一致）。
/// 单个 DataFrame 输入会被包装为单元素数组处理，输出仍为 DataFrameArray。
///
/// 返回值:
/// - 0:  成功
/// - -1: runtime 加载失败
/// - -3: 缺少输入数据
/// - -4: 输入不是 DataFrame / DataFrameArray 类型
/// - -5: 输入 DataFrame 数组为空
/// - -6: 表达式参数为空
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
        eprintln!("表达式算子: {}", err_msg);
        return -1;
    }

    let params_json_str = if params_json.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(params_json).to_str().unwrap_or("") }
    };
    let params = parse_params(params_json_str);

    // 校验表达式
    let expr_trimmed = params.expression.trim();
    if expr_trimmed.is_empty() {
        let err = "表达式参数为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("表达式算子: {}", err);
        return -6;
    }

    let ast = match parse_expression(expr_trimmed) {
        Ok(a) => a,
        Err(e) => {
            let err = format!("表达式解析失败: {}", e);
            let c_msg = CString::new(err.clone()).unwrap_or_default();
            c_set_last_error(c_msg.as_ptr());
            eprintln!("表达式算子: {}", err);
            return -7;
        }
    };

    // 解析输出列名（为空回退 signal）
    let output_col_name = if params.column_name.trim().is_empty() {
        "signal".to_string()
    } else {
        params.column_name.trim().to_string()
    };

    if input_count == 0 || inputs.is_null() {
        let err = "缺少输入数据".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("表达式算子: {}", err);
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
            eprintln!("表达式算子: {}", err);
            return -4;
        }
    };

    if input_dfs.is_empty() {
        let err = "输入 DataFrameArray 为空".to_string();
        let c_msg = CString::new(err.clone()).unwrap_or_default();
        c_set_last_error(c_msg.as_ptr());
        eprintln!("表达式算子: {}", err);
        return -5;
    }

    // 收集引用列名（用于日志）
    let mut referenced = Vec::new();
    collect_columns(&ast, &mut referenced);

    println!(
        "表达式算子: 表达式='{}', 输出列='{}', 引用列={:?}, 输入 DataFrame 数量={}, 首个行数={}",
        params.expression,
        output_col_name,
        referenced,
        input_dfs.len(),
        input_dfs[0].row_count
    );

    // 逐个 DataFrame 求值
    let mut out_dfs: Vec<DataFrame> = Vec::with_capacity(input_dfs.len());
    for (i, df) in input_dfs.iter().enumerate() {
        if df.row_count == 0 {
            eprintln!("表达式算子: 第 {} 个 DataFrame 为空，原样保留", i);
            out_dfs.push(df.clone());
            continue;
        }
        match apply_expression(df, &output_col_name, &ast) {
            Ok(out_df) => out_dfs.push(out_df),
            Err(e) => {
                let err = format!("第 {} 个 DataFrame 求值失败: {}", i, e);
                let c_msg = CString::new(err.clone()).unwrap_or_default();
                c_set_last_error(c_msg.as_ptr());
                eprintln!("表达式算子: {}", err);
                return -8;
            }
        }
    }

    // 清空错误信息（成功执行）
    let c_msg = CString::new("").unwrap_or_default();
    c_set_last_error(c_msg.as_ptr());

    // 输出统一为 DataFrameArray（与端口声明一致）
    let port_data = PortData::DataFrameArray(out_dfs);
    if !outputs.is_null() && output_cap > 0 {
        let c_pd = portdata_to_c(&port_data);
        unsafe {
            *outputs = c_pd;
            if output_cap > 1 {
                *outputs.add(1) = CPortData {
                    type_tag: TYPE_NULL,
                    value: CPortValue {
                        str_ptr: std::ptr::null_mut(),
                    },
                };
            }
        }
    }

    0
}

/// 释放 C ABI PortData 内存（由调用方调用）
#[no_mangle]
pub extern "C" fn release_port_data(data_ptr: *mut CPortData) {
    if !data_ptr.is_null() {
        operator_runtime::c_abi::c_pd_free(data_ptr);
    }
}

/// 获取表达式算子版本
#[no_mangle]
pub extern "C" fn expression_operator_version() -> *const c_char {
    b"0.1.0\0".as_ptr() as *const c_char
}

#[cfg(test)]
mod tests;
