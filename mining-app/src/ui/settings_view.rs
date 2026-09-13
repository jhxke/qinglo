//! 系统设置视图：品牌与外观 + 功能面开关（隐藏/显示「挖掘」DAG 入口）。
//!
//! - 「品牌与外观」卡片：应用名 / 副标题徽标 / Logo 来源 / 标题栏 Logo 样式；
//!   输入框暂存草稿值，点「应用并保存」才合并到 `state.settings.brand` 并
//!   刷新 `state.brand_snapshot`，让 title / 标题栏即时生效；
//! - 「隐藏挖掘 DAG 入口」开关：切换即时生效并落盘。

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{button, checkbox, column, container, row, scrollable, text,
    text_input, radio};
use iced::{Alignment, Color, Element, Length, Padding};

use super::state::{Message, LogoSource, TitleLogo, UiState};
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
/// 文本输入框宽度（占满父容器，仅留出右侧操作按钮空间）。
const INPUT_WIDTH: Length = Length::Fill;

pub fn view_settings(state: &UiState) -> Element<'_, Message> {
    // ===== 品牌与外观卡片 =====
    let brand_card = render_brand_card(state);

    // ===== 功能面卡片：隐藏挖掘 DAG 入口 =====
    let hide_mining_card = render_toggle_card(
        "挖掘 DAG 入口",
        "隐藏后活动栏不再显示「挖掘」按钮，编排相关界面随之收起；再次打开即可恢复。开关立即生效并保存。",
        "隐藏",
        state.settings.hide_mining,
        Message::ToggleHideMining,
    );

    let body = column![brand_card, hide_mining_card]
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

