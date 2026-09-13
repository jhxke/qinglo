//! 网页菜单插件系统（仅 Windows）。
//!
//! 设计对标算子体系（`operator.json` 元数据 + 注册表 + 动态加载），
//! 让浏览器菜单的每一项功能都是一个可自由组合 / 收缩 / 动态扩展的插件：
//!
//! - [`MenuPlugin`]：菜单插件 trait（类似算子的执行 trait），声明 id、标题、
//!   分组、卡片 HTML、可选脚本以及 IPC 指令处理；
//! - [`PluginRegistry`]：插件注册表（类似算子注册表），内置插件在编译期注册，
//!   外部插件在启动时从 `webview_plugins/` 目录扫描加载（免重新编译）；
//! - 页面由注册表按 slot / section / order **动态拼装**，不再硬编码；
//! - 每个卡片插件可折叠，折叠状态记在浏览器 localStorage（灵活收缩）。
//!
//! # IPC 协议
//!
//! JS → Rust 统一为 `cmd|<plugin_id>|<action>|<payload>`（payload 可含 `|`，
//! 按前三个分隔符切分）；心跳仍为 `hb|...`。
//! Rust → JS 统一调用 `window.rustReply(pluginId, kind, text)`，由页面外壳
//! 分发到对应插件通过 `window.qinglo.onReply(id, cb)` 注册的回调。
//!
//! # 外部插件
//!
//! 搜索目录（与算子目录约定一致，两者都扫描、按 id 去重）：
//! 1. EXE 同级 `webview_plugins/`（部署期）；
//! 2. 工作区根 `webview_plugins/`（开发期，基于 `CARGO_MANIFEST_DIR`）。
//!
//! 支持两种落盘形式：
//! - 单文件：`<id>.json`，body / script 直接内联在 JSON；
//! - 目录型：`<id>/plugin.json` + `body.html` + `main.js`（路径相对
//!   plugin.json，字段 `body_file` / `script_file`）。
//!
//! 外部插件是与算子 DLL 同级的本地可信扩展，纯前端实现（HTML/CSS/JS），
//! 通过 `window.qinglo.send / onReply` 与宿主双向通信。

#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::webview_menu::diag;

// ===== 插件元数据类型 =====

/// 插件挂载位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSlot {
    /// 顶栏（如「返回主界面」按钮），不可折叠。
    Header,
    /// 主区卡片，可折叠，可跨两列。
    Card,
}

/// 卡片分组（渲染时按固定顺序出分区标题）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuSection {
    /// 系统类（环境 / 日志等宿主自带功能）。
    System,
    /// 工具类（计算 / 小工具等，外部插件默认归入此组）。
    Tool,
}

impl MenuSection {
    fn label(self) -> &'static str {
        match self {
            MenuSection::System => "系统",
            MenuSection::Tool => "工具",
        }
    }

    fn key(self) -> &'static str {
        match self {
            MenuSection::System => "system",
            MenuSection::Tool => "tool",
        }
    }

    fn from_key(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "system" | "sys" => MenuSection::System,
            _ => MenuSection::Tool,
        }
    }
}

/// 插件要求宿主执行的动作（与页面无关的窗口 / 视图操作）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAction {
    /// 返回挖掘分析主视图（隐藏 webview）。
    Back,
}

/// 插件处理一条 IPC 指令后的响应。
#[derive(Debug, Default, Clone)]
pub struct PluginResponse {
    /// 回填给页面插件的结果：(kind, text) → window.rustReply。
    pub reply: Option<(String, String)>,
    /// 要求宿主执行的动作（如返回主界面）。
    pub host: Option<HostAction>,
}

impl PluginResponse {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reply(kind: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            reply: Some((kind.into(), text.into())),
            host: None,
        }
    }

    pub fn host(action: HostAction) -> Self {
        Self {
            reply: None,
            host: Some(action),
        }
    }
}

