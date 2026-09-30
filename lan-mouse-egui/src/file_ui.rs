// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use super::{GREEN, MUTED, RED, card, page_title, primary_button, section_title};
use eframe::egui::{self, RichText};
use lan_mouse_ipc::UiSnapshot;
use serde_json::Value;
use std::{
    collections::VecDeque,
    io::{self, BufRead, BufReader},
    net::{IpAddr, SocketAddr},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

enum Event {
    Json(Value),
    Text(String),
    Closed,
}
struct Job {
    receiving: bool,
    child: Child,
    events: mpsc::Receiver<Event>,
    closed: usize,
    exited: Option<bool>,
    status: String,
    error: bool,
    ratio: f32,
    output: Option<PathBuf>,
    logs: VecDeque<String>,
}
impl Job {
    fn start(mut command: Command, receiving: bool) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (sender, events) = mpsc::channel();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("文件进程输出未连接"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("文件进程错误输出未连接"))?;
        let output = sender.clone();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let event = serde_json::from_str(&line)
                    .map(Event::Json)
                    .unwrap_or(Event::Text(line));
                if output.send(event).is_err() {
                    return;
                }
            }
            let _ = output.send(Event::Closed);
        });
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if sender.send(Event::Text(line)).is_err() {
                    return;
                }
            }
            let _ = sender.send(Event::Closed);
        });
        Ok(Self {
            receiving,
            child,
            events,
            closed: 0,
            exited: None,
            status: "正在启动…".into(),
            error: false,
            ratio: 0.0,
            output: None,
            logs: VecDeque::new(),
        })
    }
    fn running(&self) -> bool {
        self.exited.is_none() || self.closed < 2
    }
    fn cancel(&mut self) {
        if self.exited.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.exited = Some(false);
        }
        self.status = "已停止；重新发送同一批文件可续传".into();
    }
    fn log(&mut self, text: String) {
        if self.logs.len() == 30 {
            self.logs.pop_front();
        }
        self.logs.push_back(text);
    }
    fn poll(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Closed => self.closed += 1,
                Event::Text(text) => self.log(text),
                Event::Json(value) => match value["event"].as_str() {
                    Some("preparing") => self.status = "接收端正在检查续传内容…".into(),
                    Some("listening") => {
                        self.status =
                            format!("接收已开启 · {}", value["address"].as_str().unwrap_or(""))
                    }
                    Some("progress") => {
                        let bytes = value["network_bytes"].as_u64().unwrap_or(0);
                        let total = value["network_total"].as_u64().unwrap_or(1).max(1);
                        self.ratio = (bytes as f64 / total as f64).clamp(0.0, 1.0) as f32;
                        self.status = format!(
                            "{:.1} / {:.1} MiB · {:.1} MiB/s",
                            bytes as f64 / 1048576.0,
                            total as f64 / 1048576.0,
                            value["mib_per_second"].as_f64().unwrap_or(0.0)
                        );
                    }
                    Some("completed") => {
                        self.ratio = 1.0;
                        self.output = value["output"].as_str().map(PathBuf::from);
                        self.status = format!(
                            "完成并校验 · {:.1} MiB/s · 续传 {:.1} MiB",
                            value["mib_per_second"].as_f64().unwrap_or(0.0),
                            value["resumed_bytes"].as_u64().unwrap_or(0) as f64 / 1048576.0
                        );
                    }
                    Some("clipboard_error") => self.log(format!("剪贴板：{}", value["error"])),
                    Some("clipboard_ready") => self.log("文件已放入剪贴板，可以粘贴".into()),
                    Some("clipboard_skipped") => {
                        self.log("文件已保存；保留新复制的剪贴板内容".into())
                    }
                    Some("watching") => self.status = "自动复制已开启 · 等待下一次复制文件".into(),
                    Some("auto_sending") => {
                        self.ratio = 0.0;
                        self.status = "检测到文件复制，正在自动发送…".into();
                    }
                    Some("auto_retry") => {
                        self.status = "自动发送失败，等待重试；请检查对端是否开启接收".into();
                        self.log(value["error"].as_str().unwrap_or("").into());
                    }
                    Some("auto_sent") => self.log(format!(
                        "已自动送达并校验 · {} 个文件 · {:.1} MiB/s",
                        value["files"],
                        value["mib_per_second"].as_f64().unwrap_or(0.0)
                    )),
                    _ => {}
                },
            }
        }
        if self.exited.is_none() {
            match self.child.try_wait() {
                Ok(Some(status)) => self.exited = Some(status.success()),
                Err(error) => {
                    self.exited = Some(false);
                    self.log(error.to_string());
                }
                Ok(None) => {}
            }
        }
        if self.exited == Some(false) && self.closed == 2 && !self.status.starts_with("已停止") {
            self.error = true;
            self.status = self
                .logs
                .back()
                .cloned()
                .unwrap_or_else(|| "文件任务失败，请检查对端接收状态".into());
        }
    }
    fn show(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(if self.error { RED } else { GREEN }, &self.status);
        if self.ratio > 0.0 {
            ui.add(egui::ProgressBar::new(self.ratio).show_percentage());
        }
        if self.running() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
            if ui.button("停止任务").clicked() {
                self.cancel();
            }
        }
        if let Some(path) = &self.output {
            ui.label(format!(
                "{}保存目录：{}",
                if self.receiving { "本机" } else { "对端" },
                path.display()
            ));
            if self.receiving && ui.button("打开保存目录").clicked() {
                let mut command = Command::new(if cfg!(target_os = "macos") {
                    "open"
                } else if cfg!(windows) {
                    "explorer"
                } else {
                    "xdg-open"
                });
                let _ = command.arg(path).spawn();
            }
        }
        ui.collapsing("任务日志", |ui| {
            for line in &self.logs {
                ui.label(line);
            }
        });
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        if self.exited.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

