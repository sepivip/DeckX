use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

use anyhow::Result;
use eframe::egui::{self, Color32, Context, RichText, Ui};

use super::DeckX;
use crate::{
    mcp::{
        self,
        codex_setup::{self, Registration},
    },
    theme,
};

const GUIDE: &str =
    "https://learn.chatgpt.com/docs/extend/mcp?surface=cli#configure-in-the-chatgpt-desktop-app";
const EXAMPLE: &str = "Show my DeckX setup.";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    General,
    Mcp,
}

#[derive(Default)]
pub(super) struct State {
    pub(super) server_error: Option<String>,
    pub(super) last_request: Option<Instant>,
    registration: Option<Result<Registration, String>>,
    registration_job: Option<Receiver<Result<Registration, String>>>,
    test_job: Option<Receiver<Result<(), String>>>,
    feedback: Option<(String, Color32)>,
}

impl State {
    #[cfg(test)]
    pub(super) fn setup_fixture() -> Self {
        Self {
            registration: Some(Ok(Registration::Missing)),
            ..Default::default()
        }
    }

    pub(super) fn refresh_registration(&mut self) {
        self.registration = None;
    }

    fn check_registration(&mut self, ctx: &Context, add: bool) {
        let job = if add {
            codex_setup::add
        } else {
            codex_setup::inspect
        };
        match background(ctx, job) {
            Ok(receiver) => self.registration_job = Some(receiver),
            Err(error) => self.registration = Some(Err(format!("{error:#}"))),
        }
    }

    fn poll(&mut self) {
        if let Some(result) = completed(&mut self.registration_job) {
            self.registration = Some(result);
        }
        if let Some(result) = completed(&mut self.test_job) {
            self.feedback = Some(match result {
                Ok(()) => ("DeckX answered a local MCP request.".into(), theme::MINT),
                Err(error) => (format!("Connection test failed. {error}"), theme::ERROR),
            });
        }
    }

    fn registration_status(&self) -> (&str, Color32) {
        if self.registration_job.is_some() {
            return ("Checking…", theme::MUTED);
        }
        match self.registration.as_ref() {
            Some(Ok(Registration::Configured)) => ("Configured", theme::MINT),
            Some(Ok(Registration::Missing)) => ("Not configured", theme::MUTED),
            Some(Ok(Registration::Disabled)) => ("Disabled in Codex", theme::GOLD),
            Some(Ok(Registration::Different)) => ("Different command", theme::GOLD),
            Some(Err(_)) => ("Check unavailable", theme::GOLD),
            None => ("Checking…", theme::MUTED),
        }
    }
}

fn background<T: Send + 'static>(
    ctx: &Context,
    work: fn() -> Result<T>,
) -> Result<Receiver<Result<T, String>>> {
    let (send, receive) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::Builder::new()
        .name("DeckX MCP settings".into())
        .spawn(move || {
            let _ = send.send(work().map_err(|error| format!("{error:#}")));
            ctx.request_repaint();
        })?;
    Ok(receive)
}

fn completed<T>(job: &mut Option<Receiver<Result<T, String>>>) -> Option<Result<T, String>> {
    let result = match job.as_ref()?.try_recv() {
        Ok(result) => result,
        Err(TryRecvError::Empty) => return None,
        Err(TryRecvError::Disconnected) => Err("The check ended unexpectedly.".into()),
    };
    *job = None;
    Some(result)
}

impl DeckX {
    pub(super) fn start_saved_mcp(&mut self, ctx: &Context) {
        match mcp::enabled().and_then(|enabled| {
            enabled
                .then(|| mcp::AppEndpoint::start(ctx.clone()))
                .transpose()
        }) {
            Ok(endpoint) => self.mcp = endpoint,
            Err(error) => self.mcp_settings.server_error = Some(format!("{error:#}")),
        }
    }

    fn set_mcp_enabled(&mut self, ctx: &Context, enabled: bool) -> Result<()> {
        let endpoint = enabled
            .then(|| mcp::AppEndpoint::start(ctx.clone()))
            .transpose()?;
        mcp::save_enabled(enabled)?;
        self.mcp = endpoint;
        self.mcp_settings.last_request = None;
        Ok(())
    }