/// 注册表分发后带插件 id 的结果（供 main 层回填 / 执行宿主动作）。
#[derive(Debug, Clone)]
pub struct PluginOutcome {
    pub plugin: String,
    pub reply: Option<(String, String)>,
    pub host: Option<HostAction>,
}

// ===== 插件 trait =====

/// 网页菜单插件：一个插件 = 一块可自由组合的菜单功能。
///
/// 内置插件在 Rust 中实现本 trait；外部插件由 [`ExternalPlugin`] 承载。
pub trait MenuPlugin {
    /// 唯一 id（IPC 路由与 localStorage 命名空间均使用）。
    fn id(&self) -> &str;
    /// 卡片 / 按钮标题。
    fn title(&self) -> &str;
    /// 副标题说明（可空）。
    fn description(&self) -> &str {
        ""
    }
    /// 挂载位置，默认主区卡片。
    fn slot(&self) -> PluginSlot {
        PluginSlot::Card
    }
    /// 卡片分组，默认工具组。
    fn section(&self) -> MenuSection {
        MenuSection::Tool
    }
    /// 同组内排序，升序；默认 100。
    fn order(&self) -> i32 {
        100
    }
    /// 卡片是否跨两列整行（如日志面板）。
    fn full_width(&self) -> bool {
        false
    }
    /// 卡片主体 HTML（不含卡片标题栏，外壳统一提供折叠头）。
    fn body_html(&self) -> String;
    /// 注入到页面末尾的插件脚本（外壳与 `window.qinglo` 已先行就绪）。
    fn script(&self) -> Option<String> {
        None
    }
    /// 处理来自本插件卡片的 IPC 指令。
    fn on_command(&mut self, _action: &str, _payload: &str) -> PluginResponse {
        PluginResponse::new()
    }
}

// ===== 注册表 =====

/// 菜单插件注册表：持有内置 + 外部插件，负责 IPC 路由与整页 HTML 拼装。
pub struct PluginRegistry {
    plugins: Vec<Box<dyn MenuPlugin>>,
}

impl Default for PluginRegistry {
    fn default() -> Self {
        Self::load()
    }
}

impl PluginRegistry {
    /// 构造注册表：注册内置插件并扫描外部插件目录。
    pub fn load() -> Self {
        let mut registry = Self {
            plugins: Vec::new(),
        };
        registry.register(Box::new(NavPlugin));
        registry.register(Box::new(EnvPlugin));
        registry.register(Box::new(SumPlugin));
        registry.register(Box::new(EchoPlugin));
        registry.register(Box::new(LogPlugin));
        registry.scan_external();
        registry
    }

    /// 追加一个插件；同 id 已存在时保留先注册者（外部目录扫描时用于去重）。
    pub fn register(&mut self, plugin: Box<dyn MenuPlugin>) {
        let id = plugin.id().to_string();
        if self.plugins.iter().any(|p| p.id() == id) {
            diag(&format!("插件 id 冲突，忽略重复注册：{id}"));
            return;
        }
        diag(&format!(
            "已注册菜单插件：{id}（{} / {} / order {}）",
            match plugin.slot() {
                PluginSlot::Header => "顶栏",
                PluginSlot::Card => "卡片",
            },
            plugin.section().label(),
            plugin.order(),
        ));
        self.plugins.push(plugin);
    }

    /// 路由一条 `cmd|plugin|action|payload` 指令；未命中插件返回 None。
    pub fn dispatch(
        &mut self,
        plugin_id: &str,
        action: &str,
        payload: &str,
    ) -> Option<PluginOutcome> {
        let plugin = self.plugins.iter_mut().find(|p| p.id() == plugin_id)?;
        let resp = plugin.on_command(action, payload);
        Some(PluginOutcome {
            plugin: plugin_id.to_string(),
            reply: resp.reply,
            host: resp.host,
        })
    }

