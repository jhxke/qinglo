//! K线 DSL 渲染引擎。
//!
//! 由算子输出的自定义文本 DSL（`kline "标题" { candle ...; line ... }`）经
//! 词法器 + 递归下降解析器转为 AST，再用 egui `Painter` 手画蜡烛图（含 MA
//! 折线、坐标轴、缩放/滚动、十字光标 tooltip）。
//!
//! 入口 [`render_kline_preview_window`]：读取节点预览缓存中的第一个 `PortData::String`
//! 输出，解析为 [`KlineDoc`] 后以 tab 形式渲染各 `kline` 块。

use egui::{Color32, FontId, Painter, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, Align2};
use operator_executor_client::{DataFrame, DataType, PortData};
use operator_executor_client::runtime_client::DebugNodeMeta;

use super::state::{DagTab, DebugPreviewState};
use crate::data_preview;

// ============================================================================
// Debug 模式前端渲染：从上游 DataFrame 按算子参数直接生成 DSL
// ============================================================================

/// K线算子参数（与 operator/kline_visualization_operator 定义一致，均为 String）。
#[derive(Debug, Clone, Default)]
struct KlineFrontendParams {
    pub indices: String,
    pub open_col: String,
    pub high_col: String,
    pub low_col: String,
    pub close_col: String,
    pub date_col: String,
    pub ma5_col: String,
    pub ma10_col: String,
}

impl KlineFrontendParams {
    fn with_defaults(self) -> KlineFrontendParams {
        KlineFrontendParams {
            indices: self.indices,
            open_col: if self.open_col.is_empty() { "open".to_string() } else { self.open_col },
            high_col: if self.high_col.is_empty() { "high".to_string() } else { self.high_col },
            low_col: if self.low_col.is_empty() { "low".to_string() } else { self.low_col },
            close_col: if self.close_col.is_empty() { "close".to_string() } else { self.close_col },
            date_col: if self.date_col.is_empty() { "date".to_string() } else { self.date_col },
            ma5_col: if self.ma5_col.is_empty() { "ma5".to_string() } else { self.ma5_col },
            ma10_col: if self.ma10_col.is_empty() { "ma10".to_string() } else { self.ma10_col },
        }
    }
}

/// 从节点的 operator_type 中提取 K线算子参数。
fn extract_kline_params(tab: &DagTab, node_id: &str) -> KlineFrontendParams {
    use crate::dag::{OperatorPortParamDef, PortDirection};
    let Some(node) = tab.graph.get_node(node_id) else {
        return KlineFrontendParams::default().with_defaults();
    };
    let def = match &node.operator_type {
        crate::dag::OperatorType::Custom(d) => d,
    };
    let get = |name: &str| -> String {
        def.port_params
            .iter()
            .find(|p: &&OperatorPortParamDef| {
                p.direction == PortDirection::Param && p.name == name
            })
            .map(|p| p.default_value.clone())
            .unwrap_or_default()
    };
    KlineFrontendParams {
        indices: get("indices"),
        open_col: get("open_col"),
        high_col: get("high_col"),
        low_col: get("low_col"),
        close_col: get("close_col"),
        date_col: get("date_col"),
        ma5_col: get("ma5_col"),
        ma10_col: get("ma10_col"),
    }
    .with_defaults()
}

/// 在 DAG 中查找 `(target_node_id, target_port_idx)` 连接的源节点和源输出端口。
fn find_upstream_source(
    tab: &DagTab,
    target_node_id: &str,
    target_port: usize,
) -> Option<(String, usize)> {
    tab.graph
        .get_edges_to_node(target_node_id)
        .into_iter()
        .find(|e| e.target_port == target_port)
        .map(|e| (e.source_node_id.clone(), e.source_port))
}

// ---------- 前端 emit_chart（从 DataFrame + 参数生成单个 kline 块 DSL）----------

fn extract_f64_col(df: &DataFrame, name: &str) -> Option<Vec<Option<f64>>> {
    let col = df.column(name)?;
    if !matches!(col.data_type, DataType::Float64) {
        return None;
    }
    Some(col.to_f64_vec())
}

fn extract_date_col(df: &DataFrame, name: &str, n: usize) -> Vec<String> {
    if let Some(col) = df.column(name) {
        match col.data_type {
            DataType::String => (0..n).map(|i| col.get_string(i).unwrap_or("").to_string()).collect(),
            DataType::Int64 => (0..n).map(|i| col.get_i64(i).map(|v| v.to_string()).unwrap_or_default()).collect(),
            DataType::Float64 => (0..n).map(|i| col.get_f64(i).map(format_float).unwrap_or_default()).collect(),
            _ => (0..n).map(|i| format!("#{}", i)).collect(),
        }
    } else {
        (0..n).map(|i| format!("#{}", i)).collect()
    }
}

fn format_float(v: f64) -> String {
    if v.is_nan() || v.is_infinite() {
        format!("{:?}", v)
    } else {
        format!("{:?}", v)
    }
}

fn escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn push_ma_values(out: &mut String, values: Vec<Option<f64>>, n: usize) {
    let len = values.len().min(n);
    for i in 0..len {
        if i > 0 {
            out.push_str(", ");
        }
        match values[i] {
            Some(v) if v.is_finite() => out.push_str(&format_float(v)),
            _ => out.push('_'),
        }
    }
}

/// 前端版 emit_chart：为单个 DataFrame 生成 `kline "标题" { ... }` 块。
///
/// 返回 `Ok(String)`：成功，含 0 行也返回空块（便于调试）。
/// 返回 `Err(String)`：缺少 OHLC 四列中任意一列。
fn emit_chart_frontend(
    df: &DataFrame,
    params: &KlineFrontendParams,
    title: &str,
) -> Result<String, String> {
    let n = df.row_count;
    if n == 0 {
        return Ok(format!("kline {} {{\n}}\n", escape_str(title)));
    }

    let open = extract_f64_col(df, &params.open_col)
        .ok_or_else(|| format!("缺少开盘价列 '{}' (Float64)", params.open_col))?;
    let high = extract_f64_col(df, &params.high_col)
        .ok_or_else(|| format!("缺少最高价列 '{}' (Float64)", params.high_col))?;
    let low = extract_f64_col(df, &params.low_col)
        .ok_or_else(|| format!("缺少最低价列 '{}' (Float64)", params.low_col))?;
    let close = extract_f64_col(df, &params.close_col)
        .ok_or_else(|| format!("缺少收盘价列 '{}' (Float64)", params.close_col))?;

    let dates = extract_date_col(df, &params.date_col, n);
    let ma5 = extract_f64_col(df, &params.ma5_col);
    let ma10 = extract_f64_col(df, &params.ma10_col);

    let mut out = String::new();
    out.push_str(&format!("kline {} {{\n", escape_str(title)));

    for i in 0..n {
        let (o, h, l, c) = match (open[i], high[i], low[i], close[i]) {
            (Some(o), Some(h), Some(l), Some(c))
                if o.is_finite() && h.is_finite() && l.is_finite() && c.is_finite() =>
            {
                (o, h, l, c)
            }
            _ => continue,
        };
        out.push_str(&format!(
            "  candle {} {} {} {} {}\n",
            escape_str(&dates[i]),
            format_float(o),
            format_float(h),
            format_float(l),
            format_float(c),
        ));
    }

    if let Some(ma) = ma5 {
        out.push_str(&format!("  line {} \"#FFD700\" [", escape_str("MA5")));
        push_ma_values(&mut out, ma, n);
        out.push_str("]\n");
    }
    if let Some(ma) = ma10 {
        out.push_str(&format!("  line {} \"#9370DB\" [", escape_str("MA10")));
        push_ma_values(&mut out, ma, n);
        out.push_str("]\n");
    }

    out.push_str("}\n");
    Ok(out)
}

