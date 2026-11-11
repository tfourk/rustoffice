//! "Import from Outlook": picks a `.pst` / `.ost` file, runs the import in
//! the background and shows its progress and result.

use std::time::Duration;

use egui::{Align, CornerRadius, Frame, Id, Layout, Margin, Modal, RichText};

use crate::app::{App, ToastKind};
use crate::config::Paths;
use crate::icons;
use crate::outlook::{self, Job, Progress};
use crate::theme::{font, pal};
use crate::ui::widgets;

pub struct OutlookImport {
    job: Job,
    /// The result has been taken over into the app (mailbox added, lists
    /// refreshed); the window only waits to be closed.
    applied: bool,
    /// Whether a mailbox was added.
    mailbox: bool,
}

fn file_name(job: &Job) -> String {
    job.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

impl App {
    /// Asks for an Outlook data file and starts importing it.
    pub fn import_outlook(&mut self) {
        if self.outlook.is_some() {
            return;
        }
        let Some(file) = rfd::FileDialog::new().add_filter("Outlook data file", &["pst", "ost"]).pick_file() else {
            return;
        };
        let ctx = self.ctx.clone();
        let account = outlook::mailbox_account(&file, &self.cfg);
        let job = outlook::start(file, Paths::get().db_file(), account, move || ctx.request_repaint());
        self.outlook = Some(OutlookImport {
            job,
            applied: false,
            mailbox: false,
        });
    }

    /// Takes a finished import over into the running app.
    fn apply_outlook_import(&mut self, progress: &Progress) {
        let Some(account) = self.outlook.as_ref().map(|o| o.job.account.clone()) else { return };
        let mailbox = outlook::apply(&mut self.cfg, &self.store, account, progress);
        if let Some(o) = self.outlook.as_mut() {
            o.applied = true;
            o.mailbox = mailbox;
        }
        if matches!(progress.finished, Some(Err(_))) {
            return;
        }
        self.save_config_now();
        self.mail.folders_dirty = true;
        self.mail.list_dirty = true;
        self.people.dirty = true;
        self.cal.dirty = true;
        self.todo.dirty = true;
        if mailbox {
            // First mailbox: the welcome screen has done its job.
            if self.wizard.as_ref().is_some_and(|w| w.first_run) {
                self.wizard = None;
            }
            self.mail.select_default_folder(&self.store, &self.cfg);
        }
    }

    pub fn outlook_import_ui(&mut self, ctx: &egui::Context) {
        let Some(import) = &self.outlook else { return };
        let progress = import.job.snapshot();
        let running = progress.finished.is_none();
        if !running && !import.applied {
            self.apply_outlook_import(&progress);
            if progress.finished == Some(Ok(())) {
                self.toast(progress.summary(), ToastKind::Success);
            }
        }
        let Some(import) = &self.outlook else { return };
        let p = pal();
        let name = file_name(&import.job);
        let stopping = import.job.cancel.load(std::sync::atomic::Ordering::Relaxed);
        let mut stop = false;
        let mut close = false;
        let modal = Modal::new(Id::new("outlook-import")).frame(
            Frame::NONE
                .fill(p.card)
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(24))
                .shadow(crate::theme::window_shadow()),
        );
        modal.show(ctx, |ui| {
            ui.set_width(460.0);
            let small = |ui: &mut egui::Ui, text: String| {
                ui.label(RichText::new(text).font(font(12.5)).color(p.text3));
            };
            match &progress.finished {
                None => {
                    widgets::heading(ui, "Importing from Outlook", 18.0);
                    ui.add_space(4.0);
                    ui.label(RichText::new(&name).color(p.text2));
                    ui.add_space(14.0);
                    let fraction = if progress.total > 0 { progress.done as f32 / progress.total as f32 } else { 0.0 };
                    ui.add(egui::ProgressBar::new(fraction.clamp(0.0, 1.0)).desired_width(ui.available_width()).show_percentage());
                    ui.add_space(8.0);
                    if progress.total == 0 {
                        small(ui, "Reading the file…".into());
                    } else {
                        small(ui, format!("{} of {} items · {}", progress.done, progress.total, crate::util::truncate_chars(&progress.folder, 44)));
                    }
                    ui.add_space(16.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::secondary_button_ex(ui, None, if stopping { "Stopping…" } else { "Stop" }, !stopping)
                            .on_hover_text("Keeps what has been imported so far")
                            .clicked()
                        {
                            stop = true;
                        }
                    });
                    ctx.request_repaint_after(Duration::from_millis(150));
                }
                Some(Ok(())) => {
                    widgets::heading(ui, if progress.stopped { "Import stopped" } else { "Import finished" }, 18.0);
                    ui.add_space(4.0);
                    ui.label(RichText::new(&name).color(p.text2));
                    ui.add_space(12.0);
                    ui.label(format!("{}.", progress.summary()));
                    if import.mailbox {
                        ui.add_space(6.0);
                        ui.label(
                            RichText::new(format!(
                                "The mail is in the mailbox \"{}\" in the folder pane. It is an offline copy on this computer; contacts, events and tasks are with your others.",
                                import.job.account.label
                            ))
                            .color(p.text2),
                        );
                    }
                    if progress.skipped > 0 {
                        ui.add_space(6.0);
                        small(ui, format!("{} item(s) were left out: already imported, or of a kind RustMail has no place for (distribution lists, journal entries).", progress.skipped));
                    }
                    if progress.failed > 0 {
                        ui.add_space(6.0);
                        ui.label(RichText::new(format!("{} {} item(s) could not be read:", icons::WARNING, progress.failed)).color(p.danger));
                        egui::ScrollArea::vertical().max_height(110.0).show(ui, |ui| {
                            for e in &progress.errors {
                                small(ui, crate::util::truncate_chars(e, 160));
                            }
                        });
                    }
                    ui.add_space(16.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::primary_button(ui, None, "Done").clicked() {
                            close = true;
                        }
                    });
                }
                Some(Err(error)) => {
                    widgets::heading(ui, "Couldn't import this file", 18.0);
                    ui.add_space(4.0);
                    ui.label(RichText::new(&name).color(p.text2));
                    ui.add_space(12.0);
                    ui.label(error.as_str());
                    ui.add_space(6.0);
                    small(ui, "If the file is open in Outlook, close Outlook and try again. A damaged file can often be repaired with Outlook's Inbox Repair tool (scanpst.exe).".into());
                    ui.add_space(16.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if widgets::primary_button(ui, None, "Close").clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        if stop {
            import.job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if close {
            self.outlook = None;
        }
    }
}
