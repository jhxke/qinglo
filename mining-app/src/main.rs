mod icon;

use eframe::egui;
use std::path::Path;

use mining_app::ui::{
    UiState, ViewType, poll_dag_exec_task, render_activity_bar, render_mining_analysis_view,
    render_settings_view, render_status_bar, theme,
};

struct MyApp {
    ui_state: UiState,
    logo_animation_time: f64,
}

impl Default for MyApp {
    fn default() -> Self {
        Self {
            ui_state: UiState::default(),
            logo_animation_time: 0.0,
        }
    }
}

/// 全局空闲检测：当前 UI 是否处于「无动画需求」的安静状态。
///
/// 返回 true 表示：
///   - 没有后台 DAG 执行任务（不需要 20FPS 的脉冲动画节流）
///   - 没有聊天预览窗口处于 streaming 状态（不需要 500ms 光标闪烁）
///
/// 空闲状态下 `update()` 末尾会请求一个较长的 repaint_after 间隔
/// （`IDLE_REPAINT_INTERVAL_MS`，当前 500ms），把 eframe 事件循环从
/// 显示器刷新率（60–144Hz）人工降到约 2 FPS，显著降低空闲时 CPU/GPU 占用。
///
/// 注意：任何用户输入事件（鼠标移动/点击、键盘、窗口事件等）都会由
/// egui 后端立即唤醒 UI 线程并重绘，不受此空闲节流影响。
fn is_ui_idle(ui_state: &UiState) -> bool {
    // 条件 1：没有正在执行的 DAG 任务
    if ui_state.dag_editor.dag_exec_task.is_some() {
        return false;
    }
    // 条件 2：任何激活 tab 都没有打开的 streaming 聊天预览
    // （chat streaming 状态每 500ms 自己会 request_repaint_after，
    //  这里只要保守判断——只要有 chat_preview 打开，就不进入深度空闲）
    for tab in &ui_state.dag_editor.tabs {
        if tab.chat_preview_node_id.is_some() {
            return false;
        }
    }
    true
}

/// 空闲时的重绘间隔：500ms ≈ 2 FPS。
///
/// 在完全无操作、无任务期间，仅以这个低频刷新界面，用户几乎感知不到卡顿，
/// 但 CPU/GPU 占用可以从持续 3–10% 降到 <1%。
const IDLE_REPAINT_INTERVAL_MS: u64 = 500;

impl MyApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_chinese_fonts(&cc.egui_ctx);
        setup_dark_theme(&cc.egui_ctx);
        Self::default()
    }
}

fn setup_chinese_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    
    let font_candidates = [
        "C:\\Windows\\Fonts\\msyh.ttc",
        "C:\\Windows\\Fonts\\simsun.ttc",
        "C:\\Windows\\Fonts\\msyhbd.ttc",
    ];

    for (i, path) in font_candidates.iter().enumerate() {
        if Path::new(path).exists() {
            if let Ok(font_bytes) = std::fs::read(path) {
                let font_name = format!("chinese_{}", i);
                fonts.font_data.insert(font_name.clone(), egui::FontData::from_owned(font_bytes));
                fonts.families.get_mut(&egui::FontFamily::Proportional)
                    .unwrap()
                    .insert(0, font_name.clone());
                fonts.families.get_mut(&egui::FontFamily::Monospace)
                    .unwrap()
                    .push(font_name);
                break;
            }
        }
    }
    
    ctx.set_fonts(fonts);
}

