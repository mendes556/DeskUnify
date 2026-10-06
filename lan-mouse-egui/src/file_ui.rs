// DeskUnify changes, 2026-10-06; derived from Lan Mouse, GPL-3.0-or-later.
use super::{MUTED, RED, card, page_title, primary_button, section_title};
use eframe::egui::{self, RichText};
use lan_mouse_ipc::{UiAction, UiSnapshot};
#[derive(Default)]
pub(super) struct FilePanel {
    directory: String,
    saved: String,
}
impl FilePanel {
    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: Option<&UiSnapshot>) -> Option<UiAction> {
        page_title(
            ui,
            "FILES",
            "文件复制粘贴",
            "在 Finder / 文件资源管理器复制文件，传输完成后到目标设备粘贴。",
        );
        let Some(snapshot) = snapshot else {
            ui.label("等待本机后台…");
            return None;
        };
        if self.saved != snapshot.file_directory {
            self.saved = snapshot.file_directory.clone();
            self.directory = self.saved.clone();
        }
        let mut action = None;
        card(ui, |ui| {
            section_title(ui, "自动共享", "由后台运行，关闭窗口后仍然有效");
            let target = snapshot
                .clients
                .iter()
                .find(|(id, _, _)| Some(*id) == snapshot.clipboard_target);
            if snapshot.paused {
                ui.label("共享已暂停");
            } else if let Some((_, config, state)) = target {
                ui.label(format!("当前目标：{}", super::client_name(config)));
                let enabled = state.active
                    && config.sharing.files
                    && state.peer_sharing.is_some_and(|p| p.files)
                    && state.sharing_error.is_none();
                ui.label(if enabled {
                    "已开启 · 复制文件即可发送"
                } else {
                    "暂不可用 · 请检查设备的文件共享设置和连接状态"
                });
            } else {
                ui.label("尚未选择目标 · 将鼠标移动到已配对设备后，复制粘贴会跟随该设备");
            }
            ui.label(RichText::new("回到本机后沿用最近目标；传输途中切换设备不会更换收件人。暂停、停用设备或关闭文件共享会停止对应传输。").color(MUTED));
            if let Some(error) = &snapshot.files_error {
                ui.colored_label(RED, error);
            }
        });
        ui.add_space(16.0);
        card(ui, |ui| {
            section_title(
                ui,
                "接收目录",
                "文件校验完成后放入本机剪贴板，已有文件不会被覆盖",
            );
            ui.text_edit_singleline(&mut self.directory);
            if ui.add(primary_button(ui, "保存接收目录")).clicked() {
                action = Some(UiAction::SetFileDirectory {
                    directory: self.directory.trim().into(),
                });
            }
            if ui.button("打开接收目录").clicked() {
                let _ = std::process::Command::new(if cfg!(target_os = "macos") {
                    "open"
                } else if cfg!(windows) {
                    "explorer"
                } else {
                    "xdg-open"
                })
                .arg(&snapshot.file_directory)
                .spawn();
            }
        });
        action
    }
}