    /// 扫描外部插件目录并注册合法插件。
    fn scan_external(&mut self) {
        for dir in plugin_directories() {
            if !dir.exists() {
                continue;
            }
            let entries = match fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(e) => {
                    diag(&format!("读取插件目录失败 {}：{e}", dir.display()));
                    continue;
                }
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let manifest = path.join("plugin.json");
                    if manifest.is_file() {
                        self.load_external_manifest(&manifest);
                    }
                } else if path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("json"))
                    .unwrap_or(false)
                {
                    self.load_external_manifest(&path);
                }
            }
        }
    }

    fn load_external_manifest(&mut self, manifest_path: &Path) {
        match ExternalPlugin::from_manifest(manifest_path) {
            Ok(Some(plugin)) => self.register(Box::new(plugin)),
            Ok(None) => diag(&format!(
                "插件已禁用或无内容，跳过：{}",
                manifest_path.display()
            )),
            Err(e) => diag(&format!(
                "加载外部插件失败 {}：{e}",
                manifest_path.display()
            )),
        }
    }

    /// 按当前注册表动态拼装完整菜单页 HTML。
    ///
    /// `brand` 用于替换模板中的 `{{APP_NAME}}` / `{{LOGO_INITIAL}}` 占位符，
    /// 让设置页改名后下次进入 WebView 视图即生效（每次 build 都重新 render）。
    pub fn render_page(&self, brand: &crate::config::BrandConfig) -> String {
        let header_html: String = self
            .plugins
            .iter()
            .filter(|p| p.slot() == PluginSlot::Header)
            .map(|p| p.body_html())
            .collect();

        let sections = [MenuSection::System, MenuSection::Tool];
        let mut cards_html = String::new();
        for section in sections {
            let mut cards: Vec<&Box<dyn MenuPlugin>> = self
                .plugins
                .iter()
                .filter(|p| p.slot() == PluginSlot::Card && p.section() == section)
                .collect();
            if cards.is_empty() {
                continue;
            }
            cards.sort_by_key(|p| (p.order(), p.id().to_string()));
            cards_html.push_str(&format!(
                r#"<div class="section-title" data-section="{}">{}插件</div>"#,
                section.key(),
                section.label(),
            ));
            for p in cards {
                cards_html.push_str(&render_card(p.as_ref()));
            }
        }

        // 插件脚本在外壳脚本之后注入，确保 window.qinglo 已就绪。
        let scripts_html: String = self
            .plugins
            .iter()
            .filter_map(|p| {
                p.script().map(|s| {
                    format!(
                        "<script>\ntry {{\n{}\n}} catch (e) {{\n  console.error('插件 {} 脚本异常：', e);\n}}\n</script>\n",
                        sanitize_script(&s),
                        escape_js(p.id())
                    )
                })
            })
            .collect();

        PAGE_TEMPLATE
            .replace("__HEADER_SLOT__", &header_html)
            .replace("__CARDS__", &cards_html)
            .replace("__PLUGIN_SCRIPTS__", &scripts_html)
            // 品牌占位符替换：app_name 空值回退 "青萝"；logo_initial 取首字符
            .replace("{{APP_NAME}}", brand.effective_app_name())
            .replace("{{LOGO_INITIAL}}", &brand.logo_initial_char().to_string())
    }
}

/// 渲染一张可折叠卡片。
fn render_card(p: &dyn MenuPlugin) -> String {
    let desc_html = if p.description().is_empty() {
        String::new()
    } else {
        format!(
            r#"<span class="card-desc">{}</span>"#,
            escape_html(p.description())
        )
    };
    let full = if p.full_width() { " full" } else { "" };
    format!(
        r#"<section class="card{full}" id="card-{id}">
  <header class="card-head" onclick="window.qinglo.toggle('{id}')">
    <div class="card-title"><h2>{title}</h2>{desc}</div>
    <span class="chev" aria-hidden="true"></span>
  </header>
  <div class="card-body">
{body}
  </div>
</section>"#,
        full = full,
        id = escape_attr(p.id()),
        title = escape_html(p.title()),
        desc = desc_html,
        body = p.body_html(),
    )
}