pub(super) struct FilePanel {
    target: String,
    port: String,
    fingerprint: String,
    paths: String,
    output: String,
    full_speed: bool,
    clipboard: bool,
    after_receive_clipboard: bool,
    allow_benchmark: bool,
    sender: Option<Job>,
    receiver: Option<Job>,
    sync: Option<Job>,
    error: Option<String>,
}
impl Default for FilePanel {
    fn default() -> Self {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_default();
        Self {
            target: String::new(),
            port: "4243".into(),
            fingerprint: String::new(),
            paths: String::new(),
            output: PathBuf::from(home)
                .join("Downloads")
                .join("LAN Bridge")
                .display()
                .to_string(),
            full_speed: false,
            clipboard: false,
            after_receive_clipboard: false,
            allow_benchmark: false,
            sender: None,
            receiver: None,
            sync: None,
            error: None,
        }
    }
}

fn command(snapshot: Option<&UiSnapshot>) -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    if let Some(snapshot) = snapshot {
        command.args(["--config", &snapshot.config_path]);
    }
    // Match explicit identity overrides used by this GUI and its daemon.
    let args = std::env::args().collect::<Vec<_>>();
    for pair in args.windows(2) {
        if pair[0] == "--cert-path" || (pair[0] == "--config" && snapshot.is_none()) {
            command.args(pair);
        }
    }
    command.args(["files", "--json"]);
    Ok(command)
}