fn setup_dark_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();

    // Trae 风格深黑主题
    visuals.panel_fill = theme::PANEL_BG;            // #121212 主内容区底色
    visuals.window_fill = theme::CARD_BG;            // #1C1C1E 浮层窗口底色
    visuals.extreme_bg_color = egui::Color32::from_rgb(12, 12, 12);
    visuals.faint_bg_color = egui::Color32::from_rgb(34, 34, 36);

    // 选中 / 强调色（蓝色）
    visuals.selection.bg_fill = theme::ACCENT;
    visuals.selection.stroke = egui::Stroke::new(1.0, theme::ACCENT);
    visuals.hyperlink_color = egui::Color32::from_rgb(86, 156, 214);

    // 控件 / 分隔线
    visuals.window_stroke = egui::Stroke::new(1.0, theme::CARD_STROKE);
    visuals.widgets.noninteractive.bg_fill = theme::PANEL_BG;
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, theme::TEXT_WEAK);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, theme::DIVIDER);

    // ===== 全局圆角：让按钮 / 输入框 / 组合框 / 窗口 / 菜单都带柔和圆角 =====
    visuals.window_rounding = egui::Rounding::same(theme::FLOAT_ROUNDING);
    visuals.menu_rounding = egui::Rounding::same(theme::WIDGET_ROUNDING);
    visuals.popup_shadow = egui::epaint::Shadow {
        extrusion: 12.0,
        color: egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140),
    };
    // 各交互态控件统一圆角
    let widget_rounding = egui::Rounding::same(theme::WIDGET_ROUNDING);
    visuals.widgets.noninteractive.rounding = widget_rounding;
    visuals.widgets.inactive.rounding = widget_rounding;
    visuals.widgets.hovered.rounding = widget_rounding;
    visuals.widgets.active.rounding = widget_rounding;
    visuals.widgets.open.rounding = widget_rounding;
    // 输入框 / 组合框底色略提亮，与纯黑底拉开层次
    visuals.widgets.inactive.bg_fill = theme::CARD_BG;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, theme::CARD_STROKE);
    visuals.widgets.hovered.bg_fill = theme::HOVER_BG;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, theme::CARD_STROKE);
    visuals.widgets.active.bg_fill = theme::HOVER_BG;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, theme::ACCENT);

    // 全局交互光标: 所有 Button(含右键菜单项/对话框按钮/设置页按钮等)悬停时
    // 显示食指指向小手, 统一可点击交互的视觉提示. egui 的 Button widget 内部
    // 会读取 visuals.interact_cursor 并在 hovered 时调用 set_cursor_icon.
    visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);

    ctx.set_visuals(visuals);
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 轮询后台 DAG 执行任务（无论当前处于哪个视图都排空工作线程消息、回填结果）
        poll_dag_exec_task(ctx, &mut self.ui_state.dag_editor);

        // 顶部标题栏：Logo + 应用名（左） + 窗口控制按钮（右）
        // stroke 提供与下方内容区的 1px 分隔线（顶/左/右贴窗边不可见，仅底部可见）
        let logo_hovered = egui::TopBottomPanel::top("title_bar")
            .frame(
                egui::Frame::none()
                    .fill(theme::TITLE_BAR_BG)
                    .stroke(egui::Stroke::new(1.0, theme::DIVIDER)),
            )
            .show(ctx, |ui| {
                let rect = ui.available_rect_before_wrap();
                ui.set_height(40.0);

                let logo_hovered = ui.horizontal(|ui| {
                    ui.set_width(ui.available_width());
                    // 左侧：Logo + 应用名
                    let logo_hovered = render_logo(ui, self.logo_animation_time);
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new("青萝")
                            .color(theme::TEXT_HOVER)
                            .font(egui::FontId::proportional(13.0))
                            .strong(),
                    );

                    // 右侧：窗口控制按钮
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 关闭按钮
                        let close_button = render_window_button(ui, "×", egui::Color32::from_rgb(239, 68, 68), egui::Color32::from_rgb(220, 38, 38));
                        if close_button.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                        // 最大化/还原按钮
                        let is_maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
                        let max_button = render_window_button(ui, if is_maximized { "□" } else { "▭" }, egui::Color32::from_rgb(100, 100, 100), egui::Color32::from_rgb(70, 70, 70));
                        if max_button.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!is_maximized));
                        }
                        // 最小化按钮
                        let min_button = render_window_button(ui, "−", egui::Color32::from_rgb(100, 100, 100), egui::Color32::from_rgb(70, 70, 70));
                        if min_button.clicked() {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    });
                    logo_hovered
                }).inner;

                // 窗口拖拽功能
                let response = ui.interact(rect, egui::Id::new("title_bar_drag"), egui::Sense::drag());
                if response.dragged() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                logo_hovered
            }).inner;

        // Logo 动画时间推进：仅在悬停时推进，避免空闲时每帧做 sin 计算
        // （悬停动画的 wave_offset 依赖此时间；非悬停时动画冻结在 wave_offset=0，
        //  渲染结果与设计稿静态图标完全一致，无视觉差异）
        if logo_hovered {
            self.logo_animation_time += ctx.input(|i| i.unstable_dt as f64);
        }

        // 底部状态栏（Trae / VS Code 风格）：跨整个窗口宽度，展示最新提醒与执行状态。
        // 必须在活动栏（SidePanel::left）之前声明，才能占据完整窗口宽度，
        // 否则只会贴在活动栏右侧的主内容区下方。
        egui::TopBottomPanel::bottom("status_bar")
            .resizable(false)
            .exact_height(24.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::STATUS_BAR_BG)
                    .inner_margin(egui::Margin::symmetric(8.0, 0.0)),
            )
            .show(ctx, |ui| {
                render_status_bar(ui, &self.ui_state);
            });

        // 左侧活动栏（Trae / VS Code 风格）：图标承载功能入口
        egui::SidePanel::left("activity_bar")
            .resizable(false)
            .exact_width(48.0)
            .frame(
                egui::Frame::none()
                    .fill(theme::ACTIVITY_BAR_BG)
                    .stroke(egui::Stroke::new(1.0, theme::DIVIDER)),
            )
            .show(ctx, |ui| {
                render_activity_bar(ui, &mut self.ui_state);
            });

        // 主内容区：根据活动栏选中项切换视图
        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(theme::PANEL_BG))
            .show(ctx, |ui| {
                match self.ui_state.current_view {
                    ViewType::MiningAnalysis => render_mining_analysis_view(ui, &mut self.ui_state.dag_editor),
                    ViewType::Settings => render_settings_view(ui, &mut self.ui_state),
                }
            });

        // ===== 全局空闲降频 =====
        // 在所有视图渲染完成后（确保各组件按需发出的 request_repaint* 已生效），
        // 如果检测到 UI 处于「无任务、无流式预览、Logo 也未悬停」的安静状态，
        // 就用一个较长的 repaint_after 间隔把 eframe 从显示器刷新率（60-144Hz）
        // 人工压到约 2 FPS。任何用户输入事件都会立即唤醒并重绘，交互无感知。
        if is_ui_idle(&self.ui_state) && !logo_hovered {
            ctx.request_repaint_after(std::time::Duration::from_millis(IDLE_REPAINT_INTERVAL_MS));
        }
    }
}

