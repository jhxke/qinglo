//! 带行号栏与 Rust 语法高亮的代码编辑器
//!
//! 实现思路:
//! - 用 [`egui::TextEdit`] 作为底层编辑控件, 通过 `layouter` 注入自定义语法高亮.
//! - 在 TextEdit 左侧用 painter 手动绘制行号栏, 二者共享同一个 `ScrollArea`
//!   以保证垂直滚动同步, 行号与代码行一一对应.
//! - 语法高亮使用自带的轻量 Rust 词法分析器, 不引入额外依赖 (syntect 等).

use egui::{Color32, FontId, Galley, ScrollArea, Sense, TextEdit, TextFormat, Ui, Vec2};
use std::sync::Arc;

const FONT_SIZE: f32 = 13.0;
/// 行号栏宽度 (像素)
const GUTTER_WIDTH: f32 = 48.0;
/// 行号文本与栏右边缘的距离 (像素)
const GUTTER_RIGHT_PAD: f32 = 10.0;
/// TextEdit 上下内边距合计, 用于让行号栏与编辑器等高
const EDITOR_VERTICAL_PADDING: f32 = 8.0;
/// 最少显示行数, 避免代码过短时编辑器太矮
const MIN_VISIBLE_ROWS: usize = 20;

/// 渲染带行号与语法高亮的 Rust 代码编辑器
pub fn render_code_editor(ui: &mut Ui, code: &mut String) {
    let font_id = FontId::monospace(FONT_SIZE);
    // 用 '\n' 计数而非 lines(), 否则 "a\n" 末尾的空行不会被计入, 行号会少一行
    let line_count = code.matches('\n').count() + 1;
    let visible_rows = line_count.max(MIN_VISIBLE_ROWS);
    let row_height = ui.fonts(|f| f.row_height(&font_id));

    // 预先布局行号 galley, 与 TextEdit 使用同一字体, 保证行高一致
    let line_numbers: String = (1..=visible_rows)
        .map(|i| format!("{:>3}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let gutter_color = Color32::from_rgb(133, 133, 133);
    let gutter_galley =
        ui.fonts(|f| f.layout_no_wrap(line_numbers, font_id.clone(), gutter_color));

    let editor_height = row_height * visible_rows as f32 + EDITOR_VERTICAL_PADDING;

    // 单一 ScrollArea 同时容纳行号栏与 TextEdit, 二者垂直滚动自然同步
    ScrollArea::vertical()
        .max_height(320.0)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                render_line_gutter(ui, gutter_galley, editor_height, gutter_color);

                // layouter 闭包: 将文本切片交给语法高亮器, 返回带颜色的 galley.
                // 用 move 捕获 font_id, 避免与外部借用纠缠.
                let font_id_for_layout = font_id.clone();
                let mut layouter = move |ui: &Ui, text: &str, _wrap_width: f32| -> Arc<Galley> {
                    highlight_rust(text, ui.ctx(), &font_id_for_layout)
                };

                ui.add_sized(
                    Vec2::new(ui.available_width(), editor_height),
                    TextEdit::multiline(code)
                        .font(font_id.clone())
                        .desired_width(f32::INFINITY)
                        .desired_rows(visible_rows)
                        .layouter(&mut layouter),
                );
            });
        });
}

/// 绘制行号栏: 深色背景 + 右侧分隔线 + 右对齐的行号
fn render_line_gutter(ui: &mut Ui, gutter_galley: Arc<Galley>, height: f32, color: Color32) {
    let (gutter_rect, _) =
        ui.allocate_exact_size(Vec2::new(GUTTER_WIDTH, height), Sense::hover());

    // 行号栏背景
    ui.painter()
        .rect_filled(gutter_rect, 0.0, Color32::from_rgb(28, 28, 28));

    // 右侧分隔线, 与编辑器视觉分离
    ui.painter().line_segment(
        [
            egui::pos2(gutter_rect.right(), gutter_rect.top()),
            egui::pos2(gutter_rect.right(), gutter_rect.bottom()),
        ],
        egui::Stroke::new(1.0, Color32::from_rgb(50, 50, 50)),
    );

    // 行号右对齐; 顶部偏移 4px, 与 TextEdit 默认内边距 [4.0, 2.0] + frame 大致对齐
    let galley_size = gutter_galley.size();
    let text_pos = egui::pos2(
        gutter_rect.right() - GUTTER_RIGHT_PAD - galley_size.x,
        gutter_rect.top() + 4.0,
    );
    ui.painter().galley(text_pos, gutter_galley, color);
}

