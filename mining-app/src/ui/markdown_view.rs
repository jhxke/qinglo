//! 轻量级 Markdown 渲染器（egui）。
//!
//! 用于在「算子运行参数」面板中以阅读模式展示算子的 `description_md`，
//! 帮助用户快速了解算子用法与示例，降低上手难度。
//!
//! 实现为逐行解析的块级解析器 + 行内片段解析器，不依赖外部 markdown
//! crate，保持依赖与二进制体积精简。支持的语法：
//! - 标题 `#`~`######`
//! - 段落、粗体 `**b**`、斜体 `*i*`、行内代码 `` `c` ``
//! - 无序列表 `- ` / `* `、有序列表 `1. `
//! - 引用 `> `、水平线 `---`、代码块 ``` ``` ```
//! - 表格 `| a | b |`

use egui::{Color32, Frame, Margin, RichText, Stroke, Ui};
use super::theme;

/// 行内片段样式
#[derive(Debug, Clone)]
enum Seg {
    Normal(String),
    Bold(String),
    Italic(String),
    Code(String),
}

/// 在「阅读模式」下渲染 Markdown 内容。
///
/// 不再自带外层卡片容器——由调用方（算子参数编辑器的 `card_frame`）提供统一的
/// 功能区域背景，避免出现与卡片底色不同的第三种背景色。
pub fn render_markdown(ui: &mut Ui, md: &str) {
    if md.trim().is_empty() {
        ui.label(RichText::new("（暂无详细说明）").weak().small());
        return;
    }

    render_blocks(ui, md);
}

fn render_blocks(ui: &mut Ui, md: &str) {
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;
    let mut tbl_idx: u64 = 0;
    while i < lines.len() {
        let line = lines[i];

        // 代码块
        if code_fence_len(line).is_some() {
            i += 1;
            let mut code_lines: Vec<&str> = Vec::new();
            while i < lines.len() && code_fence_len(lines[i]).is_none() {
                code_lines.push(lines[i]);
                i += 1;
            }
            if i < lines.len() {
                i += 1; // 跳过闭合 ```
            }
            render_code_block(ui, &code_lines.join("\n"));
            continue;
        }

        // 标题
        if let Some((level, text)) = strip_heading(line) {
            render_heading(ui, level, text);
            i += 1;
            continue;
        }

        // 水平线
        if is_hr(line) {
            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);
            i += 1;
            continue;
        }

        // 表格（连续的 | 开头行）
        if line.trim_start().starts_with('|') {
            let mut table_lines: Vec<&str> = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                table_lines.push(lines[i]);
                i += 1;
            }
            render_table(ui, &table_lines, tbl_idx);
            tbl_idx += 1;
            continue;
        }

        // 引用
        if let Some(q) = strip_blockquote(line) {
            let mut buf = q.to_string();
            i += 1;
            while i < lines.len() {
                if let Some(qq) = strip_blockquote(lines[i]) {
                    buf.push('\n');
                    buf.push_str(qq);
                    i += 1;
                } else {
                    break;
                }
            }
            render_blockquote(ui, &buf);
            continue;
        }

        // 无序列表
        if let Some(text) = strip_unordered_item(line) {
            let mut items: Vec<&str> = Vec::new();
            items.push(text);
            i += 1;
            while i < lines.len() {
                if let Some(t) = strip_unordered_item(lines[i]) {
                    items.push(t);
                    i += 1;
                } else {
                    break;
                }
            }
            render_unordered_list(ui, &items);
            continue;
        }

        // 有序列表
        if let Some((num, text)) = strip_ordered_item(line) {
            let mut items: Vec<(String, &str)> = Vec::new();
            items.push((num, text));
            i += 1;
            while i < lines.len() {
                if let Some((n, t)) = strip_ordered_item(lines[i]) {
                    items.push((n, t));
                    i += 1;
                } else {
                    break;
                }
            }
            render_ordered_list(ui, &items);
            continue;
        }

        // 空行
        if line.trim().is_empty() {
            ui.add_space(6.0);
            i += 1;
            continue;
        }

        // 段落：聚合连续的普通行
        let mut para = String::from(line);
        i += 1;
        while i < lines.len() {
            let l = lines[i];
            if l.trim().is_empty()
                || code_fence_len(l).is_some()
                || strip_heading(l).is_some()
                || is_hr(l)
                || l.trim_start().starts_with('|')
                || strip_blockquote(l).is_some()
                || strip_unordered_item(l).is_some()
                || strip_ordered_item(l).is_some()
            {
                break;
            }
            para.push('\n');
            para.push_str(l);
            i += 1;
        }
        render_paragraph(ui, &para);
    }
}