// =============================== 词法器 ===============================

/// DSL 词法 token。关键字（kline/candle/line）以 `Ident` 承载，由解析器按字符串分派。
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Str(String),
    Num(f64),
    Underscore,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Eof,
}

/// token 在源码中的位置（1 基行列），用于错误提示。
#[derive(Debug, Clone, Copy)]
struct TokenPos {
    line: usize,
    col: usize,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    input: &'a str,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Lexer {
            chars: input.char_indices().peekable(),
            input,
            line: 1,
            col: 1,
        }
    }

    /// 跳过空白与 `//` 行注释。
    fn skip_ws_and_comments(&mut self) {
        while let Some(&(i, ch)) = self.chars.peek() {
            if ch.is_whitespace() {
                self.advance(i, ch);
            } else if ch == '/' {
                // 判定是否为 // 注释：需要看下一个字符
                let mut it = self.chars.clone();
                it.next();
                if let Some(&(_, '/')) = it.peek() {
                    // 注释到行尾
                    while let Some(&(j, c)) = self.chars.peek() {
                        self.advance(j, c);
                        if c == '\n' {
                            break;
                        }
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    }

    /// 推进一个字符并维护 line/col。
    fn advance(&mut self, idx: usize, ch: char) {
        self.chars.next();
        if ch == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        let _ = idx;
    }

    fn next_token(&mut self) -> Result<(Token, TokenPos), ParseError> {
        self.skip_ws_and_comments();
        let (i, ch) = match self.chars.peek() {
            Some(&c) => c,
            None => return Ok((Token::Eof, TokenPos { line: self.line, col: self.col })),
        };
        let pos = TokenPos { line: self.line, col: self.col };

        // 单字符 token
        match ch {
            '{' => { self.advance(i, ch); return Ok((Token::LBrace, pos)); }
            '}' => { self.advance(i, ch); return Ok((Token::RBrace, pos)); }
            '[' => { self.advance(i, ch); return Ok((Token::LBracket, pos)); }
            ']' => { self.advance(i, ch); return Ok((Token::RBracket, pos)); }
            ',' => { self.advance(i, ch); return Ok((Token::Comma, pos)); }
            '"' => return self.lex_string(pos),
            _ => {}
        }

        // `_` 作为 null 标记（单独的 _ ，后跟非标识符字符）
        if ch == '_' {
            // 看下一个字符：若为标识符字符则按标识符处理，否则为 Underscore
            let mut it = self.chars.clone();
            it.next();
            match it.peek() {
                Some(&(_, c)) if c.is_alphanumeric() || c == '_' => {}
                _ => {
                    self.advance(i, ch);
                    return Ok((Token::Underscore, pos));
                }
            }
        }

        // 标识符 [a-zA-Z_][a-zA-Z0-9_]*
        if ch.is_alphabetic() || ch == '_' {
            return Ok((Token::Ident(self.lex_ident()), pos));
        }

        // 数字（含负数、小数、科学计数法）
        if ch.is_ascii_digit() || ch == '.' || (ch == '-' && self.peek_is_digit_or_dot()) {
            return self.lex_number(pos);
        }

        Err(ParseError {
            message: format!("非法字符 '{}'", ch),
            line: pos.line,
            col: pos.col,
        })
    }

    fn peek_is_digit_or_dot(&self) -> bool {
        let mut it = self.chars.clone();
        it.next();
        matches!(it.peek(), Some(&(_, c)) if c.is_ascii_digit() || c == '.')
    }

    fn lex_ident(&mut self) -> String {
        let mut s = String::new();
        while let Some(&(i, ch)) = self.chars.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                s.push(ch);
                self.advance(i, ch);
            } else {
                break;
            }
        }
        s
    }

    fn lex_string(&mut self, pos: TokenPos) -> Result<(Token, TokenPos), ParseError> {
        // 消费开头的 "
        let (i, ch) = *self.chars.peek().unwrap();
        self.advance(i, ch);
        let mut s = String::new();
        loop {
            match self.chars.peek() {
                None => {
                    return Err(ParseError {
                        message: "字符串未闭合".to_string(),
                        line: pos.line,
                        col: pos.col,
                    });
                }
                Some(&(j, '"')) => {
                    self.advance(j, '"');
                    return Ok((Token::Str(s), pos));
                }
                Some(&(j, '\\')) => {
                    self.advance(j, '\\');
                    match self.chars.peek() {
                        Some(&(_, '"')) => s.push('"'),
                        Some(&(_, '\\')) => s.push('\\'),
                        Some(&(_, 'n')) => s.push('\n'),
                        Some(&(_, 'r')) => s.push('\r'),
                        Some(&(_, 't')) => s.push('\t'),
                        Some(&(_, c)) => s.push(c),
                        None => {
                            return Err(ParseError {
                                message: "字符串转义未闭合".to_string(),
                                line: pos.line,
                                col: pos.col,
                            });
                        }
                    }
                    if let Some(&(k, c)) = self.chars.peek() {
                        self.advance(k, c);
                    }
                }
                Some(&(j, c)) => {
                    s.push(c);
                    self.advance(j, c);
                }
            }
        }
    }

    fn lex_number(&mut self, pos: TokenPos) -> Result<(Token, TokenPos), ParseError> {
        let start_byte = self.chars.peek().unwrap().0;
        let mut end_byte = start_byte;
        // 先吃符号
        if let Some(&(_, '-')) = self.chars.peek() {
            end_byte = self.consume_into(&mut end_byte);
        }
        // 整数部分
        while let Some(&(i, ch)) = self.chars.peek() {
            if ch.is_ascii_digit() {
                end_byte = i + ch.len_utf8();
                self.advance(i, ch);
            } else {
                break;
            }
        }
        // 小数部分
        if let Some(&(i, '.')) = self.chars.peek() {
            end_byte = i + 1;
            self.advance(i, '.');
            while let Some(&(j, ch)) = self.chars.peek() {
                if ch.is_ascii_digit() {
                    end_byte = j + ch.len_utf8();
                    self.advance(j, ch);
                } else {
                    break;
                }
            }
        }
        // 指数部分
        if let Some(&(i, e)) = self.chars.peek() {
            if e == 'e' || e == 'E' {
                self.advance(i, e);
                end_byte = i + 1;
                if let Some(&(j, sign)) = self.chars.peek() {
                    if sign == '+' || sign == '-' {
                        end_byte = j + 1;
                        self.advance(j, sign);
                    }
                }
                while let Some(&(j, ch)) = self.chars.peek() {
                    if ch.is_ascii_digit() {
                        end_byte = j + ch.len_utf8();
                        self.advance(j, ch);
                    } else {
                        break;
                    }
                }
            }
        }
        let text = &self.input[start_byte..end_byte];
        match text.parse::<f64>() {
            Ok(v) => Ok((Token::Num(v), pos)),
            Err(_) => Err(ParseError {
                message: format!("非法数字 '{}'", text),
                line: pos.line,
                col: pos.col,
            }),
        }
    }

    /// 消费当前字符并返回其结束字节偏移。
    fn consume_into(&mut self, _start: &usize) -> usize {
        if let Some((i, ch)) = self.chars.next() {
            if ch == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            i + ch.len_utf8()
        } else {
            *_start
        }
    }
}

fn lex(input: &str) -> Result<Vec<(Token, TokenPos)>, ParseError> {
    let mut lexer = Lexer::new(input);
    let mut tokens = Vec::new();
    loop {
        let (tok, pos) = lexer.next_token()?;
        let is_eof = tok == Token::Eof;
        tokens.push((tok, pos));
        if is_eof {
            break;
        }
    }
    Ok(tokens)
}

// =============================== 解析器 ===============================

/// DSL 解析错误（带行列位置）。
#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

/// 解析后的 K线文档（含若干图表）。
#[derive(Debug, Clone)]
pub struct KlineDoc {
    pub charts: Vec<KlineChart>,
}

#[derive(Debug, Clone)]
pub struct KlineChart {
    pub title: String,
    pub candles: Vec<Candle>,
    pub ma_lines: Vec<MaLine>,
}

#[derive(Debug, Clone)]
pub struct Candle {
    pub date: String,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
}

#[derive(Debug, Clone)]
pub struct MaLine {
    pub name: String,
    pub color: Color32,
    pub values: Vec<Option<f64>>,
}

struct Parser {
    tokens: Vec<(Token, TokenPos)>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        &self.tokens[self.pos].0
    }
    fn peek_pos(&self) -> TokenPos {
        self.tokens[self.pos].1
    }
    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].0.clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }
    fn err<T>(&self, msg: impl Into<String>) -> Result<T, ParseError> {
        let p = self.peek_pos();
        Err(ParseError { message: msg.into(), line: p.line, col: p.col })
    }

    fn expect(&mut self, expected: &Token) -> Result<(), ParseError> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            self.err(format!("期望 {:?}，实际 {:?}", expected, self.peek()))
        }
    }

    fn expect_ident(&mut self, name: &str) -> Result<(), ParseError> {
        match self.peek() {
            Token::Ident(s) if s == name => {
                self.advance();
                Ok(())
            }
            other => self.err(format!("期望关键字 '{}'，实际 {:?}", name, other)),
        }
    }

    fn expect_string(&mut self) -> Result<String, ParseError> {
        match self.peek().clone() {
            Token::Str(s) => {
                self.advance();
                Ok(s)
            }
            other => self.err(format!("期望字符串，实际 {:?}", other)),
        }
    }

    fn expect_number(&mut self) -> Result<f64, ParseError> {
        match self.peek().clone() {
            Token::Num(n) => {
                self.advance();
                Ok(n)
            }
            other => self.err(format!("期望数字，实际 {:?}", other)),
        }
    }

    fn parse_doc(&mut self) -> Result<KlineDoc, ParseError> {
        let mut charts = Vec::new();
        while !matches!(self.peek(), Token::Eof) {
            charts.push(self.parse_chart()?);
        }
        Ok(KlineDoc { charts })
    }

    fn parse_chart(&mut self) -> Result<KlineChart, ParseError> {
        self.expect_ident("kline")?;
        let title = self.expect_string()?;
        self.expect(&Token::LBrace)?;
        let mut candles = Vec::new();
        let mut ma_lines = Vec::new();
        while !matches!(self.peek(), Token::RBrace) {
            match self.peek() {
                Token::Ident(s) if s == "candle" => candles.push(self.parse_candle()?),
                Token::Ident(s) if s == "line" => ma_lines.push(self.parse_line()?),
                other => return self.err(format!("期望 'candle' 或 'line'，实际 {:?}", other)),
            }
        }
        self.expect(&Token::RBrace)?;
        Ok(KlineChart { title, candles, ma_lines })
    }

    fn parse_candle(&mut self) -> Result<Candle, ParseError> {
        self.expect_ident("candle")?;
        let date = self.expect_string()?;
        let o = self.expect_number()?;
        let h = self.expect_number()?;
        let l = self.expect_number()?;
        let c = self.expect_number()?;
        Ok(Candle { date, o, h, l, c })
    }

    fn parse_line(&mut self) -> Result<MaLine, ParseError> {
        self.expect_ident("line")?;
        let name = self.expect_string()?;
        let color_str = self.expect_string()?;
        let color = parse_color(&color_str);
        self.expect(&Token::LBracket)?;
        let mut values = Vec::new();
        // 允许空数组
        if !matches!(self.peek(), Token::RBracket) {
            loop {
                match self.peek().clone() {
                    Token::Num(n) => {
                        self.advance();
                        values.push(Some(n));
                    }
                    Token::Underscore => {
                        self.advance();
                        values.push(None);
                    }
                    other => return self.err(format!("期望数字或 '_'，实际 {:?}", other)),
                }
                match self.peek() {
                    Token::Comma => { self.advance(); }
                    Token::RBracket => break,
                    other => return self.err(format!("期望 ',' 或 ']'，实际 {:?}", other)),
                }
            }
        }
        self.expect(&Token::RBracket)?;
        Ok(MaLine { name, color, values })
    }
}

