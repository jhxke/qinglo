//! 左侧面板「算子面板」子页：搜索框 + 算子目录（占满全高）。
//!
//! 从 `mining_analysis_view` 拆出，仅负责算子面板的渲染（标题栏 + 搜索框 +
//! 分类树）。入口 [`view_operator_panel`] 由
//! `mining_analysis_view::view_sidebar` 在 `LeftPanelTab::Operators` 分支调用。
//! 节点参数已迁移至右侧抽屉（画布双击节点弹出，详见 `mining_analysis_view::view_params_drawer`）。

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Alignment, Color, Element, Length, Padding};

use super::state::{Message, UiState};
use super::theme;
use crate::mining::dag::get_operator_categories;

/// 左侧面板「算子面板」子页 v2：搜索框 + 算子目录（占满全高）。
/// 节点参数已迁移至右侧抽屉（画布双击节点弹出，详见 `view_params_drawer`）。
pub fn view_operator_panel(state: &UiState) -> Element<'_, Message> {
    let editor = &state.dag_editor;
    let search_value = editor
        .active_tab()
        .map(|t| t.operator_search_filter.clone())
        .unwrap_or_default();

    // 标题栏
    let header = container(
        row![
            text("算子面板").color(theme::text_strong()).size(13.0),
            text("点击添加节点").color(theme::text_weak()).size(9.5),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fixed(38.0))
    .align_y(Alignment::Center)
    .padding(Padding { top: 0.0, bottom: 0.0, left: 14.0, right: 14.0 });

    let header_divider = container(row![])
        .width(Length::Fill)
        .height(Length::Fixed(1.0))
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::divider()).into());
            s
        });

    // 搜索框 v2
    let search = text_input("搜索算子…", &search_value)
        .on_input(Message::OperatorSearchInput)
        .width(Length::Fill)
        .size(11.0)
        .padding(Padding { top: 7.0, bottom: 7.0, left: 10.0, right: 10.0 });
    let search_wrap = container(
        row![
            text("⌕").color(theme::text_weak()).size(12.0),
            search,
        ]
        .spacing(6)
        .align_y(Alignment::Center)
    )
    .width(Length::Fill)
    .padding(Padding { top: 0.0, bottom: 0.0, left: 8.0, right: 8.0 })
    .style(|_t| {
        let mut s = iced::widget::container::Style::default();
        s.background = Some(Color::from(theme::card_bg()).into());
        s.border.color = theme::card_stroke();
        s.border.width = 1.0;
        s.border.radius = theme::WIDGET_ROUNDING.into();
        s
    });
    let search_container = container(search_wrap)
        .width(Length::Fill)
        .padding(Padding { top: 8.0, bottom: 6.0, left: 10.0, right: 10.0 });

    // 算子目录递归渲染
    let categories = get_operator_categories();
    let filter = search_value.trim().to_lowercase();
    let mut op_col = column![].spacing(5).padding(Padding {
        top: 2.0, bottom: 8.0, left: 8.0, right: 8.0,
    });
    render_operator_categories(&categories, &filter, 0, &mut op_col);
    let op_scroll = scrollable(op_col)
        .direction(scrollable::Direction::Vertical(theme::cool_scrollbar()))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::cool_scrollbar_style());

    let op_col_top = column![search_container, op_scroll]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    // 节点参数已迁移至右侧抽屉（画布双击节点弹出），左侧算子面板占满全高。
    let col = column![header, header_divider, op_col_top]
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(0);

    container(col).width(Length::Fill).height(Length::Fill).into()
}

