use egui::{Color32, RichText, Ui, Vec2, Layout, Align};
use crate::dag::{CustomOperatorDef, OperatorPortParamDef, PortDirection, ParamType};
use crate::ui::state::{CustomOperatorDebugState, LogLevel, OperatorDevelopmentState};

pub fn render_operator_development_view(ui: &mut Ui, state: &mut OperatorDevelopmentState) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        let result_message = render_operator_editor(ui, &mut state.current_operator, &mut state.debug_state);
        if let Some(msg) = result_message {
            state.error_message = Some(msg);
        }
    });

    // 运行日志显示区
    egui::TopBottomPanel::bottom("operator_dev_log_panel")
        .default_height(150.0)
        .frame(
            egui::Frame::none()
                .fill(super::theme::CARD_BG)
                .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                .rounding(super::theme::CARD_ROUNDING)
                .stroke(egui::Stroke::new(1.0, super::theme::CARD_STROKE)),
        )
        .show_inside(ui, |ui| {
            render_run_log_panel(ui, state);
        });
}

fn render_operator_editor(
    ui: &mut Ui,
    def: &mut CustomOperatorDef,
    debug_state: &mut CustomOperatorDebugState,
) -> Option<String> {
    ui.heading("算子开发");
    ui.separator();

    let mut result_message: Option<String> = None;

    super::theme::card_frame().show(ui, |ui| {
        ui.add_space(2.0);
        ui.label("算子名称:");
        ui.text_edit_singleline(&mut def.name);

        ui.label("描述（一句话，列表悬停展示）:");
        ui.text_edit_singleline(&mut def.description);

        ui.label("摘要（一句话，列表卡片展示）:");
        ui.text_edit_singleline(&mut def.summary);

        ui.label(
            RichText::new("详细描述（Markdown，运行参数面板阅读模式展示）")
                .weak()
                .small(),
        );
        ui.add(
            egui::TextEdit::multiline(&mut def.description_md)
                .desired_width(f32::INFINITY)
                .desired_rows(8)
                .hint_text("# 标题\n支持 Markdown：**粗体**、`代码`、列表、表格等"),
        );
    });

    ui.separator();

    // 端口与参数定义管理（合并）
    super::theme::card_frame().show(ui, |ui| {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("端口与参数定义").strong());
            ui.add_space(8.0);
            if ui.button("+ 添加输入端口").clicked() {
                def.port_params.push(OperatorPortParamDef {
                    name: format!("input_{}", def.port_params.iter().filter(|p| p.direction == PortDirection::Input).count()),
                    direction: PortDirection::Input,
                    param_type: ParamType::DataFrame,
                    default_value: "".to_string(),
                });
            }
            ui.add_space(4.0);
            if ui.button("+ 添加输出端口").clicked() {
                def.port_params.push(OperatorPortParamDef {
                    name: format!("output_{}", def.port_params.iter().filter(|p| p.direction == PortDirection::Output).count()),
                    direction: PortDirection::Output,
                    param_type: ParamType::DataFrame,
                    default_value: "".to_string(),
                });
            }
            ui.add_space(4.0);
            if ui.button("+ 添加参数").clicked() {
                def.port_params.push(OperatorPortParamDef {
                    name: format!("param_{}", def.port_params.iter().filter(|p| p.direction == PortDirection::Param).count()),
                    direction: PortDirection::Param,
                    param_type: ParamType::Float,
                    default_value: "0.0".to_string(),
                });
            }
        });

        // 显示端口统计
        let input_count = def.port_params.iter().filter(|p| p.direction == PortDirection::Input).count();
        let output_count = def.port_params.iter().filter(|p| p.direction == PortDirection::Output).count();
        let param_count = def.port_params.iter().filter(|p| p.direction == PortDirection::Param).count();
        ui.label(RichText::new(format!("输入端口: {}, 输出端口: {}, 参数: {}", input_count, output_count, param_count)).small().weak());

        if def.port_params.is_empty() {
            ui.label(RichText::new("暂无端口或参数，点击上方按钮添加").weak().small());
        } else {
            let mut to_remove: Option<usize> = None;
            egui::Grid::new("port_param_definition_grid")
                .num_columns(6)
                .spacing([8.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    // 表头
                    ui.allocate_ui_with_layout(Vec2::new(50.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("方向").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(80.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("名称").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(80.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("类型").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(60.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("默认值").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(100.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("参数位置").small());
                    });
                    ui.allocate_ui_with_layout(Vec2::new(30.0, 20.0), Layout::left_to_right(Align::Center), |ui| {
                        ui.label(RichText::new("操作").small());
                    });
                    ui.end_row();

                    // 计算每个端口/参数的位置信息
                    let mut port_pos = 0;
                    for (i, pp) in def.port_params.iter_mut().enumerate() {
                        let pos_info = match pp.direction {
                            PortDirection::Input => {
                                let info = format!("inputs[{}]", port_pos);
                                port_pos += 1;
                                info
                            }
                            PortDirection::Output => {
                                let info = format!("outputs[{}]", port_pos);
                                port_pos += 1;
                                info
                            }
                            PortDirection::Param => {
                                format!("PARAM_{}", pp.name.to_uppercase())
                            }
                        };

                        ui.push_id(i, |ui| {
                            // 方向
                            ui.allocate_ui_with_layout(Vec2::new(50.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("direction", |ui| {
                                    egui::ComboBox::from_label("")
                                        .selected_text(pp.direction.to_str())
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(&mut pp.direction, PortDirection::Input, PortDirection::Input.to_str());
                                            ui.selectable_value(&mut pp.direction, PortDirection::Output, PortDirection::Output.to_str());
                                            ui.selectable_value(&mut pp.direction, PortDirection::Param, PortDirection::Param.to_str());
                                        });
                                });
                            });

                            // 名称
                            ui.allocate_ui_with_layout(Vec2::new(80.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("name", |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut pp.name)
                                            .hint_text("名称")
                                            .desired_width(80.0),
                                    );
                                });
                            });

                            // 类型（根据方向限制可选类型）
                            ui.allocate_ui_with_layout(Vec2::new(80.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("type", |ui| {
                                    egui::ComboBox::from_label("")
                                        .selected_text(pp.param_type.to_str())
                                        .show_ui(ui, |ui| {
                                            match pp.direction {
                                                PortDirection::Input | PortDirection::Output => {
                                                    ui.selectable_value(&mut pp.param_type, ParamType::DataFrame, ParamType::DataFrame.to_str());
                                                    ui.selectable_value(&mut pp.param_type, ParamType::DataFrameArray, ParamType::DataFrameArray.to_str());
                                                    ui.selectable_value(&mut pp.param_type, ParamType::Float, ParamType::Float.to_str());
                                                    ui.selectable_value(&mut pp.param_type, ParamType::Int, ParamType::Int.to_str());
                                                    ui.selectable_value(&mut pp.param_type, ParamType::String, ParamType::String.to_str());
                                                    ui.selectable_value(&mut pp.param_type, ParamType::Bool, ParamType::Bool.to_str());
                                                }
                                                PortDirection::Param => {
                                                    ui.selectable_value(&mut pp.param_type, ParamType::Float, ParamType::Float.to_str());
                                                    ui.selectable_value(&mut pp.param_type, ParamType::Int, ParamType::Int.to_str());
                                                    ui.selectable_value(&mut pp.param_type, ParamType::String, ParamType::String.to_str());
                                                    ui.selectable_value(&mut pp.param_type, ParamType::Bool, ParamType::Bool.to_str());
                                                }
                                            }
                                        });
                                });
                            });

                            // 默认值（参数类型显示，端口类型隐藏）
                            ui.allocate_ui_with_layout(Vec2::new(60.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("default_value", |ui| {
                                    if pp.direction == PortDirection::Param {
                                        ui.add(
                                            egui::TextEdit::singleline(&mut pp.default_value)
                                                .hint_text("默认值")
                                                .desired_width(60.0),
                                        );
                                    } else {
                                        ui.label(RichText::new("-").weak().small());
                                    }
                                });
                            });

                            // 参数位置/使用方式
                            ui.allocate_ui_with_layout(Vec2::new(100.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("pos_info", |ui| {
                                    ui.label(RichText::new(pos_info).monospace().small().weak());
                                });
                            });

                            // 删除按钮
                            ui.allocate_ui_with_layout(Vec2::new(30.0, 24.0), Layout::left_to_right(Align::Center), |ui| {
                                ui.push_id("delete", |ui| {
                                    if ui.add(egui::Button::new("✕").small()).clicked() {
                                        to_remove = Some(i);
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

    super::theme::card_frame().show(ui, |ui| {
        ui.add_space(2.0);
        ui.label("Rust 代码:");
        super::code_editor::render_code_editor(ui, &mut def.code);
    });

    ui.separator();

    ui.label(RichText::new("代码说明").small());
    ui.label("• 函数签名由端口/参数定义自动生成");
    ui.label("• 输入端口 (DataFrame/DataFrameArray): `inputs: *const *const PortData`（以 NULL 结尾，自行扫描）");
    ui.label("• 输出端口 (DataFrame/DataFrameArray): `outputs: *mut *mut PortData`（按序写入，NULL 结尾）");
    ui.label("• 参数 (Float/Int/String/Bool): 通过常量 PARAM_参数名 访问");
    ui.label("• 返回 0 表示成功，非 0 表示失败");
    ui.label("• 默认模板实现 N 日动量因子，可在此基础上修改");

    ui.separator();

    ui.horizontal(|ui| {
        if ui.button("编译").clicked() {
            // 注入参数到代码中（使用算子定义中的参数默认值）
            let params: Vec<_> = def.port_params.iter().filter(|p| p.direction == PortDirection::Param).collect();
            let code_with_params = crate::operator_executor::inject_params_into_code(&def.code, &params);
            match crate::operator_executor::compile_only(&code_with_params, &def.name) {
                Ok(dll_path) => {
                    // 编译成功后自动保存 JSON 到 DLL 同一目录，文件名为 operator.json
                    if let Some(dll_dir) = dll_path.parent() {
                        let json_path = dll_dir.join("operator.json");
                        
                        match serde_json::to_string_pretty(def) {
                            Ok(content) => {
                                match std::fs::write(&json_path, content) {
                                    Ok(_) => {
                                        result_message = Some(format!("编译成功!\nDLL: {}\nJSON: {}", dll_path.display(), json_path.display()));
                                    }
                                    Err(e) => {
                                        result_message = Some(format!("编译成功，但保存 JSON 失败: {}", e));
                                    }
                                }
                            }
                            Err(e) => {
                                result_message = Some(format!("编译成功，但序列化失败: {}", e));
                            }
                        }
                    } else {
                        result_message = Some("编译成功，但无法获取 DLL 目录".to_string());
                    }
                }
                Err(e) => {
                    result_message = Some(format!("编译失败:\n{}", e));
                }
            }
        }

        ui.add_space(8.0);

        if ui.button("启用").clicked() {
            match crate::operator_executor::enable_operator(def) {
                Ok(msg) => {
                    result_message = Some(msg);
                }
                Err(e) => {
                    result_message = Some(format!("启用失败:\n{}", e));
                }
            }
        }
    });

    ui.separator();

    // ===== Debug 面板 =====
    ui.heading(RichText::new("Debug 调试").strong());
    ui.label(
        RichText::new(
            "Debug 模式使用 -C opt-level=0 -C debuginfo=2 编译，保留临时目录与产物，\n                 输出完整 rustc 日志，便于定位问题或附加外部调试器。",
        )
        .weak()
        .small(),
    );
    ui.add_space(4.0);

    ui.horizontal(|ui| {
        ui.label("测试输入:");
        ui.add(
            egui::TextEdit::singleline(&mut debug_state.input_text)
                .hint_text("例如: 1, 2, 3, 4, 5  (多路用 ; 分隔)")
                .desired_width(260.0),
        );
    });
    ui.label(
        RichText::new("格式: 逗号分隔数字，分号分隔多路输入，如 `1,2,3;4,5,6`")
            .weak()
            .small(),
    );

    ui.horizontal(|ui| {
        let debug_btn = ui.add(
            egui::Button::new(RichText::new("🐛 Debug").color(Color32::WHITE))
                .fill(Color32::from_rgb(46, 134, 193))
                .min_size(Vec2::new(120.0, 28.0)),
        );
        if debug_btn.clicked() {
            match crate::debug_executor::parse_debug_inputs(&debug_state.input_text) {
                Ok(inputs) => {
                    // 注入参数到代码中
                    let params: Vec<_> = def.port_params.iter().filter(|p| p.direction == PortDirection::Param).collect();
                    let code_with_params = crate::operator_executor::inject_params_into_code(&def.code, &params);
                    let diag = crate::debug_executor::compile_and_execute_debug(
                        &code_with_params,
                        inputs,
                        &def.name,
                    );
                    let success = diag.success;
                    let msg = if success {
                        "Debug 运行成功".to_string()
                    } else {
                        "Debug 运行失败".to_string()
                    };
                    debug_state.diagnostics = Some(diag);
                    result_message = Some(msg);
                }
                Err(e) => {
                    result_message = Some(format!("输入解析失败: {}", e));
                }
            }
        }

        if ui.button("清空诊断").clicked() {
            debug_state.diagnostics = None;
        }
    });

    // 诊断信息展示
    if let Some(diag) = debug_state.diagnostics.as_mut() {
        ui.add_space(4.0);
        ui.separator();

        let status_color = if diag.success {
            Color32::from_rgb(46, 204, 113)
        } else {
            Color32::from_rgb(231, 76, 60)
        };
        let status_text = if diag.success { "✓ 成功" } else { "✗ 失败" };
        ui.colored_label(status_color, RichText::new(format!("状态: {}", status_text)).strong());

        ui.add_space(2.0);
        ui.label(format!("编译耗时: {} ms", diag.compile_duration_ms));
        ui.label(format!("执行耗时: {} ms", diag.execute_duration_ms));

        if let Some(lib_path) = &diag.lib_path {
            ui.label(format!("动态库: {}", lib_path.display()));
        }
        if let Some(size) = diag.lib_size_bytes {
            ui.label(format!("动态库大小: {} bytes ({} KB)", size, size / 1024));
        }
        if let Some(tmp) = &diag.temp_dir {
            ui.label(format!("临时目录: {}", tmp.display()));
        }

        // 输入回显
        ui.add_space(2.0);
        ui.label(RichText::new("实际输入:").strong());
        for (i, input) in diag.inputs.iter().enumerate() {
            ui.label(format!("  [{}]: {:?}", i, input));
        }

        // 输出
        if let Some(outputs) = &diag.outputs {
            ui.label(RichText::new("输出:").strong());
            ui.label(format!("  {:?}", outputs));
        }

        // 错误
        if let Some(err) = &diag.error {
            ui.add_space(2.0);
            ui.colored_label(
                Color32::from_rgb(231, 76, 60),
                RichText::new(format!("错误: {}", err)).strong(),
            );
        }

        // rustc stderr (含警告/错误详情)
        if !diag.rustc_stderr.trim().is_empty() {
            ui.add_space(2.0);
            ui.label(RichText::new("rustc stderr:").strong());
            egui::CollapsingHeader::new("展开查看")
                .default_open(!diag.success)
                .show(ui, |ui| {
                    ui.label(RichText::new(&diag.rustc_stderr).monospace().small());
                });
        }
    }

    ui.add_space(16.0);

    result_message
}

fn render_run_log_panel(ui: &mut Ui, state: &mut OperatorDevelopmentState) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("运行日志").strong());
        ui.add_space(8.0);
        if ui.button("清空").clicked() {
            state.clear_logs();
        }
    });
    ui.separator();
    
    egui::ScrollArea::vertical().show(ui, |ui| {
        for log in &state.run_logs {
            let color = match log.level {
                LogLevel::Info => Color32::from_rgb(180, 180, 180),
                LogLevel::Success => Color32::from_rgb(46, 204, 113),
                LogLevel::Warning => Color32::from_rgb(241, 196, 15),
                LogLevel::Error => Color32::from_rgb(231, 76, 60),
            };
            ui.colored_label(color, format!("[{}] {}", log.timestamp, log.message));
        }
        
        if state.run_logs.is_empty() {
            ui.label(RichText::new("暂无日志").weak());
        }
    });
}