/// 解析 DSL 文本为 [`KlineDoc`]。
pub fn parse(input: &str) -> Result<KlineDoc, ParseError> {
    let tokens = lex(input)?;
    let mut parser = Parser { tokens, pos: 0 };
    parser.parse_doc()
}

/// 解析颜色字符串 `#RRGGBB` / `#RRGGBBAA`；失败回退浅蓝。
fn parse_color(s: &str) -> Color32 {
    let s = s.strip_prefix('#').unwrap_or(s);
    let hex = |start: usize, end: usize| -> Option<u8> {
        s.get(start..end).and_then(|slice| u8::from_str_radix(slice, 16).ok())
    };
    let color: Option<Color32> = match s.len() {
        6 => match (hex(0, 2), hex(2, 4), hex(4, 6)) {
            (Some(r), Some(g), Some(b)) => Some(Color32::from_rgb(r, g, b)),
            _ => None,
        },
        8 => match (hex(0, 2), hex(2, 4), hex(4, 6), hex(6, 8)) {
            (Some(r), Some(g), Some(b), Some(a)) => {
                Some(Color32::from_rgba_unmultiplied(r, g, b, a))
            }
            _ => None,
        },
        _ => None,
    };
    color.unwrap_or(Color32::LIGHT_BLUE)
}

// =============================== 渲染 ===============================

/// 涨色（中国惯例：红涨）
const UP_COLOR: Color32 = Color32::from_rgb(233, 71, 71);
/// 跌色（中国惯例：绿跌）
const DOWN_COLOR: Color32 = Color32::from_rgb(42, 177, 94);
/// 十字光标颜色。egui 0.26 的 `Color32::from_rgba_unmultiplied` 非 const，故用函数构造。
fn crosshair_color() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, 90)
}
/// 单图表交互态（滚动位置 + 可见数量）
#[derive(Clone, Copy, Default)]
struct ChartState {
    first_visible: usize,
    visible_count: usize, // 0 = 全显
}
const MIN_VISIBLE: usize = 10;