impl FilePanel {
    pub fn poll(&mut self) {
        if let Some(job) = &mut self.sender {
            job.poll();
        }
        if let Some(job) = &mut self.receiver {
            job.poll();
        }
        if let Some(job) = &mut self.sync {
            job.poll();
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: Option<&UiSnapshot>) {
        page_title(
            ui,
            "FILES",
            "文件传输",
            "已配对设备间加密直传，支持文件夹、完整性校验与断点续传。",
        );
        if !cfg!(any(target_os = "macos", windows)) {
            ui.label("文件传输当前支持 macOS 和 Windows。");
            return;
        }
        ui.add_space(16.0);
        let sync_running = self.sync.as_ref().is_some_and(Job::running);
        card(ui, |ui| {
            section_title(
                ui,
                "自动文件复制粘贴",
                "两端各开启一次；之后复制文件会自动传送，完成后在对端粘贴",
            );
            let other_running = self.receiver.as_ref().is_some_and(Job::running)
                || self.sender.as_ref().is_some_and(Job::running);
            ui.add_enabled_ui(!sync_running && !other_running, |ui| {
                ui.horizontal(|ui| {
                    ui.label("对端 IP");
                    ui.text_edit_singleline(&mut self.target);
                    ui.label("文件端口");
                    ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
                });
                ui.horizontal(|ui| {
                    ui.label("接收目录");
                    ui.text_edit_singleline(&mut self.output);
                });
                if ui.add(primary_button(ui, "开启自动复制粘贴")).clicked() {
                    let result = self
                        .sync_command(snapshot)
                        .and_then(|command| Job::start(command, true));
                    match result {
                        Ok(job) => {
                            self.sync = Some(job);
                            self.error = None;
                        }
                        Err(error) => self.error = Some(error.to_string()),
                    }
                }
            });
            ui.label(RichText::new("只发送开启之后新复制的文件；大文件需等传输完成再粘贴。复制其他内容会取消尚未完成的自动发送。").color(MUTED).size(12.0));
            if other_running {
                ui.label("先停止手动发送/接收任务，再开启自动模式。");
            }
            if let Some(job) = &mut self.sync {
                job.show(ui);
            }
        });
        ui.add_space(16.0);
        ui.add_enabled_ui(!sync_running, |ui| {
            card(ui, |ui| {
                section_title(ui, "接收文件", "明确开启后，已授权设备才能向此目录发送文件");
                let running = self.receiver.as_ref().is_some_and(Job::running);
                ui.add_enabled_ui(!running, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("保存到");
                        ui.text_edit_singleline(&mut self.output);
                    });
                    ui.checkbox(
                        &mut self.after_receive_clipboard,
                        "完成后放入文件剪贴板，可在 Finder / 资源管理器粘贴",
                    );
                    ui.checkbox(
                        &mut self.allow_benchmark,
                        "允许已配对设备测速（不保存文件）",
                    );
                    if ui.add(primary_button(ui, "开启接收 · TCP 4243")).clicked() {
                        let result = command(snapshot).and_then(|mut command| {
                            if self.output.trim().is_empty() {
                                return Err(io::Error::other("请选择接收目录"));
                            }
                            command.args(["receive", "--output", self.output.trim()]);
                            if self.after_receive_clipboard {
                                command.arg("--clipboard");
                            }
                            if self.allow_benchmark {
                                command.arg("--allow-benchmark");
                            }
                            Job::start(command, true)
                        });
                        match result {
                            Ok(job) => {
                                self.receiver = Some(job);
                                self.error = None;
                            }
                            Err(error) => self.error = Some(error.to_string()),
                        }
                    }
                });
                if let Some(job) = &mut self.receiver {
                    job.show(ui);
                }
            });
            ui.add_space(16.0);
            card(ui, |ui| {
                section_title(
                    ui,
                    "发送与测速",
                    "在另一台先开启文件接收，文件端口独立于键鼠端口",
                );
                let running = self.sender.as_ref().is_some_and(Job::running);
                ui.add_enabled_ui(!running, |ui| {
                    if let Some(snapshot) = snapshot {
                        ui.horizontal_wrapped(|ui| {
                            ui.label("已配置设备");
                            for (_, config, state) in &snapshot.clients {
                                let ip = state
                                    .active_addr
                                    .map(|a| a.ip())
                                    .or_else(|| config.fix_ips.first().copied());
                                if let Some(ip) = ip {
                                    if ui.button(ip.to_string()).clicked() {
                                        self.target = ip.to_string();
                                        self.fingerprint = snapshot
                                            .discovered
                                            .iter()
                                            .find(|d| {
                                                d.ips.contains(&ip)
                                                    && snapshot
                                                        .authorized
                                                        .contains_key(&d.fingerprint)
                                            })
                                            .map(|d| d.fingerprint.clone())
                                            .unwrap_or_default();
                                    }
                                }
                            }
                        });
                    }
                    ui.horizontal(|ui| {
                        ui.label("对端 IP");
                        ui.text_edit_singleline(&mut self.target);
                        ui.label("文件端口");
                        ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
                    });
                    ui.collapsing("核对对端指纹", |ui| {
                        ui.label("仅连接已授权设备；填写完整指纹可进一步锁定目标。");
                        ui.text_edit_singleline(&mut self.fingerprint);
                    });
                    ui.checkbox(
                        &mut self.clipboard,
                        "发送 Finder / 资源管理器中已复制的文件",
                    );
                    if !self.clipboard {
                        ui.label("将文件或文件夹拖入此页，或每行输入一个完整路径");
                        let dropped = ui.ctx().input(|i| {
                            i.raw
                                .dropped_files
                                .iter()
                                .map(|f| f.path().to_path_buf())
                                .collect::<Vec<_>>()
                        });
                        for path in dropped {
                            if !self.paths.is_empty() {
                                self.paths.push('\n');
                            }
                            self.paths.push_str(&path.display().to_string());
                        }
                        ui.add(
                            egui::TextEdit::multiline(&mut self.paths)
                                .desired_rows(3)
                                .desired_width(f32::INFINITY),
                        );
                    }
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut self.full_speed, false, "自动 · 延迟升高时降速");
                        ui.radio_value(&mut self.full_speed, true, "全速");
                    });
                    ui.horizontal(|ui| {
                        if ui.add(primary_button(ui, "开始发送")).clicked() {
                            self.start(snapshot, false);
                        }
                        if ui.button("测速 · 64 MiB").clicked() {
                            self.start(snapshot, true);
                        }
                    });
                });
                if let Some(job) = &mut self.sender {
                    job.show(ui);
                }
            });
        });
        ui.add_space(10.0);
        ui.label(RichText::new("文件保存在独立批次目录，已有文件不覆盖。自动模式无需点击发送；手动传输也继续可用。").color(MUTED).size(12.0));
        if let Some(error) = &self.error {
            ui.colored_label(RED, error);
        }
    }
    fn sync_command(&self, snapshot: Option<&UiSnapshot>) -> io::Result<Command> {
        let ip: IpAddr = self
            .target
            .trim()
            .parse()
            .map_err(|_| io::Error::other("请先填写自动传送的对端 IP"))?;
        let port: u16 = self
            .port
            .trim()
            .parse()
            .map_err(|_| io::Error::other("文件端口无效"))?;
        if port == 0 || self.output.trim().is_empty() {
            return Err(io::Error::other("文件端口和接收目录无效"));
        }
        let mut command = command(snapshot)?;
        command.args([
            "sync",
            "--to",
            &SocketAddr::new(ip, port).to_string(),
            "--output",
            self.output.trim(),
            "--mode",
            if self.full_speed { "full" } else { "auto" },
        ]);
        if !self.fingerprint.trim().is_empty() {
            command.args(["--fingerprint", self.fingerprint.trim()]);
        }
        Ok(command)
    }
    fn start(&mut self, snapshot: Option<&UiSnapshot>, benchmark: bool) {
        let result = (|| {
            let ip: IpAddr = self
                .target
                .trim()
                .parse()
                .map_err(|_| io::Error::other("对端 IP 地址无效"))?;
            let port: u16 = self
                .port
                .trim()
                .parse()
                .map_err(|_| io::Error::other("文件端口无效"))?;
            if port == 0 {
                return Err(io::Error::other("文件端口不能为 0"));
            }
            let mut command = command(snapshot)?;
            command
                .arg(if benchmark { "benchmark" } else { "send" })
                .args([
                    "--to",
                    &SocketAddr::new(ip, port).to_string(),
                    "--mode",
                    if self.full_speed { "full" } else { "auto" },
                ]);
            if !self.fingerprint.trim().is_empty() {
                command.args(["--fingerprint", self.fingerprint.trim()]);
            }
            if !benchmark {
                if self.clipboard {
                    command.arg("--clipboard");
                } else {
                    let paths = self
                        .paths
                        .lines()
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .collect::<Vec<_>>();
                    if paths.is_empty() {
                        return Err(io::Error::other("请拖入文件或填写路径"));
                    }
                    command.arg("--").args(paths);
                }
            }
            Job::start(command, false)
        })();
        match result {
            Ok(job) => {
                self.sender = Some(job);
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }
}