/// 渲染「品牌与外观」卡片。
///
/// 包含四个分组：
/// 1. 应用名输入框（空 = 用默认 "青萝"）
/// 2. 副标题徽标输入框（空 = 隐藏徽标）+ 清除按钮
/// 3. 任务栏/窗口图标来源（默认折线图 / 从文件加载 + 路径输入框 + 选择文件按钮）
/// 4. 标题栏 Logo 样式单选（折线动画 / 文字首字 / 图片文件）
/// 底部为「恢复默认」+「应用并保存」按钮 + 上次操作结果提示。
fn render_brand_card(state: &UiState) -> Element<'_, Message> {
    let s = &state.settings;

    // ----- 标题 -----
    let title_w = text("品牌与外观")
        .color(theme::text_strong())
        .size(13.5);
    let desc_w = text("自定义应用名、副标题徽标与 Logo。应用名与标题栏样式即时生效；任务栏图标需重启进程。")
        .color(theme::text_weak())
        .size(11.5)
        .width(Length::Fill);
    let header = column![title_w, desc_w]
        .spacing(4)
        .width(Length::Fill);

    // ----- 1. 应用名 -----
    let name_label = text("应用名称")
        .color(theme::text_strong())
        .size(12.0);
    let name_hint = text("显示在窗口标题、标题栏、WebView 菜单。留空恢复「青萝」")
        .color(theme::text_weak())
        .size(10.5);
    let name_input = text_input("青萝", &s.brand_name_input)
        .on_input(Message::BrandNameInput)
        .style(theme::cool_text_input_style())
        .size(13.0);
    let name_col = column![
        name_label,
        name_hint,
        name_input.width(INPUT_WIDTH),
    ]
    .spacing(4)
    .width(Length::Fill);

    // ----- 2. 副标题徽标 -----
    let sub_label = text("副标题徽标")
        .color(theme::text_strong())
        .size(12.0);
    let sub_hint = text("标题栏应用名右侧的徽标文字。留空隐藏徽标")
        .color(theme::text_weak())
        .size(10.5);
    let sub_input = text_input("Quant IDE", &s.brand_subtitle_input)
        .on_input(Message::BrandSubtitleInput)
        .style(theme::cool_text_input_style())
        .size(13.0);
    let sub_clear_btn = button(text("清除").size(11.0).color(theme::text_weak()))
        .on_press(Message::BrandSubtitleInput(String::new()))
        .padding(Padding {
            top: 5.0, bottom: 5.0, left: 10.0, right: 10.0,
        })
        .style(|_t, _s| {
            let mut st = iced::widget::button::Style::default();
            st.background = Some(Color::TRANSPARENT.into());
            st.border.color = Color::from(theme::card_stroke());
            st.border.width = 1.0;
            st.border.radius = 6.0.into();
            st
        });
    let sub_row = row![sub_input.width(Length::Fill), sub_clear_btn]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);
    let sub_col = column![sub_label, sub_hint, sub_row]
        .spacing(4)
        .width(Length::Fill);

    // ----- 3. 任务栏 / 窗口图标 -----
    let logo_label = text("任务栏 / 窗口图标")
        .color(theme::text_strong())
        .size(12.0);
    let logo_hint = text("仅启动时生效，运行时改后需重启进程。⚠️ 切换后请关闭并重新打开应用")
        .color(theme::text_weak())
        .size(10.5);
    let logo_default_radio = radio(
        "默认折线图（内置）",
        LogoSource::Default,
        Some(s.brand.logo),
        |_| Message::BrandLogoDefault,
    )
    .text_size(12.0);
    let logo_file_radio_row = row![
        radio(
            "从文件加载",
            LogoSource::File,
            Some(s.brand.logo),
            |_| Message::BrandLogoFilePick,
        )
        .text_size(12.0),
    ];
    // 文件路径输入框 + 选择文件按钮（仅 File 模式下展示）
    let logo_path_input = text_input("C:\\brand\\logo.png 或相对 exe 同级路径", &s.brand_logo_path_input)
        .on_input(Message::BrandLogoPathInput)
        .style(theme::cool_text_input_style())
        .size(12.0);
    let pick_btn = button(text("选择文件…").size(11.0).color(theme::text_strong()))
        .on_press(Message::BrandLogoFilePick)
        .padding(Padding {
            top: 5.0, bottom: 5.0, left: 10.0, right: 10.0,
        })
        .style(|_t, _s| {
            let mut st = iced::widget::button::Style::default();
            st.background = Some(Color::from(theme::hover_bg()).into());
            st.border.color = Color::from(theme::card_stroke());
            st.border.width = 1.0;
            st.border.radius = 6.0.into();
            st
        });
    let is_file_mode = matches!(s.brand.logo, LogoSource::File);
    let logo_path_row: Element<'_, Message> = if is_file_mode {
        row![logo_path_input.width(Length::Fill), pick_btn]
            .spacing(8)
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .into()
    } else {
        // 非 File 模式：留个占位防止布局抖动，但不显示选择按钮
        row![text("").width(Length::Fill)]
            .width(Length::Fill)
            .height(Length::Fixed(0.0))
            .into()
    };
    let logo_col = column![
        logo_label,
        logo_hint,
        logo_default_radio,
        logo_file_radio_row,
        logo_path_row,
    ]
    .spacing(6)
    .width(Length::Fill);

    // ----- 4. 标题栏 Logo 样式 -----
    let tlogo_label = text("标题栏 Logo 样式")
        .color(theme::text_strong())
        .size(12.0);
    let tlogo_hint = text("即时生效，无需重启")
        .color(theme::text_weak())
        .size(10.5);
    let tlogo_sparkline = radio(
        "折线动画（默认）",
        TitleLogo::Sparkline,
        Some(s.brand.title_logo),
        Message::BrandTitleLogoChange,
    )
    .text_size(12.0);
    let tlogo_initial = radio(
        "文字首字",
        TitleLogo::Initial,
        Some(s.brand.title_logo),
        Message::BrandTitleLogoChange,
    )
    .text_size(12.0);
    let tlogo_imagefile = radio(
        "图片文件",
        TitleLogo::ImageFile,
        Some(s.brand.title_logo),
        Message::BrandTitleLogoChange,
    )
    .text_size(12.0);
    let tlogo_row = row![tlogo_sparkline, tlogo_initial, tlogo_imagefile]
        .spacing(20)
        .align_y(Alignment::Center);
    let tlogo_col = column![tlogo_label, tlogo_hint, tlogo_row]
        .spacing(6)
        .width(Length::Fill);

    // ----- 底部按钮 + 结果提示 -----
    let reset_btn = button(text("恢复默认").size(12.0).color(theme::text_strong()))
        .on_press(Message::BrandReset)
        .padding(Padding {
            top: 7.0, bottom: 7.0, left: 14.0, right: 14.0,
        })
        .style(|_t, _s| {
            let mut st = iced::widget::button::Style::default();
            st.background = Some(Color::TRANSPARENT.into());
            st.border.color = Color::from(theme::card_stroke());
            st.border.width = 1.0;
            st.border.radius = 6.0.into();
            st
        });
    let apply_btn = button(text("应用并保存").size(12.0).color(Color::WHITE))
        .on_press(Message::BrandApply)
        .padding(Padding {
            top: 7.0, bottom: 7.0, left: 14.0, right: 14.0,
        })
        .style(|_t, _s| {
            let mut st = iced::widget::button::Style::default();
            st.background = Some(Color::from(theme::accent()).into());
            st.border.radius = 6.0.into();
            st.text_color = Color::WHITE;
            st
        });
    let result_w: Element<'_, Message> = match &s.last_result {
        Some((ok, msg)) => {
            let color = if *ok { theme::accent_teal() } else { Color::from_rgb8(239, 68, 68) };
            text(msg.clone()).color(color).size(11.0).into()
        }
        None => text("").size(11.0).into(),
    };
    let bottom_row = row![
        reset_btn,
        // 弹性间隔把右侧按钮顶到右边
        text("").width(Length::Fill),
        result_w,
        apply_btn,
    ]
    .spacing(10)
    .align_y(Alignment::Center)
    .width(Length::Fill);

    // ----- 整体卡片 -----
    let body = column![
        header,
        name_col,
        sub_col,
        logo_col,
        tlogo_col,
        // 分隔线
        container(row![])
            .width(Length::Fill)
            .height(Length::Fixed(1.0))
            .style(|_t| {
                let mut st = iced::widget::container::Style::default();
                st.background = Some(Color {
                    r: 1.0, g: 1.0, b: 1.0, a: 8.0 / 255.0
                }.into());
                st
            }),
        bottom_row,
    ]
    .spacing(14)
    .width(Length::Fill);

    container(body)
        .width(Length::Fill)
        .padding(CARD_PADDING)
        .style(|_t| {
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
