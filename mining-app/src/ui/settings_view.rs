//! 系统设置视图：功能面开关（隐藏/显示「挖掘」DAG 入口）。
//!
//! 阶段 2 起：把原本的占位页替换为真实开关 UI。
//! 当前阶段只接入一个功能项——「隐藏挖掘 DAG 入口」：
//!
//! - `state.settings.hide_mining` 控制活动栏「挖掘」按钮是否渲染；
//! - 开关切换即时影响活动栏渲染，并立即落盘到 `config.json`（无需
//!   手动点保存按钮，开关本身就是「保存」语义，简化交互）；
//! - 若当前视图为 MiningAnalysis 且开关切到隐藏，自动跳到 Settings 视图，
//!   避免用户停留在已裁掉的视图里无入口返回。
//!
//! 后续可在本视图继续追加 Rust 工具链路径 / 编译目录等占位字段。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, checkbox, column, container, row, scrollable, text};
use iced::{Alignment, Color, Element, Length, Padding};

use super::state::{Message, UiState};
use super::theme;

/// 卡片与开关区域之间的纵向间距。
const SECTION_GAP: f32 = 14.0;
/// 卡片内边距。
const CARD_PADDING: Padding = Padding {
    top: 16.0,
    bottom: 16.0,
    left: 18.0,
    right: 18.0,
};

pub fn view_settings(state: &UiState) -> Element<'_, Message> {
    // ===== 功能面卡片：隐藏挖掘 DAG 入口 =====
    let hide_mining_card = render_toggle_card(
        "挖掘 DAG 入口",
        "隐藏后活动栏不再显示「挖掘」按钮，编排相关界面随之收起；再次打开即可恢复。开关立即生效并保存。",
        "隐藏",
        state.settings.hide_mining,
        Message::ToggleHideMining,
    );

    let body = column![hide_mining_card]
        .spacing(SECTION_GAP)
        .width(Length::Fill)
        .align_x(Alignment::Start);

    // 用容器包裹 body，显式设置 panel_bg 背景，避免 scrollable 内部默认底色与主题不一致
    let body_bg = container(body)
        .width(Length::Fill)
        .padding(Padding {
            top: 18.0,
            bottom: 18.0,
            left: 22.0,
            right: 22.0,
        })
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::panel_bg()).into());
            s
        });

    // 整体放在一张可滚动的容器里，scrollable 本身透明，背景由内层 body_bg 提供
    let scroll = scrollable(body_bg)
        .width(Length::Fill)
        .height(Length::Fill);

    container(scroll)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::panel_bg()).into());
            s
        })
        .into()
}

/// 渲染一个带标题、说明文字与右侧开关的设置卡片。
///
/// 采用 iced 内置 `checkbox` + 自定义 style 实现「标签+开关」视觉效果：
/// 复选框在右侧靠齐，标签与说明在左侧堆叠。
fn render_toggle_card<'a>(
    title: &str,
    desc: &str,
    action_label: &str,
    checked: bool,
    on_toggle: Message,
) -> Element<'a, Message> {
    let title_w = text(title.to_string())
        .color(theme::text_strong())
        .size(13.5);
    let desc_w = text(desc.to_string())
        .color(theme::text_weak())
        .size(11.5)
        .width(Length::Fill);

    let left = column![title_w, desc_w]
        .spacing(4)
        .width(Length::Fill)
        .align_x(Alignment::Start);

    // 用 checkbox 充当开关：on_toggled 直接派发主消息，由 update 翻转状态
    // + 落盘，避免再引入单独的保存按钮。
    // iced 0.14 的 checkbox 构造为 checkbox(is_checked)，标签通过 .label 设置，
    // 字体走 iced 应用级 default_font（已设为 Microsoft YaHei），无需在此重复指定。
    let toggle = checkbox(checked)
        .label(action_label.to_string())
        .on_toggle(move |_v| on_toggle.clone())
        .size(18.0)
        .spacing(6);

    // 把 checkbox 的文字隐藏起来（视觉上只显示开关本体），用一个透明容器包裹。
    let toggle_holder = container(toggle)
        .width(Length::Shrink)
        .height(Length::Shrink)
        .align_x(Horizontal::Right)
        .align_y(Vertical::Center);

    let row = row![left, toggle_holder]
        .spacing(12)
        .width(Length::Fill)
        .align_y(Alignment::Center);

    container(row)
        .width(Length::Fill)
        .padding(CARD_PADDING)
        .style(move |_t| {
            let mut s = iced::widget::container::Style::default();
            s.background = Some(Color::from(theme::card_bg()).into());
            s.border = iced::Border {
                color: Color::from(theme::card_stroke()),
                width: 1.0,
                radius: theme::CARD_ROUNDING.into(),
            };
            s
        })
        .into()
}

#[allow(dead_code)]
fn _unused_btn() -> Element<'static, Message> {
    // 仅用于占位让 button / text 的导入在阶段性编译时不被判定为未使用；
    // 后续追加「测试 / 自动检测」按钮时可直接复用本模式。
    button(text("test").size(12.0)).into()
}