    pub(super) fn show_mcp_settings(&mut self, ui: &mut Ui) {
        self.mcp_settings.poll();
        if self.mcp_settings.registration.is_none() && self.mcp_settings.registration_job.is_none()
        {
            self.mcp_settings.check_registration(ui.ctx(), false);
        }
        theme::dialog_title(ui, "DeckX MCP");
        ui.label(
            RichText::new("Let Codex configure your deck’s profiles, keys, dials, and plugins.")
                .color(theme::MUTED),
        );
        ui.add_space(theme::GAP);

        let running = self.mcp.is_some();
        let (server_label, server_color) = match (running, self.mcp_settings.server_error.is_some())
        {
            (true, _) => ("Running", theme::MINT),
            (false, true) => ("Unavailable", theme::ERROR),
            (false, false) => ("Stopped", theme::MUTED),
        };
        let mut change_server = false;
        let mut test = false;
        let mut add = false;
        theme::card(theme::WELL, theme::SECTION_PADDING).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::setting_row(ui, "Connection", |ui| {
                let testing = self.mcp_settings.test_job.is_some();
                test = theme::text_button(
                    ui,
                    if testing {
                        "Testing…"
                    } else {
                        "Test connection"
                    },
                    108.0,
                    false,
                    running && !testing,
                )
                .clicked();
            });
            ui.separator();
            theme::setting_row(ui, "Local server", |ui| {
                change_server = theme::text_button(
                    ui,
                    if running {
                        "Stop server"
                    } else {
                        "Start server"
                    },
                    91.0,
                    false,
                    self.mcp_settings.test_job.is_none(),
                )
                .clicked();
                theme::status(ui, server_label, server_color);
            });
            theme::setting_row(ui, "Codex setup", |ui| {
                if self.mcp_settings.registration_job.is_none()
                    && matches!(
                        self.mcp_settings.registration,
                        Some(Ok(Registration::Missing))
                    )
                {
                    add = theme::action_button(ui, "Add to Codex", true, true).clicked();
                }
                let (label, color) = self.mcp_settings.registration_status();
                theme::status(ui, label, color);
            });
            theme::setting_row(ui, "Last client request", |ui| {
                let label = self
                    .mcp_settings
                    .last_request
                    .map_or_else(|| "No requests yet".into(), request_age);
                ui.label(RichText::new(label).color(theme::MUTED));
            });
        });
        if self.mcp_settings.last_request.is_some() {
            ui.ctx().request_repaint_after(Duration::from_secs(15));
        }
        if change_server {
            self.mcp_settings.feedback = None;
            self.mcp_settings.server_error = self
                .set_mcp_enabled(ui.ctx(), !running)
                .err()
                .map(|error| format!("{error:#}"));
            ui.ctx().request_repaint();
        }
        if add {
            self.mcp_settings.check_registration(ui.ctx(), true);
        }
        if test {
            self.mcp_settings.feedback = None;
            if self.mcp_settings.registration_job.is_none() {
                self.mcp_settings.check_registration(ui.ctx(), false);
            }
            match background(ui.ctx(), mcp::test_connection) {
                Ok(job) => self.mcp_settings.test_job = Some(job),
                Err(error) => {
                    self.mcp_settings.feedback = Some((format!("{error:#}"), theme::ERROR))
                }
            }
        }
        if let Some(error) = &self.mcp_settings.server_error {
            ui.label(RichText::new(error).color(theme::ERROR));
        }
        if let Some((message, color)) = &self.mcp_settings.feedback {
            ui.label(RichText::new(message).color(*color));
        }
        ui.add_space(theme::GAP);
        theme::heading(ui, "Try it in Codex");
        ui.label(
            RichText::new(
                "Keep DeckX open. Restart Codex after adding the server and try this prompt.",
            )
            .color(theme::MUTED),
        );
        theme::card(theme::WELL, theme::GAP as i8).show(ui, |ui| {
            theme::setting_row(ui, EXAMPLE, |ui| {
                if theme::text_button(ui, "Copy", 54.0, false, true).clicked() {
                    ui.ctx().copy_text(EXAMPLE.into());
                    self.mcp_settings.feedback = Some(("Prompt copied.".into(), theme::MINT));
                }
            });
        });
        ui.horizontal(|ui| {
            if theme::text_button(ui, "Copy setup command", 143.0, false, true).clicked() {
                self.mcp_settings.feedback = Some(match codex_setup::powershell_command() {
                    Ok(command) => {
                        ui.ctx().copy_text(command);
                        (
                            "Setup command copied. Run it in PowerShell.".into(),
                            theme::MINT,
                        )
                    }
                    Err(error) => (format!("{error:#}"), theme::ERROR),
                });
            }
            ui.hyperlink_to("Setup guide", GUIDE);
        });
        egui::CollapsingHeader::new("Manual setup").show(ui, |ui| {
            ui.label("In Codex, open Settings → MCP servers and add a STDIO server.");
            ui.label("Name  deckx     Arguments  --mcp");
            if let Ok(exe) = std::env::current_exe() {
                ui.label(theme::label("Command"));
                ui.add(egui::Label::new(exe.display().to_string()).wrap());
                if theme::text_button(ui, "Copy executable path", 143.0, false, true).clicked() {
                    ui.ctx().copy_text(exe.display().to_string());
                    self.mcp_settings.feedback =
                        Some(("Executable path copied.".into(), theme::MINT));
                }
            }
            if let Some(Err(error)) = &self.mcp_settings.registration {
                ui.label(RichText::new(error).color(theme::GOLD));
            }
        });
    }
}

fn request_age(at: Instant) -> String {
    match at.elapsed().as_secs() {
        0..=59 => "Just now".into(),
        seconds if seconds < 3600 => format!("{} min ago", seconds / 60),
        seconds => format!("{} hr ago", seconds / 3600),
    }
}