fn render_window_button(ui: &mut egui::Ui, text: &str, hover_color: egui::Color32, pressed_color: egui::Color32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::Vec2::new(46.0, 30.0), egui::Sense::click());
    let painter = ui.painter();
    
    // 绘制按钮背景 - 使用 is_pointer_button_down_on 检测持续按下状态
    if response.is_pointer_button_down_on() {
        painter.add(egui::Shape::rect_filled(rect, 0.0, pressed_color));
    } else if response.hovered() {
        painter.add(egui::Shape::rect_filled(rect, 0.0, hover_color));
    }
    
    // 绘制按钮文字
    let text_color = if response.hovered() || response.is_pointer_button_down_on() {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_rgb(180, 180, 180)
    };
    
    // 使用 painter.text 来绘制文字
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(14.0),
        text_color,
    );
    
    response
}

fn lerp_color(start: egui::Color32, end: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    egui::Color32::from_rgba_unmultiplied(
        (start.r() as f32 + (end.r() as f32 - start.r() as f32) * t) as u8,
        (start.g() as f32 + (end.g() as f32 - start.g() as f32) * t) as u8,
        (start.b() as f32 + (end.b() as f32 - start.b() as f32) * t) as u8,
        (start.a() as f32 + (end.a() as f32 - start.a() as f32) * t) as u8,
    )
}

