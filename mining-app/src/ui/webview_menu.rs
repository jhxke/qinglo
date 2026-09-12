//! wry WebView2 菜单测试页（仅 Windows）。
//!
//! ## 宿主窗口架构（白屏排查后的最终方案）
//!
//! WebView2 **不能**直接作为 wgpu flip-model swapchain 主窗口的子 HWND：
//! 实测子窗口在、JS/IPC 正常、bounds/可见性正确，但 DWM 始终只合成
//! 父窗口的 swapchain 内容，网页画面永远不出现（WS_CLIPCHILDREN、
//! `--disable-gpu-compositing`、关闭 occlusion、DWM per-pixel alpha
//! 透明路径均无法解决——普通 redirection-bitmap HWND 的 flip 呈现
//! 与浏览器子 HWND 的合成不被支持）。
//!
//! 因此这里自建一个 **WS_POPUP + WS_EX_NOREDIRECTIONBITMAP** 的顶级
//! 宿主窗口（owner = iced 主窗口），再用 `build_as_child` 把
//! WebView2 放进该 popup：
//!
//! - `WS_EX_NOREDIRECTIONBITMAP`：无 GDI 重定向表面，WebView2 自动走
//!   DirectComposition visual 呈现（WebView2 官方推荐宿主模式）；
//! - owner 关系：popup 总在主窗口之上、任务栏不显示、主窗口最小化时
//!   系统自动隐藏；
//! - popup 定位在内容区屏幕坐标（避开自绘标题栏 / 活动栏 / 状态栏），
//!   主窗口移动 / resize / DPI 变化时重新定位。
//!
//! 通信（插件协议见 `webview_plugins` 模块文档）：
//! - JS → Rust：`window.ipc.postMessage("cmd|插件|动作|参数")` → IPC handler
//!   → mpsc → Tick 轮询 → [`super::webview_plugins::PluginRegistry`] 路由
//! - Rust → JS：`WebView::evaluate_script("window.rustReply(plugin, kind, text)")`
//!
//! 菜单内容完全插件化：页面 HTML 由插件注册表动态拼装（内置插件 +
//! `webview_plugins/` 目录下的外部插件），本模块只负责窗口宿主与消息收发。
//!
//! 注意：`wry::WebView` 是 `!Send`，整个生命周期必须留在 UI 主线程，
//! 因此本模块所有方法都只在 `MyApp::update`（winit 主线程）中调用。
//!
//! 诊断：关键事件 append 到 `%TEMP%\qinglo_webview_menu.log`。

#![cfg(windows)]

use std::fs::OpenOptions;
use std::io::Write;
use std::num::NonZero;
use std::sync::mpsc::{channel, Receiver};
use std::time::{SystemTime, UNIX_EPOCH};

