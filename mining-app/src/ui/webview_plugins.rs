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

/// 活动栏渲染菜单项所需的插件元数据（轻量快照，不含 body/script）。
#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub id: String,
    pub title: String,
    pub description: String,
    pub section: MenuSection,
    pub order: i32,
}

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

    /// 列出所有卡片插件的元数据（按 section → order → id 排序），
    /// 供活动栏把每个插件渲染为一个独立菜单项。
    pub fn card_plugin_list(&self) -> Vec<PluginMeta> {
        let mut cards: Vec<&Box<dyn MenuPlugin>> = self
            .plugins
            .iter()
            .filter(|p| p.slot() == PluginSlot::Card)
            .collect();
        cards.sort_by_key(|p| (section_rank(p.section()), p.order(), p.id().to_string()));
        cards
            .into_iter()
            .map(|p| PluginMeta {
                id: p.id().to_string(),
                title: p.title().to_string(),
                description: p.description().to_string(),
                section: p.section(),
                order: p.order(),
            })
            .collect()
    }

    /// 取指定插件的 body_html 与 script（供单插件页动态注入）。
    pub fn plugin_content(&self, id: &str) -> Option<(String, Option<String>)> {
        let p = self.plugins.iter().find(|p| p.id() == id)?;
        Some((p.body_html(), p.script()))
    }

    /// 渲染单插件页外壳：顶栏（Logo + 返回主界面）+ 内容容器 + 公共运行时。
    /// 具体插件内容由 `WebViewMenu::load_plugin` 通过 `window.loadPlugin` 注入，
    /// 避免每次切换插件都重建 WebView2（运行时冷启动代价高）。
    pub fn render_shell(&self, brand: &crate::config::BrandConfig) -> String {
        let header_html: String = self
            .plugins
            .iter()
            .filter(|p| p.slot() == PluginSlot::Header)
            .map(|p| p.body_html())
            .collect();

        SHELL_TEMPLATE
            .replace("__HEADER_SLOT__", &header_html)
            .replace("{{APP_NAME}}", brand.effective_app_name())
            .replace("{{LOGO_INITIAL}}", &brand.logo_initial_char().to_string())
    }
}

/// section 排序权重（System 在 Tool 之前）。
fn section_rank(s: MenuSection) -> u8 {
    match s {
        MenuSection::System => 0,
        MenuSection::Tool => 1,
    }
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

/// 双向通信日志：外壳页（`SHELL_TEMPLATE`）底部的 `#p-log-box` 统一展示所有
/// 插件的 IPC 收发记录，不再单独作为一个卡片插件，避免与外壳日志重复。

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

// ===== 页面模板 =====

/// 单插件页外壳模板：顶栏 + 内容容器 + 公共运行时。
/// 插件内容通过 `window.loadPlugin(bodyHtml, scriptText)` 动态注入，
/// 切换插件无需重建 WebView2。占位符：`__HEADER_SLOT__`（顶栏返回按钮）、
/// `{{APP_NAME}}` / `{{LOGO_INITIAL}}`（品牌）。
const SHELL_TEMPLATE: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{{APP_NAME}} · 插件</title>
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
    flex: none;
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
  /* 插件内容区：占满剩余空间，由插件自行决定内部滚动 */
  #plugin-content {
    flex: 1; min-height: 0; overflow: auto; padding: 18px;
  }
  /* 插件内通用样式（与卡片页保持一致，便于插件复用） */
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
  .bar {
    height: 4px; border-radius: 2px; margin-top: 14px;
    background: linear-gradient(90deg, #0891b2, #22d3ee, #0891b2);
    background-size: 200% 100%;
    animation: flow 2.2s linear infinite;
  }
  @keyframes flow { from { background-position: 0 0; } to { background-position: -200% 0; } }
  .tag { display:inline-block; font-size:10px; padding:2px 8px; border-radius:99px;
         background:rgba(34,211,238,.15); color:#67e8f9; margin-left:8px; }
  /* 底部 IPC 日志（调试用，所有插件共用） */
  #p-log-box {
    height: 90px; overflow: auto; flex: none;
    background: #0a0b0e; border-top: 1px solid rgba(148,163,184,.14);
    padding: 8px 14px; font-size: 11px; line-height: 1.7;
    font-family: Consolas, monospace;
  }
  .log-rust { color: #6ee7b7; }
  .log-js { color: #7dd3fc; }
</style>
</head>
<body>
<div class="app">
  <header class="topbar">
    <div class="logo">{{LOGO_INITIAL}}</div>
    <div>
      <h1>{{APP_NAME}} · 插件 <span class="tag">插件即菜单项</span></h1>
      <div class="sub">每个插件是一个独立的完整功能模块</div>
    </div>
    <div class="spacer"></div>
    __HEADER_SLOT__
  </header>

  <div id="plugin-content"></div>
  <div id="p-log-box" class="logbox"></div>
</div>

<script>
// ===== 单插件页公共运行时 =====
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
    send: function (plugin, action, payload) {
      payload = payload === undefined || payload === null ? '' : String(payload);
      log('log-js', 'JS → Rust', plugin + '/' + action + (payload ? '：' + payload : ''));
      window.ipc.postMessage('cmd|' + plugin + '|' + action + '|' + payload);
    },
    onReply: function (plugin, cb) { replies[plugin] = cb; }
  };

  window.rustReply = function (plugin, kind, text) {
    log('log-rust', 'Rust → JS', plugin + '：' + kind + '：' + text);
    var cb = replies[plugin];
    if (cb) { try { cb(kind, text); } catch (e) { console.error(e); } }
  };

  // 动态注入插件内容：先清掉旧回复回调，再替换 body 并执行脚本。
  // 用间接 eval（(0,eval)）让脚本在全局作用域执行，IIFE 插件不受影响。
  window.loadPlugin = function (bodyHtml, scriptText) {
    var container = document.getElementById('plugin-content');
    if (container) container.innerHTML = bodyHtml || '';
    if (scriptText) {
      try { (0, eval)(scriptText); }
      catch (e) { console.error('插件脚本异常：', e); }
    }
  };

  log('log-js', 'JS', '页面外壳加载完成，window.ipc 可用：' + (!!window.ipc));

  // 诊断心跳
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
</body>
</html>
"##;
