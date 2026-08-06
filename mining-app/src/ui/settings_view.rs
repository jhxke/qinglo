use egui::{Color32, RichText, ScrollArea, Ui};

use crate::config::{
    detect_rust_installation, get_compile_directory, get_default_compile_directory, load_config, save_compile_directory, save_rust_toolchain_path, test_rust_toolchain,
};
use super::state::UiState;

pub fn render_settings_view(ui: &mut Ui, state: &mut UiState) {
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.heading("系统设置");
            ui.separator();

            render_rust_toolchain_section(ui, state);
        });
}

fn render_rust_toolchain_section(ui: &mut Ui, state: &mut UiState) {
    super::theme::card_frame().show(ui, |ui| {
        ui.add_space(2.0);
        ui.heading(RichText::new("Rust 工具链").strong());
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "自定义算子通过调用 rustc 编译用户编写的 Rust 代码生成动态库。\
                 若系统 PATH 中找不到 rustc，请在此配置 Rust 安装位置（即包含 rustc 的 bin 目录，或其父目录）。",
            )
            .weak(),
        );
        ui.add_space(8.0);

        // 首次进入时，从磁盘配置懒加载已保存的路径到输入框
        if !state.settings.initialized {
            let config = load_config().ok();
            let saved_rust_path = config
                .as_ref()
                .and_then(|c| c.rust_toolchain_path.clone())
                .unwrap_or_default();
            // 编译目录直接使用当前有效目录（配置值或默认目录）
            let saved_compile_dir = config
                .as_ref()
                .and_then(|c| c.compile_directory.clone())
                .unwrap_or_else(|| get_default_compile_directory().display().to_string());
            state.settings.rust_path_input = saved_rust_path;
            state.settings.compile_dir_input = saved_compile_dir;
            state.settings.initialized = true;
        }

        ui.horizontal(|ui| {
            ui.label("Rust 安装路径:");
            ui.add(
                egui::TextEdit::singleline(&mut state.settings.rust_path_input)
                    .hint_text("例如: C:\\Users\\<你>\\.cargo\\bin")
                    .desired_width(360.0),
            );
        });

        ui.add_space(4.0);

        ui.horizontal(|ui| {
            if ui.button("自动检测").clicked() {
                match detect_rust_installation() {
                    Some(bin_dir) => {
                        let path_str = bin_dir.display().to_string();
                        match test_rust_toolchain(Some(&path_str)) {
                            Ok(version) => {
                                // 自动检测命中后直接落盘，避免用户忘记保存
                                if let Err(e) = save_rust_toolchain_path(Some(path_str.clone())) {
                                    state.settings.last_result =
                                        Some((false, format!("保存配置失败: {}", e)));
                                } else {
                                    state.settings.rust_path_input = path_str;
                                    state.settings.last_result =
                                        Some((true, format!("已检测并保存。{}", version)));
                                }
                            }
                            Err(e) => {
                                state.settings.last_result =
                                    Some((false, format!("检测到路径但 rustc 不可用: {}", e)));
                            }
                        }
                    }
                    None => {
                        state.settings.last_result = Some((
                            false,
                            "未在系统 PATH 中检测到 rustc，请手动填写 Rust 安装路径".to_string(),
                        ));
                    }
                }
            }

            if ui.button("测试").clicked() {
                let input = state.settings.rust_path_input.trim().to_string();
                let path_opt = if input.is_empty() { None } else { Some(input.as_str()) };
                match test_rust_toolchain(path_opt) {
                    Ok(version) => {
                        state.settings.last_result = Some((true, format!("rustc 可用: {}", version)));
                    }
                    Err(e) => {
                        state.settings.last_result = Some((false, e));
                    }
                }
            }

            if ui.button("保存").clicked() {
                let to_save = state.settings.rust_path_input.trim().to_string();
                match save_rust_toolchain_path(Some(to_save)) {
                    Ok(_) => {
                        state.settings.last_result =
                            Some((true, "Rust 安装路径已保存".to_string()));
                    }
                    Err(e) => {
                        state.settings.last_result =
                            Some((false, format!("保存失败: {}", e)));
                    }
                }
            }

            if ui.button("使用系统 PATH").clicked() {
                // 清空配置，回退到 PATH 查找
                match save_rust_toolchain_path(None) {
                    Ok(_) => {
                        state.settings.rust_path_input.clear();
                        state.settings.last_result =
                            Some((true, "已清空配置，将使用系统 PATH 中的 rustc".to_string()));
                    }
                    Err(e) => {
                        state.settings.last_result =
                            Some((false, format!("保存失败: {}", e)));
                    }
                }
            }
        });

        ui.add_space(8.0);

        // 展示最近一次操作结果
        if let Some((success, message)) = &state.settings.last_result {
            let color = if *success { Color32::from_rgb(46, 204, 113) } else { Color32::from_rgb(231, 76, 60) };
            ui.colored_label(color, RichText::new(message).strong());
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("路径说明").strong());
        ui.label("• 可填入 rustc 所在的 bin 目录，如 C:\\Users\\<你>\\.cargo\\bin");
        ui.label("• 也可填入其父目录（如 C:\\Users\\<你>\\.cargo），程序会自动查找 bin 子目录");
        ui.label("• 留空则使用系统 PATH 中的 rustc");
        ui.label("• 配置后即可在「挖掘分析」视图中编译并运行自定义算子");
    });

    ui.add_space(16.0);

    render_compile_directory_section(ui, state);
}

fn render_compile_directory_section(ui: &mut Ui, state: &mut UiState) {
    super::theme::card_frame().show(ui, |ui| {
        ui.add_space(2.0);
        ui.heading(RichText::new("编译目录").strong());
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "自定义算子编译时会在指定目录下生成临时文件（lib.rs 和动态库）。\
                 若不配置，则使用默认目录 compile（位于程序运行目录下）。",
            )
            .weak(),
        );
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("编译目录:");
            ui.add(
                egui::TextEdit::singleline(&mut state.settings.compile_dir_input)
                    .hint_text("例如: C:\\Users\\<你>\\AppData\\Local\\stock-factor-miner\\compile")
                    .desired_width(360.0),
            );
        });

        ui.add_space(4.0);

        // 显示当前有效的编译目录
        let effective_dir = get_compile_directory();
        ui.label(RichText::new(format!("当前有效目录: {}", effective_dir.display())).weak());

        ui.add_space(4.0);

        ui.horizontal(|ui| {
            if ui.button("使用默认目录").clicked() {
                let default_dir = get_default_compile_directory();
                state.settings.compile_dir_input = default_dir.display().to_string();
                match save_compile_directory(None) {
                    Ok(_) => {
                        state.settings.last_result =
                            Some((true, format!("已配置为默认目录: {}", default_dir.display())));
                    }
                    Err(e) => {
                        state.settings.last_result =
                            Some((false, format!("保存失败: {}", e)));
                    }
                }
            }

            if ui.button("保存").clicked() {
                let to_save = state.settings.compile_dir_input.trim().to_string();
                match save_compile_directory(Some(to_save)) {
                    Ok(_) => {
                        state.settings.last_result =
                            Some((true, "编译目录已保存".to_string()));
                    }
                    Err(e) => {
                        state.settings.last_result =
                            Some((false, format!("保存失败: {}", e)));
                    }
                }
            }
        });

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.label(RichText::new("目录说明").strong());
        ui.label("• 指定目录用于存放编译过程中的临时文件（lib.rs 和生成的动态库）");
        ui.label("• 留空则使用默认目录 <程序目录>/target/compile");
        ui.label("• 建议配置为项目目录下的专用文件夹，便于调试和查看编译产物");
    });
}