/// 渲染单个 K线图表到指定区域。
fn render_chart(
    ui: &mut Ui,
    chart: &KlineChart,
    node_id: &str,
    chart_idx: usize,
) {
    let n = chart.candles.len();
    if n == 0 {
        ui.vertical_centered(|ui| {
            ui.add_space(40.0);
            ui.label("无蜡烛数据");
        });
        return;
    }

    // 分配绘图区域
    let avail_h = ui.available_height().max(220.0);
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), avail_h), Sense::drag());

    let painter = ui.painter().with_clip_rect(rect);

    // 布局边距
    let pad = 8.0;
    let right_axis_w = 64.0;
    let bottom_axis_h = 22.0;
    let plot_rect = Rect::from_min_size(
        rect.min + Vec2::new(pad, pad),
        Vec2::new(
            (rect.width() - pad - right_axis_w).max(40.0),
            (rect.height() - pad - bottom_axis_h).max(40.0),
        ),
    );

    // ---- 交互态 ----
    let state_id = ui.id().with("kline_chart").with(node_id).with(chart_idx);
    let mut state: ChartState =
        ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<ChartState>(state_id));

    let total = n;
    // 初始化可见数量
    if state.visible_count == 0 {
        state.visible_count = total;
    }
    let visible_count = state.visible_count.clamp(MIN_VISIBLE.min(total), total);
    if state.first_visible + visible_count > total {
        state.first_visible = total.saturating_sub(visible_count);
    }
    let end = (state.first_visible + visible_count).min(total);

    let candle_w = plot_rect.width() / visible_count as f32;
    let body_w = (candle_w * 0.6).max(1.5);

    // ---- 滚轮缩放 ----
    if response.hovered() {
        let scroll = ui.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.0 {
            let factor = if scroll > 0.0 { 0.9 } else { 1.1 };
            let new_vc = ((visible_count as f32) * factor).round() as usize;
            let new_vc = new_vc.clamp(MIN_VISIBLE.min(total), total);
            // 以中心为锚保持
            let center = state.first_visible as f64 + visible_count as f64 / 2.0;
            state.visible_count = new_vc;
            state.first_visible =
                ((center - new_vc as f64 / 2.0).round() as isize).max(0) as usize;
            state.first_visible = state
                .first_visible
                .min(total.saturating_sub(new_vc));
        }
    }

    // ---- 拖拽水平滚动 ----
    if response.dragged() {
        let dx = response.drag_delta().x;
        if candle_w > 0.0 {
            let shift = (-dx / candle_w) as isize;
            let new_first = (state.first_visible as isize + shift).max(0) as usize;
            state.first_visible = new_first.min(total.saturating_sub(visible_count));
        }
    }

    // ---- 价格范围（可见蜡烛 + 落入范围的 MA 有效值）----
    let mut min_price = f64::INFINITY;
    let mut max_price = f64::NEG_INFINITY;
    for i in state.first_visible..end {
        let cd = &chart.candles[i];
        min_price = min_price.min(cd.l);
        max_price = max_price.max(cd.h);
    }
    for ma in &chart.ma_lines {
        for i in state.first_visible..end {
            if let Some(v) = ma.values.get(i).copied().flatten() {
                if v.is_finite() {
                    min_price = min_price.min(v);
                    max_price = max_price.max(v);
                }
            }
        }
    }
    if !min_price.is_finite() || !max_price.is_finite() {
        // 可见区无有效价格
        painter.rect_filled(plot_rect, 0.0, super::theme::CANVAS_BG);
        painter.text(
            plot_rect.center(),
            Align2::CENTER_CENTER,
            "可见区间无有效价格数据",
            FontId::proportional(13.0),
            super::theme::TEXT_WEAK,
        );
        write_state(ui, state_id, state);
        return;
    }
    let pad_price = (max_price - min_price).max(1e-9) * 0.05;
    min_price -= pad_price;
    max_price += pad_price;
    if (max_price - min_price).abs() < 1e-9 {
        min_price -= 1.0;
        max_price += 1.0;
    }

    let x_of = |i: usize| -> f32 {
        plot_rect.left() + (i as f32 - state.first_visible as f32 + 0.5) * candle_w
    };
    let y_of = |price: f64| -> f32 {
        let ratio = (price - min_price) / (max_price - min_price);
        plot_rect.bottom() - (ratio as f32) * plot_rect.height()
    };

    // ---- 背景 ----
    painter.rect_filled(plot_rect, 0.0, super::theme::CANVAS_BG);

    // ---- 水平网格 + 价格刻度 ----
    let ticks = nice_ticks(min_price, max_price, 5);
    let grid_stroke = Stroke::new(1.0, super::theme::CANVAS_GRID);
    for &t in &ticks {
        let y = y_of(t);
        if (plot_rect.top()..=plot_rect.bottom()).contains(&y) {
            painter.line_segment(
                [Pos2::new(plot_rect.left(), y), Pos2::new(plot_rect.right(), y)],
                grid_stroke,
            );
            painter.text(
                Pos2::new(plot_rect.right() + 4.0, y),
                Align2::LEFT_CENTER,
                format!("{:.2}", t),
                FontId::proportional(11.0),
                super::theme::TEXT_WEAK,
            );
        }
    }

    // ---- 蜡烛 ----
    for i in state.first_visible..end {
        let cd = &chart.candles[i];
        let x = x_of(i);
        let up = cd.c >= cd.o;
        let color = if up { UP_COLOR } else { DOWN_COLOR };
        // 影线
        painter.line_segment(
            [Pos2::new(x, y_of(cd.h)), Pos2::new(x, y_of(cd.l))],
            Stroke::new(1.0, color),
        );
        // 实体
        let top = y_of(cd.c.max(cd.o));
        let bot = y_of(cd.c.min(cd.o));
        let body_top = top.min(bot);
        let body_h = (bot - top).abs().max(1.0);
        painter.rect_filled(
            Rect::from_min_size(
                Pos2::new(x - body_w / 2.0, body_top),
                Vec2::new(body_w, body_h),
            ),
            0.0,
            color,
        );
    }

    // ---- MA 折线（按 None 分段）----
    let mut pts: Vec<Pos2> = Vec::with_capacity(visible_count);
    for ma in &chart.ma_lines {
        pts.clear();
        for i in state.first_visible..end {
            match ma.values.get(i).copied().flatten() {
                Some(v) if v.is_finite() => {
                    pts.push(Pos2::new(x_of(i), y_of(v)));
                }
                _ => {
                    if pts.len() >= 2 {
                        painter.add(Shape::line(
                            pts.clone(),
                            Stroke::new(1.5, ma.color),
                        ));
                    }
                    pts.clear();
                }
            }
        }
        if pts.len() >= 2 {
            painter.add(Shape::line(pts.clone(), Stroke::new(1.5, ma.color)));
        }
        // 图例
        if !chart.ma_lines.is_empty() {
            // 在左上角绘制图例（仅一次循环外处理）
        }
    }
    // ---- 图例 ----
    let mut legend_x = plot_rect.left() + 6.0;
    let legend_y = plot_rect.top() + 4.0;
    for ma in &chart.ma_lines {
        painter.line_segment(
            [
                Pos2::new(legend_x, legend_y + 6.0),
                Pos2::new(legend_x + 14.0, legend_y + 6.0),
            ],
            Stroke::new(2.0, ma.color),
        );
        let legend_rect = painter.text(
            Pos2::new(legend_x + 18.0, legend_y + 6.0),
            Align2::LEFT_CENTER,
            &ma.name,
            FontId::proportional(11.0),
            super::theme::TEXT_STRONG,
        );
        legend_x = legend_rect.right() + 12.0;
    }

    // ---- 日期轴 ----
    let label_step = ((visible_count as f32) * 70.0 / plot_rect.width().max(1.0))
        .ceil()
        .max(1.0) as usize;
    let date_color = super::theme::TEXT_WEAK;
    for i in state.first_visible..end {
        if (i - state.first_visible) % label_step != 0 {
            continue;
        }
        let x = x_of(i);
        let date = truncate_date(&chart.candles[i].date);
        painter.text(
            Pos2::new(x, plot_rect.bottom() + 4.0),
            Align2::CENTER_TOP,
            date,
            FontId::proportional(10.0),
            date_color,
        );
    }

    // ---- 十字光标 + tooltip ----
    if response.hovered() {
        if let Some(hover) = response.hover_pos() {
            if plot_rect.contains(hover) {
                // 竖线
                draw_dashed_v(&painter, hover.x, plot_rect.top(), plot_rect.bottom(), crosshair_color());
                // 横线
                draw_dashed_h(&painter, hover.y, plot_rect.left(), plot_rect.right(), crosshair_color());
                // 定位蜡烛
                let rel = (hover.x - plot_rect.left()) / candle_w;
                let idx = (state.first_visible as f32 + rel) as isize;
                let idx = idx.clamp(state.first_visible as isize, (end - 1) as isize) as usize;
                let cd = &chart.candles[idx];
                let chg = if cd.o != 0.0 {
                    (cd.c - cd.o) / cd.o * 100.0
                } else {
                    0.0
                };
                let chg_color = if cd.c >= cd.o { UP_COLOR } else { DOWN_COLOR };
                // tooltip 位置：鼠标右上
                let tip_pos = Pos2::new(hover.x + 12.0, hover.y + 12.0);
                let tip_id = ui.id().with("kline_tip").with(chart_idx);
                egui::Area::new(tip_id)
                    .order(egui::Order::Foreground)
                    .fixed_pos(tip_pos)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::group(ui.style())
                            .fill(super::theme::CARD_BG)
                            .show(ui, |ui| {
                                ui.set_min_width(120.0);
                                ui.label(egui::RichText::new(&cd.date).strong().color(super::theme::TEXT_STRONG));
                                ui.label(format!("开: {:.2}", cd.o));
                                ui.label(format!("高: {:.2}", cd.h));
                                ui.label(format!("低: {:.2}", cd.l));
                                ui.label(format!("收: {:.2}", cd.c));
                                ui.label(egui::RichText::new(format!("涨跌: {:+.2}%", chg)).color(chg_color));
                            });
                    });
            }
        }
    }

    write_state(ui, state_id, state);

    // 提示拖拽/缩放
    if !response.hovered() {
        let _ = response; // 保持 response 活跃
    }
}

