// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use crate::{privacy, transport};
#[path = "file_ui.rs"]
mod file_ui;
use eframe::egui::{self, Color32, RichText, Stroke, StrokeKind};
use lan_mouse_ipc::{
    ClientConfig, ClientState, DiscoveredDevice, Position, Status, UiAction, UiSnapshot,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const EDGES: [Position; 4] = [
    Position::Left,
    Position::Right,
    Position::Top,
    Position::Bottom,
];
const TEXT: Color32 = Color32::from_rgb(30, 42, 60);
const MUTED: Color32 = Color32::from_rgb(116, 129, 149);
const GREEN: Color32 = Color32::from_rgb(30, 140, 103);
const RED: Color32 = Color32::from_rgb(192, 70, 70);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum UiTheme {
    #[default]
    Purple,
    Ocean,
    Mint,
    Coral,
}

impl UiTheme {
    const ALL: [Self; 4] = [Self::Purple, Self::Ocean, Self::Mint, Self::Coral];

    fn name(self) -> &'static str {
        match self {
            Self::Purple => "紫色渐变",
            Self::Ocean => "海蓝",
            Self::Mint => "薄荷绿",
            Self::Coral => "暖珊瑚",
        }
    }

    fn colors(self) -> ThemeColors {
        match self {
            Self::Purple => ThemeColors::new((111, 71, 219), (177, 90, 235), (242, 236, 255)),
            Self::Ocean => ThemeColors::new((41, 96, 203), (45, 171, 225), (231, 242, 255)),
            Self::Mint => ThemeColors::new((22, 119, 111), (62, 180, 149), (229, 247, 242)),
            Self::Coral => ThemeColors::new((185, 72, 97), (236, 125, 93), (255, 238, 237)),
        }
    }
}

#[derive(Clone, Copy)]
struct ThemeColors {
    start: Color32,
    end: Color32,
    soft: Color32,
    bg_start: Color32,
    bg_end: Color32,
    sidebar: Color32,
    panel: Color32,
    surface: Color32,
    canvas: Color32,
    border: Color32,
}

impl ThemeColors {
    fn new(start: (u8, u8, u8), end: (u8, u8, u8), soft: (u8, u8, u8)) -> Self {
        let start = Color32::from_rgb(start.0, start.1, start.2);
        let end = Color32::from_rgb(end.0, end.1, end.2);
        Self {
            start,
            end,
            soft: Color32::from_rgb(soft.0, soft.1, soft.2),
            bg_start: blend(start, Color32::WHITE, 0.82),
            bg_end: blend(end, Color32::WHITE, 0.78),
            sidebar: blend(start, Color32::WHITE, 0.88),
            panel: blend(start, Color32::WHITE, 0.95),
            surface: blend(start, Color32::WHITE, 0.88),
            canvas: blend(end, Color32::WHITE, 0.93),
            border: blend(start, Color32::WHITE, 0.75),
        }
    }
}

fn blend(a: Color32, b: Color32, t: f32) -> Color32 {
    let channel = |a: u8, b: u8| ((a as f32 * (1.0 - t) + b as f32 * t).round()) as u8;
    Color32::from_rgb(
        channel(a.r(), b.r()),
        channel(a.g(), b.g()),
        channel(a.b(), b.b()),
    )
}

fn accent(ui: &egui::Ui) -> Color32 {
    ui.visuals().hyperlink_color
}

fn soft_accent(ui: &egui::Ui) -> Color32 {
    ui.visuals().selection.bg_fill
}

fn panel(ui: &egui::Ui) -> Color32 {
    ui.visuals().window_fill
}

fn surface(ui: &egui::Ui) -> Color32 {
    ui.visuals().faint_bg_color
}

fn canvas(ui: &egui::Ui) -> Color32 {
    ui.visuals().extreme_bg_color
}

fn border(ui: &egui::Ui) -> Color32 {
    ui.visuals().window_stroke.color
}

fn theme_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("LAN_BRIDGE_UI_PREFS") {
        return Some(PathBuf::from(path));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("APPDATA").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("lan-mouse").join("ui-theme.json"))
}

fn load_theme_from(path: &Path) -> UiTheme {
    std::fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn load_theme() -> UiTheme {
    theme_path()
        .as_deref()
        .map(load_theme_from)
        .unwrap_or_default()
}

fn save_theme_to(path: &Path, theme: UiTheme) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec(&theme).map_err(std::io::Error::other)?;
    std::fs::write(path, data)
}

fn save_theme(theme: UiTheme) -> std::io::Result<()> {
    let path = theme_path().ok_or_else(|| std::io::Error::other("找不到用户配置目录"))?;
    save_theme_to(&path, theme)
}

fn apply_theme(ctx: &egui::Context, theme: UiTheme) {
    let colors = theme.colors();
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = colors.bg_start;
    visuals.window_fill = colors.panel;
    visuals.window_stroke = Stroke::new(1.0, colors.border);
    visuals.window_corner_radius = 14.into();
    visuals.faint_bg_color = colors.surface;
    visuals.extreme_bg_color = colors.canvas;
    visuals.text_edit_bg_color = Some(colors.canvas);
    visuals.override_text_color = Some(TEXT);
    visuals.weak_text_color = Some(MUTED);
    visuals.hyperlink_color = colors.start;
    visuals.selection.bg_fill = colors.soft;
    visuals.selection.stroke = Stroke::new(1.0, colors.start);
    visuals.widgets.inactive.weak_bg_fill = colors.surface;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, colors.border);
    visuals.widgets.inactive.corner_radius = 8.into();
    visuals.widgets.hovered.weak_bg_fill = colors.soft;
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors.start);
    visuals.widgets.hovered.corner_radius = 8.into();
    visuals.widgets.active.weak_bg_fill = colors.soft;
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, colors.start);
    visuals.widgets.active.corner_radius = 8.into();
    ctx.set_theme(egui::Theme::Light);
    ctx.set_visuals(visuals);
}

fn gradient(painter: &egui::Painter, rect: egui::Rect, colors: ThemeColors, radius: f32) {
    let mut mesh = egui::Mesh::default();
    let color_at = |x: f32| {
        let t = ((x - rect.left()) / rect.width()).clamp(0.0, 1.0);
        let channel = |a: u8, b: u8| ((a as f32 * (1.0 - t) + b as f32 * t).round()) as u8;
        Color32::from_rgb(
            channel(colors.start.r(), colors.end.r()),
            channel(colors.start.g(), colors.end.g()),
            channel(colors.start.b(), colors.end.b()),
        )
    };
    let center = rect.center();
    mesh.colored_vertex(center, color_at(center.x));
    let radius = radius.min(rect.width() / 2.0).min(rect.height() / 2.0);
    let corners = [
        (rect.right() - radius, rect.top() + radius, -90.0_f32),
        (rect.right() - radius, rect.bottom() - radius, 0.0),
        (rect.left() + radius, rect.bottom() - radius, 90.0),
        (rect.left() + radius, rect.top() + radius, 180.0),
    ];
    for (cx, cy, start) in corners {
        for step in 0..=6 {
            let angle = (start + step as f32 * 15.0).to_radians();
            let point = egui::pos2(cx + radius * angle.cos(), cy + radius * angle.sin());
            mesh.colored_vertex(point, color_at(point.x));
        }
    }
    for i in 1..mesh.vertices.len() as u32 - 1 {
        mesh.add_triangle(0, i, i + 1);
    }
    mesh.add_triangle(0, mesh.vertices.len() as u32 - 1, 1);
    painter.add(egui::Shape::mesh(mesh));
}

fn card<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let width = ui.available_width();
    egui::Frame::new()
        .fill(panel(ui))
        .stroke(Stroke::new(1.0, border(ui)))
        .corner_radius(12)
        .inner_margin(egui::Margin::same(18))
        .show(ui, |ui| {
            ui.set_min_width((width - 36.0).max(0.0));
            body(ui)
        })
}

fn dialog_frame(ctx: &egui::Context) -> egui::Frame {
    let style = ctx.style_of(egui::Theme::Light);
    let visuals = &style.visuals;
    egui::Frame::new()
        .fill(visuals.window_fill)
        .stroke(Stroke::new(1.0, visuals.window_stroke.color))
        .corner_radius(14)
        .inner_margin(egui::Margin::same(20))
}

fn dialog_header(ui: &mut egui::Ui, title: &str) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).size(17.0).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = ui
                .button(RichText::new("×").size(20.0).color(MUTED))
                .on_hover_text("关闭")
                .clicked();
        });
    });
    ui.separator();
    ui.add_space(6.0);
    close
}

fn primary_button<'a>(ui: &egui::Ui, label: &'a str) -> egui::Button<'a> {
    egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
        .fill(accent(ui))
        .stroke(Stroke::NONE)
        .corner_radius(8)
}

fn section_title(ui: &mut egui::Ui, title: &str, note: &str) {
    ui.label(RichText::new(title).size(16.0).strong());
    ui.label(RichText::new(note).size(12.0).color(MUTED));
    ui.add_space(8.0);
}

fn page_title(ui: &mut egui::Ui, _eyebrow: &str, title: &str, description: &str) {
    ui.label(RichText::new(title).size(25.0).strong());
    ui.label(RichText::new(description).size(13.0).color(MUTED));
}

fn mac_backend_reports_permission_error(snapshot: &UiSnapshot) -> bool {
    cfg!(target_os = "macos")
        && !snapshot.native_ready()
        && !(snapshot.capture_backend.as_deref() == Some("dummy")
            && snapshot.emulation_backend.as_deref() == Some("dummy"))
        && [
            snapshot.capture_error.as_deref(),
            snapshot.emulation_error.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|error| error.contains("permission"))
}

fn needs_mac_permission_setup(
    owns_daemon: bool,
    privacy: privacy::PrivacyStatus,
    snapshot: &UiSnapshot,
) -> bool {
    owns_daemon && privacy.supported && mac_backend_reports_permission_error(snapshot)
}

fn edge_name(edge: Position) -> &'static str {
    match edge {
        Position::Left => "左侧",
        Position::Right => "右侧",
        Position::Top => "上方",
        Position::Bottom => "下方",
    }
}
fn port(value: &str) -> Result<u16, String> {
    value
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| "端口必须是 1–65535 的整数".into())
}

