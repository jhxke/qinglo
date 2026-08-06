use egui::{Color32, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use super::state::{UiState, ViewType};
use super::theme;

const BUTTON_SIZE: f32 = 48.0;
const ICON_SIZE: f32 = 22.0;

#[derive(Clone, Copy)]
enum IconKind {
    /// 节点图（DAG 编辑器）—— 挖掘分析
    Graph,
    /// `</>` 代码 —— 算子开发
    Code,
    /// 齿轮 —— 系统设置
    Gear,
}

/// 渲染 Trae / VS Code 风格的活动栏（最左侧窄竖条）。
///
/// 顶部为主功能入口（挖掘分析、算子开发），底部为系统设置；
/// 激活项左侧有蓝色指示条且图标提亮，非激活项灰色，悬停时背景高亮。
pub fn render_activity_bar(ui: &mut Ui, state: &mut UiState) {
    let bar_rect = ui.max_rect();
    let painter = ui.painter();
    // 活动栏底色（与外层 Frame fill 一致，这里再画一次确保覆盖整个矩形）
    painter.rect_filled(bar_rect, 0.0, theme::ACTIVITY_BAR_BG);

    let width = bar_rect.width();

    // 顶部主功能入口，自上而下排列
    let top_items: [(ViewType, IconKind, &str); 2] = [
        (ViewType::MiningAnalysis, IconKind::Graph, "挖掘分析"),
        (ViewType::OperatorDevelopment, IconKind::Code, "算子开发"),
    ];
    // 底部入口，自下而上排列
    let bottom_items: [(ViewType, IconKind, &str); 1] = [
        (ViewType::Settings, IconKind::Gear, "系统设置"),
    ];

    let mut y = bar_rect.top();
    for (vt, icon, label) in top_items {
        let rect = Rect::from_min_size(
            Pos2::new(bar_rect.left(), y),
            Vec2::new(width, BUTTON_SIZE),
        );
        render_activity_button(ui, rect, vt, icon, label, state);
        y += BUTTON_SIZE;
    }

    let mut y = bar_rect.bottom();
    for (vt, icon, label) in bottom_items {
        y -= BUTTON_SIZE;
        let rect = Rect::from_min_size(
            Pos2::new(bar_rect.left(), y),
            Vec2::new(width, BUTTON_SIZE),
        );
        render_activity_button(ui, rect, vt, icon, label, state);
    }

    // 占据整块区域，避免 egui 布局告警
    ui.allocate_rect(bar_rect, Sense::hover());
}

fn render_activity_button(
    ui: &mut Ui,
    rect: Rect,
    view_type: ViewType,
    icon: IconKind,
    label: &str,
    state: &mut UiState,
) {
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("activity_bar", label)),
        Sense::click(),
    );
    let painter = ui.painter();
    let is_active = state.current_view == view_type;

    // 悬停背景
    if response.hovered() {
        painter.rect_filled(rect, 0.0, theme::HOVER_BG);
    }

    // 激活态左侧蓝色指示条
    if is_active {
        let indicator = Rect::from_min_size(rect.min, Vec2::new(2.0, rect.height()));
        painter.rect_filled(indicator, 0.0, theme::ACCENT);
    }

    let icon_color = if is_active {
        theme::TEXT_STRONG
    } else if response.hovered() {
        theme::TEXT_HOVER
    } else {
        theme::TEXT_WEAK
    };

    let center = rect.center();
    match icon {
        IconKind::Graph => draw_graph_icon(painter, center, ICON_SIZE, icon_color),
        IconKind::Code => draw_code_icon(painter, center, ICON_SIZE, icon_color),
        IconKind::Gear => {
            draw_gear_icon(painter, center, ICON_SIZE, icon_color, theme::ACTIVITY_BAR_BG)
        }
    }

    // on_hover_text 会消费 response（按值接收 self），故先取出 clicked
    let clicked = response.clicked();
    response.on_hover_text(label);

    if clicked {
        state.current_view = view_type;
    }
}

/// 节点图图标：三个节点两两连线，呼应 DAG 编辑器。
fn draw_graph_icon(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let s = size / 24.0;
    let p = |x: f32, y: f32| Pos2::new(center.x + (x - 12.0) * s, center.y + (y - 12.0) * s);
    let n1 = p(5.0, 7.0);
    let n2 = p(19.0, 7.0);
    let n3 = p(12.0, 18.0);
    let stroke = Stroke::new(1.5 * s, color);
    painter.line_segment([n1, n2], stroke);
    painter.line_segment([n1, n3], stroke);
    painter.line_segment([n2, n3], stroke);
    let r = 2.4 * s;
    painter.circle_filled(n1, r, color);
    painter.circle_filled(n2, r, color);
    painter.circle_filled(n3, r, color);
}

/// `</>` 代码图标。
fn draw_code_icon(painter: &egui::Painter, center: Pos2, size: f32, color: Color32) {
    let s = size / 24.0;
    let p = |x: f32, y: f32| Pos2::new(center.x + (x - 12.0) * s, center.y + (y - 12.0) * s);
    let stroke = Stroke::new(1.8 * s, color);
    // <
    painter.line_segment([p(7.0, 7.0), p(3.0, 12.0)], stroke);
    painter.line_segment([p(3.0, 12.0), p(7.0, 17.0)], stroke);
    // /
    painter.line_segment([p(10.5, 17.5), p(13.5, 6.5)], stroke);
    // >
    painter.line_segment([p(17.0, 7.0), p(21.0, 12.0)], stroke);
    painter.line_segment([p(21.0, 12.0), p(17.0, 17.0)], stroke);
}

/// 齿轮图标：圆环 + 8 个齿 + 中心孔。
fn draw_gear_icon(painter: &egui::Painter, center: Pos2, size: f32, color: Color32, bg: Color32) {
    let s = size / 24.0;
    let ring_r = 6.2 * s;
    let stroke = Stroke::new(1.5 * s, color);
    painter.circle_stroke(center, ring_r, stroke);
    // 8 个齿
    for i in 0..8 {
        let a = (i as f32) * std::f32::consts::TAU / 8.0;
        let dir = Vec2::new(a.cos(), a.sin());
        let p1 = center + dir * (ring_r + 0.3 * s);
        let p2 = center + dir * (ring_r + 2.5 * s);
        painter.line_segment([p1, p2], Stroke::new(1.8 * s, color));
    }
    // 中心孔（用活动栏底色"挖空"）
    let hole_r = 2.2 * s;
    painter.circle_filled(center, hole_r, bg);
}