/// 递归渲染算子分类树（分类名 + 子分类 + 算子卡片）。
///
/// `depth` 控制左侧缩进（每层 8px）；空 filter 不过滤；非空 filter 时，
/// 只有匹配的算子/子分类才展示；匹配的算子卡片按 `AddOperator(name)` 发消息。
///
/// 注意：所有文本字段都使用 `.to_string()` 取得所有权，避免元素树借用
/// 临时 `categories` 本地变量导致的生命周期错误（E0515）。
fn render_operator_categories(
    categories: &[operator_executor_client::protocol::OperatorCategory],
    filter: &str,
    depth: u32,
    col: &mut iced::widget::Column<'_, Message>,
) {
    use iced::widget::column as col_elem;
    for cat in categories {
        // 收集该分类下匹配的算子卡片列表（拷贝必要字段，避免后续借用 categories）
        let mut matched_ops: Vec<(
            String,
            String,
            [u8; 3],
        )> = Vec::new();
        for op in &cat.operators {
            if filter.is_empty() || op.name.to_lowercase().contains(filter) {
                let desc = if op.summary.is_empty() {
                    op.description.clone()
                } else {
                    op.summary.clone()
                };
                matched_ops.push((op.name.clone(), desc, op.color));
            }
        }
        // 递归收集子分类（即使本层无匹配，子分类匹配也算本分类需要展示）
        let sub_has_match = !filter.is_empty() && {
            let mut stack: Vec<&[operator_executor_client::protocol::OperatorCategory]> = Vec::new();
            stack.push(&cat.subcategories);
            let mut hit = false;
            while let Some(cats) = stack.pop() {
                for sub in cats {
                    for op in &sub.operators {
                        if op.name.to_lowercase().contains(filter) {
                            hit = true;
                            break;
                        }
                    }
                    if !sub.subcategories.is_empty() {
                        stack.push(&sub.subcategories);
                    }
                }
                if hit {
                    break;
                }
            }
            hit
        };

        let show_category = filter.is_empty() || !matched_ops.is_empty() || sub_has_match;
        if !show_category {
            continue;
        }

        let indent = depth as f32 * 10.0;
        // 分类标签：弱化颜色（非 accent），让算子卡片更突出；加竖向留白
        let label = container(
            text(cat.name.clone())
                .color(theme::text_weak())
                .size(10.0),
        )
        .width(Length::Fill)
        .padding(Padding {
            top: 6.0,
            bottom: 2.0,
            left: indent + 2.0,
            right: 0.0,
        });
        *col = std::mem::replace(col, col_elem![]).push(label);

        for (op_name, op_desc, op_color) in matched_ops {
            let color = Color::from_rgb8(op_color[0], op_color[1], op_color[2]);
            let name_owned = op_name.clone();
            let desc_owned = op_desc.clone();
            let op_name_for_msg = op_name.clone();

            // 色点（小圆点，替代色条更精致）
            let dot = container(text("").size(1.0))
                .width(Length::Fixed(8.0))
                .height(Length::Fixed(8.0))
                .style(move |_t| {
                    let mut s = iced::widget::container::Style::default();
                    s.background = Some(color.into());
                    s.border.radius = theme::PILL_ROUNDING.into();
                    s
                });

            let card_btn = button(
                row![
                    dot,
                    column![
                        text(name_owned).color(theme::text_strong()).size(11.0),
                        text(desc_owned).color(theme::text_weak()).size(9.0),
                    ]
                    .spacing(1)
                    .width(Length::Fill)
                    .align_x(Alignment::Start),
                ]
                .align_y(Alignment::Center)
                .spacing(8)
                .width(Length::Fill),
            )
            .width(Length::Fill)
            .on_press(Message::AddOperator(op_name_for_msg))
            .padding(Padding { top: 8.0, bottom: 8.0, left: 10.0, right: 10.0 })
            .style(move |_t, status| {
                let mut s = iced::widget::button::Style::default();
                s.background = Some(Color::from(theme::card_bg()).into());
                s.border.color = theme::card_stroke();
                s.border.width = 1.0;
                s.border.radius = 9.0.into();
                s.text_color = theme::text_strong();
                if matches!(status, iced::widget::button::Status::Hovered) {
                    s.background = Some(Color::from(theme::card_hover_bg()).into());
                    s.border.color = theme::accent_dim();
                }
                s
            });

            let card = container(card_btn).padding(Padding {
                top: 0.0, bottom: 0.0, left: indent, right: 0.0,
            });
            *col = std::mem::replace(col, col_elem![]).push(card);
        }

        // 子分类（相同 filter）
        render_operator_categories(&cat.subcategories, filter, depth + 1, col);
    }
}
