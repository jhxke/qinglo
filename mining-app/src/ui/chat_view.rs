//! 聊天 DSL 渲染引擎。
//!
//! 由 chat_visualization_operator 输出的文本 DSL（`chat "标题" { user "...";
//! assistant "..."; status streaming; token_count N }`）经轻量词法器 + 递归下降
//! 解析器转为 AST，再用 egui 渲染为聊天气泡界面：
//! - `user` 消息：右对齐蓝底气泡
//! - `assistant` 消息：左对齐灰底气泡；`status=streaming` 时尾部显示闪烁「打字机」光标
//! - 状态徽标、token 计数展示在顶栏
//!
//! 入口 [`render_chat_preview_window`]：读取节点预览缓存中的第一个
//! `PortData::String` 输出，解析为 [`ChatDoc`] 后渲染。

use egui::{Color32, Frame, Margin, RichText, ScrollArea, Stroke, Ui};
use operator_executor_client::PortData;

use super::state::DagTab;
use crate::data_preview;

// =============================== 词法器 ===============================

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Ident(String),
    Str(String),
    Num(u64), // chat DSL 中 token_count 是非负整数，统一用 u64
    LBrace,
    RBrace,
    Eof,
}

#[derive(Debug, Clone, Copy)]
struct TokenPos {
    line: usize,
    col: usize,
}

struct Lexer<'a> {
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
    line: usize,
    col: usize,
}

impl<'a> Lexer<'a> {
    fn new(input: &'a str) -> Self {
        Lexer {
            chars: input.char_indices().peekable(),
            line: 1,
            col: 1,
        }
    }

