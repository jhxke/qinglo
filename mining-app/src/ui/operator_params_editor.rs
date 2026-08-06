use egui::{Align, Color32, Layout, Pos2, RichText, Sense, Stroke, Ui, Vec2};
use crate::dag::{Node, NodeIORegistry, ParamType, PortDirection};
use super::state::CustomOperatorDebugState;

/// 渲染自定义算子参数编辑器。返回 `(是否发生修改, 是否点击了关闭按钮)`：
/// 修改用于标记 tab dirty；关闭按钮由外层据此隐藏整个参数面板。
pub fn render_custom_operator_editor(
    ui: &mut Ui,
    node: &mut Node,
    _debug_state: &mut CustomOperatorDebugState,
    io_registry: &mut NodeIORegistry,
    node_id: &str,
) -> (bool, bool) {
    // 标题栏：左侧标题 + 右侧 × 关闭按钮（点击后隐藏整个参数面板）
    // × 图标采用 painter 绘制两条对角线，与 tab 关闭按钮风格保持一致
    let mut close_clicked = false;
    ui.horizontal(|ui| {
        ui.heading("算子运行参数");
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let btn_size = 18.0;
            let (rect, resp) = ui.allocate_exact_size(Vec2::splat(btn_size), Sense::click());
            let painter = ui.painter();
            let cx = rect.center();
            if resp.hovered() {
                painter.circle_filled(cx, 9.0, Color32::from_rgba_unmultiplied(255, 255, 255, 18));
            }
            let icon_color = if resp.hovered() {
                Color32::from_rgb(230, 230, 230)
            } else {
                Color32::from_rgb(180, 180, 180)
            };
            let stroke = Stroke::new(1.4, icon_color);
            let s = 4.0;
            painter.line_segment(
                [Pos2::new(cx.x - s, cx.y - s), Pos2::new(cx.x + s, cx.y + s)],
                stroke,
            );
            painter.line_segment(
                [Pos2::new(cx.x - s, cx.y + s), Pos2::new(cx.x + s, cx.y - s)],
                stroke,
            );
            // on_hover_text 消费 resp（self），须在 clicked() 之后再调用
            if resp.clicked() {
                close_clicked = true;
            }
            resp.on_hover_text("隐藏参数面板");
        });
    });
    ui.separator();

    let def = node.operator_type.as_custom_mut();

    // 贯穿整个面板的修改标记：任何字段变更都置为 true，返回后由外层标记 tab dirty
    // 并触发 io_registry 失效。提前声明以便上方的"算子名称"编辑也能累加。
    let mut node_modified = false;

    // 惰性补全旧节点的文档字段：从旧建模文件加载的节点 summary / description_md 为空
    // （旧文件保存时这两个字段尚不存在）。此处按算子名从服务器缓存回填，使面板能展示
    // 详细说明。仅在内存中补全，不标记 dirty，避免无谓的保存提示。
    if def.description_md.is_empty() {
        if let Some((summary, description_md)) = crate::dag::lookup_operator_doc(&def.name) {
            if def.summary.is_empty() {
                def.summary = summary;
            }
            if !description_md.is_empty() {
                def.description_md = description_md;
            }
        }
    }

    // 算子名称（可编辑：用户可自定义算子名称，与算子开发视图保持一致）
    super::theme::card_frame().show(ui, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("算子名称:").strong());
            let resp = ui.add(
                egui::TextEdit::singleline(&mut def.name)
                    .hint_text("输入算子名称")
                    .desired_width(220.0),
            );
            if resp.changed() {
                node_modified = true;
            }
        });
    });

    ui.separator();

    // 端口信息（输入/输出端口的数据类型，只读展示）
    // 先收集为 owned 数据，避免与下方 def 的可变借用冲突
    let port_info: Vec<(PortDirection, String, String, String)> = {
        let mut input_idx = 0usize;
        let mut output_idx = 0usize;
        def.port_params.iter()
            .filter(|p| p.direction == PortDirection::Input || p.direction == PortDirection::Output)
            .map(|p| {
                let pos = match p.direction {
                    PortDirection::Input => { let i = input_idx; input_idx += 1; format!("inputs[{}]", i) }
                    PortDirection::Output => { let i = output_idx; output_idx += 1; format!("outputs[{}]", i) }
                    _ => String::new(),
                };
                (p.direction.clone(), p.name.clone(), p.param_type.to_str().to_string(), pos)
            })
            .collect()
    };
    super::theme::card_frame().show(ui, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("端口信息").strong());
            ui.add_space(8.0);
            let in_cnt = port_info.iter().filter(|(d, _, _, _)| *d == PortDirection::Input).count();
            let out_cnt = port_info.iter().filter(|(d, _, _, _)| *d == PortDirection::Output).count();
            ui.label(RichText::new(format!("输入 {} · 输出 {}", in_cnt, out_cnt)).weak().small());
        });

        if port_info.is_empty() {
            ui.label(RichText::new("该算子无输入/输出端口").weak().small());
        } else {
            egui::Grid::new("port_info_grid")
                .num_columns(4)
                .spacing([8.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    // 表头
                    ui.allocate_ui_with_layout(Vec2::new(50.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("方向").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(110.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("端口名称").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(80.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("数据类型").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(90.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("位置").small());
                    });
                    ui.end_row();

                    for (dir, name, ty, pos) in &port_info {
                        let (dir_text, dir_color) = match dir {
                            PortDirection::Input => ("输入", Color32::from_rgb(100, 180, 255)),
                            PortDirection::Output => ("输出", Color32::from_rgb(255, 180, 100)),
                            _ => ("参数", Color32::from_gray(180)),
                        };
                        ui.allocate_ui_with_layout(Vec2::new(50.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.label(RichText::new(dir_text).small().color(dir_color));
                        });
                        ui.allocate_ui_with_layout(Vec2::new(110.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.label(RichText::new(name).small());
                        });
                        ui.allocate_ui_with_layout(Vec2::new(80.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.label(RichText::new(ty).small().color(Color32::from_rgb(180, 220, 180)));
                        });
                        ui.allocate_ui_with_layout(Vec2::new(90.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                            ui.label(RichText::new(pos).small().monospace().weak());
                        });
                        ui.end_row();
                    }
                });
        }
    });

    ui.separator();

    // 参数配置管理
    super::theme::card_frame().show(ui, |ui| {
        ui.add_space(2.0);
        ui.label(RichText::new("参数配置").strong());

        // 只过滤出参数类型的配置
        let params: Vec<_> = def.port_params.iter().filter(|p| p.direction == PortDirection::Param).collect();

        if params.is_empty() {
            ui.label(RichText::new("该算子暂无参数配置").weak().small());
        } else {
            let mut to_remove: Option<usize> = None;
            egui::Grid::new("param_definition_grid")
                .num_columns(4)
                .spacing([8.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    // 表头
                    ui.allocate_ui_with_layout(Vec2::new(100.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("参数名称").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(80.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("类型").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(120.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("默认值").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(30.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("操作").small());
                    });
                    ui.end_row();

                    for (i, pp) in def.port_params.iter_mut().enumerate() {
                        if pp.direction != PortDirection::Param {
                            continue;
                        }

                        ui.push_id(i, |ui| {
                            // 参数名称
                            let mut name_changed = false;
                            ui.allocate_ui_with_layout(Vec2::new(100.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("name", |ui| {
                                    let resp = ui.add(
                                        egui::TextEdit::singleline(&mut pp.name)
                                            .hint_text("参数名称")
                                            .desired_width(100.0),
                                    );
                                    if resp.changed() {
                                        name_changed = true;
                                    }
                                });
                            });
                            if name_changed {
                                node_modified = true;
                            }

                            // 参数类型
                            let mut type_changed = false;
                            ui.allocate_ui_with_layout(Vec2::new(80.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("type", |ui| {
                                    egui::ComboBox::from_label("")
                                        .selected_text(pp.param_type.to_str())
                                        .show_ui(ui, |ui| {
                                            if ui.selectable_value(&mut pp.param_type, ParamType::Float, ParamType::Float.to_str()).changed() {
                                                type_changed = true;
                                            }
                                            if ui.selectable_value(&mut pp.param_type, ParamType::Int, ParamType::Int.to_str()).changed() {
                                                type_changed = true;
                                            }
                                            if ui.selectable_value(&mut pp.param_type, ParamType::String, ParamType::String.to_str()).changed() {
                                                type_changed = true;
                                            }
                                            if ui.selectable_value(&mut pp.param_type, ParamType::Bool, ParamType::Bool.to_str()).changed() {
                                                type_changed = true;
                                            }
                                        });
                                });
                            });
                            if type_changed {
                                node_modified = true;
                            }

                            // 默认值
                            let mut value_changed = false;
                            ui.allocate_ui_with_layout(Vec2::new(120.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("default_value", |ui| {
                                    let resp = ui.add(
                                        egui::TextEdit::singleline(&mut pp.default_value)
                                            .hint_text("默认值")
                                            .desired_width(120.0),
                                    );
                                    if resp.changed() {
                                        value_changed = true;
                                    }
                                });
                            });
                            if value_changed {
                                node_modified = true;
                            }

                            // 删除按钮
                            ui.allocate_ui_with_layout(Vec2::new(30.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("delete", |ui| {
                                    if ui.add(egui::Button::new("✕").small()).clicked() {
                                        to_remove = Some(i);
                                        node_modified = true;
                                    }
                                });
                            });
                        });
                        ui.end_row();
                    }
                });

            // 在循环结束后执行删除操作，避免 borrow checker 问题
            if let Some(idx) = to_remove {
                def.port_params.remove(idx);
            }
        }
    });

    ui.separator();

    // 详细说明（Markdown 阅读模式）：展示算子用法、参数说明与示例。
    // 置于面板末尾且默认折叠，使参数配置优先可见；需要时再展开查看。
    if !def.description_md.trim().is_empty() {
        super::theme::card_frame().show(ui, |ui| {
            ui.add_space(2.0);
            egui::CollapsingHeader::new(RichText::new("📖 详细说明").strong())
                .default_open(false)
                .show(ui, |ui| {
                    super::markdown_view::render_markdown(ui, &def.description_md);
                });
        });
    }

    if node_modified {
        io_registry.invalidate_node(node_id);
    }

    (node_modified, close_clicked)
}
