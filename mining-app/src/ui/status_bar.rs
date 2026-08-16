use egui::{Color32, FontId, Layout, RichText, Sense, Ui, Vec2};

use super::state::{LogLevel, UiState, ViewType};
use super::theme;

/// 渲染 Trae / VS Code 风格的底部状态栏。
///
/// 状态栏跨整个窗口宽度（位于活动栏下方），左侧展示「最新提醒」——
/// 聚合各视图已有的提醒源（DAG tab 的 `action_logs`、算子开发的 `run_logs` /
/// `error_message`、设置页的 `last_result`），右侧展示执行状态、当前视图名与未保存标记。
///
/// 本模块为纯展示层：不持有额外状态，也不修改 `UiState`，所有信息均从现有状态读取，
/// 从而避免「提醒」在多处重复存储。
pub fn render_status_bar(ui: &mut Ui, state: &UiState) {
    ui.horizontal(|ui| {
        ui.set_width(ui.available_width());
        ui.set_height(24.0);
        ui.spacing_mut().item_spacing.x = 6.0;

        // 左侧：状态色点 + 最新提醒消息
        let (level, message) = current_status(state);

        let (dot_rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
        ui.painter()
            .circle_filled(dot_rect.center(), 3.5, level_color(&level));
        ui.label(
            RichText::new(&message)
                .color(Color32::WHITE)
                .font(FontId::proportional(11.5)),
        );

        // 右侧：执行状态 / 当前视图 / 未保存标记
        ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 6.0;

            // 当前视图名
            let view_name = match state.current_view {
                ViewType::MiningAnalysis => "挖掘分析",
                ViewType::Settings => "系统设置",
            };
            status_item(ui, view_name);

            // 未保存标记（仅挖掘分析视图下的活动 tab 有 dirty 概念）
            if state.current_view == ViewType::MiningAnalysis {
                if let Some(tab) = state.dag_editor.active_tab() {
                    if tab.dirty {
                        status_item(ui, "● 未保存");
                    }
                }
            }

            // DAG 执行状态：后台任务运行中显示转圈 + 「执行中」
            if state.dag_editor.dag_exec_task.is_some() {
                status_item_with(ui, "执行中", |ui| {
                    ui.add(egui::Spinner::new().size(12.0).color(Color32::WHITE))
                });
            }
        });
    });
}

/// 计算当前应展示的提醒消息与级别。
///
/// 优先级：执行中 > 视图级错误 > 视图最新提醒日志 > 默认「就绪」。
/// 这样保证运行态、错误态、用户操作反馈三类信息都能及时在状态栏出现。
fn current_status(state: &UiState) -> (LogLevel, String) {
    // 1. 后台 DAG 执行中
    if state.dag_editor.dag_exec_task.is_some() {
        return (LogLevel::Info, "正在执行 DAG 流程…".into());
    }

    // 2. 按当前视图取最新提醒
    match state.current_view {
        ViewType::MiningAnalysis => {
            if let Some(tab) = state.dag_editor.active_tab() {
                if let Some(err) = &tab.error_message {
                    return (LogLevel::Error, err.clone());
                }
                if let Some(last) = tab.action_logs.last() {
                    return (last.level.clone(), last.message.clone());
                }
            }
        }
        ViewType::Settings => {}
    }

    // 3. 无任何提醒时显示就绪
    (LogLevel::Info, "就绪".into())
}

/// 把日志级别映射为状态色点的颜色（在灰色状态栏上保持高可读对比度）。
fn level_color(level: &LogLevel) -> Color32 {
    match level {
        LogLevel::Info => Color32::from_rgb(140, 200, 255),
        LogLevel::Success => Color32::from_rgb(80, 220, 110),
        LogLevel::Warning => Color32::from_rgb(240, 190, 70),
        LogLevel::Error => Color32::from_rgb(245, 100, 100),
    }
}

/// 状态栏右侧的一个可悬停高亮片段：左竖分隔线 + 文本。
///
/// 模仿 VS Code 状态栏条目的交互：悬停时背景变亮，给用户「可点击」的视觉暗示，
/// 此处仅作展示，暂不绑定动作。
fn status_item(ui: &mut Ui, text: &str) {
    status_item_with(ui, text, |_| {});
}

/// 与 [`status_item`] 相同，但允许在分隔线与文本之间插入一个自定义控件（如转圈）。
fn status_item_with<R>(ui: &mut Ui, text: &str, insert: impl FnOnce(&mut Ui) -> R) -> R {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(0.0, 24.0), Sense::hover());
    let _ = rect;
    // 左侧竖分隔线
    let sep_rect = egui::Rect::from_min_size(
        response.rect.left_top() + egui::Vec2::Y * 4.0,
        egui::Vec2::new(1.0, response.rect.height() - 8.0),
    );
    ui.painter().rect_filled(
        sep_rect,
        0.0,
        Color32::from_rgba_unmultiplied(255, 255, 255, 40),
    );

    if response.hovered() {
        ui.painter()
            .rect_filled(response.rect, 0.0, theme::status_bar_hover());
    }

    ui.label(
        RichText::new(text)
            .color(Color32::WHITE)
            .font(FontId::proportional(11.5)),
    );
    insert(ui)
}