// ===== 行内解析与渲染 =====

fn parse_inline(text: &str) -> Vec<Seg> {
    let chars: Vec<char> = text.chars().collect();
    let mut segs: Vec<Seg> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;
    let n = chars.len();
    let flush = |buf: &mut String, segs: &mut Vec<Seg>| {
        if !buf.is_empty() {
            segs.push(Seg::Normal(std::mem::take(buf)));
        }
    };
    while i < n {
        // 行内代码 `...`
        if chars[i] == '`' {
            flush(&mut buf, &mut segs);
            let start = i + 1;
            let mut j = start;
            while j < n && chars[j] != '`' {
                j += 1;
            }
            let code: String = chars[start..j].iter().collect();
            segs.push(Seg::Code(code));
            i = if j < n { j + 1 } else { j };
            continue;
        }
        // 粗体 **...**
        if i + 1 < n && chars[i] == '*' && chars[i + 1] == '*' {
            flush(&mut buf, &mut segs);
            let start = i + 2;
            let mut j = start;
            while j + 1 < n && !(chars[j] == '*' && chars[j + 1] == '*') {
                j += 1;
            }
            let inner: String = chars[start..j].iter().collect();
            segs.push(Seg::Bold(inner));
            i = if j + 1 < n { j + 2 } else { n };
            continue;
        }
        // 斜体 *...*
        if chars[i] == '*' {
            flush(&mut buf, &mut segs);
            let start = i + 1;
            let mut j = start;
            while j < n && chars[j] != '*' {
                j += 1;
            }
            let inner: String = chars[start..j].iter().collect();
            segs.push(Seg::Italic(inner));
            i = if j < n { j + 1 } else { j };
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut buf, &mut segs);
    segs
}

/// 在一行内渲染行内片段（可自动换行）。
fn render_inline(ui: &mut Ui, text: &str, base_color: Color32, size: f32) {
    let segs = parse_inline(text);
    if segs.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for seg in segs {
            match seg {
                Seg::Normal(s) => {
                    ui.label(RichText::new(s).color(base_color).size(size));
                }
                Seg::Bold(s) => {
                    ui.label(RichText::new(s).strong().color(theme::TEXT_STRONG).size(size));
                }
                Seg::Italic(s) => {
                    ui.label(RichText::new(s).italics().color(base_color).size(size));
                }
                Seg::Code(s) => {
                    Frame::none()
                        .fill(Color32::from_rgb(45, 45, 45))
                        .inner_margin(Margin::symmetric(4.0, 1.0))
                        .rounding(3.0)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(s)
                                    .monospace()
                                    .color(Color32::from_rgb(220, 180, 120))
                                    .size(size),
                            );
                        });
                }
            }
        }
    });
}

// ===== 块级渲染 =====

fn render_heading(ui: &mut Ui, level: u8, text: &str) {
    ui.add_space(6.0);
    let (size, color) = match level {
        1 => (20.0, theme::TEXT_STRONG),
        2 => (17.0, theme::TEXT_STRONG),
        3 => (15.0, theme::TEXT_HOVER),
        _ => (13.5, theme::TEXT_HOVER),
    };
    ui.label(RichText::new(text.trim()).strong().size(size).color(color));
    if level <= 2 {
        ui.add_space(2.0);
        ui.separator();
    }
    ui.add_space(2.0);
}

fn render_paragraph(ui: &mut Ui, para: &str) {
    for line in para.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        render_inline(ui, line, theme::TEXT_HOVER, 13.0);
    }
    ui.add_space(4.0);
}

fn render_code_block(ui: &mut Ui, code: &str) {
    ui.add_space(2.0);
    Frame::none()
        .fill(Color32::from_rgb(22, 22, 22))
        .inner_margin(Margin::same(10.0))
        .stroke(Stroke::new(1.0, theme::DIVIDER))
        .rounding(5.0)
        .show(ui, |ui| {
            ui.label(
                RichText::new(code)
                    .monospace()
                    .color(Color32::from_rgb(210, 210, 210))
                    .size(12.0),
            );
        });
    ui.add_space(4.0);
}

fn render_blockquote(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    Frame::none()
        .fill(Color32::from_rgba_unmultiplied(56, 130, 245, 18))
        .inner_margin(Margin::same(10.0))
        .stroke(Stroke::new(1.0, Color32::from_rgba_unmultiplied(56, 130, 245, 60)))
        .rounding(4.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("▍").color(theme::ACCENT).size(13.0));
                ui.vertical(|ui| {
                    for line in text.split('\n') {
                        if line.trim().is_empty() {
                            continue;
                        }
                        render_inline(ui, line, theme::TEXT_WEAK, 12.5);
                    }
                });
            });
        });
    ui.add_space(4.0);
}