#[derive(Clone)]
struct DeviceForm {
    id: Option<u64>,
    name: String,
    ips: String,
    port: String,
    position: Position,
    active: bool,
    fingerprint: String,
    enter_hook: String,
    leave_hook: String,
}
impl Default for DeviceForm {
    fn default() -> Self {
        Self {
            id: None,
            name: String::new(),
            ips: String::new(),
            port: "4242".into(),
            position: Position::Right,
            active: true,
            fingerprint: String::new(),
            enter_hook: String::new(),
            leave_hook: String::new(),
        }
    }
}
impl DeviceForm {
    fn client(id: u64, config: &ClientConfig, state: &ClientState) -> Self {
        Self {
            id: Some(id),
            name: config.hostname.clone().unwrap_or_default(),
            ips: config
                .fix_ips
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            port: config.port.to_string(),
            position: config.pos,
            active: state.active,
            fingerprint: String::new(),
            enter_hook: config.cmd.clone().unwrap_or_default(),
            leave_hook: config.leave_cmd.clone().unwrap_or_default(),
        }
    }
    fn discovered(device: &DiscoveredDevice) -> Self {
        Self {
            // mDNS display names may contain spaces and are not DNS hostnames.
            name: if valid_hostname(&device.name) {
                device.name.clone()
            } else {
                String::new()
            },
            ips: device
                .ips
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", "),
            port: device.port.to_string(),
            fingerprint: device.fingerprint.clone(),
            ..Default::default()
        }
    }
    fn action(&self) -> Result<UiAction, String> {
        let hostname = (!self.name.trim().is_empty()).then(|| self.name.trim().to_owned());
        let ips = self
            .ips
            .split([',', ' ', '\n', ';'])
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<IpAddr>().map_err(|_| format!("IP 地址无效：{s}")))
            .collect::<Result<Vec<_>, _>>()?;
        if hostname.is_none() && ips.is_empty() {
            return Err("请填写设备名称或 IP 地址".into());
        }
        let port = port(&self.port)?;
        Ok(match self.id {
            Some(id) => UiAction::UpdateClient {
                id,
                hostname,
                ips,
                port,
                position: self.position,
                active: self.active,
            },
            None => UiAction::AddClient {
                hostname,
                ips,
                port,
                position: self.position,
                enter_hook: hook(&self.enter_hook),
                leave_hook: hook(&self.leave_hook),
                fingerprint: (!self.fingerprint.trim().is_empty())
                    .then(|| self.fingerprint.trim().to_owned()),
            },
        })
    }
    fn hooks_action(&self) -> Option<UiAction> {
        self.id.map(|id| UiAction::SetHooks {
            id,
            enter_hook: hook(&self.enter_hook),
            leave_hook: hook(&self.leave_hook),
        })
    }
}

fn valid_hostname(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && !name.chars().any(|c| c.is_whitespace() || c.is_control())
}
fn hook(command: &str) -> Option<String> {
    (!command.trim().is_empty()).then(|| command.to_owned())
}