// ---------------------------------------------------------------------------
// 语法高亮
// ---------------------------------------------------------------------------

/// 将文本按 Rust 语法着色, 返回可被 TextEdit 直接使用的 galley
fn highlight_rust(text: &str, ctx: &egui::Context, font_id: &FontId) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::default();
    // LayoutJob::default() 已是 no-wrap (max_width = INFINITY) 且 break_on_newline = true,
    // 适合代码场景, 无需额外配置 wrap.

    for token in tokenize_rust(text) {
        let color = color_for_kind(token.kind);
        job.append(
            token.text,
            0.0,
            TextFormat::simple(font_id.clone(), color),
        );
    }

    ctx.fonts(|f| f.layout_job(job))
}

#[derive(Clone, Copy, PartialEq)]
enum TokenKind {
    Keyword,
    PrimitiveType,
    Type,
    String,
    Comment,
    Number,
    Function,
    Macro,
    Attribute,
    Lifetime,
    Char,
    Ident,
    Punct,
    Whitespace,
}

struct Token<'a> {
    kind: TokenKind,
    text: &'a str,
}

/// 颜色方案参考 VS Code Dark+ 主题
fn color_for_kind(kind: TokenKind) -> Color32 {
    match kind {
        TokenKind::Keyword => Color32::from_rgb(86, 156, 214),
        TokenKind::PrimitiveType | TokenKind::Type | TokenKind::Lifetime => {
            Color32::from_rgb(78, 201, 176)
        }
        TokenKind::String | TokenKind::Char => Color32::from_rgb(206, 145, 120),
        TokenKind::Comment => Color32::from_rgb(106, 115, 128),
        TokenKind::Number => Color32::from_rgb(181, 206, 168),
        TokenKind::Function => Color32::from_rgb(220, 220, 170),
        TokenKind::Macro => Color32::from_rgb(215, 186, 125),
        TokenKind::Attribute => Color32::from_rgb(155, 185, 85),
        TokenKind::Ident => Color32::from_rgb(212, 212, 212),
        TokenKind::Punct => Color32::from_rgb(180, 180, 180),
        TokenKind::Whitespace => Color32::from_rgb(212, 212, 212),
    }
}

const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub",
    "ref", "return", "self", "Self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where", "while", "union",
];

const PRIMITIVES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32",
    "f64", "bool", "char", "str",
];

const TYPES: &[&str] = &[
    "String", "Vec", "Option", "Result", "Box", "Rc", "Arc", "HashMap", "HashSet", "BTreeMap",
    "BTreeSet", "Cow", "Cell", "RefCell", "Mutex", "RwLock",
];