// ===== 外部插件 =====

/// plugin.json 清单结构（对标 operator.json 的元数据角色）。
#[derive(Deserialize)]
struct ExternalManifest {
    id: String,
    title: String,
    #[serde(default)]
    description: String,
    /// "system" / "tool"（默认 tool）。
    #[serde(default)]
    section: Option<String>,
    #[serde(default = "default_order")]
    order: i32,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    full_width: bool,
    /// 内联卡片 HTML。
    #[serde(default)]
    body: Option<String>,
    /// 卡片 HTML 文件（相对清单路径）。
    #[serde(default)]
    body_file: Option<String>,
    /// 内联脚本。
    #[serde(default)]
    script: Option<String>,
    /// 脚本文件（相对清单路径）。
    #[serde(default)]
    script_file: Option<String>,
}

fn default_order() -> i32 {
    100
}
fn default_true() -> bool {
    true
}

/// 外部文件插件：纯前端工具，无 Rust 端指令处理（消息可发，默认无响应）。
pub struct ExternalPlugin {
    id: String,
    title: String,
    description: String,
    section: MenuSection,
    order: i32,
    full_width: bool,
    body: String,
    script: Option<String>,
}

impl ExternalPlugin {
    fn from_manifest(manifest_path: &Path) -> Result<Option<Self>, String> {
        let raw = fs::read_to_string(manifest_path)
            .map_err(|e| format!("读取清单失败：{e}"))?;
        let m: ExternalManifest =
            serde_json::from_str(&raw).map_err(|e| format!("解析 JSON 失败：{e}"))?;
        if !m.enabled {
            return Ok(None);
        }
        let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
        let read_relative = |field: Option<String>| -> Result<Option<String>, String> {
            match field {
                Some(rel) if !rel.trim().is_empty() => Ok(Some(
                    fs::read_to_string(base.join(rel.trim()))
                        .map_err(|e| format!("读取外部文件 {rel} 失败：{e}"))?,
                )),
                _ => Ok(None),
            }
        };
        let body_file = read_relative(m.body_file)?;
        let script_file = read_relative(m.script_file)?;
        let body = m
            .body
            .or(body_file)
            .ok_or_else(|| "插件缺少 body / body_file 内容".to_string())?;
        let script = m.script.or(script_file);

        Ok(Some(Self {
            id: m.id,
            title: m.title,
            description: m.description,
            section: m
                .section
                .as_deref()
                .map(MenuSection::from_key)
                .unwrap_or(MenuSection::Tool),
            order: m.order,
            full_width: m.full_width,
            body,
            script,
        }))
    }
}

impl MenuPlugin for ExternalPlugin {
    fn id(&self) -> &str {
        &self.id
    }
    fn title(&self) -> &str {
        &self.title
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn section(&self) -> MenuSection {
        self.section
    }
    fn order(&self) -> i32 {
        self.order
    }
    fn full_width(&self) -> bool {
        self.full_width
    }
    fn body_html(&self) -> String {
        self.body.clone()
    }
    fn script(&self) -> Option<String> {
        self.script.clone()
    }
}

/// 外部插件搜索目录：EXE 同级优先，工作区根次之（开发期）。
fn plugin_directories() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            dirs.push(exe_dir.join("webview_plugins"));
        }
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("webview_plugins");
    dirs.push(workspace);
    dirs
}

// ===== 内置插件 =====

/// 顶栏导航：返回挖掘分析主视图。
struct NavPlugin;

impl MenuPlugin for NavPlugin {
    fn id(&self) -> &str {
        "nav"
    }
    fn title(&self) -> &str {
        "返回主界面"
    }
    fn slot(&self) -> PluginSlot {
        PluginSlot::Header
    }
    fn order(&self) -> i32 {
        10
    }
    fn body_html(&self) -> String {
        r#"<button class="btn primary" onclick="window.qinglo.send('nav','back')">← 返回主界面</button>"#
            .to_string()
    }
    fn on_command(&mut self, action: &str, _payload: &str) -> PluginResponse {
        if action == "back" {
            PluginResponse::host(HostAction::Back)
        } else {
            PluginResponse::new()
        }
    }
}