enum Command {
    Action(UiAction, &'static str),
    Stop,
}
struct Reply {
    label: Option<&'static str>,
    result: Result<UiSnapshot, String>,
}
struct Worker {
    commands: mpsc::Sender<Command>,
    replies: mpsc::Receiver<Reply>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Worker {
    fn new(ctx: egui::Context, _owns_daemon: bool) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (commands, requests) = mpsc::channel();
        let (results, replies) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("lan-bridge-ui-ipc".into())
            .spawn(move || {
                let mut next_poll = Instant::now();
                loop {
                    let request =
                        requests.recv_timeout(next_poll.saturating_duration_since(Instant::now()));
                    let (action, label) = match request {
                        Ok(Command::Action(action, label)) => (action, Some(label)),
                        Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => (UiAction::Snapshot, None),
                    };
                    // One ordered worker prevents old polls from overwriting confirmed operations.
                    let shutdown = matches!(action, UiAction::Shutdown);
                    let result = runtime.block_on(transport::execute(action));
                    let shutdown_confirmed = shutdown && result.is_ok();
                    if results.send(Reply { label, result }).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                    if shutdown_confirmed {
                        break;
                    }
                    next_poll = Instant::now() + Duration::from_secs(1);
                }
            })?;
        Ok(Self {
            commands,
            replies,
            thread: Some(thread),
        })
    }
    fn stop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Files,
    Devices,
    Settings,
    Trust,
    Diagnostics,
    Activity,
}

pub(crate) struct BridgeApp {
    logo: egui::TextureHandle,
    files: file_ui::FilePanel,
    worker: Worker,
    snapshot: Option<UiSnapshot>,
    online: bool,
    busy: bool,
    message: String,
    error: bool,
    tab: Tab,
    theme: UiTheme,
    reset_scroll: bool,
    selected_client: Option<u64>,
    form: Option<DeviceForm>,
    scan_open: bool,
    settings_port: String,
    settings_clipboard: bool,
    settings_dirty: bool,
    trust_name: String,
    trust_fingerprint: String,
    logs: VecDeque<String>,
    privacy: privacy::PrivacyStatus,
    next_privacy: Instant,
    permission_dialog_open: bool,
    permission_prompted: bool,
    permission_request_next_frame: bool,
    permission_retry_pending: bool,
    owns_daemon: bool,
    diagnostic_local: bool,
    confirm_shutdown: bool,
    shutdown_confirmed: bool,
}
impl BridgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>, owns_daemon: bool) -> std::io::Result<Self> {
        configure_fonts(&cc.egui_ctx);
        let icon = eframe::icon_data::from_png_bytes(crate::APP_ICON_PNG)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let logo = cc.egui_ctx.load_texture(
            "deskunify-logo",
            egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            ),
            egui::TextureOptions::LINEAR,
        );
        let theme = load_theme();
        let privacy = privacy::status();
        apply_theme(&cc.egui_ctx, theme);
        cc.egui_ctx.style_mut_of(egui::Theme::Light, |s| {
            s.spacing.item_spacing = egui::vec2(12.0, 8.0);
            s.spacing.interact_size.y = 30.0;
            s.spacing.button_padding = egui::vec2(13.0, 9.0);
            s.text_styles
                .insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
            s.text_styles
                .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
        });
        Ok(Self {
            logo,
            files: file_ui::FilePanel::default(),
            worker: Worker::new(cc.egui_ctx.clone(), owns_daemon)?,
            snapshot: None,
            online: false,
            busy: false,
            message: "正在连接本机后台…".into(),
            error: false,
            tab: Tab::Devices,
            theme,
            reset_scroll: false,
            selected_client: None,
            form: None,
            scan_open: false,
            settings_port: String::new(),
            settings_clipboard: false,
            settings_dirty: false,
            trust_name: String::new(),
            trust_fingerprint: String::new(),
            logs: VecDeque::new(),
            privacy,
            next_privacy: Instant::now(),
            permission_dialog_open: false,
            permission_prompted: false,
            permission_request_next_frame: false,
            permission_retry_pending: owns_daemon && privacy.ready(),
            owns_daemon,
            diagnostic_local: false,
            confirm_shutdown: false,
            shutdown_confirmed: false,
        })
    }
    fn send(&mut self, action: UiAction, label: &'static str) {
        if self.busy {
            return;
        }
        match self.worker.commands.send(Command::Action(action, label)) {
            Ok(()) => {
                self.busy = true;
                self.error = false;
                self.message = format!("{label}：等待后台确认…");
            }
            Err(_) => self.fail("后台通信线程已退出，请重启应用".into()),
        }
    }
    fn record(&mut self, message: String) {
        if self.logs.len() == 100 {
            self.logs.pop_front();
        }
        self.logs.push_back(message);
    }
    fn fail(&mut self, message: String) {
        self.error = true;
        self.message = message.clone();
        self.record(message);
    }
    fn request_privacy(&mut self) {
        self.permission_prompted = true;
        match privacy::request() {
            Ok(status) => {
                self.update_privacy(status);
                self.permission_retry_pending = self.owns_daemon && status.ready();
                self.error = false;
                self.message = if self.owns_daemon {
                    "已向 macOS 请求键鼠权限。请在系统弹窗中允许 DeskUnify；授权后如仍未就绪，请完全退出应用再打开"
                        .into()
                } else {
                    "已请求当前 GUI 的权限。现有后台需授权启动它的 Terminal/iTerm；以后端状态为准"
                        .into()
                };
                self.record(self.message.clone());
                self.retry_after_permission_change();
            }
            Err(error) => self.fail(error),
        }
    }
    fn update_privacy(&mut self, status: privacy::PrivacyStatus) {
        let gained_permission = !self.privacy.ready() && status.ready();
        self.privacy = status;
        if !status.ready() {
            self.permission_retry_pending = false;
        } else if self.owns_daemon && gained_permission {
            self.permission_retry_pending = true;
        }
    }

    fn retry_after_permission_change(&mut self) {
        if self.snapshot.as_ref().is_some_and(|s| s.native_ready()) {
            self.permission_retry_pending = false;
        } else if self.permission_retry_pending
            && self.owns_daemon
            && self.online
            && !self.busy
            && self
                .snapshot
                .as_ref()
                .is_some_and(mac_backend_reports_permission_error)
        {
            // Preserve the grant transition while an earlier IPC action is busy.
            self.permission_retry_pending = false;
            self.send(UiAction::RetryBackends, "重新检测后端");
        }
    }

    fn receive(&mut self) {
        while let Ok(reply) = self.worker.replies.try_recv() {
            if reply.label.is_some() {
                self.busy = false;
            }
            match reply.result {
                Ok(snapshot) => {
                    if !self.settings_dirty || reply.label == Some("保存设置") {
                        self.settings_port = snapshot.port.to_string();
                        self.settings_clipboard = snapshot.clipboard;
                        self.settings_dirty = false;
                    }
                    if let Some(label) = reply.label {
                        self.error = false;
                        self.message = format!("{label}：后台已确认");
                        if label == "保存设备" || label == "删除设备" {
                            self.form = None;
                        }
                        if label == "授权设备" {
                            self.trust_name.clear();
                            self.trust_fingerprint.clear();
                        }
                        if label == "扫描局域网" {
                            self.message =
                                format!("扫描完成：发现 {} 台其他设备", snapshot.discovered.len());
                        }
                        if label == "重新检测后端" && !snapshot.native_ready() {
                            self.error = true;
                            self.message =
                                "重新检测完成：原生键鼠后端仍未就绪，请查看诊断中的后端错误".into();
                        }
                        if label == "运行诊断" {
                            self.error = !snapshot.doctor_ready(self.diagnostic_local);
                            self.message =
                                diagnostic_summary(&snapshot, self.diagnostic_local).into();
                        }
                        if label == "退出后台" {
                            self.online = false;
                            self.shutdown_confirmed = true;
                            self.confirm_shutdown = false;
                        }
                        self.record(self.message.clone());
                    } else if !self.online && !self.busy {
                        self.message = "后台已连接".into();
                        self.error = false;
                    }
                    if let Some(previous) = self.snapshot.clone() {
                        if previous.capture != snapshot.capture
                            || previous.emulation != snapshot.emulation
                        {
                            self.record(format!(
                                "输入状态更新：采集 {:?} / 模拟 {:?}",
                                snapshot.capture, snapshot.emulation
                            ));
                        }
                        if previous.discovered.len() != snapshot.discovered.len() {
                            self.record(format!(
                                "扫描发现 {} 台其他设备",
                                snapshot.discovered.len()
                            ));
                        }
                        for (id, config, state) in &snapshot.clients {
                            if previous
                                .clients
                                .iter()
                                .find(|(previous_id, _, _)| previous_id == id)
                                .is_some_and(|(_, _, old)| old.alive != state.alive)
                            {
                                self.record(format!(
                                    "{}：{}",
                                    client_name(config),
                                    if state.alive {
                                        "连接已建立"
                                    } else {
                                        "连接已断开"
                                    }
                                ));
                            }
                        }
                    }
                    self.online = !self.shutdown_confirmed;
                    self.snapshot = Some(snapshot);
                }
                Err(error) => {
                    if reply.label.is_none() {
                        self.online = false;
                    }
                    if reply.label.is_some() || self.message != error {
                        self.fail(error);
                    }
                }
            }
        }
        if Instant::now() >= self.next_privacy {
            self.update_privacy(privacy::status());
            self.next_privacy = Instant::now() + Duration::from_secs(2);
        }
        self.retry_after_permission_change();
        if self.snapshot.as_ref().is_some_and(|s| s.native_ready()) {
            self.permission_dialog_open = false;
        } else if !self.permission_prompted
            && self
                .snapshot
                .as_ref()
                .is_some_and(|s| needs_mac_permission_setup(self.owns_daemon, self.privacy, s))
        {
            self.permission_prompted = true;
            self.permission_dialog_open = true;
            self.permission_request_next_frame = true;
        }
    }
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::Image::new(&self.logo)
                    .fit_to_exact_size(egui::vec2(40.0, 40.0))
                    .corner_radius(10),
            );
            ui.vertical(|ui| {
                ui.label(RichText::new("DeskUnify").size(17.0).strong());
                ui.label(RichText::new("连接你的工作空间").size(10.0).color(MUTED));
            });
        });
        ui.add_space(34.0);
        ui.label(RichText::new("工作空间").size(11.0).color(MUTED));
        ui.add_space(8.0);
        for (tab, title) in [
            (Tab::Devices, "设备与布局"),
            (Tab::Settings, "共享设置"),
            (Tab::Files, "文件传输"),
            (Tab::Trust, "设备授权"),
            (Tab::Diagnostics, "连接诊断"),
            (Tab::Activity, "活动记录"),
        ] {
            let active = self.tab == tab;
            let response = ui.add_sized(
                [ui.available_width(), 43.0],
                egui::Button::new(
                    RichText::new(format!("    {title}"))
                        .size(13.0)
                        .color(if active { accent(ui) } else { MUTED }),
                )
                .fill(if active {
                    soft_accent(ui)
                } else {
                    self.theme.colors().sidebar
                })
                .stroke(Stroke::NONE)
                .corner_radius(9),
            );
            navigation_icon(
                ui.painter(),
                response.rect.left_center() + egui::vec2(21.0, 0.0),
                tab,
                if active { accent(ui) } else { MUTED },
                panel(ui),
            );
            if response.clicked() {
                self.tab = tab;
                self.reset_scroll = true;
            }
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(12.0);
            ui.label(RichText::new("局域网内 · 设备之间").size(11.0).color(MUTED));
            ui.separator();
            ui.label(
                RichText::new(if self.online {
                    "●  本机服务在线"
                } else {
                    "○  正在连接服务"
                })
                .size(12.0)
                .color(if self.online { GREEN } else { MUTED }),
            );
        });
    }
    fn header(&mut self, ui: &mut egui::Ui) {
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 68.0), egui::Sense::hover());
        gradient(ui.painter(), rect, self.theme.colors(), 13.0);
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect.shrink(17.0)), |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("DeskUnify")
                            .size(16.0)
                            .strong()
                            .color(Color32::WHITE),
                    );
                    ui.label(
                        RichText::new("我的工作空间")
                            .size(11.0)
                            .color(Color32::from_rgb(245, 239, 255)),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_enabled_ui(self.online && !self.busy, |ui| {
                        if ui
                            .add(egui::Button::new("返回本机").fill(panel(ui)))
                            .clicked()
                        {
                            self.send(UiAction::Release, "返回本机");
                        }
                        let paused = self.snapshot.as_ref().is_some_and(|s| s.paused);
                        if ui
                            .add(
                                egui::Button::new(if paused {
                                    "恢复共享"
                                } else {
                                    "暂停共享"
                                })
                                .fill(panel(ui)),
                            )
                            .clicked()
                        {
                            self.send(UiAction::SetPaused { paused: !paused }, "切换共享状态");
                        }
                    });
                    if self.busy {
                        ui.spinner();
                    }
                });
            });
        });
        ui.add_space(18.0);
        if self.error || self.busy || (!self.message.is_empty() && self.message != "后台已连接")
        {
            egui::Frame::new()
                .fill(if self.error {
                    Color32::from_rgb(255, 239, 237)
                } else {
                    soft_accent(ui)
                })
                .corner_radius(8)
                .inner_margin(egui::Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(&self.message)
                            .size(12.0)
                            .color(if self.error { RED } else { accent(ui) }),
                    );
                });
            ui.add_space(12.0);
        }
    }
    fn devices(&mut self, ui: &mut egui::Ui, snapshot: &UiSnapshot) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                page_title(
                    ui,
                    "WORKSPACE",
                    "设备与布局",
                    "拖动设备到本机边缘，决定跨屏方向。",
                );
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(self.online && !self.busy, |ui| {
                    if ui.button("手动添加").clicked() {
                        self.form = Some(DeviceForm::default());
                    }
                    if ui.add(primary_button(ui, "扫描设备")).clicked() {
                        self.scan_open = true;
                        self.send(UiAction::Scan, "扫描局域网");
                    }
                });
            });
        });
        ui.add_space(14.0);
        card(ui, |ui| {
            ui.columns(4, |columns| {
                metric(
                    &mut columns[0],
                    "远端设备",
                    &snapshot.clients.len().to_string(),
                    "已配置",
                );
                metric(
                    &mut columns[1],
                    "已连接",
                    &snapshot
                        .clients
                        .iter()
                        .filter(|(_, _, s)| s.alive)
                        .count()
                        .to_string(),
                    "台电脑",
                );
                metric(
                    &mut columns[2],
                    "输入状态",
                    if snapshot.native_ready() {
                        "就绪"
                    } else {
                        "待处理"
                    },
                    if snapshot.paused {
                        "共享已暂停"
                    } else {
                        "未暂停"
                    },
                );
                metric(
                    &mut columns[3],
                    "文本剪贴板",
                    if snapshot.clipboard {
                        "开启"
                    } else {
                        "关闭"
                    },
                    "按设备配置",
                );
            });
        });
        ui.add_space(18.0);
        let width = ui.available_width();
        if width >= 780.0 {
            let layout_width = width * 0.62;
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(layout_width, 380.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        card(ui, |ui| {
                            ui.set_min_width(layout_width - 36.0);
                            section_title(ui, "屏幕排列", "将右侧设备拖到本机周围的位置");
                            self.layout(ui, snapshot);
                            ui.label(
                                RichText::new("鼠标越过相应边缘时切换到该设备")
                                    .size(11.0)
                                    .color(MUTED),
                            );
                        });
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(width - layout_width - 12.0, 380.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        self.device_column(ui, snapshot);
                    },
                );
            });
        } else {
            card(ui, |ui| {
                section_title(ui, "屏幕排列", "将设备拖到本机周围的位置");
                self.layout(ui, snapshot);
            });
            ui.add_space(12.0);
            self.device_column(ui, snapshot);
        }
        ui.add_space(14.0);
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.colored_label(
                    if snapshot.native_ready() {
                        GREEN
                    } else {
                        MUTED
                    },
                    "●",
                );
                ui.label(if snapshot.native_ready() {
                    "键鼠输入已就绪"
                } else {
                    "键鼠输入尚未就绪"
                });
                ui.label(
                    RichText::new(format!("{} · 端口 {}", snapshot.platform, snapshot.port))
                        .size(11.0)
                        .color(MUTED),
                );
                if ui.small_button("查看诊断").clicked() {
                    self.tab = Tab::Diagnostics;
                    self.reset_scroll = true;
                }
            });
            ui.collapsing("后端详情与紧急释放快捷键", |ui| {
                backend_line(
                    ui,
                    "输入采集",
                    snapshot.capture,
                    &snapshot.capture_backend,
                    &snapshot.capture_error,
                );
                backend_line(
                    ui,
                    "输入模拟",
                    snapshot.emulation,
                    &snapshot.emulation_backend,
                    &snapshot.emulation_error,
                );
                ui.monospace(snapshot.release_bind.join(" + "));
            });
        });
    }
    fn device_column(&mut self, ui: &mut egui::Ui, snapshot: &UiSnapshot) {
        let width = ui.available_width();
        card(ui, |ui| {
            ui.set_min_width((width - 36.0).max(0.0));
            section_title(ui, "局域网设备", "选择设备查看连接和配置");
            if snapshot.clients.is_empty() {
                ui.add_space(28.0);
                ui.label(RichText::new("还没有设备").size(16.0).strong());
                ui.label(
                    RichText::new("在另一台电脑启动 DeskUnify 后扫描，或手动添加 IP。")
                        .size(12.0)
                        .color(MUTED),
                );
            }
            let selected = self
                .selected_client
                .or_else(|| snapshot.clients.first().map(|(id, _, _)| *id));
            for (id, config, state) in &snapshot.clients {
                let active = selected == Some(*id);
                egui::Frame::new()
                    .fill(if active { soft_accent(ui) } else { surface(ui) })
                    .stroke(Stroke::new(
                        1.0,
                        if active { accent(ui) } else { border(ui) },
                    ))
                    .corner_radius(9)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let response = ui.add(
                                egui::Button::new(RichText::new(client_name(config)).strong())
                                    .fill(Color32::TRANSPARENT)
                                    .stroke(Stroke::NONE)
                                    .sense(egui::Sense::click_and_drag()),
                            );
                            if response.drag_started() {
                                response.dnd_set_drag_payload(*id);
                            }
                            if response.clicked() {
                                self.selected_client = Some(*id);
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.colored_label(
                                        if state.alive { GREEN } else { MUTED },
                                        if !state.active {
                                            "已停用"
                                        } else if state.alive {
                                            "● 已连接"
                                        } else {
                                            "○ 等待"
                                        },
                                    );
                                },
                            );
                        });
                        ui.label(
                            RichText::new(format!(
                                "{} · 端口 {}",
                                edge_name(config.pos),
                                config.port
                            ))
                            .size(11.0)
                            .color(MUTED),
                        );
                    });
            }
            if let Some((id, config, state)) = snapshot
                .clients
                .iter()
                .find(|(id, _, _)| selected == Some(*id))
            {
                ui.add_space(8.0);
                ui.separator();
                ui.label(RichText::new("选中设备").size(11.0).color(accent(ui)));
                ui.label(RichText::new(client_name(config)).size(17.0).strong());
                ui.label(
                    RichText::new(format!(
                        "{}:{} · {}",
                        config
                            .fix_ips
                            .first()
                            .map(ToString::to_string)
                            .unwrap_or_else(|| config
                                .hostname
                                .clone()
                                .unwrap_or_else(|| "尚未解析".into())),
                        config.port,
                        edge_name(config.pos)
                    ))
                    .size(12.0)
                    .color(MUTED),
                );
                if snapshot.active_client == Some(*id) {
                    ui.colored_label(GREEN, "● 当前正在控制");
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.online && !self.busy, primary_button(ui, "编辑设备"))
                        .clicked()
                    {
                        self.form = Some(DeviceForm::client(*id, config, state));
                    }
                    if ui
                        .add_enabled(
                            self.online && !self.busy,
                            egui::Button::new(if state.active { "停用" } else { "启用" }),
                        )
                        .clicked()
                    {
                        self.send(
                            UiAction::SetActive {
                                id: *id,
                                active: !state.active,
                            },
                            "切换设备状态",
                        );
                    }
                });
                ui.add_space(10.0);
                ui.label(RichText::new("此设备的共享功能").strong());
                let mut sharing = config.sharing;
                let mut changed = false;
                ui.add_enabled_ui(self.online && !self.busy, |ui| {
                    changed |= ui
                        .checkbox(&mut sharing.mouse, "鼠标（移动、按键和滚轮）")
                        .changed();
                    changed |= ui.checkbox(&mut sharing.keyboard, "键盘").changed();
                    changed |= ui
                        .checkbox(&mut sharing.clipboard, "文字复制粘贴")
                        .changed();
                    changed |= ui.checkbox(&mut sharing.files, "文件复制粘贴").changed();
                });
                if changed {
                    self.send(
                        UiAction::SetSharing { id: *id, sharing },
                        "保存设备共享设置",
                    );
                }
                if snapshot.paused {
                    ui.label("全局已暂停");
                } else if !state.active {
                    ui.label("设备已停用，四项共享均停止");
                } else if let Some(error) = &state.sharing_error {
                    ui.colored_label(RED, format!("连接或配对尚未就绪：{error}"));
                } else if let Some(peer) = state.peer_sharing {
                    if let Some(note) = &state.peer_note {
                        ui.colored_label(MUTED, note);
                    }
                    let off = [
                        (!peer.mouse, "鼠标"),
                        (!peer.keyboard, "键盘"),
                        (!peer.clipboard, "文字"),
                        (!peer.files, "文件"),
                    ]
                    .into_iter()
                    .filter_map(|(off, label)| off.then_some(label))
                    .collect::<Vec<_>>();
                    if !off.is_empty() {
                        ui.label(format!("对端关闭：{}", off.join("、")));
                    } else {
                        ui.label("双方共享设置已确认");
                    }
                    if !snapshot.native_ready() {
                        ui.label("键鼠权限尚未就绪；文字和文件共享独立运行");
                    }
                } else {
                    ui.label(if config.fingerprint.is_none() {
                        "请扫描配对设备，核对并保存对端指纹"
                    } else {
                        "等待对端确认共享设置（需要两端使用新版）"
                    });
                }
                ui.collapsing("连接详情", |ui| {
                    client_details(ui, config, state);
                });
            }
        });
    }
    fn layout(&mut self, ui: &mut egui::Ui, snapshot: &UiSnapshot) {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), 310.0),
            egui::Sense::hover(),
        );
        let painter = ui.painter().clone();
        painter.rect_filled(rect, 10.0, canvas(ui));
        painter.rect_stroke(rect, 10.0, Stroke::new(1.0, border(ui)), StrokeKind::Inside);
        let mut x = rect.left() + 16.0;
        while x < rect.right() - 12.0 {
            let mut y = rect.top() + 16.0;
            while y < rect.bottom() - 12.0 {
                painter.circle_filled(egui::pos2(x, y), 0.7, border(ui));
                y += 22.0;
            }
            x += 22.0;
        }
        let center = rect.center();
        let size = egui::vec2((rect.width() / 3.0 - 20.0).clamp(110.0, 160.0), 82.0);
        let local = egui::Rect::from_center_size(center, size);
        for edge in EDGES {
            let offset = match edge {
                Position::Left => egui::vec2(-size.x - 18.0, 0.0),
                Position::Right => egui::vec2(size.x + 18.0, 0.0),
                Position::Top => egui::vec2(0.0, -104.0),
                Position::Bottom => egui::vec2(0.0, 104.0),
            };
            let zone = egui::Rect::from_center_size(center + offset, size);
            let response = ui.interact(
                zone,
                ui.id().with(edge.to_string()),
                egui::Sense::click_and_drag(),
            );
            let client = snapshot
                .clients
                .iter()
                .find(|(_, c, s)| c.pos == edge && s.active);
            let hovered = response.dnd_hover_payload::<u64>().is_some();
            let color = if hovered {
                soft_accent(ui)
            } else if client.is_some() {
                panel(ui)
            } else {
                canvas(ui)
            };
            if client.is_some() {
                painter.line_segment(
                    [local.center(), zone.center()],
                    Stroke::new(2.0, if hovered { accent(ui) } else { border(ui) }),
                );
            }
            painter.rect_filled(zone, 8.0, color);
            painter.rect_stroke(
                zone,
                8.0,
                Stroke::new(
                    1.0,
                    if hovered {
                        accent(ui)
                    } else if client.is_some() {
                        Color32::from_rgb(168, 187, 220)
                    } else {
                        border(ui)
                    },
                ),
                StrokeKind::Inside,
            );
            let title = client
                .map(|(_, c, _)| client_name(c))
                .unwrap_or_else(|| "+".into());
            let title = if title.chars().count() > 15 {
                format!("{}…", title.chars().take(14).collect::<String>())
            } else {
                title
            };
            painter.text(
                zone.center() + egui::vec2(0.0, -9.0),
                egui::Align2::CENTER_CENTER,
                title,
                egui::FontId::proportional(if client.is_some() { 14.0 } else { 22.0 }),
                if client.is_some() { TEXT } else { MUTED },
            );
            painter.text(
                zone.center() + egui::vec2(0.0, 15.0),
                egui::Align2::CENTER_CENTER,
                if client.is_some() {
                    edge_name(edge).to_owned()
                } else {
                    format!("放在{}", edge_name(edge))
                },
                egui::FontId::proportional(11.0),
                if client.is_some_and(|(_, _, s)| s.alive) {
                    GREEN
                } else {
                    MUTED
                },
            );
            if let Some((_, _, state)) = client {
                monitor_stand(&painter, zone, Color32::from_rgb(168, 187, 220));
                painter.circle_filled(
                    zone.right_top() + egui::vec2(-10.0, 10.0),
                    3.0,
                    if state.alive { GREEN } else { MUTED },
                );
            }
            if let Some((id, _, _)) = client {
                if response.drag_started() {
                    response.dnd_set_drag_payload(*id);
                }
                if response.clicked() {
                    self.selected_client = Some(*id);
                }
            }
            if self.online && !self.busy {
                if let Some(id) = response.dnd_release_payload::<u64>() {
                    self.send(
                        UiAction::SetPosition {
                            id: *id,
                            position: edge,
                        },
                        "调整屏幕位置",
                    );
                }
            }
        }
        painter.rect_filled(
            local.translate(egui::vec2(0.0, 4.0)).expand(3.0),
            12.0,
            soft_accent(ui),
        );
        gradient(&painter, local, self.theme.colors(), 9.0);
        monitor_stand(&painter, local, accent(ui));
        painter.text(
            local.center() + egui::vec2(0.0, -12.0),
            egui::Align2::CENTER_CENTER,
            "本机",
            egui::FontId::proportional(17.0),
            Color32::WHITE,
        );
        painter.text(
            local.center() + egui::vec2(0.0, 16.0),
            egui::Align2::CENTER_CENTER,
            format!("{} · 本机屏幕", snapshot.platform),
            egui::FontId::proportional(10.0),
            Color32::from_rgb(222, 234, 255),
        );
    }
    fn settings(&mut self, ui: &mut egui::Ui, snapshot: &UiSnapshot) {
        page_title(
            ui,
            "PREFERENCES",
            "共享设置",
            "调整连接、剪贴板与系统权限。",
        );
        ui.add_space(20.0);
        card(ui, |ui| {
            section_title(ui, "外观主题", "仅影响当前控制界面，选择后自动保存");
            ui.horizontal_wrapped(|ui| {
                for theme in UiTheme::ALL {
                    let selected = self.theme == theme;
                    let colors = theme.colors();
                    let response = ui.add_sized(
                        [142.0, 39.0],
                        egui::Button::new(
                            RichText::new(format!("     {}", theme.name()))
                                .size(12.0)
                                .color(if selected { colors.start } else { TEXT }),
                        )
                        .fill(if selected { colors.soft } else { canvas(ui) })
                        .stroke(Stroke::new(
                            1.0,
                            if selected { colors.start } else { border(ui) },
                        ))
                        .corner_radius(9),
                    );
                    gradient(
                        ui.painter(),
                        egui::Rect::from_center_size(
                            response.rect.left_center() + egui::vec2(18.0, 0.0),
                            egui::vec2(18.0, 18.0),
                        ),
                        colors,
                        7.0,
                    );
                    if response.clicked() && !selected {
                        self.theme = theme;
                        apply_theme(ui.ctx(), theme);
                        ui.ctx().request_repaint();
                        if let Err(error) = save_theme(theme) {
                            self.fail(format!("主题已切换，但保存偏好失败：{error}"));
                        }
                    }
                }
            });
        });
        ui.add_space(14.0);
        let width = ui.available_width();
        if width >= 850.0 {
            let left = width * 0.45;
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(left, 330.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        card(ui, |ui| self.settings_connection(ui, snapshot));
                    },
                );
                ui.allocate_ui_with_layout(
                    egui::vec2(width - left - 12.0, 330.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        card(ui, |ui| self.settings_permissions(ui, snapshot));
                    },
                );
            });
        } else {
            card(ui, |ui| self.settings_connection(ui, snapshot));
            ui.add_space(14.0);
            card(ui, |ui| self.settings_permissions(ui, snapshot));
        }
        ui.add_space(14.0);
        card(ui, |ui| self.settings_backend_info(ui, snapshot));
    }

    fn settings_connection(&mut self, ui: &mut egui::Ui, _snapshot: &UiSnapshot) {
        section_title(ui, "连接与同步", "配置会保存到当前后台");
        ui.add_enabled_ui(!self.busy, |ui| {
            ui.horizontal(|ui| {
                ui.label("监听端口");
                self.settings_dirty |= ui
                    .add(egui::TextEdit::singleline(&mut self.settings_port).desired_width(180.0))
                    .changed();
            });
        });
        ui.label(
            RichText::new(
                "鼠标、键盘、文字和文件开关在「设备与布局」的具体设备中设置；新配对默认全部开启。",
            )
            .size(12.0)
            .color(MUTED),
        );
        ui.add_enabled_ui(self.online && !self.busy, |ui| {
            if ui.add(primary_button(ui, "保存设置")).clicked() {
                match port(&self.settings_port) {
                    Ok(port) => self.send(
                        UiAction::SetSettings {
                            port,
                            clipboard: self.settings_clipboard,
                        },
                        "保存设置",
                    ),
                    Err(error) => self.fail(error),
                }
            }
            if ui.button("重新检测输入后端").clicked() {
                self.send(UiAction::RetryBackends, "重新检测后端");
            }
        });
    }

    fn settings_permissions(&mut self, ui: &mut egui::Ui, snapshot: &UiSnapshot) {
        section_title(ui, "系统权限", "共享能力以当前后台的实际状态为准");
        backend_line(
            ui,
            "输入采集",
            snapshot.capture,
            &snapshot.capture_backend,
            &snapshot.capture_error,
        );
        backend_line(
            ui,
            "输入模拟",
            snapshot.emulation,
            &snapshot.emulation_backend,
            &snapshot.emulation_error,
        );
        if self.privacy.supported {
            ui.label(
                RichText::new("下面检查当前 GUI 进程；是否可用以后台的原生后端为准。")
                    .size(12.0)
                    .color(MUTED),
            );
            if !self.owns_daemon {
                ui.label(
                    RichText::new("现有后台若由 Terminal/iTerm 启动，需授权那个终端 App。")
                        .size(12.0)
                        .color(MUTED),
                );
            }
            ui.label(format!(
                "辅助功能：{}    输入监控：{}    输入控制：{}",
                grant(self.privacy.accessibility),
                grant(self.privacy.input_monitoring),
                grant(self.privacy.input_control)
            ));
            if ui.button("请求键鼠权限").clicked() {
                self.request_privacy();
            }
            ui.horizontal_wrapped(|ui| {
                for (label, pane) in [
                    ("辅助功能设置", privacy::PrivacyPane::Accessibility),
                    ("输入监控设置", privacy::PrivacyPane::InputMonitoring),
                    ("本地网络设置", privacy::PrivacyPane::LocalNetwork),
                ] {
                    if ui.button(label).clicked() {
                        if let Err(error) = privacy::open(pane) {
                            self.fail(error);
                        }
                    }
                }
            });
        } else {
            ui.label("请确保系统允许应用采集和模拟键鼠事件，并放行局域网 TCP/UDP 监听端口与 mDNS UDP 5353。");
        }
    }

    fn permission_banner(&mut self, ui: &mut egui::Ui) {
        let needs_gui_permission = self
            .snapshot
            .as_ref()
            .is_some_and(|s| needs_mac_permission_setup(self.owns_daemon, self.privacy, s));
        let needs_terminal_permission = !self.owns_daemon
            && self
                .snapshot
                .as_ref()
                .is_some_and(mac_backend_reports_permission_error);
        if !needs_gui_permission && !needs_terminal_permission {
            return;
        }
        egui::Frame::new()
            .fill(Color32::from_rgb(255, 244, 239))
            .stroke(Stroke::new(1.0, RED))
            .corner_radius(10)
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(RED, "键鼠共享尚未就绪");
                    ui.label(if needs_terminal_permission {
                        "请在 macOS 设置中允许启动后台的 Terminal/iTerm，并重启后台。"
                    } else if self.privacy.ready() {
                        "系统权限已允许，但后台仍报权限错误；请重新启动应用。"
                    } else {
                        "请允许 DeskUnify 的辅助功能、输入监控和输入控制。"
                    });
                    if needs_gui_permission && ui.add(primary_button(ui, "处理权限")).clicked()
                    {
                        self.permission_dialog_open = true;
                    } else if needs_terminal_permission && ui.button("打开辅助功能设置").clicked()
                    {
                        if let Err(error) = privacy::open(privacy::PrivacyPane::Accessibility) {
                            self.fail(error);
                        }
                    }
                });
            });
        ui.add_space(14.0);
    }

    fn permission_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.permission_dialog_open;
        let mut close_clicked = false;
        egui::Window::new("键鼠共享权限")
            .open(&mut open)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(dialog_frame(ctx))
            .default_width(550.0)
            .show(ctx, |ui| {
                close_clicked = dialog_header(ui, "允许键鼠共享");
                ui.label(if self.privacy.ready() {
                    "GUI 权限已允许，但后台仍报告权限错误。请完全退出 DeskUnify 后重新打开；若仍失败，检查系统设置中的应用授权。"
                } else {
                    "DeskUnify 尚未获得 macOS 键鼠权限。系统授权窗口会由 macOS 弹出，是否允许由你决定。"
                });
                ui.add_space(8.0);
                ui.label(format!(
                    "辅助功能：{}    输入监控：{}    输入控制：{}",
                    grant(self.privacy.accessibility),
                    grant(self.privacy.input_monitoring),
                    grant(self.privacy.input_control)
                ));
                ui.label(RichText::new("若系统没有弹窗，请打开下方设置并将 DeskUnify 加入允许列表。")
                    .size(12.0)
                    .color(MUTED));
                ui.label("更新后开关已打开却仍未允许：请在辅助功能和输入监控中移除旧的 DeskUnify 条目，再用「+」添加下面的当前 App 并允许。");
                if let Ok(executable) = std::env::current_exe() {
                    let app = executable.ancestors().find(|path| {
                        path.extension().is_some_and(|extension| extension == "app")
                    }).unwrap_or(&executable);
                    ui.label(RichText::new(app.display().to_string()).monospace().size(11.0));
                }
                ui.add_space(10.0);
                if !self.privacy.ready()
                    && ui.add(primary_button(ui, "请求系统授权")).clicked()
                {
                    self.request_privacy();
                }
                ui.horizontal_wrapped(|ui| {
                    for (label, pane) in [
                        ("打开辅助功能设置", privacy::PrivacyPane::Accessibility),
                        ("打开输入监控设置", privacy::PrivacyPane::InputMonitoring),
                    ] {
                        if ui.button(label).clicked() {
                            if let Err(error) = privacy::open(pane) {
                                self.fail(error);
                            }
                        }
                    }
                });
                ui.label(RichText::new("允许后会自动重新检测；若状态仍未更新，请完全退出 DeskUnify 后重新打开。")
                    .size(12.0)
                    .color(MUTED));
                if ui.button("稍后处理").clicked() {
                    close_clicked = true;
                }
            });
        self.permission_dialog_open &=
            open && !close_clicked && self.snapshot.as_ref().is_some_and(|s| !s.native_ready());
    }

    fn settings_backend_info(&mut self, ui: &mut egui::Ui, snapshot: &UiSnapshot) {
        section_title(ui, "后台与配置", "当前窗口连接的本机服务");
        ui.label(format!("配置文件：{}", snapshot.config_path));
        ui.label(format!("本地 IPC 版本：{}", snapshot.protocol_version));
        ui.label(if self.owns_daemon {
            "后台由此窗口启动"
        } else {
            "此窗口连接到已有后台"
        });
        ui.label(
            RichText::new("窗口关闭时，仅停止由此窗口启动的后台；连接到已有后台时保留后台运行。")
                .size(12.0)
                .color(MUTED),
        );
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.online && !self.busy,
                    primary_button(ui, "保存当前配置到文件"),
                )
                .clicked()
            {
                self.send(UiAction::SaveConfig, "保存配置");
            }
            if ui
                .add_enabled(
                    self.online && !self.busy,
                    egui::Button::new("退出后台并关闭窗口"),
                )
                .clicked()
            {
                self.confirm_shutdown = true;
            }
        });
    }
    fn diagnostics(&mut self, ui: &mut egui::Ui, snapshot: &UiSnapshot) {
        page_title(
            ui,
            "DIAGNOSTICS",
            "连接诊断",
            "查看真实后端与设备连接状态。",
        );
        ui.add_space(20.0);
        card(ui, |ui| {
            section_title(ui, "就绪检查", "与 CLI doctor 使用同一套就绪判断");
            ui.checkbox(
                &mut self.diagnostic_local,
                "只检查本机后端（不要求设备连接）",
            );
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.online && !self.busy, primary_button(ui, "运行诊断"))
                    .clicked()
                {
                    self.send(UiAction::Snapshot, "运行诊断");
                }
                if ui.button("复制诊断 JSON").clicked() {
                    match serde_json::to_string_pretty(snapshot) {
                        Ok(report) => ui.ctx().copy_text(report),
                        Err(error) => self.fail(format!("无法生成诊断：{error}")),
                    }
                }
            });
            let ready = self.online && snapshot.doctor_ready(self.diagnostic_local);
            ui.colored_label(
                if ready { GREEN } else { RED },
                if self.online {
                    diagnostic_summary(snapshot, self.diagnostic_local)
                } else {
                    "后台离线，下方为最后一次状态"
                },
            );
        });
        ui.add_space(14.0);
        card(ui, |ui| {
            section_title(ui, "输入后端", "采集与模拟必须都使用原生后端");
            backend_line(
                ui,
                "输入采集",
                snapshot.capture,
                &snapshot.capture_backend,
                &snapshot.capture_error,
            );
            backend_line(
                ui,
                "输入模拟",
                snapshot.emulation,
                &snapshot.emulation_backend,
                &snapshot.emulation_error,
            );
            ui.label(format!(
                "设备 {} · 启用 {} · 已连接 {} · 授权 {} · 剪贴板 {}",
                snapshot.clients.len(),
                snapshot.clients.iter().filter(|(_, _, s)| s.active).count(),
                snapshot.clients.iter().filter(|(_, _, s)| s.alive).count(),
                snapshot.authorized.len(),
                if snapshot.clipboard {
                    "开启"
                } else {
                    "关闭"
                }
            ));
            if snapshot.paused {
                ui.label("当前暂停：点击顶部「恢复共享」恢复输入和剪贴板同步。");
            }
            if let Some(error) = &snapshot.discovery_error {
                ui.colored_label(RED, format!("发现错误：{error}"));
            }
        });
        ui.add_space(14.0);
        card(ui, |ui| {
            section_title(ui, "连接详情", "当前设备、地址与紧急释放键");
            ui.label(format!(
                "平台 {} · 监听端口 {} · 配置 {}",
                snapshot.platform, snapshot.port, snapshot.config_path
            ));
            ui.label(format!("紧急释放：{}", snapshot.release_bind.join(" + ")));
            for (id, config, state) in &snapshot.clients {
                egui::Frame::new()
                    .fill(surface(ui))
                    .stroke(Stroke::new(1.0, border(ui)))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.strong(format!(
                            "#{} {} · {}",
                            id,
                            client_name(config),
                            edge_name(config.pos)
                        ));
                        client_details(ui, config, state);
                    });
            }
        });
        ui.add_space(14.0);
        card(ui, |ui| {
            section_title(ui, "使用提示", "完成诊断后还需实体跨屏验证");
            ui.label("A 发起配对、B 允许后自动建立双向设备；控制键鼠时将鼠标移到对应屏幕边缘。就绪后，请逐项实测键盘、修饰键、鼠标点击、滚轮、跨屏返回和紧急释放。");
            ui.label("原生后端失败时先处理权限错误，再重新检测；macOS 新授权可能需要完全退出启动 App 并重新运行后台。");
        });
    }
    fn shutdown_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.confirm_shutdown;
        let mut close_clicked = false;
        egui::Window::new("退出后台")
            .open(&mut open)
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .frame(dialog_frame(ctx))
            .show(ctx, |ui| {
                close_clicked = dialog_header(ui, "退出后台");
                ui.label("停止本机全部共享和自动文件传输，释放输入并关闭窗口。配置会保留。");
                if !self.owns_daemon {
                    ui.label("此操作也会停止你从 CLI 启动的现有后台。");
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.online && !self.busy, egui::Button::new("确认退出后台"))
                        .clicked()
                    {
                        self.send(UiAction::Shutdown, "退出后台");
                    }
                    if ui
                        .add_enabled(!self.busy, egui::Button::new("取消"))
                        .clicked()
                    {
                        self.confirm_shutdown = false;
                    }
                });
                if self.error {
                    ui.colored_label(RED, &self.message);
                }
            });
        self.confirm_shutdown &= open && !close_clicked;
    }
    fn trust(&mut self, ui: &mut egui::Ui, snapshot: &UiSnapshot) {
        page_title(
            ui,
            "TRUST",
            "设备授权",
            "两台电脑需相互核对完整指纹并授权。",
        );
        ui.add_space(20.0);
        card(ui, |ui| {
            section_title(ui, "本机身份", "在另一台电脑上授权这个公开指纹");
            ui.label("两台设备需相互授权。请在对方电脑上核对指纹后添加授权。");
            ui.monospace(&snapshot.fingerprint);
            if ui.button("复制本机指纹").clicked() {
                ui.ctx().copy_text(snapshot.fingerprint.clone());
            }
        });
        ui.add_space(14.0);
        card(ui, |ui| {
            section_title(ui, "授权设备", "仅在核对对方完整指纹后提交");
            ui.horizontal(|ui| {
                ui.label("设备备注");
                ui.text_edit_singleline(&mut self.trust_name);
            });
            ui.horizontal(|ui| {
                ui.label("对方指纹");
                ui.text_edit_singleline(&mut self.trust_fingerprint);
            });
            ui.add_enabled_ui(self.online && !self.busy, |ui| {
                if ui.add(primary_button(ui, "授权设备")).clicked() {
                    self.send(
                        UiAction::Authorize {
                            description: self.trust_name.trim().to_owned(),
                            fingerprint: self.trust_fingerprint.trim().to_owned(),
                        },
                        "授权设备",
                    );
                }
            });
        });
        if !snapshot.connection_attempts.is_empty() {
            ui.add_space(14.0);
            card(ui, |ui| {
                section_title(ui, "待核对请求", "连接请求不会自动获得授权");
                for fingerprint in &snapshot.connection_attempts {
                    ui.group(|ui| {
                        ui.label("待核对的连接请求");
                        ui.monospace(fingerprint);
                        if ui.button("填入此指纹").clicked() {
                            self.trust_fingerprint = fingerprint.clone();
                        }
                    });
                }
            });
        }
        ui.add_space(14.0);
        card(ui, |ui| {
            section_title(
                ui,
                &format!("已授权设备 · {}", snapshot.authorized.len()),
                "撤销后该设备需重新授权",
            );
            let mut authorized: Vec<_> = snapshot.authorized.iter().collect();
            authorized.sort();
            if authorized.is_empty() {
                ui.label(RichText::new("暂无已授权设备").color(MUTED));
            }
            for (fingerprint, description) in authorized {
                egui::Frame::new()
                    .fill(surface(ui))
                    .stroke(Stroke::new(1.0, border(ui)))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::same(12))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.strong(description);
                            if ui
                                .add_enabled(
                                    self.online && !self.busy,
                                    egui::Button::new("撤销授权"),
                                )
                                .clicked()
                            {
                                self.send(
                                    UiAction::Revoke {
                                        fingerprint: fingerprint.clone(),
                                    },
                                    "撤销授权",
                                );
                            }
                        });
                        ui.monospace(fingerprint);
                    });
            }
        });
    }
    fn scan_dialog(&mut self, ctx: &egui::Context, snapshot: &UiSnapshot) {
        let mut open = self.scan_open;
        let mut close_clicked = false;
        egui::Window::new("扫描局域网设备").open(&mut open).title_bar(false).collapsible(false).resizable(true).anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO).frame(dialog_frame(ctx)).default_width(580.0).show(ctx, |ui| {
            close_clicked = dialog_header(ui, "扫描局域网设备");
            ui.label(RichText::new("发现同一局域网内运行 DeskUnify 的电脑").size(13.0).color(MUTED));
            ui.add_space(8.0);
            if let Some(error) = &snapshot.discovery_error { ui.colored_label(RED, error); }
            if snapshot.discovered.is_empty() {
                ui.label(if self.busy { "正在等待设备响应（约 3 秒）…" } else { "尚未发现其他设备。请确认另一台电脑已启动 DeskUnify，位于同一局域网，并允许本地网络访问。" });
            }
            for device in &snapshot.discovered {
                egui::Frame::new().fill(surface(ui)).stroke(Stroke::new(1.0, border(ui))).corner_radius(9).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
                    ui.label(RichText::new(format!("{} · {}",device.name,device.platform)).size(15.0).strong());
                    ui.label(RichText::new(format!("{}:{}",device.ips.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "),device.port)).color(MUTED));
                    ui.label(RichText::new(&device.fingerprint).monospace().size(11.0).color(MUTED));
                    let added = snapshot.clients.iter().any(|(_,c,_)| c.port == device.port && c.fix_ips.iter().any(|ip| device.ips.contains(ip)));
                    if ui.add_enabled(self.online && !self.busy && !added, primary_button(ui, if added { "已添加" } else { "选择设备" })).clicked() { self.form = Some(DeviceForm::discovered(device)); self.scan_open = false; }
                });
            }
            ui.add_space(8.0);
            if ui.add_enabled(self.online && !self.busy, egui::Button::new("重新扫描")).clicked() { self.send(UiAction::Scan, "扫描局域网"); }
            ui.label(RichText::new("选择设备并核对指纹；对端点击允许后会自动添加反向设备。").size(11.0).color(MUTED));
        });
        self.scan_open &= open && !close_clicked;
    }
    fn form_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut form) = self.form.take() else {
            return;
        };
        let mut open = true;
        let mut close_clicked = false;
        let mut action = None;
        egui::Window::new(if form.id.is_some() {
            "编辑设备"
        } else {
            "添加设备"
        })
        .id(egui::Id::new("device_form"))
        .open(&mut open)
        .title_bar(false)
        .collapsible(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .frame(dialog_frame(ctx))
        .default_width(540.0)
        .show(ctx, |ui| {
            close_clicked = dialog_header(
                ui,
                if form.id.is_some() {
                    "编辑设备"
                } else {
                    "添加设备"
                },
            );
            if self.busy {
                ui.disable();
            }
            ui.label(
                RichText::new("填写设备名称或固定 IP；多个 IP 使用逗号分隔。")
                    .size(12.0)
                    .color(MUTED),
            );
            ui.add_space(10.0);
            egui::Grid::new("device_fields")
                .num_columns(2)
                .spacing([14.0, 14.0])
                .show(ui, |ui| {
                    ui.label("设备名称 / 主机名");
                    ui.add(egui::TextEdit::singleline(&mut form.name).desired_width(330.0));
                    ui.end_row();
                    ui.label("固定 IP");
                    ui.add(egui::TextEdit::singleline(&mut form.ips).desired_width(330.0));
                    ui.end_row();
                    ui.label("端口");
                    ui.add(egui::TextEdit::singleline(&mut form.port).desired_width(330.0));
                    ui.end_row();
                    ui.label("屏幕位置");
                    egui::ComboBox::from_id_salt("position")
                        .width(330.0)
                        .selected_text(edge_name(form.position))
                        .show_ui(ui, |ui| {
                            for edge in EDGES {
                                ui.selectable_value(&mut form.position, edge, edge_name(edge));
                            }
                        });
                    ui.end_row();
                    if form.id.is_none() {
                        ui.label("对方证书指纹");
                        ui.add(
                            egui::TextEdit::singleline(&mut form.fingerprint).desired_width(330.0),
                        );
                        ui.end_row();
                    }
                });
            if form.id.is_some() {
                ui.checkbox(&mut form.active, "启用此设备");
            } else {
                ui.label("扫描所得指纹需与对方核对；留空时需要到「设备授权」单独授权。");
            }
            ui.collapsing("进入 / 离开设备的 hook", |ui| {
                ui.label("在本机进入或离开此设备时执行命令；留空表示清除。只填写你信任的命令。");
                ui.label("进入命令");
                ui.add(egui::TextEdit::multiline(&mut form.enter_hook).desired_rows(2));
                ui.label("离开命令");
                ui.add(egui::TextEdit::multiline(&mut form.leave_hook).desired_rows(2));
                if form.id.is_some() {
                    ui.label("hook 单独保存；下方「保存设备」只保存网络与布局配置。");
                    if ui
                        .add_enabled(self.online && !self.busy, egui::Button::new("保存 hook"))
                        .clicked()
                    {
                        action = form.hooks_action().map(|action| (action, "保存 hook"));
                    }
                } else {
                    ui.label("添加设备时将同时保存这些命令。");
                }
            });
            ui.add_enabled_ui(self.online && !self.busy, |ui| {
                if ui.add(primary_button(ui, "保存设备")).clicked() {
                    match form.action() {
                        Ok(a) => action = Some((a, "保存设备")),
                        Err(e) => self.fail(e),
                    }
                }
                if let Some(id) = form.id {
                    if ui.button("删除设备").clicked() {
                        action = Some((UiAction::RemoveClient { id }, "删除设备"));
                    }
                }
            });
            if self.error {
                ui.colored_label(RED, &self.message);
            }
            ui.label("删除设备配置后，证书授权仍保留，可在授权页单独撤销。");
        });
        if open && !close_clicked {
            self.form = Some(form);
        }
        if let Some((action, label)) = action {
            self.send(action, label);
        }
    }
}
impl eframe::App for BridgeApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.permission_request_next_frame {
            self.permission_request_next_frame = false;
            if self
                .snapshot
                .as_ref()
                .is_some_and(|s| needs_mac_permission_setup(self.owns_daemon, self.privacy, s))
            {
                self.request_privacy();
            }
        }
        self.receive();
        if self.shutdown_confirmed {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|i| i.modifiers.command && i.key_pressed(egui::Key::Q)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let colors = self.theme.colors();
        egui::Panel::left("navigation")
            .exact_size(194.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(colors.sidebar)
                    .inner_margin(egui::Margin::symmetric(16, 20)),
            )
            .show(ui, |ui| self.sidebar(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(colors.bg_end)
                    .inner_margin(egui::Margin::symmetric(24, 18)),
            )
            .show(ui, |ui| {
                gradient(
                    ui.painter(),
                    ui.max_rect(),
                    ThemeColors {
                        start: colors.bg_start,
                        end: colors.bg_end,
                        ..colors
                    },
                    0.0,
                );
                self.header(ui);
                self.permission_banner(ui);
                if let Some(request) = self
                    .snapshot
                    .as_ref()
                    .and_then(|s| s.pair_requests.first())
                    .cloned()
                {
                    card(ui, |ui| {
                        section_title(
                            ui,
                            "新设备请求配对",
                            &format!("{} · {}", request.name, request.ip),
                        );
                        ui.label("确认允许后，将自动建立双向连接并默认开启四项共享。");
                        ui.label(RichText::new(&request.fingerprint).monospace().size(10.0));
                        ui.add_enabled_ui(!self.busy, |ui| {
                            ui.horizontal(|ui| {
                                if ui.add(primary_button(ui, "允许配对")).clicked() {
                                    self.send(
                                        UiAction::Authorize {
                                            description: request.name.clone(),
                                            fingerprint: request.fingerprint.clone(),
                                        },
                                        "允许配对",
                                    );
                                }
                                if ui.button("拒绝").clicked() {
                                    self.send(
                                        UiAction::RejectPair {
                                            fingerprint: request.fingerprint.clone(),
                                        },
                                        "拒绝配对",
                                    );
                                }
                            });
                        });
                    });
                }
                let mut scroll = egui::ScrollArea::vertical().id_salt(self.tab as u8);
                if self.reset_scroll {
                    scroll = scroll.vertical_scroll_offset(0.0);
                }
                scroll.show(ui, |ui| {
                    if self.tab == Tab::Files {
                        if let Some(action) = self.files.show(ui, self.snapshot.as_ref()) {
                            self.send(action, "保存文件配置");
                        }
                        return;
                    }
                    if let Some(snapshot) = self.snapshot.clone() {
                        match self.tab {
                            Tab::Files => unreachable!(),
                            Tab::Devices => self.devices(ui, &snapshot),
                            Tab::Settings => self.settings(ui, &snapshot),
                            Tab::Trust => self.trust(ui, &snapshot),
                            Tab::Diagnostics => self.diagnostics(ui, &snapshot),
                            Tab::Activity => {
                                page_title(ui, "ACTIVITY", "活动记录", "最近的连接和配置操作。");
                                ui.add_space(20.0);
                                card(ui, |ui| {
                                    section_title(ui, "本次窗口记录", "不记录按键内容或剪贴板文本");
                                    if self.logs.is_empty() {
                                        ui.label(RichText::new("暂无活动").color(MUTED));
                                    }
                                    for entry in self.logs.iter().rev() {
                                        egui::Frame::new()
                                            .fill(surface(ui))
                                            .corner_radius(8)
                                            .inner_margin(egui::Margin::symmetric(12, 9))
                                            .show(ui, |ui| {
                                                ui.horizontal(|ui| {
                                                    ui.colored_label(accent(ui), "●");
                                                    ui.label(entry);
                                                });
                                            });
                                    }
                                });
                            }
                        }
                    } else {
                        card(ui, |ui| {
                            section_title(ui, "等待本机后台", "正在获取当前连接状态");
                            ui.label("若持续无法连接，请确认没有运行旧版本后台。");
                        });
                    }
                });
                self.reset_scroll = false;
            });
        let ctx = ui.ctx().clone();
        if self.scan_open {
            if let Some(snapshot) = self.snapshot.clone() {
                self.scan_dialog(&ctx, &snapshot);
            }
        }
        self.form_dialog(&ctx);
        if self.confirm_shutdown {
            self.shutdown_dialog(&ctx);
        }
        if self.permission_dialog_open {
            self.permission_dialog(&ctx);
        }
    }
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.worker.stop();
    }
}
fn monitor_stand(painter: &egui::Painter, screen: egui::Rect, color: Color32) {
    let base = screen.center_bottom();
    painter.line_segment([base, base + egui::vec2(0.0, 8.0)], Stroke::new(2.0, color));
    painter.line_segment(
        [base + egui::vec2(-15.0, 8.0), base + egui::vec2(15.0, 8.0)],
        Stroke::new(2.0, color),
    );
}
fn navigation_icon(
    painter: &egui::Painter,
    center: egui::Pos2,
    tab: Tab,
    color: Color32,
    icon_background: Color32,
) {
    let stroke = Stroke::new(1.4, color);
    let p = |x, y| center + egui::vec2(x, y);
    match tab {
        Tab::Files => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(-7.0, -8.0), p(7.0, 8.0)),
                2.0,
                stroke,
                StrokeKind::Inside,
            );
            painter.line_segment([p(-4.0, -2.0), p(4.0, -2.0)], stroke);
            painter.line_segment([p(-4.0, 3.0), p(4.0, 3.0)], stroke);
        }
        Tab::Devices => {
            painter.rect_stroke(
                egui::Rect::from_min_max(p(-8.0, -6.0), p(5.0, 3.0)),
                2.0,
                stroke,
                StrokeKind::Inside,
            );
            painter.line_segment([p(-2.0, 3.0), p(-2.0, 7.0)], stroke);
            painter.line_segment([p(-6.0, 7.0), p(3.0, 7.0)], stroke);
            painter.line_segment([p(8.0, -3.0), p(8.0, 6.0)], stroke);
        }
        Tab::Settings => {
            for (y, x) in [(-6.0, -2.0), (0.0, 4.0), (6.0, -4.0)] {
                painter.line_segment([p(-8.0, y), p(8.0, y)], stroke);
                painter.circle_filled(p(x, y), 2.8, icon_background);
                painter.circle_stroke(p(x, y), 2.3, stroke);
            }
        }
        Tab::Trust => {
            painter.add(egui::Shape::closed_line(
                vec![
                    p(-7.0, -6.0),
                    p(0.0, -8.0),
                    p(7.0, -6.0),
                    p(6.0, 3.0),
                    p(0.0, 8.0),
                    p(-6.0, 3.0),
                ],
                stroke,
            ));
            painter.line_segment([p(-3.0, 0.0), p(-1.0, 2.0)], stroke);
            painter.line_segment([p(-1.0, 2.0), p(3.0, -3.0)], stroke);
        }
        Tab::Diagnostics => {
            painter.add(egui::Shape::line(
                vec![
                    p(-8.0, 1.0),
                    p(-4.0, 1.0),
                    p(-1.0, -6.0),
                    p(2.0, 6.0),
                    p(5.0, -1.0),
                    p(8.0, -1.0),
                ],
                stroke,
            ));
        }
        Tab::Activity => {
            for y in [-6.0, 0.0, 6.0] {
                painter.circle_filled(p(-7.0, y), 1.3, color);
                painter.line_segment([p(-2.0, y), p(8.0, y)], stroke);
            }
        }
    }
}
fn client_name(config: &ClientConfig) -> String {
    config
        .hostname
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            config
                .fix_ips
                .first()
                .map(ToString::to_string)
                .unwrap_or_else(|| "未命名设备".into())
        })
}
fn metric(ui: &mut egui::Ui, label: &str, value: &str, note: &str) {
    ui.spacing_mut().item_spacing.y = 3.0;
    ui.label(RichText::new(label).size(11.0).color(MUTED));
    ui.horizontal(|ui| {
        ui.label(RichText::new(value).size(20.0).strong().color(match value {
            "就绪" => GREEN,
            "待处理" => RED,
            _ => TEXT,
        }));
        ui.label(RichText::new(note).size(10.0).color(MUTED));
    });
}
fn grant(value: bool) -> &'static str {
    if value { "已允许" } else { "未允许" }
}
fn diagnostic_summary(snapshot: &UiSnapshot, local: bool) -> &'static str {
    if !snapshot.native_ready() {
        "原生键鼠后端尚未就绪，请处理下方错误后重新检测"
    } else if snapshot.paused {
        "共享已暂停，请恢复共享"
    } else if !snapshot.doctor_ready(local) {
        "没有已连接的启用设备，请检查双向授权并将鼠标移到配置边缘"
    } else if local {
        "本机原生键鼠后端就绪；仍需双机实体输入测试"
    } else {
        "原生后端和设备连接已就绪；请跨屏实测键盘、点击、滚轮及紧急释放"
    }
}
fn client_details(ui: &mut egui::Ui, config: &ClientConfig, state: &ClientState) {
    ui.label(format!(
        "启用 {} · 已连接 {} · 正在解析 {} · 按键未释放 {}",
        state.active, state.alive, state.resolving, state.has_pressed_keys
    ));
    ui.label(format!(
        "主机名：{}",
        config.hostname.as_deref().unwrap_or("未设置")
    ));
    ui.label(format!(
        "固定 IP：{:?} · DNS IP：{:?}",
        config.fix_ips, state.dns_ips
    ));
    ui.label(format!(
        "连接地址：{} · 对端构建：{}",
        state
            .active_addr
            .map(|addr| addr.to_string())
            .unwrap_or_else(|| "尚未连接".into()),
        state
            .peer_commit
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
            .unwrap_or_else(|| "未知".into())
    ));
    ui.label(format!(
        "进入命令：{}",
        config.cmd.as_deref().unwrap_or("无")
    ));
    ui.label(format!(
        "离开命令：{}",
        config.leave_cmd.as_deref().unwrap_or("无")
    ));
}
fn backend_line(
    ui: &mut egui::Ui,
    label: &str,
    status: Status,
    backend: &Option<String>,
    error: &Option<String>,
) {
    let ready = lan_mouse_ipc::native_backend_ready(status, backend.as_deref());
    let backend = backend.as_deref().unwrap_or("尚未选择");
    ui.label(
        RichText::new(format!(
            "{label}：{} · {backend}",
            if ready {
                "就绪"
            } else if backend == "dummy" {
                "虚拟后端，不能共享真实键鼠"
            } else {
                "未就绪"
            }
        ))
        .size(13.0)
        .color(if ready { GREEN } else { RED }),
    );
    if let Some(error) = error {
        ui.label(RichText::new(error).size(11.0).color(RED));
    }
}
fn configure_fonts(ctx: &egui::Context) {
    // Load installed CJK fonts rather than redistribute proprietary OS fonts.
    let paths = [
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Medium.ttc",
        "C:\\Windows\\Fonts\\msyh.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    let mut fonts = egui::FontDefinitions::default();
    for path in paths {
        if let Ok(bytes) = std::fs::read(path) {
            fonts
                .font_data
                .insert("cjk".into(), egui::FontData::from_owned(bytes).into());
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".into());
            }
            ctx.set_fonts(fonts);
            return;
        }
    }
    log::warn!("No CJK font found; install Noto Sans CJK for Chinese text");
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    #[test]
    fn theme_switch_changes_workspace_and_card_backgrounds() {
        let ctx = egui::Context::default();
        let mut previous = None;
        for theme in UiTheme::ALL {
            apply_theme(&ctx, theme);
            let colors = theme.colors();
            let visuals = &ctx.style_of(egui::Theme::Light).visuals;
            assert_eq!(visuals.panel_fill, colors.bg_start);
            assert_eq!(visuals.window_fill, colors.panel);
            assert_eq!(visuals.faint_bg_color, colors.surface);
            assert_ne!(colors.bg_start, colors.bg_end);
            if let Some(old) = previous {
                assert_ne!(old, colors.bg_start);
            }
            previous = Some(colors.bg_start);
        }
    }

    #[test]
    fn theme_defaults_to_purple_and_restores_saved_selection() {
        let path = std::env::temp_dir().join(format!(
            "lan-bridge-theme-test-{}-{:?}.json",
            std::process::id(),
            std::thread::current().id()
        ));
        assert_eq!(load_theme_from(&path), UiTheme::Purple);
        for theme in UiTheme::ALL {
            save_theme_to(&path, theme).unwrap();
            assert_eq!(load_theme_from(&path), theme);
        }
        std::fs::write(&path, b"invalid").unwrap();
        assert_eq!(load_theme_from(&path), UiTheme::Purple);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_zero_and_invalid_ports() {
        for value in ["0", "65536", "-1", "abc"] {
            assert!(port(value).is_err());
        }
        assert_eq!(port(" 4242 ").unwrap(), 4242);
    }
    #[test]
    fn validates_form_before_sending() {
        let mut form = DeviceForm::default();
        assert!(form.action().is_err());
        form.ips = "192.168.1.2, bad".into();
        assert!(form.action().is_err());
        form.ips = "192.168.1.2, 192.168.1.3".into();
        assert!(matches!(form.action().unwrap(), UiAction::AddClient { ips,.. } if ips.len() == 2));
    }
    #[test]
    fn editing_preserves_inactive_state() {
        let form = DeviceForm::client(7, &ClientConfig::default(), &ClientState::default());
        let form = DeviceForm {
            name: "peer.local".into(),
            ..form
        };
        assert!(matches!(
            form.action().unwrap(),
            UiAction::UpdateClient {
                id: 7,
                active: false,
                ..
            }
        ));
    }
    #[test]
    fn discovered_identity_goes_to_pairing() {
        let device = DiscoveredDevice {
            name: "peer".into(),
            ips: vec!["192.168.1.2".parse().unwrap()],
            port: 4444,
            fingerprint: "ab".into(),
            platform: "macos".into(),
        };
        assert!(
            matches!(DeviceForm::discovered(&device).action().unwrap(),UiAction::AddClient {port:4444,fingerprint:Some(fp),..} if fp == "ab")
        );
    }
    #[test]
    fn mdns_display_name_does_not_break_pairing() {
        let device = DiscoveredDevice {
            name: "My MacBook Pro".into(),
            ips: vec!["192.168.1.2".parse().unwrap()],
            port: 4242,
            fingerprint: "ab".into(),
            platform: "macos".into(),
        };
        assert!(
            matches!(DeviceForm::discovered(&device).action().unwrap(), UiAction::AddClient { hostname: None, ips, .. } if ips.len() == 1)
        );
    }
    #[test]
    fn hooks_are_loaded_added_and_cleared_explicitly() {
        let mut form = DeviceForm::client(
            7,
            &ClientConfig {
                cmd: Some("echo entered".into()),
                leave_cmd: Some("echo left".into()),
                ..Default::default()
            },
            &ClientState::default(),
        );
        assert!(
            matches!(form.hooks_action(), Some(UiAction::SetHooks { id: 7, enter_hook: Some(enter), leave_hook: Some(leave) }) if enter == "echo entered" && leave == "echo left")
        );
        form.enter_hook.clear();
        form.leave_hook = "   ".into();
        assert!(matches!(
            form.hooks_action(),
            Some(UiAction::SetHooks {
                enter_hook: None,
                leave_hook: None,
                ..
            })
        ));
        form.id = None;
        form.ips = "192.168.1.2".into();
        form.enter_hook = "echo entered".into();
        assert!(
            matches!(form.action().unwrap(), UiAction::AddClient { enter_hook: Some(command), .. } if command == "echo entered")
        );
    }
    fn fixture() -> (BridgeApp, mpsc::Receiver<Command>, mpsc::Sender<Reply>) {
        let (commands, requests) = mpsc::channel();
        let (results, replies) = mpsc::channel();
        let app = BridgeApp {
            logo: egui::Context::default().load_texture(
                "test-logo",
                egui::ColorImage::new([1, 1], vec![Color32::BLACK]),
                egui::TextureOptions::LINEAR,
            ),
            files: file_ui::FilePanel::default(),
            worker: Worker {
                commands,
                replies,
                thread: None,
            },
            snapshot: None,
            online: true,
            busy: false,
            message: String::new(),
            error: false,
            tab: Tab::Devices,
            theme: UiTheme::default(),
            reset_scroll: false,
            selected_client: None,
            form: None,
            scan_open: false,
            settings_port: "4242".into(),
            settings_clipboard: false,
            settings_dirty: false,
            trust_name: String::new(),
            trust_fingerprint: String::new(),
            logs: VecDeque::new(),
            privacy: privacy::PrivacyStatus {
                supported: false,
                accessibility: false,
                input_monitoring: false,
                input_control: false,
            },
            next_privacy: Instant::now() + Duration::from_secs(100),
            permission_dialog_open: false,
            permission_prompted: false,
            permission_request_next_frame: false,
            permission_retry_pending: false,
            owns_daemon: false,
            diagnostic_local: false,
            confirm_shutdown: false,
            shutdown_confirmed: false,
        };
        (app, requests, results)
    }
    pub(crate) fn snapshot() -> UiSnapshot {
        UiSnapshot {
            pair_requests: vec![],
            clipboard_target: None,
            file_directory: String::new(),
            files_error: None,
            protocol_version: lan_mouse_ipc::IPC_VERSION,
            clients: vec![(
                7,
                ClientConfig {
                    hostname: Some("peer".into()),
                    pos: Position::Right,
                    ..Default::default()
                },
                ClientState {
                    active: true,
                    ..Default::default()
                },
            )],
            fingerprint: String::new(),
            authorized: Default::default(),
            port: 4242,
            clipboard: false,
            clipboard_supported: true,
            paused: false,
            active_client: None,
            capture: Status::Disabled,
            capture_backend: None,
            emulation_backend: None,
            capture_error: None,
            emulation_error: None,
            emulation: Status::Disabled,
            release_bind: vec![],
            platform: "test".into(),
            config_path: "test.toml".into(),
            connection_attempts: vec![],
            discovered: vec![],
            discovery_error: None,
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn permission_grant_waits_for_busy_action_and_retries_once() {
        let (mut app, requests, _) = fixture();
        app.owns_daemon = true;
        app.busy = true;
        let mut state = snapshot();
        state.capture_error = Some("accessibility permission is required".into());
        app.snapshot = Some(state);
        let granted = privacy::PrivacyStatus {
            supported: true,
            accessibility: true,
            input_monitoring: true,
            input_control: true,
        };
        app.update_privacy(granted);
        app.retry_after_permission_change();
        assert!(app.permission_retry_pending);
        assert!(requests.try_recv().is_err());

        app.update_privacy(granted);
        app.busy = false;
        app.retry_after_permission_change();
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::Action(UiAction::RetryBackends, _)
        ));
        app.busy = false;
        app.retry_after_permission_change();
        assert!(requests.try_recv().is_err());

        app.owns_daemon = false;
        app.permission_retry_pending = true;
        app.retry_after_permission_change();
        assert!(requests.try_recv().is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_permission_failure_opens_onboarding_once() {
        let (mut app, _, results) = fixture();
        app.owns_daemon = true;
        app.privacy = privacy::PrivacyStatus {
            supported: true,
            accessibility: false,
            input_monitoring: false,
            input_control: false,
        };
        let mut state = snapshot();
        state.capture_error = Some("accessibility permission is required".into());
        results
            .send(Reply {
                label: None,
                result: Ok(state.clone()),
            })
            .unwrap();
        app.receive();
        assert!(app.permission_dialog_open);
        assert!(app.permission_request_next_frame);
        assert!(app.permission_prompted);

        app.permission_dialog_open = false;
        app.permission_request_next_frame = false;
        app.receive();
        assert!(
            !app.permission_dialog_open,
            "polling must not reopen the dialog"
        );

        state.capture_backend = Some("dummy".into());
        state.emulation_backend = Some("dummy".into());
        assert!(!needs_mac_permission_setup(true, app.privacy, &state));
        state.capture_backend = None;
        assert!(!needs_mac_permission_setup(false, app.privacy, &state));
        state.capture_error = Some("unknown backend".into());
        assert!(!needs_mac_permission_setup(true, app.privacy, &state));
        state.capture_error = Some("accessibility permission is required".into());
        let granted = privacy::PrivacyStatus {
            supported: true,
            accessibility: true,
            input_monitoring: true,
            input_control: true,
        };
        assert!(needs_mac_permission_setup(true, granted, &state));
    }
    #[test]
    fn polls_preserve_unsaved_forms_and_settings() {
        let (mut app, _, results) = fixture();
        app.form = Some(DeviceForm {
            name: "unsaved".into(),
            ..Default::default()
        });
        app.settings_port = "9999".into();
        app.settings_dirty = true;
        results
            .send(Reply {
                label: None,
                result: Ok(snapshot()),
            })
            .unwrap();
        app.receive();
        assert_eq!(app.form.as_ref().unwrap().name, "unsaved");
        assert_eq!(app.settings_port, "9999");
        results
            .send(Reply {
                label: Some("保存设置"),
                result: Ok(snapshot()),
            })
            .unwrap();
        app.receive();
        assert_eq!(app.settings_port, "4242");
        assert!(!app.settings_dirty);
    }
    #[test]
    fn failed_save_preserves_drafts_and_never_claims_confirmation() {
        let (mut app, _, results) = fixture();
        app.form = Some(DeviceForm {
            name: "draft".into(),
            ..Default::default()
        });
        app.busy = true;
        results
            .send(Reply {
                label: Some("保存设备"),
                result: Err("配置保存失败".into()),
            })
            .unwrap();
        app.receive();
        assert!(!app.busy && app.error);
        assert_eq!(app.form.unwrap().name, "draft");
        assert_eq!(app.message, "配置保存失败");
    }
    #[test]
    fn diagnosis_agrees_with_cli_and_requires_an_identified_native_backend() {
        let mut s = snapshot();
        s.capture = Status::Enabled;
        s.emulation = Status::Enabled;
        assert!(!s.doctor_ready(true));
        s.capture_backend = Some("dummy".into());
        s.emulation_backend = Some("dummy".into());
        assert!(!s.doctor_ready(true));
        s.capture_backend = Some("MacOS".into());
        s.emulation_backend = Some("macos".into());
        assert!(s.doctor_ready(true));
        assert!(!s.doctor_ready(false));
        s.clients[0].2.alive = true;
        assert!(s.doctor_ready(false));
        s.clients[0].2.active = false;
        assert!(!s.doctor_ready(false));
        s.paused = true;
        assert!(!s.doctor_ready(true));
    }
    #[test]
    fn retry_result_retains_actual_backend_failure() {
        let (mut app, _, results) = fixture();
        let mut s = snapshot();
        s.capture_error = Some("permission required".into());
        results
            .send(Reply {
                label: Some("重新检测后端"),
                result: Ok(s),
            })
            .unwrap();
        app.receive();
        assert!(app.error);
        assert!(app.message.contains("仍未就绪"));
        assert_eq!(
            app.snapshot.unwrap().capture_error.as_deref(),
            Some("permission required")
        );
    }
    #[test]
    fn shutdown_closes_only_after_confirmation() {
        let (mut app, _, results) = fixture();
        results
            .send(Reply {
                label: Some("退出后台"),
                result: Err("release failed".into()),
            })
            .unwrap();
        app.receive();
        assert!(!app.shutdown_confirmed);
        results
            .send(Reply {
                label: Some("退出后台"),
                result: Ok(snapshot()),
            })
            .unwrap();
        app.receive();
        assert!(app.shutdown_confirmed && !app.online);
    }
    #[test]
    fn dragging_a_screen_requests_confirmed_position_change() {
        let (mut app, requests, _) = fixture();
        let snapshot = snapshot();
        let ctx = egui::Context::default();
        let mut frame = |events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1160.0, 800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| app.layout(ui, &snapshot),
            );
            output.textures_delta.clear();
        };
        let source = egui::pos2(750.0, 155.0);
        let target = egui::pos2(400.0, 155.0);
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(vec![]);
        frame(vec![
            egui::Event::PointerMoved(source),
            button(source, true),
        ]);
        frame(vec![egui::Event::PointerMoved(egui::pos2(735.0, 155.0))]);
        frame(vec![egui::Event::PointerMoved(target)]);
        frame(vec![button(target, false)]);
        assert!(matches!(
            requests.try_recv().unwrap(),
            Command::Action(
                UiAction::SetPosition {
                    id: 7,
                    position: Position::Left
                },
                _
            )
        ));
    }
}
