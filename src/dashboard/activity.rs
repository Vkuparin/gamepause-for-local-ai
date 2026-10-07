//! The Activity page and the short-lived result toast. Both are bounded: the
//! log keeps a fixed number of entries and reads only the tail of the worker log.

use super::{Dashboard, Page, render_verify_report};
use crate::{
    app::Shared,
    commands::Outcome,
    dashboard_theme::{self as design, Palette},
    tray,
};
use eframe::egui::*;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};
pub(super) struct ActivityLog {
    pub(super) entries: VecDeque<String>,
    pub(super) last: String,
}
impl ActivityLog {
    pub(super) fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            last: String::new(),
        }
    }
    pub(super) fn record(&mut self, detail: String) {
        if self.last == detail {
            return;
        }
        self.last = detail.clone();
        let mut local: windows_sys::Win32::Foundation::SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe {
            windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut local);
        }
        let detail = detail.chars().take(8000).collect::<String>();
        self.entries.push_front(format!(
            "{:02}:{:02}:{:02}  {}",
            local.wHour, local.wMinute, local.wSecond, detail
        ));
        self.entries.truncate(160);
    }
}
pub(super) struct Toast {
    pub(super) key: Option<(u64, Outcome, String)>,
    pub(super) since: Instant,
}
impl Toast {
    pub(super) fn new() -> Self {
        Self {
            key: None,
            since: Instant::now(),
        }
    }
    pub(super) fn message<'a>(&mut self, s: &'a Shared, now: Instant) -> Option<(&'a str, bool)> {
        if !s.settings_error.is_empty() {
            return Some((&s.settings_error, true));
        }
        let result = s.commands.latest.as_ref()?;
        let key = (result.id, result.outcome, result.message.clone());
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.since = now;
        }
        let persistent = matches!(
            result.outcome,
            Outcome::Failed | Outcome::Requested | Outcome::Working
        );
        (persistent || now.duration_since(self.since) < Duration::from_secs(5))
            .then_some((&result.message, result.outcome == Outcome::Failed))
    }
}
pub(super) fn read_worker_log(folder: &std::path::Path) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(crate::app::worker_log_path(folder)?)?;
    let len = file.metadata()?.len();
    let skipped = len.saturating_sub(65536);
    file.seek(SeekFrom::Start(skipped))?;
    let mut bytes = Vec::new();
    file.take(65536).read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(if skipped > 0 {
        text.split_once('\n').map_or("", |(_, tail)| tail).into()
    } else {
        text.into_owned()
    })
}
impl Dashboard {
    pub(super) fn activity(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        ui.horizontal(|ui| {
            ui.heading("Activity");
            if ui.button("Back to games").clicked() {
                self.set_page(Page::Games);
            }
            if ui.button("Open logs folder").clicked() {
                tray::request_folder(&self.shared, &self.folder.join("logs"));
            }
            if ui.button("Load recent worker log").clicked() {
                self.worker_log = Some(
                    read_worker_log(&self.folder)
                        .unwrap_or_else(|error| format!("Could not read worker log: {error}")),
                );
            }
        });
        ui.colored_label(p.muted,"Recent observed status changes while this dashboard is open. Detailed worker logs remain in the data folder.");
        ui.colored_label(
            p.muted,
            format!(
                "Running GamePause {} (PID {}). Logs: {}",
                env!("CARGO_PKG_VERSION"),
                std::process::id(),
                self.folder.display()
            ),
        );
        if let Some(error) = crate::app::log_error(&self.folder) {
            ui.colored_label(p.error, error);
        }
        p.card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Current state");
            let summary = crate::presentation::summarize(s);
            ui.label(&summary.games);
            ui.label(summary.ai_text());
            if let Some(result) = &s.commands.latest {
                ui.label(format!(
                    "Command #{}: {:?}\n{}",
                    result.id, result.outcome, result.message
                ));
            }
            if let Some(feedback) = &s.restore_feedback {
                ui.label(feedback.text());
            }
            if let Some(report) = &s.verify_report {
                ui.label(render_verify_report(report));
            }
            for (source, error) in &s.discovery_errors {
                ui.colored_label(p.error, format!("{source}: {error}"));
            }
        });
        ui.add_space(design::GAP);
        p.card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Recent events");
            for entry in &self.log.entries {
                ui.separator();
                ui.add(Label::new(entry).wrap());
            }
        });
        if let Some(text) = self.worker_log.as_mut() {
            ui.add_space(design::GAP);
            p.card().show(ui, |ui| {
                ui.strong("Recent worker log");
                ui.colored_label(
                    p.muted,
                    "Loaded on request. Up to 64 KiB from the current local log.",
                );
                ui.add(
                    TextEdit::multiline(text)
                        .desired_width(f32::INFINITY)
                        .desired_rows(12)
                        .interactive(false),
                );
            });
        }
    }
}