fn write_state(ui: &Ui, id: egui::Id, state: ChartState) {
    ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<ChartState>(id) = state);
}

/// 画竖向虚线。
fn draw_dashed_v(painter: &Painter, x: f32, y0: f32, y1: f32, color: Color32) {
    let mut y = y0;
    let stroke = Stroke::new(1.0, color);
    while y < y1 {
        let y_next = (y + 6.0).min(y1);
        painter.line_segment([Pos2::new(x, y), Pos2::new(x, y_next)], stroke);
        y = y_next + 4.0;
    }
}

/// 画横向虚线。
fn draw_dashed_h(painter: &Painter, y: f32, x0: f32, x1: f32, color: Color32) {
    let mut x = x0;
    let stroke = Stroke::new(1.0, color);
    while x < x1 {
        let x_next = (x + 6.0).min(x1);
        painter.line_segment([Pos2::new(x, y), Pos2::new(x_next, y)], stroke);
        x = x_next + 4.0;
    }
}

/// 生成 [min, max] 区间约 `count` 个「整齐」刻度值。
fn nice_ticks(min: f64, max: f64, count: usize) -> Vec<f64> {
    if !min.is_finite() || !max.is_finite() || min >= max || count == 0 {
        return vec![];
    }
    let range = max - min;
    let raw_step = range / count as f64;
    let mag = 10f64.powf(raw_step.log10().floor());
    let norm = raw_step / mag;
    let step = (if norm < 1.5 {
        1.0
    } else if norm < 3.0 {
        2.0
    } else if norm < 7.0 {
        5.0
    } else {
        10.0
    }) * mag;
    let start = (min / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut v = start;
    let mut guard = 0;
    while v <= max + step * 0.5 && guard < 100 {
        ticks.push(v);
        v += step;
        guard += 1;
    }
    ticks
}

/// 日期轴标签截断：保留月-日（若有），否则原样截断到 10 字符。
fn truncate_date(s: &str) -> String {
    // 常见 "2024-01-02" → "01-02"
    if s.len() >= 10 && s.as_bytes()[4] == b'-' {
        s[5..10].to_string()
    } else if s.chars().count() > 8 {
        s.chars().take(8).collect()
    } else {
        s.to_string()
    }
}

// =============================== 预览窗口 ===============================

/// Debug 模式下 DataFrame 端口分页的每页行数（与服务端 PREVIEW_ROW_LIMIT 一致）。
/// K 线算子输出为 String，服务端对 String 端口不分页，但常量保持一致以便复用查询函数。
const DEBUG_PAGE_SIZE: usize = 200;

/// 渲染 K线图预览浮动窗口。`tab.kline_preview_node_id` 为 None 时直接返回。
///
/// Debug 模式下（`tab.debug_mode && tab.debug_session_id.is_some()`），不再读本地
/// `cache/{node_id}.json`（仅含截断预览），而是向服务端 debug session 查询完整
/// DSL（K 线算子输出端口固定为 0，String 类型 page_idx=0 一次性返回完整内容），
/// 这样可以展示所有 K 线并支持多图表 ComboBox 切换。
pub fn render_kline_preview_window(ui: &mut Ui, tab: &mut DagTab) {
    let node_id = match tab.kline_preview_node_id.clone() {
        Some(id) => id,
        None => return,
    };

    // 判断是否走 Debug 模式预览（服务端查询）
    let debug_active = tab.debug_mode && tab.debug_session_id.is_some();
    let session_id = tab.debug_session_id.clone();

    // 节点可能已删除：优先用缓存里的名称，其次从图查找，最后回退到 ID。
    let cache = if debug_active { None } else { data_preview::load_preview_cache(&node_id) };
    let graph_name = tab
        .graph
        .get_node(&node_id)
        .map(|n| n.operator_type.name().to_string());
    let node_name = if debug_active {
        graph_name.clone().unwrap_or_else(|| node_id.clone())
    } else {
        cache
            .as_ref()
            .map(|c| c.node_name.clone())
            .filter(|n| !n.is_empty())
            .or(graph_name)
            .unwrap_or_else(|| node_id.clone())
    };

    let mut open = true;
    let title = if debug_active {
        format!("K线图预览 [Debug] - {}", node_name)
    } else {
        format!("K线图预览 - {}", node_name)
    };

    let screen = ui.ctx().screen_rect();
    let max_w = (screen.width() * 0.85).max(560.0);
    let max_h = (screen.height() * 0.85).max(360.0);
    let default_w = 900.0f32.min(max_w);
    let default_h = 560.0f32.min(max_h);

    egui::Window::new(title)
        .open(&mut open)
        .default_width(default_w)
        .default_height(default_h)
        .max_size(egui::vec2(max_w, max_h))
        .min_width(420.0)
        .min_height(280.0)
        .resizable(true)
        .collapsible(false)
        .show(ui.ctx(), |ui| {
            if debug_active {
                render_kline_debug_body(ui, tab, &node_id, &session_id.unwrap(), &node_name);
            } else {
                match &cache {
                    None => {
                        ui.vertical_centered(|ui| {
                            ui.add_space(24.0);
                            ui.label("该节点尚无预览数据");
                            ui.add_space(4.0);
                            ui.label("请先执行该算子（右键「运行到此结点」或顶部运行）。");
                        });
                    }
                    Some(data) => render_kline_body(ui, data, &node_id),
                }
            }
        });

    if !open {
        tab.kline_preview_node_id = None;
        // Debug 预览状态清空，避免下次打开其它节点时复用旧状态
        tab.debug_preview = None;
    }
}

fn render_kline_body(ui: &mut Ui, data: &data_preview::PreviewData, node_id: &str) {
    // 顶栏信息
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("节点: {}", data.node_name));
        ui.separator();
        ui.label(format!("保存时间: {}", data.saved_at));
    });
    ui.separator();

    if data.outputs.is_empty() {
        ui.label("该算子无输出数据。");
        return;
    }

    // 找第一个 String 输出
    let dsl_str = data.outputs.iter().find_map(|p| match p {
        PortData::String(s) => Some(s.as_str()),
        _ => None,
    });

    let dsl_str = match dsl_str {
        Some(s) => s,
        None => {
            // 列出输出类型供排查
            let types: Vec<&str> = data.outputs.iter().map(|p| p.type_name()).collect();
            ui.colored_label(
                Color32::from_rgb(231, 76, 60),
                format!("该节点输出非 K线 DSL 字符串（输出类型: {}）", types.join(", ")),
            );
            ui.add_space(6.0);
            ui.label("K线图预览仅适用于「K线可视化算子」节点的输出。");
            return;
        }
    };

    render_dsl_body(ui, dsl_str, node_id);
}