/// 运行环境信息卡片（纯前端）。
struct EnvPlugin;

impl MenuPlugin for EnvPlugin {
    fn id(&self) -> &str {
        "env"
    }
    fn title(&self) -> &str {
        "运行环境"
    }
    fn description(&self) -> &str {
        "浏览器内核与视口"
    }
    fn section(&self) -> MenuSection {
        MenuSection::System
    }
    fn order(&self) -> i32 {
        10
    }
    fn body_html(&self) -> String {
        r#"<div class="kv"><b>内核</b><span id="p-env-ua">检测中…</span></div>
<div class="kv"><b>视口</b><span id="p-env-vp">-</span>（窗口 resize 应实时变化）</div>
<div class="kv"><b>渲染</b>HTML / CSS / 浏览器合成器（非 wgpu）</div>
<div class="bar"></div>"#
            .to_string()
    }
    fn script(&self) -> Option<String> {
        Some(
            r#"document.getElementById('p-env-ua').textContent = navigator.userAgent;
function pEnvUpdateVp() {
  document.getElementById('p-env-vp').textContent =
    window.innerWidth + ' × ' + window.innerHeight + ' 逻辑像素';
}
pEnvUpdateVp();
window.addEventListener('resize', pEnvUpdateVp);"#
                .to_string(),
        )
    }
}

/// Rust 求和卡片：验证 JS → Rust 请求 / 计算 / 回填链路。
struct SumPlugin;

impl MenuPlugin for SumPlugin {
    fn id(&self) -> &str {
        "sum"
    }
    fn title(&self) -> &str {
        "Rust 求和"
    }
    fn description(&self) -> &str {
        "JS → Rust IPC"
    }
    fn section(&self) -> MenuSection {
        MenuSection::Tool
    }
    fn order(&self) -> i32 {
        10
    }
    fn body_html(&self) -> String {
        r#"<div class="row">
  <input class="num" id="p-sum-a" type="number" value="1">
  <span>+</span>
  <input class="num" id="p-sum-b" type="number" value="2">
  <button class="action" onclick="window.qinglo.send('sum','calc',
      document.getElementById('p-sum-a').value+'|'+document.getElementById('p-sum-b').value)">请求 Rust 计算</button>
</div>
<div class="result" id="p-sum-out">等待计算…</div>"#
            .to_string()
    }
    fn script(&self) -> Option<String> {
        Some(
            r#"window.qinglo.onReply('sum', function (kind, text) {
  if (kind === 'sum') document.getElementById('p-sum-out').textContent = text;
});"#
                .to_string(),
        )
    }
    fn on_command(&mut self, action: &str, payload: &str) -> PluginResponse {
        if action != "calc" {
            return PluginResponse::new();
        }
        let mut parts = payload.splitn(2, '|');
        let parse_next = |it: Option<&str>| -> Option<f64> {
            it?.trim().parse::<f64>().ok()
        };
        let a = match parse_next(parts.next()) {
            Some(v) => v,
            None => return PluginResponse::reply("sum", "参数 A 不是有效数字"),
        };
        let b = match parse_next(parts.next()) {
            Some(v) => v,
            None => return PluginResponse::reply("sum", "参数 B 不是有效数字"),
        };
        PluginResponse::reply("sum", &format!("{a} + {b} = {}", a + b))
    }
}

/// 回声卡片：任意文本往返，并在加载时自动做一次双向链路自测。
struct EchoPlugin;