fn render_unordered_list(ui: &mut Ui, items: &[&str]) {
    ui.vertical(|ui| {
        for item in items {
            ui.horizontal(|ui| {
                ui.label(RichText::new("•").color(theme::ACCENT).size(13.0));
                ui.add_space(2.0);
                render_inline(ui, item, theme::TEXT_HOVER, 13.0);
            });
        }
    });
    ui.add_space(4.0);
}

fn render_ordered_list(ui: &mut Ui, items: &[(String, &str)]) {
    ui.vertical(|ui| {
        for (num, item) in items {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{}.", num)).color(theme::ACCENT).size(13.0));
                ui.add_space(2.0);
                render_inline(ui, item, theme::TEXT_HOVER, 13.0);
            });
        }
    });
    ui.add_space(4.0);
}

fn render_table(ui: &mut Ui, lines: &[&str], idx: u64) {
    let rows: Vec<Vec<String>> = lines
        .iter()
        .map(|l| split_table_row(l))
        .filter(|r| !r.is_empty())
        .collect();
    if rows.is_empty() {
        return;
    }

    // 分离表头 / 分隔行 / 数据行
    let (header, body): (Vec<Vec<String>>, Vec<Vec<String>>) = {
        let mut header: Vec<Vec<String>> = Vec::new();
        let mut body: Vec<Vec<String>> = Vec::new();
        let mut taken_header = false;
        for r in rows {
            let is_sep = r
                .iter()
                .all(|c| !c.trim().is_empty() && c.trim().chars().all(|ch| ch == '-' || ch == ':' || ch == ' '));
            if is_sep {
                continue;
            }
            if !taken_header {
                header.push(r);
                taken_header = true;
            } else {
                body.push(r);
            }
        }
        (header, body)
    };

    let n_cols = header.first().map(|h| h.len()).unwrap_or(0);
    if n_cols == 0 {
        return;
    }

    ui.add_space(2.0);
    Frame::none()
        .fill(Color32::from_rgb(28, 28, 28))
        .inner_margin(Margin::same(8.0))
        .stroke(Stroke::new(1.0, theme::DIVIDER))
        .rounding(5.0)
        .show(ui, |ui| {
            ui.push_id(("md_table", idx), |ui| {
                egui::Grid::new("md_table_grid")
                    .num_columns(n_cols)
                    .striped(true)
                    .spacing([14.0, 5.0])
                    .show(ui, |ui| {
                        if let Some(h) = header.first() {
                            for cell in h {
                                ui.label(
                                    RichText::new(cell.trim())
                                        .strong()
                                        .color(theme::TEXT_STRONG)
                                        .size(12.5),
                                );
                            }
                            ui.end_row();
                        }
                        for row in &body {
                            for cell in row {
                                render_inline(ui, cell.trim(), theme::TEXT_HOVER, 12.5);
                            }
                            ui.end_row();
                        }
                    });
            });
        });
    ui.add_space(4.0);
}

// ===== 行类型识别辅助 =====

fn code_fence_len(line: &str) -> Option<usize> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("```") {
        let _ = rest;
        Some(3)
    } else {
        None
    }
}

fn strip_heading(line: &str) -> Option<(u8, &str)> {
    let t = line.trim_start();
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if hashes > 0 && hashes <= 6 {
        let rest = &t[hashes..];
        if rest.starts_with(' ') || rest.is_empty() {
            return Some((hashes as u8, rest.trim_start()));
        }
    }
    None
}

fn is_hr(line: &str) -> bool {
    let t: String = line.trim().chars().filter(|&c| c != ' ').collect();
    if t.len() < 3 {
        return false;
    }
    let first = t.chars().next().unwrap();
    (first == '-' || first == '*' || first == '_') && t.chars().all(|c| c == first)
}

fn strip_blockquote(line: &str) -> Option<&str> {
    let t = line.trim_start();
    t.strip_prefix('>').map(|r| r.trim_start())
}

fn strip_unordered_item(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if let Some(rest) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
        Some(rest)
    } else {
        None
    }
}

fn strip_ordered_item(line: &str) -> Option<(String, &str)> {
    let t = line.trim_start();
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &t[digits.len()..];
    if let Some(after) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(")")) {
        Some((digits, after.trim_start()))
    } else {
        None
    }
}

fn split_table_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let inner = t.strip_prefix('|').unwrap_or(t);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    inner
        .split('|')
        .map(|c| c.trim().to_string())
        .collect()
}