/// 由 DSL 字符串直接渲染 K 线图主体（多图表 ComboBox 切换 + 解析错误展示）。
/// 供本地缓存模式与 Debug 模式共用。
fn render_dsl_body(ui: &mut Ui, dsl_str: &str, node_id: &str) {
    match parse(dsl_str) {
        Err(e) => {
            ui.colored_label(
                Color32::from_rgb(231, 76, 60),
                format!("DSL 解析失败 (行 {}, 列 {}): {}", e.line, e.col, e.message),
            );
            ui.separator();
            ui.label("原始 DSL：");
            egui::ScrollArea::both()
                .id_source("kline_dsl_raw")
                .max_height(ui.available_height().max(120.0))
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut dsl_str.to_string())
                            .font(egui::TextStyle::Monospace)
                            .desired_width(f32::INFINITY)
                            .interactive(false),
                    );
                });
        }
        Ok(doc) => {
            if doc.charts.is_empty() {
                ui.label("DSL 中无 kline 块。");
                return;
            }
            // 多图表 tab 切换
            if doc.charts.len() == 1 {
                ui.heading(&doc.charts[0].title);
                ui.separator();
                render_chart(ui, &doc.charts[0], node_id, 0);
            } else {
                let tab_id = ui.id().with("kline_chart_tab").with(node_id);
                let mut current: usize =
                    ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<usize>(tab_id));
                if current >= doc.charts.len() {
                    current = 0;
                }
                ui.horizontal(|ui| {
                    ui.strong(format!("共 {} 个图表", doc.charts.len()));
                    ui.separator();
                    egui::ComboBox::from_id_source(tab_id)
                        .selected_text(&doc.charts[current].title)
                        .show_ui(ui, |ui| {
                            for (i, ch) in doc.charts.iter().enumerate() {
                                ui.selectable_value(&mut current, i, &ch.title);
                            }
                        });
                });
                ui.ctx().data_mut(|d| *d.get_temp_mut_or_default::<usize>(tab_id) = current);
                ui.separator();
                render_chart(ui, &doc.charts[current], node_id, current);
            }
        }
    }
}

// ============================================================================
// Debug 模式：服务端查询完整 DSL
// ============================================================================

/// 向服务端查询 Debug 会话中某节点的输出元信息。
fn query_meta_from_server(
    session_id: &str,
    node_id: &str,
) -> Result<DebugNodeMeta, String> {
    crate::operator_executor::with_runtime_client(|client| {
        client.query_debug_node_meta(session_id, node_id, DEBUG_PAGE_SIZE)
    })
    .map_err(|e| e.to_string())
}

/// 向服务端查询 Debug 会话中某节点指定端口、指定页的数据切片。
fn query_page_from_server(
    session_id: &str,
    node_id: &str,
    port_idx: usize,
    page_idx: usize,
) -> Result<Option<PortData>, String> {
    crate::operator_executor::with_runtime_client(|client| {
        client.query_debug_node_page(
            session_id,
            node_id,
            port_idx,
            page_idx,
            DEBUG_PAGE_SIZE,
        )
    })
    .map(|p| p.page_data)
    .map_err(|e| e.to_string())
}

/// Debug 模式下 K 线预览主体。
///
/// 两条分支（由参数 `indices` 是否为空决定）：
/// - **传了 indices**：沿用旧逻辑，从 K线算子自身输出端口读取完整 DSL（String）。
/// - **不传 indices**：从上游节点（K线算子输入端口 0 的源节点）输出端口的
///   DebugSession 中按 DataFrameArray 分页查询原始数据，根据算子参数
///   （open_col/high_col/...）在前端按需生成单个 kline 块 DSL 并渲染。
///   用户可通过顶部导航在各 DataFrame 间切换，避免一次性渲染成百上千个 K线图。
fn render_kline_debug_body(
    ui: &mut Ui,
    tab: &mut DagTab,
    node_id: &str,
    session_id: &str,
    node_name: &str,
) {
    // ---- 从节点定义读取参数（每次都读，参数可能改了）----
    let params = extract_kline_params(tab, node_id);
    let use_frontend_render = params.indices.trim().is_empty();

    // ---- 1. 初始化 / 重置状态 ----
    let needs_init = tab
        .debug_preview
        .as_ref()
        .map_or(true, |s| s.node_id != node_id);
    if needs_init {
        tab.debug_preview = Some(DebugPreviewState {
            node_id: node_id.to_string(),
            ..Default::default()
        });
    }

    // ---- 1b. 检测渲染模式切换（indices 空↔非空）----
    // 两条分支查询的节点不同（算子自身 vs 上游），meta / cached_page 类型不兼容。
    // 模式切换时必须清空旧缓存，否则 port_type 校验会误判（如把 String DSL
    // 的 cached_page 当成 DataFrame 使用，导致"无法切换"或显示错误）。
    let mode_key = ui.id().with("kline_debug_render_mode").with(node_id);
    let prev_mode: Option<bool> = ui.ctx().data(|d| d.get_temp::<bool>(mode_key));
    if prev_mode.is_some() && prev_mode != Some(use_frontend_render) {
        if let Some(state) = &mut tab.debug_preview {
            state.meta = None;
            state.cached_page = None;
            state.error = None;
        }
    }
    ui.ctx().data_mut(|d| d.insert_temp(mode_key, use_frontend_render));

    // 顶部信息栏
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("节点: {}", node_name));
        ui.separator();
        ui.colored_label(
            Color32::from_rgb(100, 200, 255),
            format!("Debug 会话: {}…{}", &session_id[..8], &session_id[session_id.len()-4..]),
        );
        ui.separator();
        if use_frontend_render {
            ui.colored_label(
                Color32::from_rgb(88, 166, 120),
                "前端渲染（indices 为空，按 DataFrame 切换）",
            );
        } else {
            ui.colored_label(
                Color32::from_rgb(210, 140, 80),
                format!("算子 DSL（indices={})", params.indices),
            );
        }
    });
    ui.separator();

    if use_frontend_render {
        render_kline_debug_frontend(ui, tab, node_id, session_id, &params);
    } else {
        render_kline_debug_operator(ui, tab, node_id, session_id);
    }
}

// ---------- 分支 A：使用算子输出的 DSL（indices 非空）----------