impl MenuPlugin for EchoPlugin {
    fn id(&self) -> &str {
        "echo"
    }
    fn title(&self) -> &str {
        "回声测试"
    }
    fn description(&self) -> &str {
        "Rust → JS 回填"
    }
    fn section(&self) -> MenuSection {
        MenuSection::Tool
    }
    fn order(&self) -> i32 {
        20
    }
    fn body_html(&self) -> String {
        r#"<div class="row">
  <input class="txt" id="p-echo-msg" value="你好，Rust！">
  <button class="action" onclick="window.qinglo.send('echo','say',document.getElementById('p-echo-msg').value)">发送</button>
</div>
<div class="result" id="p-echo-out">等待回声…</div>"#
            .to_string()
    }
    fn script(&self) -> Option<String> {
        Some(
            r#"window.qinglo.onReply('echo', function (kind, text) {
  if (kind === 'echo') document.getElementById('p-echo-out').textContent = text;
});
window.qinglo.send('echo', 'say', '自动双向 IPC 自测');"#
                .to_string(),
        )
    }
    fn on_command(&mut self, action: &str, payload: &str) -> PluginResponse {
        if action == "say" {
            PluginResponse::reply("echo", payload)
        } else {
            PluginResponse::new()
        }
    }
}

/// 双向通信日志卡片（整行展示，所有插件的收发消息统一落这里）。
struct LogPlugin;

impl MenuPlugin for LogPlugin {
    fn id(&self) -> &str {
        "log"
    }
    fn title(&self) -> &str {
        "双向通信日志"
    }
    fn description(&self) -> &str {
        "全部插件的 IPC 收发记录"
    }
    fn section(&self) -> MenuSection {
        MenuSection::System
    }
    fn order(&self) -> i32 {
        100
    }
    fn full_width(&self) -> bool {
        true
    }
    fn body_html(&self) -> String {
        r#"<div id="p-log-box" class="logbox"></div>"#.to_string()
    }
}

// ===== 消息解析 =====

/// JS → Rust 消息（drain 时逐条解析）。
pub enum IncomingMessage {
    /// 诊断心跳：内容只写日志。
    Heartbeat(String),
    /// 插件指令：(plugin_id, action, payload)。
    Command(String, String, String),
}

/// 解析网页 IPC 文本：
/// - `hb|<info>` 心跳；
/// - `cmd|<plugin>|<action>|<payload>` 插件指令（payload 可含 `|`）。
pub fn parse_message(body: &str) -> Option<IncomingMessage> {
    let body = body.trim();
    if let Some(info) = body.strip_prefix("hb|") {
        return Some(IncomingMessage::Heartbeat(info.to_string()));
    }
    let rest = body.strip_prefix("cmd|")?;
    let mut parts = rest.splitn(3, '|');
    let plugin = parts.next()?.trim();
    let action = parts.next()?.trim();
    let payload = parts.next().unwrap_or("");
    if plugin.is_empty() || action.is_empty() {
        return None;
    }
    Some(IncomingMessage::Command(
        plugin.to_string(),
        action.to_string(),
        payload.to_string(),
    ))
}

// ===== HTML / JS 转义与脚本净化 =====

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn escape_attr(s: &str) -> String {
    escape_html(s).replace('\'', "&#39;")
}