use iced::Size;
use wry::dpi::{LogicalPosition, LogicalSize, Position, Size as DpiSize};
use wry::raw_window_handle::{
    RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use wry::{Rect, WebView, WebViewBuilder, WebViewBuilderExtWindows};

use super::webview_plugins::{
    IncomingMessage, PluginOutcome, PluginRegistry, parse_message,
};

// ===== 内容区边距（相对 iced 主窗口客户区，逻辑像素）=====
// 与 title_bar.rs / activity_bar.rs / status_bar.rs 的布局常量保持一致：
// 标题栏 40 + 下分隔 1；活动栏 62 + 右分隔 1；状态栏 29。
const LEFT_INSET: f64 = 63.0;
const TOP_INSET: f64 = 41.0;
const BOTTOM_INSET: f64 = 29.0;

/// WebView2 菜单页的宿主状态。
///
/// `webview` 为 None 时尚未创建（首次进入菜单时懒创建）；
/// 创建后常驻，切走时仅隐藏 popup，避免反复启动浏览器进程。
pub struct WebViewMenu {
    webview: Option<WebView>,
    /// 自建 popup 宿主窗口 HWND（0 = 尚未创建）。
    popup_hwnd: isize,
    /// 主窗口 HWND（0 = 尚未通过 `iced::window::run` 取到）。
    hwnd: isize,
    /// 主窗口整窗逻辑尺寸。
    width: f32,
    height: f32,
    /// popup 当前是否应对用户可见。
    visible: bool,
    /// IPC 接收端；Sender 在 build 时 move 进 wry 的 ipc handler。
    ipc_rx: Option<Receiver<String>>,
    /// 菜单插件注册表：内置插件 + 外部 `webview_plugins/` 扫描结果。
    registry: PluginRegistry,
}

impl Default for WebViewMenu {
    fn default() -> Self {
        Self {
            webview: None,
            popup_hwnd: 0,
            hwnd: 0,
            width: 0.0,
            height: 0.0,
            visible: false,
            ipc_rx: None,
            registry: PluginRegistry::load(),
        }
    }
}

impl Drop for WebViewMenu {
    fn drop(&mut self) {
        // 先显式结束 WebView（wry 会销毁其控制器与子窗口），再销毁 popup。
        self.webview = None;
        if self.popup_hwnd != 0 {
            unsafe {
                let _ = DestroyWindow(self.popup_hwnd);
            }
            diag("popup 宿主窗口已销毁");
        }
    }
}

impl WebViewMenu {
    pub fn is_ready(&self) -> bool {
        self.webview.is_some()
    }

    pub fn set_hwnd(&mut self, hwnd: isize) {
        self.hwnd = hwnd;
        diag(&format!("主窗口 HWND 已获取：{hwnd:#x}"));
    }

    /// 从 iced 窗口对象提取 Win32 HWND。
    pub fn extract_hwnd(window: &dyn iced::window::Window) -> Option<isize> {
        let handle = window.window_handle().ok()?;
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            Some(win32.hwnd.get() as isize)
        } else {
            None
        }
    }

    /// 首次创建 popup 宿主窗口 + 嵌入 WebView2（必须在主线程调用）。
    pub fn build(&mut self, size: Size) -> Result<(), String> {
        if self.webview.is_some() {
            return Ok(());
        }
        if self.hwnd == 0 {
            return Err("HWND 尚未获取".to_string());
        }

        let (tx, rx) = channel::<String>();
        self.ipc_rx = Some(rx);
        self.width = size.width.max(1.0);
        self.height = size.height.max(1.0);
        diag(&format!(
            "开始创建 WebView2，整窗逻辑尺寸 {:.0}x{:.0}，内容区 {:.0}x{:.0}",
            self.width,
            self.height,
            (self.width as f64 - LEFT_INSET).max(1.0),
            (self.height as f64 - TOP_INSET - BOTTOM_INSET).max(1.0),
        ));

        // 1) 自建 WS_POPUP | WS_EX_NOREDIRECTIONBITMAP 顶级宿主窗口。
        let popup_hwnd = self.create_popup()?;
        self.popup_hwnd = popup_hwnd;

        // SAFETY: popup 为本进程刚创建的有效窗口，webview 生命周期不超过它。
        let window_handle = unsafe {
            let hwnd_nz = NonZero::new(popup_hwnd).ok_or("popup HWND 为空")?;
            let win32 = Win32WindowHandle::new(hwnd_nz);
            WindowHandle::borrow_raw(RawWindowHandle::Win32(win32))
        };

        // 页面 HTML 由插件注册表动态拼装（内置插件 + 外部插件）。
        let page_html = self.registry.render_page();

        // 2) WebView2 作为 popup 的子窗口铺满整个 popup 客户区。
        let webview = WebViewBuilder::new()
            .with_bounds(self.content_rect())
            .with_html(&page_html)
            // 控制器背景透明：popup 无重定向表面，首帧网页内容到达前
            // 透出后面的 iced 占位层（天然的"加载中"背景）。
            .with_transparent(true)
            // 显式浏览器参数（显式设置会整体覆盖 wry 默认值，需保留其默认
            // 关闭的 feature 列表）。CalculateNativeWinOcclusion 排除宿主
            // 遮挡检测导致渲染表面冻结的已知问题。
            .with_additional_browser_args(
                "--disable-features=CalculateNativeWinOcclusion,msWebOOUI,msPdfOOUI,msSmartScreenProtection",
            )
            // 测试期开放 DevTools（右键 → 检查），正式集成时应关闭。
            .with_devtools(true)
            .with_ipc_handler(move |request| {
                let body = request.body().clone();
                diag(&format!("IPC 收到：{body}"));
                let _: Result<(), _> = tx.send(body);
            })
            .build_as_child(&window_handle)
            .map_err(|e| {
                let msg = format!("WebView2 创建失败：{e}");
                diag(&msg);
                msg
            })?;

        self.webview = Some(webview);
        diag("WebView2 创建成功（popup + DComp 宿主模式）");
        Ok(())
    }

    /// 创建 popup 宿主窗口并定位到主窗口内容区。
    fn create_popup(&mut self) -> Result<isize, String> {
        const WS_POPUP: u32 = 0x8000_0000;
        const WS_CLIPCHILDREN: u32 = 0x0200_0000;
        const WS_CLIPSIBLINGS: u32 = 0x0400_0000;
        const WS_EX_NOREDIRECTIONBITMAP: u32 = 0x0020_0000;
        const CS_HREDRAW: u32 = 0x0002;
        const CS_VREDRAW: u32 = 0x0001;

        #[link(name = "user32")]
        extern "system" {
            fn RegisterClassExW(class: *const WNDCLASSEXW) -> u16;
            fn CreateWindowExW(
                ex_style: u32,
                class_name: *const u16,
                window_name: *const u16,
                style: u32,
                x: i32,
                y: i32,
                width: i32,
                height: i32,
                parent: isize,
                menu: isize,
                instance: isize,
                param: isize,
            ) -> isize;
            fn DefWindowProcW(hwnd: isize, msg: u32, wparam: isize, lparam: isize) -> isize;
            fn GetDpiForWindow(hwnd: isize) -> u32;
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetModuleHandleW(name: *const u16) -> isize;
        }

        // 简单的 DefWindowProc 包装（不能直接把 DefWindowProcW 作为 lpfnWndProc，
        // 调用约定一致但 Rust 要求函数项类型显式）。
        unsafe extern "system" fn host_wndproc(
            hwnd: isize,
            msg: u32,
            wparam: isize,
            lparam: isize,
        ) -> isize {
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        #[repr(C)]
        struct WNDCLASSEXW {
            cb_size: u32,
            style: u32,
            lpfn_wnd_proc: Option<unsafe extern "system" fn(isize, u32, isize, isize) -> isize>,
            cls_extra: i32,
            wnd_extra: i32,
            instance: isize,
            icon: isize,
            cursor: isize,
            hbr_background: isize,
            menu_name: *const u16,
            class_name: *const u16,
            icon_sm: isize,
        }

        let class_name: Vec<u16> = "QingloWebViewHost\0".encode_utf16().collect();
        let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
        let class = WNDCLASSEXW {
            cb_size: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfn_wnd_proc: Some(host_wndproc),
            cls_extra: 0,
            wnd_extra: 0,
            instance,
            icon: 0,
            cursor: 0,
            // WS_EX_NOREDIRECTIONBITMAP 窗口没有 GDI 背景，背景刷无意义。
            hbr_background: 0,
            menu_name: std::ptr::null(),
            class_name: class_name.as_ptr(),
            icon_sm: 0,
        };
        // 重复注册返回 ERROR_CLASS_ALREADY_EXISTS(1412)，忽略即可。
        let atom = unsafe { RegisterClassExW(&class) };
        if atom == 0 {
            diag("RegisterClassExW 返回 0（可能类已存在，继续）");
        }

        let (x, y, phys_w, phys_h) = self.content_rect_physical();
        // 创建为不可见的 owned popup（hWndParent 传 owner）。
        let popup_hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOREDIRECTIONBITMAP,
                class_name.as_ptr(),
                std::ptr::null(),
                WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                x,
                y,
                phys_w,
                phys_h,
                self.hwnd,
                0,
                instance,
                0,
            )
        };
        if popup_hwnd == 0 {
            let err = std::io::Error::last_os_error();
            return Err(format!("CreateWindowExW(popup) 失败：{err}"));
        }

        let dpi = unsafe { GetDpiForWindow(popup_hwnd) };
        diag(&format!(
            "popup 宿主窗口已创建：{popup_hwnd:#x}，物理位置 ({x},{y}) 尺寸 {phys_w}x{phys_h}，DPI {dpi}"
        ));
        Ok(popup_hwnd)
    }

    pub fn show(&mut self) {
        const SW_SHOW: i32 = 5;
        self.visible = true;
        self.relocate();
        if self.popup_hwnd != 0 {
            unsafe {
                ShowWindow(self.popup_hwnd, SW_SHOW);
            }
        }
        if let Some(webview) = &self.webview {
            let _ = webview.set_bounds(self.content_rect());
        }
        diag("popup + WebView2 已显示");
    }

    pub fn hide(&mut self) {
        const SW_HIDE: i32 = 0;
        self.visible = false;
        if self.popup_hwnd != 0 {
            unsafe {
                ShowWindow(self.popup_hwnd, SW_HIDE);
            }
        }
        diag("popup + WebView2 已隐藏");
    }

    /// 跟随 iced 主窗口尺寸更新（resize / DPI 变化）。
    pub fn set_size(&mut self, size: Size) {
        self.width = size.width.max(1.0);
        self.height = size.height.max(1.0);
        self.relocate();
    }

    /// 主窗口移动后重新定位 popup。
    pub fn relocate(&mut self) {
        if self.popup_hwnd == 0 {
            return;
        }
        let (x, y, phys_w, phys_h) = self.content_rect_physical();
        const SWP_NOACTIVATE: u32 = 0x0010;
        const SWP_NOZORDER: u32 = 0x0004;
        #[link(name = "user32")]
        extern "system" {
            fn SetWindowPos(
                hwnd: isize,
                after: isize,
                x: i32,
                y: i32,
                cx: i32,
                cy: i32,
                flags: u32,
            ) -> i32;
        }
        let ok = unsafe {
            SetWindowPos(
                self.popup_hwnd,
                0,
                x,
                y,
                phys_w,
                phys_h,
                SWP_NOACTIVATE | SWP_NOZORDER,
            )
        };
        if ok == 0 {
            diag("relocate：SetWindowPos 失败");
        }
        if let Some(webview) = &self.webview {
            let _ = webview.set_bounds(self.content_rect());
        }
    }

    /// 轮询兜底：对比 popup 当前矩形与目标矩形，不一致则重新定位。
    ///
    /// 覆盖 Moved/Resized 事件漏掉的状态变化（最大化 / 贴靠 / DPI 切换
    /// 的中间态）。每 120ms 一次的 GetWindowRect 开销可忽略。
    pub fn sync_bounds(&mut self) {
        if !self.visible || self.popup_hwnd == 0 {
            return;
        }
        #[repr(C)]
        #[derive(Clone, Copy)]
        struct RECT {
            left: i32,
            top: i32,
            right: i32,
            bottom: i32,
        }
        #[link(name = "user32")]
        extern "system" {
            fn GetWindowRect(hwnd: isize, rect: *mut RECT) -> i32;
        }
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if unsafe { GetWindowRect(self.popup_hwnd, &mut rect) } == 0 {
            return;
        }
        let (x, y, w, h) = self.content_rect_physical();
        if rect.left != x
            || rect.top != y
            || rect.right - rect.left != w
            || rect.bottom - rect.top != h
        {
            diag(&format!(
                "sync_bounds 检测到偏移（{} {},{}x{} → {} {},{}x{}），校准",
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                x,
                y,
                w,
                h
            ));
            self.relocate();
        }
    }

    /// 计算 popup 相对屏幕的物理矩形（主窗口内容区）。
    fn content_rect_physical(&self) -> (i32, i32, i32, i32) {
        #[repr(C)]
        #[derive(Clone, Copy, Default)]
        struct POINT {
            x: i32,
            y: i32,
        }
        #[link(name = "user32")]
        extern "system" {
            fn GetDpiForWindow(hwnd: isize) -> u32;
            fn ClientToScreen(hwnd: isize, point: *mut POINT) -> i32;
        }
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 };
        let left = (LEFT_INSET * scale).round() as i32;
        let top = (TOP_INSET * scale).round() as i32;
        let bottom = (BOTTOM_INSET * scale).round() as i32;
        let mut origin = POINT { x: left, y: top };
        unsafe {
            ClientToScreen(self.hwnd, &mut origin);
        }
        let phys_w = ((self.width as f64) * scale - LEFT_INSET * scale).round() as i32;
        let phys_h =
            ((self.height as f64) * scale - (TOP_INSET + BOTTOM_INSET) * scale).round() as i32;
        (
            origin.x,
            origin.y,
            phys_w.max(1),
            phys_h.max(1),
        )
    }

    /// wry 逻辑坐标矩形：铺满 popup 客户区（popup 客户区 == 内容区）。
    fn content_rect(&self) -> Rect {
        Rect {
            position: Position::Logical(LogicalPosition::new(0.0, 0.0)),
            size: DpiSize::Logical(LogicalSize::new(
                (self.width as f64 - LEFT_INSET).max(1.0),
                (self.height as f64 - TOP_INSET - BOTTOM_INSET).max(1.0),
            )),
        }
    }

    /// 非阻塞排空 JS → Rust 的 IPC 消息，解析并经插件注册表路由，
    /// 返回各插件处理后的结果（回复 / 宿主动作）。
    pub fn drain_commands(&mut self) -> Vec<PluginOutcome> {
        let Some(rx) = &self.ipc_rx else {
            return Vec::new();
        };
        // 先把通道内消息收集出来，释放对 self.ipc_rx 的不可变借用，
        // 随后才能可变借用 self.registry 做分发。
        let bodies: Vec<String> = rx.try_iter().collect();
        let mut outcomes = Vec::new();
        for body in bodies {
            match parse_message(&body) {
                Some(IncomingMessage::Heartbeat(info)) => {
                    // JS 诊断心跳只写日志，不产生指令。
                    diag(&format!("JS 心跳：{info}"));
                }
                Some(IncomingMessage::Command(plugin, action, payload)) => {
                    diag(&format!(
                        "IPC 插件指令：{plugin}/{action}{}",
                        if payload.is_empty() {
                            String::new()
                        } else {
                            format!("：{payload}")
                        }
                    ));
                    match self.registry.dispatch(&plugin, &action, &payload) {
                        Some(outcome) => outcomes.push(outcome),
                        None => diag(&format!("无插件处理指令：{plugin}/{action}")),
                    }
                }
                None => diag(&format!("IPC 无法解析：{body}")),
            }
        }
        outcomes
    }

    /// Rust → JS：调用页面注入的 `window.rustReply(plugin, kind, text)`
    /// 把结果回填给指定插件。
    pub fn notify_reply(&self, plugin: &str, kind: &str, text: &str) {
        if let Some(webview) = &self.webview {
            let plugin_json =
                serde_json::to_string(plugin).unwrap_or_else(|_| "\"\"".into());
            let kind_json = serde_json::to_string(kind).unwrap_or_else(|_| "\"\"".into());
            let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
            let js = format!(
                "window.rustReply && window.rustReply({plugin_json}, {kind_json}, {text_json});"
            );
            match webview.evaluate_script(&js) {
                Ok(()) => diag(&format!(
                    "evaluate_script 成功：{plugin} / {kind} / {text}"
                )),
                Err(e) => diag(&format!("evaluate_script 失败：{e}")),
            }
        }
    }
}

// ===== 顶层 FFI（跨方法共用）=====
#[link(name = "user32")]
extern "system" {
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn DestroyWindow(hwnd: isize) -> i32;
}

/// 追加一行诊断日志到 `%TEMP%\qinglo_webview_menu.log`。
///
/// `pub(crate)` 供同属菜单体系的 `webview_plugins` 模块复用同一日志文件。
pub(crate) fn diag(msg: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = std::env::temp_dir().join("qinglo_webview_menu.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