fn render_kline_debug_operator(
    ui: &mut Ui,
    tab: &mut DagTab,
    node_id: &str,
    session_id: &str,
) {
    // ---- 1. 查询 K线算子节点自身输出 meta ----
    let need_meta = tab
        .debug_preview
        .as_ref()
        .map_or(true, |s| s.meta.is_none() && s.error.is_none());
    if need_meta {
        match query_meta_from_server(session_id, node_id) {
            Ok(meta) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.meta = Some(meta);
                    state.error = None;
                }
            }
            Err(e) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.error = Some(format!("查询节点元信息失败: {}", e));
                }
            }
        }
    }

    let meta_opt = tab.debug_preview.as_ref().and_then(|s| s.meta.clone());
    let error_opt = tab.debug_preview.as_ref().and_then(|s| s.error.clone());

    if let Some(err) = &error_opt {
        ui.colored_label(Color32::from_rgb(231, 76, 60), err);
        ui.add_space(8.0);
        if ui.button("重试").clicked() {
            if let Some(state) = &mut tab.debug_preview {
                state.meta = None;
                state.error = None;
                state.cached_page = None;
            }
        }
        return;
    }

    let meta = match &meta_opt {
        Some(m) => m,
        None => {
            ui.label("正在查询节点元信息...");
            return;
        }
    };

    if meta.port_types.is_empty() {
        ui.colored_label(
            Color32::from_rgb(220, 180, 80),
            "该节点不在 Debug 会话中（可能未执行或执行失败）。请先在 Debug 模式下执行该算子。",
        );
        return;
    }

    let first_type = meta.port_types.first().map(|s| s.as_str()).unwrap_or("");
    if first_type != "String" {
        let types: Vec<&str> = meta.port_types.iter().map(|s| s.as_str()).collect();
        ui.colored_label(
            Color32::from_rgb(231, 76, 60),
            format!("该节点首端口非 String（实际: {}）。K线图预览仅适用于「K线可视化算子」节点。", types.join(", ")),
        );
        return;
    }

    // ---- 2. 查询 port_idx=0, page_idx=0 的完整 DSL ----
    let cache_valid = tab
        .debug_preview
        .as_ref()
        .and_then(|s| s.cached_page.as_ref())
        .map_or(false, |(p, _pg, _)| *p == 0);

    if !cache_valid {
        match query_page_from_server(session_id, node_id, 0, 0) {
            Ok(data) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.cached_page = Some((0, 0, data));
                }
            }
            Err(e) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.cached_page = None;
                    state.error = Some(format!("查询 DSL 数据失败: {}", e));
                }
                ui.colored_label(Color32::from_rgb(231, 76, 60), format!("查询 DSL 数据失败: {}", e));
                return;
            }
        }
    }

    let cached_data = tab
        .debug_preview
        .as_ref()
        .and_then(|s| s.cached_page.as_ref())
        .map(|(_, _, d)| d.clone());

    match cached_data {
        None => {
            ui.label("正在查询 DSL 数据...");
        }
        Some(None) => {
            ui.colored_label(
                Color32::from_rgb(220, 180, 80),
                "该节点无 DSL 数据（端口或页号越界）。",
            );
        }
        Some(Some(PortData::String(dsl))) => {
            ui.horizontal(|ui| {
                ui.label(format!("DSL 长度: {} 字符", dsl.chars().count()));
            });
            ui.separator();
            render_dsl_body(ui, dsl.as_str(), node_id);
        }
        Some(Some(other)) => {
            ui.colored_label(
                Color32::from_rgb(231, 76, 60),
                format!("服务端返回非 String 数据（类型: {}）。", other.type_name()),
            );
        }
    }
}

// ---------- 分支 B：前端直接从上游 DataFrame 生成 DSL（indices 为空）----------