fn escape_js(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// 防止外部脚本提前闭合内联 `<script>` 标签。
fn sanitize_script(s: &str) -> String {
    s.replace("</script", "<\\/script").replace("</SCRIPT", "<\\/SCRIPT")
}

// ===== 页面模板 =====

/// 菜单外壳模板：CSS + 顶栏插槽 + 卡片容器 + 公共运行时（window.qinglo）。
/// 占位符：
/// - `__HEADER_SLOT__` / `__CARDS__` / `__PLUGIN_SCRIPTS__`：插件内容插槽；
/// - `{{APP_NAME}}` / `{{LOGO_INITIAL}}`：品牌占位符，由 `render_page` 按当前
///   `BrandConfig` 替换，让用户在设置页改名后下次进入 WebView 视图即生效。
const PAGE_TEMPLATE: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{{APP_NAME}} · WebView 菜单</title>
<style>
  * { box-sizing: border-box; margin: 0; padding: 0; }
  html, body { height: 100%; }
  body {
    font-family: "Microsoft YaHei", "Segoe UI", sans-serif;
    background: #0b0c0f;
    color: #e2e8f0;
    overflow: hidden;
  }
  .app { display: flex; flex-direction: column; height: 100%; }
  header.topbar {
    display: flex; align-items: center; gap: 14px;
    padding: 0 18px; height: 52px;
    background: linear-gradient(90deg, rgba(8,145,178,.22), rgba(34,211,238,.10));
    border-bottom: 1px solid rgba(148,163,184,.18);
  }
  .logo {
    width: 26px; height: 26px; border-radius: 8px;
    background: linear-gradient(135deg, #0891b2, #22d3ee);
    display: grid; place-items: center; font-weight: 700; color: #0b0c0f;
  }
  h1 { font-size: 15px; font-weight: 600; letter-spacing: .5px; }
  .sub { font-size: 11px; color: #94a3b8; }
  .spacer { flex: 1; }
  .btn {
    border: 1px solid rgba(148,163,184,.35); background: rgba(27,29,34,.8);
    color: #e2e8f0; border-radius: 8px; padding: 7px 14px; font-size: 12px;
    cursor: pointer; transition: background .15s, border-color .15s;
  }
  .btn:hover { background: rgba(55,60,70,.9); border-color: rgba(148,163,184,.6); }
  .btn.primary { border-color: rgba(34,211,238,.55); color: #a5f3fc; }
  main {
    flex: 1; overflow: auto; padding: 18px;
    display: grid; grid-template-columns: 1fr 1fr; gap: 16px;
    align-content: start;
  }
  .section-title {
    grid-column: 1 / -1;
    font-size: 11px; letter-spacing: 2px; color: #64748b;
    margin: 4px 2px -4px;
  }
  .card {
    background: rgba(27,29,34,.7);
    border: 1px solid rgba(148,163,184,.16);
    border-radius: 12px; overflow: hidden;
  }
  .card.full { grid-column: 1 / -1; }
  .card-head {
    display: flex; align-items: center; gap: 8px;
    padding: 12px 16px; cursor: pointer; user-select: none;
    border-bottom: 1px solid rgba(148,163,184,.12);
  }
  .card-head:hover { background: rgba(148,163,184,.06); }
  .card-title { display: flex; align-items: baseline; gap: 10px; flex: 1; min-width: 0; }
  .card-title h2 { font-size: 13px; color: #a5f3fc; font-weight: 600; }
  .card-desc { font-size: 11px; color: #64748b; }
  .chev { width: 8px; height: 8px; flex: none;
    border-right: 1.6px solid #7dd3fc; border-bottom: 1.6px solid #7dd3fc;
    transform: rotate(45deg) translate(-1px, -1px); transition: transform .18s; }
  .card.collapsed .card-head { border-bottom-color: transparent; }
  .card.collapsed .chev { transform: rotate(-135deg) translate(-1px, -1px); }
  .card.collapsed .card-body { display: none; }
  .card-body { padding: 16px; }
  .kv { font-size: 12px; color: #cbd5e1; line-height: 1.9; word-break: break-all; }
  .kv b { color: #94a3b8; font-weight: 400; margin-right: 6px; }
  .row { display: flex; gap: 8px; margin-top: 10px; align-items: center; flex-wrap: wrap; }
  .result { margin-top: 10px; font-size: 12px; color: #6ee7b7; }
  input {
    background: #141519; border: 1px solid rgba(148,163,184,.3);
    border-radius: 7px; color: #e2e8f0; padding: 7px 10px; font-size: 12px;
    outline: none; min-width: 0;
  }
  input:focus { border-color: rgba(34,211,238,.7); }
  input.num { width: 90px; }
  input.txt { flex: 1; min-width: 140px; }
  button.action {
    background: linear-gradient(135deg, #0891b2, #22d3ee);
    border: none; border-radius: 7px; color: #fff;
    padding: 8px 16px; font-size: 12px; cursor: pointer;
  }
  button.action:active { transform: translateY(1px); }
  .logbox {
    height: 150px; overflow: auto;
    background: #0a0b0e; border: 1px solid rgba(148,163,184,.14);
    border-radius: 8px; padding: 10px; font-size: 12px; line-height: 1.8;
    font-family: Consolas, monospace;
  }
  .log-rust { color: #6ee7b7; }
  .log-js { color: #7dd3fc; }
  .bar {
    height: 4px; border-radius: 2px; margin-top: 14px;
    background: linear-gradient(90deg, #0891b2, #22d3ee, #0891b2);
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
  <header class="topbar">
    <div class="logo">{{LOGO_INITIAL}}</div>
    <div>
      <h1>{{APP_NAME}} · WebView 菜单 <span class="tag">插件化</span></h1>
      <div class="sub">菜单项即插件 — 可组合 · 可收缩 · 可动态扩展</div>
    </div>
    <div class="spacer"></div>
    __HEADER_SLOT__
  </header>

  <main>
__CARDS__
  </main>
</div>

<script>
// ===== 菜单插件公共运行时（所有插件脚本注入前就绪）=====
(function () {
  var replies = {};

  function log(cls, who, text) {
    var box = document.getElementById('p-log-box');
    if (!box) return;
    var line = document.createElement('div');
    line.className = cls;
    line.textContent = '[' + who + '] ' + text;
    box.appendChild(line);
    box.scrollTop = box.scrollHeight;
  }

  window.qinglo = {
    // JS → Rust：cmd|<plugin>|<action>|<payload>
    send: function (plugin, action, payload) {
      payload = payload === undefined || payload === null ? '' : String(payload);
      log('log-js', 'JS → Rust', plugin + '/' + action + (payload ? '：' + payload : ''));
      window.ipc.postMessage('cmd|' + plugin + '|' + action + '|' + payload);
    },
    // 插件订阅本插件的 Rust 回填
    onReply: function (plugin, cb) { replies[plugin] = cb; },
    // 卡片折叠 / 展开，状态持久化到 localStorage
    toggle: function (id) {
      var card = document.getElementById('card-' + id);
      if (!card) return;
      var collapsed = card.classList.toggle('collapsed');
      try { localStorage.setItem('qinglo:collapse:' + id, collapsed ? '1' : '0'); } catch (e) {}
    }
  };

  // Rust → JS 统一入口：先写全局日志，再分发给对应插件回调
  window.rustReply = function (plugin, kind, text) {
    log('log-rust', 'Rust → JS', plugin + '：' + kind + '：' + text);
    var cb = replies[plugin];
    if (cb) { try { cb(kind, text); } catch (e) { console.error(e); } }
  };

  // 恢复折叠状态
  document.querySelectorAll('.card').forEach(function (card) {
    var id = card.id.replace(/^card-/, '');
    try {
      if (localStorage.getItem('qinglo:collapse:' + id) === '1') {
        card.classList.add('collapsed');
      }
    } catch (e) {}
  });

  log('log-js', 'JS', '页面加载完成，window.ipc 可用：' + (!!window.ipc));

  // 诊断心跳：每秒上报可见性 / 焦点 / rAF 帧数 / 视口。
  var rafCount = 0;
  function rafLoop() { rafCount++; requestAnimationFrame(rafLoop); }
  rafLoop();
  setInterval(function () {
    window.ipc.postMessage('hb|' + document.visibilityState + '|hidden=' + document.hidden
      + '|focus=' + document.hasFocus() + '|raf=' + rafCount
      + '|vp=' + window.innerWidth + 'x' + window.innerHeight);
  }, 1000);
})();
</script>
__PLUGIN_SCRIPTS__
</body>
</html>
"##;