fn render_logo(ui: &mut egui::Ui, time: f64) -> bool {
    // 设计稿基于 24×24 坐标系，所有内部坐标/线宽均按 s 等比缩放。
    // 想再放大/缩小 logo，只改 LOGO_DISPLAY_SIZE 即可。
    const LOGO_DESIGN_SIZE: f32 = 24.0;
    const LOGO_DISPLAY_SIZE: f32 = 32.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::Vec2::splat(LOGO_DISPLAY_SIZE), egui::Sense::hover());
    let hovered = response.hovered();
    let painter = ui.painter();

    let s = rect.width() / LOGO_DESIGN_SIZE; // 缩放因子
    let center = rect.center();
    let size = rect.width() * 0.8;
    let half_size = size / 2.0;

    // 渐变色
    let start_color = egui::Color32::from_rgb(0, 122, 255);
    let end_color = egui::Color32::from_rgb(52, 199, 89);

    // 波形偏移：仅悬停时启用 sin 动画，否则冻结为 0（与设计稿静态点一致）
    let wave_offset = if hovered {
        (time * 2.0).sin() as f32 * 0.5
    } else {
        0.0
    };

    // 折线图形状（上升趋势）：坐标基于 24×24 设计稿，通过 s 缩放到实际尺寸。
    // 与 icon.rs::create_app_icon 的静态点保持一致：6 点上升趋势，末尾顶部突破点
    // (18, -18) 在原峰值正上方，象征数据挖掘的“发现峰值”，落点为纯绿 #34C759。
    // wave_offset 仅作用于肩部点（index 1/4），保证 wave_offset=0 时与图标完全一致。
    let px = |dx: f32, dy: f32| {
        egui::Pos2::new(center.x - half_size + dx * s, center.y + half_size + dy * s)
    };
    let points = [
        px(4.0, -2.0),
        px(8.0, -4.0 + wave_offset),
        px(10.0, -3.0),
        px(12.0, -10.0),
        px(16.0, -9.0 + wave_offset),
        px(20.0, -16.0),
    ];

    // 绘制折线阴影
    painter.add(egui::Shape::line(
        points
            .iter()
            .map(|p| egui::Pos2::new(p.x + 1.0 * s, p.y + 1.0 * s))
            .collect(),
        egui::Stroke::new(2.0 * s, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 60)),
    ));

    // 绘制渐变折线
    for i in 0..points.len() - 1 {
        let progress = i as f32 / (points.len() - 2) as f32;
        let color = lerp_color(start_color, end_color, progress);
        painter.add(egui::Shape::line(
            vec![points[i], points[i + 1]],
            egui::Stroke::new(2.5 * s, color),
        ));
    }

    // 绘制数据点
    for (i, &point) in points.iter().enumerate() {
        let progress = i as f32 / (points.len() - 1) as f32;
        let color = lerp_color(start_color, end_color, progress);

        // 外圈光晕半径：悬停时呼吸，否则固定 3.0
        let glow_radius = if hovered {
            (4.0 + (time * 3.0).sin() as f32 * 1.0) * s
        } else {
            3.0 * s
        };
        let glow_color = egui::Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            (100.0 * (1.0 - progress)).round() as u8,
        );
        painter.add(egui::Shape::circle_filled(point, glow_radius, glow_color));

        // 内圈实心点
        painter.add(egui::Shape::circle_filled(point, 2.0 * s, color));
    }

    // 悬停时的发光背板
    if hovered {
        let glow_rect =
            egui::Rect::from_center_size(center, egui::Vec2::splat(32.0 * s));
        painter.add(egui::Shape::rect_filled(
            glow_rect,
            16.0 * s,
            egui::Color32::from_rgba_unmultiplied(0, 122, 255, 20),
        ));
    }

    hovered
}

fn main() -> Result<(), eframe::Error> {
    let viewport = egui::ViewportBuilder::default()
        .with_title("青萝")
        .with_inner_size([1280.0, 800.0])
        // 限制窗口最小内尺寸：避免缩到太小后左侧建模/算子面板（220+240）被挤压、
        // 中央画布无可用空间。最小宽度 ≈ 活动栏48 + 建模面板220 + 算子面板240 + 画布350；
        // 最小高度保留标题栏/状态栏/Tab栏/日志面板 + 画布的合理可用区。
        .with_min_inner_size([860.0, 560.0])
        .with_decorations(false)
        .with_icon(icon::create_app_icon());

    let native_options = eframe::NativeOptions {
        viewport,
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "青萝",
        native_options,
        Box::new(|cc| Box::new(MyApp::new(cc))),
    )
}
