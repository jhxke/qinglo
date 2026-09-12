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
//! 通信：
//! - JS → Rust：`window.ipc.postMessage(...)` → IPC handler → mpsc → Tick 轮询
//! - Rust → JS：`WebView::evaluate_script("window.rustReply(...)")`
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

// ===== 内容区边距（相对 iced 主窗口客户区，逻辑像素）=====
// 与 title_bar.rs / activity_bar.rs / status_bar.rs 的布局常量保持一致：
// 标题栏 40 + 下分隔 1；活动栏 62 + 右分隔 1；状态栏 29。
const LEFT_INSET: f64 = 63.0;
const TOP_INSET: f64 = 41.0;
const BOTTOM_INSET: f64 = 29.0;

/// JS → Rust 的 IPC 指令（测试协议：纯文本、`|` 分隔）。
pub enum IpcCommand {
    /// 网页内「返回主界面」：隐藏 webview，切回挖掘分析视图。
    Back,
    /// 请求 Rust 做求和，参数为两个数字。
    Sum(f64, f64),
    /// 回声测试，参数为任意文本。
    Echo(String),
}

/// WebView2 菜单测试页的宿主状态。
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

        // 2) WebView2 作为 popup 的子窗口铺满整个 popup 客户区。
        let webview = WebViewBuilder::new()
            .with_bounds(self.content_rect())
            .with_html(MENU_HTML)
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

    /// 非阻塞排空 JS → Rust 的 IPC 消息并解析为指令。
    pub fn drain_commands(&mut self) -> Vec<IpcCommand> {
        let Some(rx) = &self.ipc_rx else { return Vec::new() };
        let mut commands = Vec::new();
        while let Ok(body) = rx.try_recv() {
            // JS 诊断心跳只写日志，不产生指令。
            if let Some(info) = body.strip_prefix("hb|") {
                diag(&format!("JS 心跳：{info}"));
                continue;
            }
            if let Some(cmd) = parse_command(&body) {
                commands.push(cmd);
            } else {
                diag(&format!("IPC 无法解析：{body}"));
            }
        }
        commands
    }

    /// Rust → JS：调用页面注入的 `window.rustReply(kind, text)` 回填结果。
    pub fn notify(&self, kind: &str, text: &str) {
        if let Some(webview) = &self.webview {
            let kind_json = serde_json::to_string(kind).unwrap_or_else(|_| "\"\"".into());
            let text_json = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into());
            let js = format!("window.rustReply && window.rustReply({kind_json}, {text_json});");
            match webview.evaluate_script(&js) {
                Ok(()) => diag(&format!("evaluate_script 成功：{kind} / {text}")),
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
fn diag(msg: &str) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = std::env::temp_dir().join("qinglo_webview_menu.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

/// 解析网页 IPC 文本协议：
/// - `back`
/// - `sum|<a>|<b>`
/// - `echo|<任意文本>`
fn parse_command(body: &str) -> Option<IpcCommand> {
    let body = body.trim();
    if body == "back" {
        return Some(IpcCommand::Back);
    }
    if let Some(rest) = body.strip_prefix("sum|") {
        let mut parts = rest.splitn(2, '|');
        let a = parts.next()?.trim().parse::<f64>().ok()?;
        let b = parts.next()?.trim().parse::<f64>().ok()?;
        return Some(IpcCommand::Sum(a, b));
    }
    if let Some(text) = body.strip_prefix("echo|") {
        return Some(IpcCommand::Echo(text.to_string()));
    }
    None
}

/// 菜单测试页 HTML。
///
/// 整页自带深色风格，验证项：
/// 1. 纯 HTML/CSS 菜单渲染 + CSS 动画（合成器存活）
/// 2. navigator.userAgent 识别 WebView2/Edge 内核版本
/// 3. 视口实时尺寸（验证 set_bounds resize 跟随）
/// 4. JS → Rust IPC：返回 / 求和 / 回声（加载后自动跑一次双向自测）
/// 5. Rust → JS：结果经 window.rustReply 回填到日志区
const MENU_HTML: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>青萝 · WebView 菜单</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  html, body { height: 100%; }
  body {
    font-family: "Microsoft YaHei", "Segoe UI", sans-serif;
    background: #0b1020;
    color: #e2e8f0;
    overflow: hidden;
  }
  .app { display: flex; flex-direction: column; height: 100%; }
  header {
    display: flex; align-items: center; gap: 14px;
    padding: 0 18px; height: 52px;
    background: linear-gradient(90deg, rgba(99,102,241,.22), rgba(34,211,238,.10));
    border-bottom: 1px solid rgba(148,163,184,.18);
  }
  .logo {
    width: 26px; height: 26px; border-radius: 8px;
    background: linear-gradient(135deg, #6366f1, #22d3ee);
    display: grid; place-items: center; font-weight: 700; color: #0b1020;
  }
  h1 { font-size: 15px; font-weight: 600; letter-spacing: .5px; }
  .sub { font-size: 11px; color: #94a3b8; }
  .spacer { flex: 1; }
  .btn {
    border: 1px solid rgba(148,163,184,.35); background: rgba(30,41,59,.7);
    color: #e2e8f0; border-radius: 8px; padding: 7px 14px; font-size: 12px;
    cursor: pointer; transition: background .15s, border-color .15s;
  }
  .btn:hover { background: rgba(71,85,105,.9); border-color: rgba(148,163,184,.6); }
  .btn.primary { border-color: rgba(34,211,238,.55); color: #a5f3fc; }
  main {
    flex: 1; overflow: auto; padding: 18px;
    display: grid; grid-template-columns: 1fr 1fr; gap: 16px;
    align-content: start;
  }
  .card {
    background: rgba(30,41,59,.55);
    border: 1px solid rgba(148,163,184,.16);
    border-radius: 12px; padding: 16px;
  }
  .card h2 { font-size: 13px; color: #a5f3fc; margin-bottom: 12px; font-weight: 600; }
  .kv { font-size: 12px; color: #cbd5e1; line-height: 1.9; word-break: break-all; }
  .kv b { color: #94a3b8; font-weight: 400; margin-right: 6px; }
  .row { display: flex; gap: 8px; margin-top: 10px; align-items: center; flex-wrap: wrap; }
  input {
    background: #0f172a; border: 1px solid rgba(148,163,184,.3);
    border-radius: 7px; color: #e2e8f0; padding: 7px 10px; font-size: 12px;
    outline: none; min-width: 0;
  }
  input:focus { border-color: rgba(34,211,238,.7); }
  input.num { width: 90px; }
  input.txt { flex: 1; min-width: 140px; }
  button.action {
    background: linear-gradient(135deg, #6366f1, #0ea5e9);
    border: none; border-radius: 7px; color: #fff;
    padding: 8px 16px; font-size: 12px; cursor: pointer;
  }
  button.action:active { transform: translateY(1px); }
  #log {
    margin-top: 10px; height: 150px; overflow: auto;
    background: #0b1226; border: 1px solid rgba(148,163,184,.14);
    border-radius: 8px; padding: 10px; font-size: 12px; line-height: 1.8;
    font-family: Consolas, monospace;
  }
  .log-rust { color: #6ee7b7; }
  .log-js { color: #93c5fd; }
  .bar {
    height: 4px; border-radius: 2px; margin-top: 14px;
    background: linear-gradient(90deg, #6366f1, #22d3ee, #6366f1);
    background-size: 200% 100%;
    animation: flow 2.2s linear infinite;
  }
  @keyframes flow { from { background-position: 0 0; } to { background-position: -200% 0; } }
  .tag { display:inline-block; font-size:10px; padding:2px 8px; border-radius:99px;
         background:rgba(34,211,238,.15); color:#67e8f9; margin-left:8px; }
</style>
</head>
<body>
<div class="app">
  <header>
    <div class="logo">萝</div>
    <div>
      <h1>青萝 · WebView 菜单 <span class="tag">wry + WebView2</span></h1>
      <div class="sub">整个菜单由浏览器窗口渲染 — 可行性验证页</div>
    </div>
    <div class="spacer"></div>
    <button class="btn primary" onclick="send('back')">← 返回主界面</button>
  </header>

  <main>
    <section class="card">
      <h2>运行环境</h2>
      <div class="kv"><b>内核</b><span id="ua">检测中…</span></div>
      <div class="kv"><b>视口</b><span id="vp">-</span>（窗口 resize 应实时变化）</div>
      <div class="kv"><b>渲染</b>HTML / CSS / 浏览器合成器（非 wgpu）</div>
      <div class="bar"></div>
    </section>

    <section class="card">
      <h2>JS → Rust IPC</h2>
      <div class="kv" style="margin-bottom:4px">Rust 求和：
        <div class="row">
          <input class="num" id="a" type="number" value="1">
          <span>+</span>
          <input class="num" id="b" type="number" value="2">
          <button class="action" onclick="sendSum()">请求 Rust 计算</button>
        </div>
      </div>
      <div class="kv" style="margin-top:12px">回声：
        <div class="row">
          <input class="txt" id="msg" value="你好，Rust！">
          <button class="action" onclick="sendEcho()">发送</button>
        </div>
      </div>
    </section>

    <section class="card" style="grid-column: 1 / -1">
      <h2>双向通信日志</h2>
      <div id="log"></div>
    </section>
  </main>
</div>

<script>
  function log(cls, who, text) {
    var el = document.getElementById('log');
    var line = document.createElement('div');
    line.className = cls;
    line.textContent = '[' + who + '] ' + text;
    el.appendChild(line);
    el.scrollTop = el.scrollHeight;
  }
  function send(payload) {
    log('log-js', 'JS → Rust', payload);
    window.ipc.postMessage(payload);
  }
  function sendSum() {
    var a = document.getElementById('a').value;
    var b = document.getElementById('b').value;
    send('sum|' + a + '|' + b);
  }
  function sendEcho() {
    send('echo|' + document.getElementById('msg').value);
  }
  // Rust → JS 回填入口
  window.rustReply = function (kind, text) {
    log('log-rust', 'Rust → JS', kind + '：' + text);
  };
  // 环境信息
  document.getElementById('ua').textContent = navigator.userAgent;
  function updateVp() {
    document.getElementById('vp').textContent =
      window.innerWidth + ' × ' + window.innerHeight + ' 逻辑像素';
  }
  updateVp();
  window.addEventListener('resize', updateVp);
  log('log-js', 'JS', '页面加载完成，window.ipc 可用：' + (!!window.ipc));
  // 自动发起一次双向 IPC 自测：若日志区出现绿色 Rust → JS 行，说明双向链路正常
  send('echo|自动双向 IPC 自测');
  // 诊断心跳：每秒上报可见性 / 焦点 / rAF 累计帧数 / 视口。
  var rafCount = 0;
  function rafLoop() { rafCount++; requestAnimationFrame(rafLoop); }
  rafLoop();
  setInterval(function () {
    window.ipc.postMessage('hb|' + document.visibilityState + '|hidden=' + document.hidden
      + '|focus=' + document.hasFocus() + '|raf=' + rafCount
      + '|vp=' + window.innerWidth + 'x' + window.innerHeight);
  }, 1000);
</script>
</body>
</html>
"##;