    fn skip_ws_and_comments(&mut self) {
        while let Some(&(i, ch)) = self.chars.peek() {
            if ch.is_whitespace() {
                self.advance(i, ch);
            } else if ch == '/' {
                let mut it = self.chars.clone();
                it.next();
                if let Some(&(_, '/')) = it.peek() {
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

        match ch {
            '{' => { self.advance(i, ch); return Ok((Token::LBrace, pos)); }
            '}' => { self.advance(i, ch); return Ok((Token::RBrace, pos)); }
            '"' => return self.lex_string(pos),
            _ => {}
        }

        if ch.is_alphabetic() || ch == '_' {
            return Ok((Token::Ident(self.lex_ident()), pos));
        }

        if ch.is_ascii_digit() {
            return self.lex_number(pos);
        }

        Err(ParseError {
            message: format!("非法字符 '{}'", ch),
            line: pos.line,
            col: pos.col,
        })
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
        let mut val: u64 = 0;
        while let Some(&(i, ch)) = self.chars.peek() {
            if let Some(d) = ch.to_digit(10) {
                val = val
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(d as u64))
                    .ok_or_else(|| ParseError {
                        message: "数字溢出 u64".to_string(),
                        line: pos.line,
                        col: pos.col,
                    })?;
                self.advance(i, ch);
            } else {
                break;
            }
        }
        Ok((Token::Num(val), pos))
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

#[derive(Debug, Clone)]
pub struct ParseError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

/// DSL 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatStatus {
    Streaming,
    Done,
    Error,
    /// 解析失败时的兜底，前端渲染成灰色问号
    Unknown,
}

#[derive(Debug, Clone)]
pub struct ChatDoc {
    pub title: String,
    pub user: Option<String>,
    pub assistant: String,
    pub status: ChatStatus,
    pub token_count: u64,
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
        if std::mem::discriminant(self.peek()) == std::mem::discriminant(expected)
            && self.peek() == expected
        {
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

    fn expect_number(&mut self) -> Result<u64, ParseError> {
        match self.peek().clone() {
            Token::Num(n) => {
                self.advance();
                Ok(n)
            }
            other => self.err(format!("期望整数，实际 {:?}", other)),
        }
    }

    fn parse_doc(&mut self) -> Result<ChatDoc, ParseError> {
        self.expect_ident("chat")?;
        // 标题是字符串（必需）
        let title = self.expect_string()?;
        self.expect(&Token::LBrace)?;

        let mut user: Option<String> = None;
        let mut assistant: String = String::new();
        let mut status: ChatStatus = ChatStatus::Unknown;
        let mut token_count: u64 = 0;

        while !matches!(self.peek(), Token::RBrace) {
            match self.peek() {
                Token::Ident(s) if s == "user" => {
                    self.advance();
                    let s = self.expect_string()?;
                    if user.is_none() {
                        user = Some(s);
                    } else {
                        return self.err("重复的 'user' 语句");
                    }
                }
                Token::Ident(s) if s == "assistant" => {
                    self.advance();
                    assistant = self.expect_string()?;
                }
                Token::Ident(s) if s == "status" => {
                    self.advance();
                    // status 可以是 identifier (streaming/done/error) 或字符串
                    match self.advance() {
                        Token::Ident(ident) => {
                            status = match ident.as_str() {
                                "streaming" => ChatStatus::Streaming,
                                "done" => ChatStatus::Done,
                                "error" => ChatStatus::Error,
                                other => {
                                    return self.err(format!(
                                        "未知 status 标识符 '{}'，期望 streaming/done/error",
                                        other
                                    ));
                                }
                            };
                        }
                        Token::Str(s) => {
                            status = match s.as_str() {
                                "streaming" => ChatStatus::Streaming,
                                "done" => ChatStatus::Done,
                                "error" => ChatStatus::Error,
                                _ => ChatStatus::Unknown,
                            };
                        }
                        other => {
                            return self.err(format!(
                                "status 后期望标识符或字符串，实际 {:?}",
                                other
                            ));
                        }
                    }
                }
                Token::Ident(s) if s == "token_count" => {
                    self.advance();
                    token_count = self.expect_number()?;
                }
                other => {
                    return self.err(format!(
                        "语句期望 'user'/'assistant'/'status'/'token_count'，实际 {:?}",
                        other
                    ));
                }
            }
        }
        self.expect(&Token::RBrace)?;
        Ok(ChatDoc { title, user, assistant, status, token_count })
    }
}

pub fn parse(input: &str) -> Result<ChatDoc, ParseError> {
    let tokens = lex(input)?;
    let mut parser = Parser { tokens, pos: 0 };
    parser.parse_doc()
}

// =============================== 渲染 ===============================

/// 颜色常量
const USER_BG: Color32 = Color32::from_rgb(59, 130, 246); // 蓝
const USER_FG: Color32 = Color32::from_rgb(248, 250, 252); // 浅白
const ASSISTANT_BG: Color32 = Color32::from_rgb(55, 65, 81); // 灰
const ASSISTANT_FG: Color32 = Color32::from_rgb(229, 231, 235); // 浅灰
const STATUS_STREAMING: Color32 = Color32::from_rgb(16, 185, 129); // 绿
const STATUS_DONE: Color32 = Color32::from_rgb(59, 130, 246); // 蓝
const STATUS_ERROR: Color32 = Color32::from_rgb(239, 68, 68); // 红
const STATUS_UNKNOWN: Color32 = Color32::from_rgb(156, 163, 175); // 灰

fn status_color(s: ChatStatus) -> Color32 {
    match s {
        ChatStatus::Streaming => STATUS_STREAMING,
        ChatStatus::Done => STATUS_DONE,
        ChatStatus::Error => STATUS_ERROR,
        ChatStatus::Unknown => STATUS_UNKNOWN,
    }
}

fn status_text(s: ChatStatus) -> &'static str {
    match s {
        ChatStatus::Streaming => "生成中",
        ChatStatus::Done => "已完成",
        ChatStatus::Error => "错误",
        ChatStatus::Unknown => "未知",
    }
}

/// 聊天气泡：user 右对齐蓝底，assistant 左对齐灰底；可选中复制正文；
/// `show_cursor=true` 时在 assistant 末尾加闪烁「▍」打字机光标。
fn bubble_v2(ui: &mut Ui, role_label: &str, text: &str, is_user: bool, show_cursor: bool) {
    // 外层水平布局控制对齐
    ui.horizontal(|ui| {
        if is_user {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::TOP), |ui| {
                draw_bubble_content(ui, role_label, text, USER_BG, USER_FG, true, show_cursor);
                ui.add_space(8.0);
            });
        } else {
            ui.add_space(8.0);
            draw_bubble_content(ui, role_label, text, ASSISTANT_BG, ASSISTANT_FG, false, show_cursor);
        }
    });
    ui.add_space(8.0);
}

fn draw_bubble_content(
    ui: &mut Ui,
    role_label: &str,
    text: &str,
    bg: Color32,
    fg: Color32,
    _is_user: bool,
    show_cursor: bool,
) {
    let max_w = (ui.available_width() * 0.85).max(260.0);
    Frame::none()
        .fill(bg)
        .inner_margin(Margin::symmetric(12.0, 10.0))
        .rounding(12.0)
        .show(ui, |ui| {
            ui.set_max_width(max_w);
            // 角色标签
            ui.label(
                RichText::new(role_label)
                    .size(11.0)
                    .color(Color32::from_rgba_unmultiplied(255, 255, 255, 190))
                    .strong(),
            );
            ui.add_space(3.0);

            if text.is_empty() && !show_cursor {
                ui.label(
                    RichText::new("（等待内容…）")
                        .size(13.0)
                        .color(Color32::from_rgba_unmultiplied(255, 255, 255, 120))
                        .italics(),
                );
                return;
            }

            // 显示正文 + 可选闪烁光标（每 ~0.5s 切换，60fps 约 30 帧）
            let frame = ui.ctx().frame_nr();
            let show_blink = show_cursor && (frame % 60 < 30);

            // 文本拆开：末尾光标渲染为独立形状（RichText 里难以做到「和字等高的块状光标」）
            // 所以这里用 Label 渲染整段（可选中复制），另外在末尾附加一个单独的小色块 ▍
            if !text.is_empty() {
                // Label 可选中复制；颜色用 fg；显式 .wrap(true) 强制在气泡宽度内折行
                // （egui 0.26 的 wrap 支持按空白与 CJK 字符级断行，长无空串也会换行）
                ui.add(
                    egui::Label::new(
                        RichText::new(text)
                            .size(13.5)
                            .color(fg),
                    )
                    .wrap(true),
                );
            }
            if show_blink {
                // 把光标作为单独的 Label 放在同一行
                ui.label(
                    RichText::new("▍")
                        .size(15.0)
                        .color(Color32::WHITE)
                        .strong(),
                );
                // 强制下一帧重绘，让光标真的闪烁（否则 egui 会认为无脏数据跳帧）
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        });
}

// =============================== 预览窗口 ===============================

/// 渲染聊天预览浮动窗口。`tab.chat_preview_node_id` 为 None 时直接返回。
pub fn render_chat_preview_window(ui: &mut Ui, tab: &mut DagTab) {
    let node_id = match tab.chat_preview_node_id.clone() {
        Some(id) => id,
        None => return,
    };

    let cache = data_preview::load_preview_cache(&node_id);
    let graph_name = tab
        .graph
        .get_node(&node_id)
        .map(|n| n.operator_type.name().to_string());
    let node_name = cache
        .as_ref()
        .map(|c| c.node_name.clone())
        .filter(|n| !n.is_empty())
        .or(graph_name)
        .unwrap_or_else(|| node_id.clone());

    let mut open = true;
    let title = format!("聊天预览 - {}", node_name);

    let screen = ui.ctx().screen_rect();
    let max_w = (screen.width() * 0.85).max(420.0);
    let max_h = (screen.height() * 0.85).max(360.0);
    let default_w = 640.0f32.min(max_w);
    let default_h = 560.0f32.min(max_h);

    egui::Window::new(title)
        .open(&mut open)
        .default_width(default_w)
        .default_height(default_h)
        .max_size(egui::vec2(max_w, max_h))
        .min_width(360.0)
        .min_height(280.0)
        .resizable(true)
        .collapsible(false)
        .show(ui.ctx(), |ui| {
            match &cache {
                None => {
                    ui.vertical_centered(|ui| {
                        ui.add_space(24.0);
                        ui.label("该节点尚无预览数据");
                        ui.add_space(4.0);
                        ui.label("请先执行该算子（右键「运行到此结点」或顶部运行）。");
                    });
                }
                Some(data) => render_chat_body(ui, data, &node_id),
            }
        });

    if !open {
        tab.chat_preview_node_id = None;
    }
}

fn render_chat_body(ui: &mut Ui, data: &data_preview::PreviewData, node_id: &str) {
    // 顶栏：节点 + 时间
    ui.horizontal_wrapped(|ui| {
        ui.strong(format!("节点: {}", data.node_name));
        ui.separator();
        ui.label(format!("保存时间: {}", data.saved_at));
    });

    if data.outputs.is_empty() {
        ui.label("该算子无输出数据。");
        return;
    }

    let dsl_str = data.outputs.iter().find_map(|p| match p {
        PortData::String(s) => Some(s.as_str()),
        _ => None,
    });

    let dsl_str = match dsl_str {
        Some(s) => s,
        None => {
            let types: Vec<&str> = data.outputs.iter().map(|p| p.type_name()).collect();
            ui.colored_label(
                Color32::from_rgb(231, 76, 60),
                format!("该节点输出非聊天 DSL 字符串（输出类型: {}）", types.join(", ")),
            );
            ui.add_space(6.0);
            ui.label("聊天预览仅适用于「DSL流式对话展示」算子节点的输出。");
            return;
        }
    };

    match parse(dsl_str) {
        Err(e) => {
            ui.colored_label(
                Color32::from_rgb(231, 76, 60),
                format!("DSL 解析失败 (行 {}, 列 {}): {}", e.line, e.col, e.message),
            );
            ui.add_space(4.0);
            // 针对常见误操作给出更明确的提示：
            // 1) 对 ollama 节点点了聊天预览（此时输出为纯文本而非 DSL）
            // 2) 上游没有接 DSL流式对话展示 算子
            ui.label(
                RichText::new("💡 可能原因与解决方式：")
                    .size(12.5)
                    .strong()
                    .color(Color32::from_rgb(243, 156, 18)),
            );
            ui.indent("chat_dsl_hint", |ui| {
                ui.label(format!(
                    "① 确认当前节点是「DSL流式对话展示」算子，而不是上游的「{}」等原始输出节点。",
                    data.node_name
                ));
                ui.label("② 正确的连线方式：ollama流式对话.output  →  DSL流式对话展示.tokens");
                ui.label("   （可选再连一条：用户问题文本  →  DSL流式对话展示.prompt）");
                ui.label("③ 右键「DSL流式对话展示」算子节点 → 选择「聊天预览」，即可看到聊天气泡。");
            });
            ui.separator();
            ui.label("节点当前保存的原始输出（非 DSL）：");
            ScrollArea::both()
                .id_source(("chat_dsl_raw", node_id))
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
            // 标题 + 状态栏
            ui.horizontal(|ui| {
                ui.heading(&doc.title);
                ui.separator();
                // status 徽标
                Frame::none()
                    .fill(status_color(doc.status).linear_multiply(0.18))
                    .inner_margin(Margin::symmetric(8.0, 2.0))
                    .rounding(4.0)
                    .stroke(Stroke::new(1.0, status_color(doc.status)))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(status_text(doc.status))
                                .size(11.0)
                                .color(status_color(doc.status))
                                .strong(),
                        );
                    });
                ui.separator();
                ui.label(
                    RichText::new(format!("tokens: {}", doc.token_count))
                        .size(12.0)
                        .color(Color32::from_rgb(180, 180, 200)),
                );
                // 如果是 streaming，持续触发重绘以刷新闪烁光标
                if doc.status == ChatStatus::Streaming {
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
                }
            });
            ui.separator();

            // 聊天气泡区域（垂直滚动）
            let inner_id = ui.id().with("chat_scroll").with(node_id);
            ScrollArea::vertical()
                .id_source(inner_id)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // 让整个滚动区都有间距，避免气泡贴边缘
                    Frame::none()
                        .inner_margin(Margin::symmetric(4.0, 6.0))
                        .show(ui, |ui| {
                            ui.vertical(|ui| {
                                if let Some(u) = &doc.user {
                                    bubble_v2(ui, "用户", u, true, false);
                                }
                                let show_cursor = doc.status == ChatStatus::Streaming;
                                bubble_v2(ui, "助手", &doc.assistant, false, show_cursor);
                            });
                        });
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_doc() {
        let dsl = r##"chat "对话标题" {
  user "用户的问题"
  assistant "助手回答\n换行"
  status streaming
  token_count 42
}"##;
        let doc = parse(dsl).unwrap();
        assert_eq!(doc.title, "对话标题");
        assert_eq!(doc.user.as_deref(), Some("用户的问题"));
        assert_eq!(doc.assistant, "助手回答\n换行");
        assert_eq!(doc.status, ChatStatus::Streaming);
        assert_eq!(doc.token_count, 42);
    }