fn render_kline_debug_frontend(
    ui: &mut Ui,
    tab: &mut DagTab,
    node_id: &str,
    session_id: &str,
    params: &KlineFrontendParams,
) {
    // ---- 1. 找到上游节点 ----
    let Some((upstream_id, upstream_port)) = find_upstream_source(tab, node_id, 0) else {
        ui.colored_label(
            Color32::from_rgb(220, 180, 80),
            "K线算子的输入端口未连接，无法获取上游 DataFrame。",
        );
        return;
    };

    // ---- 2. 查询上游节点输出 meta ----
    let need_meta = tab
        .debug_preview
        .as_ref()
        .map_or(true, |s| s.meta.is_none() && s.error.is_none());
    if need_meta {
        match query_meta_from_server(session_id, &upstream_id) {
            Ok(meta) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.meta = Some(meta);
                    state.error = None;
                }
            }
            Err(e) => {
                if let Some(state) = &mut tab.debug_preview {
                    state.error = Some(format!("查询上游节点元信息失败: {}", e));
                }
            }
        }
    }

    let meta_opt = tab.debug_preview.as_ref().and_then(|s| s.meta.clone());
    let error_opt = tab.debug_preview.as_ref().and_then(|s| s.error.clone());

    if let Some(err) = &error_opt {
        ui.colored_label(Color32::from_rgb(231, 76, 60), err);
        ui.add_space(8.0);
        if ui.button("重试").clicked() {
            if let Some(state) = &mut tab.debug_preview {
                state.meta = None;
                state.error = None;
                state.cached_page = None;
            }
        }
        return;
    }

    let meta = match &meta_opt {
        Some(m) => m,
        None => {
            ui.label("正在查询上游节点元信息...");
            return;
        }
    };

    if meta.port_types.is_empty() {
        ui.colored_label(
            Color32::from_rgb(220, 180, 80),
            "上游节点不在 Debug 会话中（可能未执行或执行失败）。请先在 Debug 模式下执行。",
        );
        return;
    }

    let port_type = meta.port_types.get(upstream_port).map(|s| s.as_str()).unwrap_or("");
    let is_dfa = port_type == "DataFrameArray";
    let is_df = port_type == "DataFrame";
    if !is_dfa && !is_df {
        ui.colored_label(
            Color32::from_rgb(231, 76, 60),
            format!(
                "上游节点端口 #{} 非 DataFrameArray/DataFrame（实际: {}）。",
                upstream_port, port_type
            ),
        );
        return;
    }

    // ---- 3. DataFrame 切换导航 ----
    let df_count = if is_dfa {
        meta.port_page_counts.get(upstream_port).copied().unwrap_or(0)
    } else {
        1
    };

    if df_count == 0 {
        ui.colored_label(
            Color32::from_rgb(220, 180, 80),
            "上游无 DataFrame 数据。",
        );
        return;
    }

    let mut current_df = tab
        .debug_preview
        .as_ref()
        .and_then(|s| s.current_pages.get(&upstream_port).copied())
        .unwrap_or(0);
    if current_df >= df_count {
        current_df = 0;
    }

    if df_count > 1 {
        ui.horizontal(|ui| {
            ui.strong(format!("DataFrame 切换 (共 {} 个)", df_count));
            ui.separator();
            if ui.button("‹").on_hover_text("上一个").clicked() {
                current_df = if current_df == 0 { df_count - 1 } else { current_df - 1 };
            }
            ui.label(format!("[{}/{}]", current_df + 1, df_count));
            if ui.button("›").on_hover_text("下一个").clicked() {
                current_df = (current_df + 1) % df_count;
            }
            ui.separator();
            let total_rows = meta.port_row_counts.get(upstream_port).copied().unwrap_or(0);
            ui.colored_label(
                Color32::from_rgb(180, 200, 220),
                format!("合计 {} 行 / {} 个 DataFrame", total_rows, df_count),
            );
        });
        ui.separator();
    }

    // ---- 4. 查询当前 DataFrame（DataFrameArray: page_idx = df_idx；DataFrame: page_idx = 0 全部返回）----
    // 关键修复：把 `current_pages` 写回放在 cache_valid 检查之前，
    // 确保本帧内 state 与 current_df 保持同步，避免重绘时被旧缓存覆盖。
    let page_idx = if is_dfa { current_df } else { 0 };
    let cached_data: Option<Option<PortData>> = {
        // 先用局部 current_df（而不是 state 中的旧值）做缓存命中判断
        let cache_valid = tab
            .debug_preview
            .as_ref()
            .and_then(|s| s.cached_page.as_ref())
            .map_or(false, |(p, pg, _)| *p == upstream_port && *pg == page_idx);

        if cache_valid {
            // 直接用 state 里的当前缓存（命中）
            tab.debug_preview
                .as_ref()
                .and_then(|s| s.cached_page.as_ref())
                .map(|(_, _, d)| d.clone())
        } else {
            // 缓存未命中：同步查询服务端，拿到本帧内唯一可信的 data
            match query_page_from_server(session_id, &upstream_id, upstream_port, page_idx) {
                Ok(data) => {
                    if let Some(state) = &mut tab.debug_preview {
                        state.cached_page = Some((upstream_port, page_idx, data.clone()));
                        state.error = None;
                        // 查询成功后再写回 current_pages，保证 state 与数据一致
                        state.current_pages.insert(upstream_port, current_df);
                    }
                    Some(data)
                }
                Err(e) => {
                    if let Some(state) = &mut tab.debug_preview {
                        state.cached_page = None;
                        state.error = Some(format!("查询 DataFrame 失败: {}", e));
                        // 查询失败：把 current_pages 回滚到上一次成功的位置
                        // 避免下一次重绘时 state 页码领先于实际缓存
                    }
                    ui.colored_label(
                        Color32::from_rgb(231, 76, 60),
                        format!("查询 DataFrame 失败: {}", e),
                    );
                    return;
                }
            }
        }
    };

    let df = match cached_data {
        None => {
            ui.label("正在查询 DataFrame 数据...");
            return;
        }
        Some(None) => {
            ui.colored_label(
                Color32::from_rgb(220, 180, 80),
                format!("DataFrame #{} 不存在（越界）。", current_df),
            );
            return;
        }
        Some(Some(PortData::DataFrame(df))) => df,
        // 防御性兜底：服务端通常返回 DataFrame，但若返回 DataFrameArray 取首个
        Some(Some(PortData::DataFrameArray(dfs))) => {
            if dfs.is_empty() {
                ui.colored_label(
                    Color32::from_rgb(220, 180, 80),
                    "上游返回空 DataFrameArray。",
                );
                return;
            }
            dfs.into_iter().next().unwrap()
        }
        Some(Some(other)) => {
            ui.colored_label(
                Color32::from_rgb(231, 76, 60),
                format!("服务端返回非 DataFrame（类型: {}）。", other.type_name()),
            );
            return;
        }
    };

    // ---- 5. 前端按参数生成 DSL 并渲染 ----
    let title = format!("DataFrame #{}", current_df + 1);
    ui.horizontal(|ui| {
        ui.label(format!(
            "当前 DF: {} 行 × {} 列",
            df.row_count,
            df.columns.len()
        ));
        ui.separator();
        ui.label(format!(
            "列配置: open={} high={} low={} close={}",
            params.open_col, params.high_col, params.low_col, params.close_col
        ));
    });
    ui.separator();

    match emit_chart_frontend(&df, params, &title) {
        Ok(dsl) => {
            // 传入带 df_idx 后缀的 node_id，让每个 DataFrame 拥有独立的缩放/滚动
            // 状态。否则切换 DF 后图表仍用旧 DF 的缩放位置，新蜡烛可能在可见范围
            // 之外，用户看到空白以为"无法切换"。
            let chart_node_id = format!("{}_df{}", node_id, current_df);
            render_dsl_body(ui, dsl.as_str(), &chart_node_id);
        }
        Err(e) => {
            ui.colored_label(
                Color32::from_rgb(231, 76, 60),
                format!("生成 K线 DSL 失败: {}", e),
            );
            ui.add_space(4.0);
            ui.label("请检查列名配置（open_col/high_col/low_col/close_col 必须为 Float64 列）。");
            ui.separator();
            // 列出 DataFrame 的所有列名和类型，方便排查
            ui.label(format!("当前 DataFrame 列列表（共 {} 列）：", df.columns.len()));
            for col in &df.columns {
                let tname = match col.data_type {
                    DataType::Float64 => "Float64",
                    DataType::Int64 => "Int64",
                    DataType::String => "String",
                    DataType::Bool => "Bool",
                    DataType::Null => "Null",
                };
                ui.label(format!("  {} ({})", col.name, tname));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_dsl() {
        let dsl = r##"kline "测试" {
            candle "2024-01-01" 10.0 10.5 9.8 10.3
            candle "2024-01-02" 10.3 10.6 10.2 10.1
            line "MA5" "#FFD700" [ _, _, _, _, 10.1, 10.2 ]
            line "MA10" "#9370DB" [ _, _, 9.5, 9.6 ]
        }"##;
        let doc = parse(dsl).unwrap();
        assert_eq!(doc.charts.len(), 1);
        let ch = &doc.charts[0];
        assert_eq!(ch.title, "测试");
        assert_eq!(ch.candles.len(), 2);
        assert_eq!(ch.candles[0].o, 10.0);
        assert_eq!(ch.ma_lines.len(), 2);
        assert_eq!(ch.ma_lines[0].values.len(), 6);
        assert_eq!(ch.ma_lines[0].values[0], None);
        assert_eq!(ch.ma_lines[0].values[4], Some(10.1));
        assert_eq!(ch.ma_lines[1].color, Color32::from_rgb(147, 112, 219));
    }

    #[test]
    fn parse_multiple_charts_and_comments() {
        let dsl = r##"
        // 第一个图表
        kline "A" { candle "d1" 1.0 2.0 0.5 1.5 }
        kline "B" { candle "d2" 5.0 6.0 4.0 5.5 line "MA5" "#00FF00" [ 5.0 ] }
        "##;
        let doc = parse(dsl).unwrap();
        assert_eq!(doc.charts.len(), 2);
        assert_eq!(doc.charts[1].ma_lines[0].color, Color32::from_rgb(0, 255, 0));
    }

    #[test]
    fn parse_negative_and_scientific() {
        let dsl = r##"kline "X" { candle "d1" -1.5 2.0 -2.0 1e-1 line "M" "#000" [ -3.5, 1.2e2 ] }"##;
        let doc = parse(dsl).unwrap();
        assert_eq!(doc.charts[0].candles[0].o, -1.5);
        assert_eq!(doc.charts[0].candles[0].c, 0.1);
        assert_eq!(doc.charts[0].ma_lines[0].values[1], Some(120.0));
    }

    #[test]
    fn parse_empty_ma_array() {
        let dsl = r##"kline "X" { candle "d1" 1.0 2.0 0.5 1.5 line "M" "#000" [] }"##;
        let doc = parse(dsl).unwrap();
        assert!(doc.charts[0].ma_lines[0].values.is_empty());
    }

    #[test]
    fn parse_error_on_bad_token() {
        let dsl = r#"kline "X" { candle "d1" 1.0 2.0 0.5 1.5 }"#;
        // 缺少 close → 实际这里 close=1.5 合法；构造真正错误
        let bad = r#"kline "X" { badstmt "d1" }"#;
        assert!(parse(bad).is_err());
        let _ = parse(dsl).unwrap();
    }

    #[test]
    fn nice_ticks_basic() {
        let t = nice_ticks(0.0, 10.0, 5);
        assert!(t.contains(&0.0));
        assert!(*t.last().unwrap() <= 10.0);
    }

    #[test]
    fn color_parsing() {
        assert_eq!(parse_color("#FFD700"), Color32::from_rgb(255, 215, 0));
        assert_eq!(parse_color("#9370DB"), Color32::from_rgb(147, 112, 219));
        assert_eq!(parse_color("bad"), Color32::LIGHT_BLUE);
    }
}