/// 轻量 Rust 词法分析器: 扫描字符流, 输出 (类型, 文本切片) 序列.
///
/// 使用 `char_indices` 跟踪字节偏移, 正确处理 UTF-8. 不追求 100% 语法正确,
/// 只为编辑器提供合理的视觉着色.
fn tokenize_rust(text: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut i = 0;
    let n = chars.len();
    let byte_len = text.len();

    // 将 char 索引区间转回原字符串的字节切片
    let slice = |start_idx: usize, end_idx: usize| -> &str {
        let start_byte = chars[start_idx].0;
        let end_byte = if end_idx < n {
            chars[end_idx].0
        } else {
            byte_len
        };
        &text[start_byte..end_byte]
    };

    while i < n {
        let start = i;
        let c = chars[i].1;

        // 空白
        if c.is_whitespace() {
            while i < n && chars[i].1.is_whitespace() {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Whitespace,
                text: slice(start, i),
            });
            continue;
        }

        // 行注释 //
        if c == '/' && i + 1 < n && chars[i + 1].1 == '/' {
            while i < n && chars[i].1 != '\n' {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Comment,
                text: slice(start, i),
            });
            continue;
        }

        // 块注释 /* */ (支持嵌套)
        if c == '/' && i + 1 < n && chars[i + 1].1 == '*' {
            i += 2;
            let mut depth = 1;
            while i < n && depth > 0 {
                if chars[i].1 == '/' && i + 1 < n && chars[i + 1].1 == '*' {
                    depth += 1;
                    i += 2;
                } else if chars[i].1 == '*' && i + 1 < n && chars[i + 1].1 == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            tokens.push(Token {
                kind: TokenKind::Comment,
                text: slice(start, i),
            });
            continue;
        }

        // 属性 #[ ... ] / #![ ... ]
        // # 后紧跟 [ 或 ![
        let is_attribute = c == '#'
            && i + 1 < n
            && (chars[i + 1].1 == '['
                || (chars[i + 1].1 == '!' && i + 2 < n && chars[i + 2].1 == '['));
        if is_attribute {
            i += 1; // 跳过 #
            if i < n && chars[i].1 == '!' {
                i += 1;
            }
            if i < n && chars[i].1 == '[' {
                i += 1;
            }
            // 按括号深度匹配到闭合 ]
            let mut depth = 1;
            while i < n && depth > 0 {
                match chars[i].1 {
                    '[' => {
                        depth += 1;
                        i += 1;
                    }
                    ']' => {
                        depth -= 1;
                        i += 1;
                    }
                    _ => {
                        i += 1;
                    }
                }
            }
            tokens.push(Token {
                kind: TokenKind::Attribute,
                text: slice(start, i),
            });
            continue;
        }

        // 标识符 / 关键字 / 字符串前缀
        if c.is_alphabetic() || c == '_' {
            // 原始字符串: r"...", r#"..."#, r##"..."##
            if c == 'r' && i + 1 < n && (chars[i + 1].1 == '"' || chars[i + 1].1 == '#') {
                if let Some(end) = scan_raw_string(&chars, i, n) {
                    i = end;
                    tokens.push(Token {
                        kind: TokenKind::String,
                        text: slice(start, i),
                    });
                    continue;
                }
                // 不是合法原始字符串, 按普通标识符处理
            }
            // 字节字符串: b"...", b'...', br"..."
            if c == 'b' && i + 1 < n {
                let next = chars[i + 1].1;
                if next == '"' {
                    // b"..."
                    i += 2;
                    while i < n && chars[i].1 != '"' {
                        if chars[i].1 == '\\' && i + 1 < n {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    if i < n {
                        i += 1;
                    }
                    tokens.push(Token {
                        kind: TokenKind::String,
                        text: slice(start, i),
                    });
                    continue;
                }
                if next == '\'' {
                    // b'x' 字节字符
                    i += 2;
                    while i < n && chars[i].1 != '\'' {
                        if chars[i].1 == '\\' && i + 1 < n {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    if i < n {
                        i += 1;
                    }
                    tokens.push(Token {
                        kind: TokenKind::Char,
                        text: slice(start, i),
                    });
                    continue;
                }
                if next == 'r' && i + 2 < n && (chars[i + 2].1 == '"' || chars[i + 2].1 == '#') {
                    // br"..." 原始字节字符串
                    if let Some(end) = scan_raw_string(&chars, i + 1, n) {
                        i = end;
                        tokens.push(Token {
                            kind: TokenKind::String,
                            text: slice(start, i),
                        });
                        continue;
                    }
                }
            }

            // 普通标识符
            while i < n && (chars[i].1.is_alphanumeric() || chars[i].1 == '_') {
                i += 1;
            }
            let word = slice(start, i);
            let kind = classify_ident(word, &chars, i, n);
            tokens.push(Token { kind, text: word });
            continue;
        }

        // 普通字符串 "..."
        if c == '"' {
            i += 1;
            while i < n && chars[i].1 != '"' {
                if chars[i].1 == '\\' && i + 1 < n {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            if i < n {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::String,
                text: slice(start, i),
            });
            continue;
        }

        // 字符 'x' 或 生命周期 'a
        if c == '\'' {
            if let Some(end) = scan_char_or_lifetime(&chars, i, n) {
                let kind = if chars[end - 1].1 == '\'' {
                    TokenKind::Char
                } else {
                    TokenKind::Lifetime
                };
                i = end;
                tokens.push(Token {
                    kind,
                    text: slice(start, i),
                });
                continue;
            }
            i += 1;
            tokens.push(Token {
                kind: TokenKind::Punct,
                text: slice(start, i),
            });
            continue;
        }

        // 数字
        if c.is_ascii_digit() {
            // 0x / 0o / 0b 前缀
            if c == '0' && i + 1 < n {
                let next = chars[i + 1].1;
                if next == 'x' || next == 'o' || next == 'b' {
                    i += 2;
                    while i < n && (chars[i].1.is_ascii_alphanumeric() || chars[i].1 == '_') {
                        i += 1;
                    }
                    tokens.push(Token {
                        kind: TokenKind::Number,
                        text: slice(start, i),
                    });
                    continue;
                }
            }
            // 十进制 / 浮点
            while i < n && (chars[i].1.is_ascii_digit() || chars[i].1 == '_') {
                i += 1;
            }
            if i < n && chars[i].1 == '.' && i + 1 < n && chars[i + 1].1.is_ascii_digit() {
                i += 1;
                while i < n && (chars[i].1.is_ascii_digit() || chars[i].1 == '_') {
                    i += 1;
                }
            }
            if i < n && (chars[i].1 == 'e' || chars[i].1 == 'E') {
                i += 1;
                if i < n && (chars[i].1 == '+' || chars[i].1 == '-') {
                    i += 1;
                }
                while i < n && chars[i].1.is_ascii_digit() {
                    i += 1;
                }
            }
            // 类型后缀: 1u32, 1.0f64
            while i < n && chars[i].1.is_alphanumeric() {
                i += 1;
            }
            tokens.push(Token {
                kind: TokenKind::Number,
                text: slice(start, i),
            });
            continue;
        }

        // 其它标点
        i += 1;
        tokens.push(Token {
            kind: TokenKind::Punct,
            text: slice(start, i),
        });
    }

    tokens
}

/// 判断标识符的 token 类型, 并向前看一位识别函数调用 / 宏
fn classify_ident(word: &str, chars: &[(usize, char)], i: usize, n: usize) -> TokenKind {
    if KEYWORDS.contains(&word) {
        return TokenKind::Keyword;
    }
    if PRIMITIVES.contains(&word) {
        return TokenKind::PrimitiveType;
    }
    if TYPES.contains(&word) {
        return TokenKind::Type;
    }
    // 跳过空白向前看一个非空白字符
    let mut j = i;
    while j < n && chars[j].1.is_whitespace() {
        j += 1;
    }
    if j < n {
        match chars[j].1 {
            '!' => return TokenKind::Macro,
            '(' => return TokenKind::Function,
            // 泛型或类型上下文: ident<  或  ident::  视作类型
            '<' | ':' => return TokenKind::Type,
            _ => {}
        }
    }
    TokenKind::Ident
}

/// 扫描原始字符串 r"...", r#"..."#, 返回结束后的 char 索引
fn scan_raw_string(chars: &[(usize, char)], start: usize, n: usize) -> Option<usize> {
    // 跳过前缀 r
    let mut i = start + 1;
    let mut hash_count = 0;
    while i < n && chars[i].1 == '#' {
        hash_count += 1;
        i += 1;
    }
    if i >= n || chars[i].1 != '"' {
        return None;
    }
    i += 1; // 跳过开引号
    // 寻找闭引号后跟相同数量的 #
    while i < n {
        if chars[i].1 == '"' {
            let mut k = i + 1;
            let mut matched = 0;
            while k < n && matched < hash_count && chars[k].1 == '#' {
                matched += 1;
                k += 1;
            }
            if matched == hash_count {
                return Some(k);
            }
        }
        i += 1;
    }
    // 未闭合, 取到末尾
    Some(n)
}

/// 扫描字符 'x' 或 生命周期 'a. 返回结束后的 char 索引.
/// 通过是否以闭合 ' 结束来区分二者.
fn scan_char_or_lifetime(chars: &[(usize, char)], start: usize, n: usize) -> Option<usize> {
    // start 指向开 '
    let mut i = start + 1;
    if i >= n {
        return None;
    }
    let next = chars[i].1;
    if next == '\\' {
        // 转义字符 '\n', '\u{...}' 等
        i += 1;
        if i < n && chars[i].1 == 'u' {
            // \u{XXXX}
            i += 1;
            if i < n && chars[i].1 == '{' {
                i += 1;
                while i < n && chars[i].1 != '}' {
                    i += 1;
                }
                if i < n {
                    i += 1; // 跳过 }
                }
            }
        } else if i < n {
            i += 1; // 跳过转义字符
        }
        // 期望闭合 '
        if i < n && chars[i].1 == '\'' {
            return Some(i + 1);
        }
        return Some(i);
    }
    if next.is_alphabetic() || next == '_' {
        // 'x' 单字符, 或 'ident 生命周期
        if i + 1 < n && chars[i + 1].1 == '\'' {
            return Some(i + 2); // 闭合的 char
        }
        // 生命周期: 消费后续 ident 字符
        i += 1;
        while i < n && (chars[i].1.is_alphanumeric() || chars[i].1 == '_') {
            i += 1;
        }
        return Some(i);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_keywords_and_strings() {
        let code = "fn main() { let s = \"hello\"; }";
        let tokens = tokenize_rust(code);
        let kinds: Vec<_> = tokens.iter().map(|t| t.kind).collect();
        assert!(kinds.contains(&TokenKind::Keyword)); // fn, let
        assert!(kinds.contains(&TokenKind::Function)); // main
        assert!(kinds.contains(&TokenKind::String)); // "hello"
    }

    #[test]
    fn tokenize_comments() {
        let code = "// line comment\nlet x = 1; /* block */";
        let tokens = tokenize_rust(code);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Comment));
    }

    #[test]
    fn tokenize_raw_string() {
        // 用普通字符串 + 转义, 避免 r#"..."# 嵌套问题
        let code = "let s = r#\"raw\"#;";
        let tokens = tokenize_rust(code);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::String));
    }

    #[test]
    fn tokenize_numbers() {
        let code = "let x = 42; let y = 3.14; let z = 0xFF_u8;";
        let tokens = tokenize_rust(code);
        let count = tokens.iter().filter(|t| t.kind == TokenKind::Number).count();
        assert_eq!(count, 3);
    }

    #[test]
    fn tokenize_lifetime_vs_char() {
        let code = "let c = 'x'; fn f<'a>(x: &'a i32) {}";
        let tokens = tokenize_rust(code);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Char));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Lifetime));
    }

    #[test]
    fn tokenize_macro_and_attribute() {
        let code = "#[derive(Debug)]\nprintln!(\"hi\");";
        let tokens = tokenize_rust(code);
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Attribute));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::Macro));
    }

    #[test]
    fn line_count_handles_trailing_newline() {
        // 编辑器中 "a\n" 应显示两行 (第二行为空)
        assert_eq!("a".matches('\n').count() + 1, 1);
        assert_eq!("a\n".matches('\n').count() + 1, 2);
        assert_eq!("a\nb".matches('\n').count() + 1, 2);
    }
}