    #[test]
    fn parse_no_user() {
        let dsl = r#"chat "T" { assistant "hi" status done token_count 2 }"#;
        let doc = parse(dsl).unwrap();
        assert!(doc.user.is_none());
        assert_eq!(doc.assistant, "hi");
        assert_eq!(doc.status, ChatStatus::Done);
    }

    #[test]
    fn parse_status_string_form() {
        let dsl = r#"chat "x" { assistant "" status "error" token_count 0 }"#;
        let doc = parse(dsl).unwrap();
        assert_eq!(doc.status, ChatStatus::Error);
    }

    #[test]
    fn parse_empty_assistant_and_status_unknown() {
        let dsl = r#"chat "x" { assistant "" status "weird" token_count 0 }"#;
        let doc = parse(dsl).unwrap();
        assert_eq!(doc.status, ChatStatus::Unknown);
        assert!(doc.assistant.is_empty());
    }

    #[test]
    fn parse_rejects_bad_token() {
        let bad = r#"chat "x" { garbage "" }"#;
        assert!(parse(bad).is_err());
    }

    #[test]
    fn parse_unterminated_string_errors() {
        let bad = r#"chat "x { assistant "" status done token_count 0 }"#;
        // 第一个字符串 "x 未闭合
        assert!(parse(bad).is_err());
    }

    #[test]
    fn parse_escape_sequences() {
        let dsl = r##"chat "t" { assistant "a\"b\\c\nd\re\tf" status streaming token_count 0 }"##;
        let doc = parse(dsl).unwrap();
        assert_eq!(doc.assistant, "a\"b\\c\nd\re\tf");
    }

    #[test]
    fn status_texts_and_colors_are_distinct() {
        let all = [
            (ChatStatus::Streaming, "生成中", STATUS_STREAMING),
            (ChatStatus::Done, "已完成", STATUS_DONE),
            (ChatStatus::Error, "错误", STATUS_ERROR),
            (ChatStatus::Unknown, "未知", STATUS_UNKNOWN),
        ];
        for (s, t, c) in all {
            assert_eq!(status_text(s), t);
            assert_eq!(status_color(s), c);
        }
    }
}
