//! People module: contact list, details and editor.

use egui::{
    vec2, Align, Align2, CornerRadius, CursorIcon, Frame, Layout, Margin, Pos2, RichText,
    ScrollArea, Sense, Ui,
};

use crate::app::{App, ToastKind};
use crate::icons;
use crate::model::*;
use crate::theme::{font, font_semibold, icon_fill_font, icon_font, pal, hex};
use crate::ui::widgets::{self, ribbon_button, ribbon_separator};
use crate::util;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PeopleFilter {
    #[default]
    All,
    Favorites,
    Frequent,
}

#[derive(Default)]
pub struct PeopleState {
    pub contacts: Vec<Contact>,
    pub dirty: bool,
    pub loaded: bool,
    pub selected: Option<i64>,
    pub filter: PeopleFilter,
    pub editor: Option<ContactEditor>,
    pub recent: Option<(i64, Vec<MessageSummary>)>,
}

pub struct ContactEditor {
    pub c: Contact,
    pub emails: String,
    pub phones: String,
}

impl ContactEditor {
    fn new(c: Contact) -> Self {
        ContactEditor {
            emails: c.emails.join(", "),
            phones: c.phones.join(", "),
            c,
        }
    }
}

impl App {
    pub fn people_new_contact(&mut self) {
        self.module = crate::app::Module::People;
        self.people.editor = Some(ContactEditor::new(Contact::default()));
    }

    pub fn ribbon_people(&mut self, ui: &mut Ui, wide: bool) {
        if widgets::primary_button(ui, Some(icons::USER_PLUS), "New contact").clicked() {
            self.people_new_contact();
        }
        ribbon_separator(ui);
        let sel = self.people.selected.and_then(|id| self.people.contacts.iter().find(|c| c.id == id).cloned());
        let has = sel.is_some();
        if ribbon_button(ui, icons::PENCIL_SIMPLE, "Edit", wide, has).clicked() {
            if let Some(c) = &sel {
                self.people.editor = Some(ContactEditor::new(c.clone()));
            }
        }
        if ribbon_button(ui, icons::ENVELOPE_SIMPLE, "Send email", wide, has).clicked() {
            if let Some(c) = &sel {
                let to = Addr::new(c.name.clone(), c.primary_email().to_string()).to_header();
                self.compose_to(&to);
            }
        }
        let fav = sel.as_ref().map(|c| c.favorite).unwrap_or(false);
        if widgets::ribbon_button_ex(ui, icons::STAR, if fav { "Unfavorite" } else { "Favorite" }, wide, has, false, fav).clicked() {
            if let Some(mut c) = sel.clone() {
                c.favorite = !c.favorite;
                let _ = self.store.save_contact(&c);
                self.people.dirty = true;
            }
        }
        if ribbon_button(ui, icons::TRASH, "Delete", wide, has).clicked() {
            if let Some(c) = &sel {
                let _ = self.store.delete_contact(c.id);
                self.people.selected = None;
                self.people.dirty = true;
                self.toast("Contact deleted", ToastKind::Info);
            }
        }
        ribbon_separator(ui);
        if ribbon_button(ui, icons::UPLOAD_SIMPLE, "Import vCard", wide, true).clicked() {
            self.import_vcard();
        }
        if ribbon_button(ui, icons::EXPORT, "Export vCard", wide, true).clicked() {
            self.export_vcard();
        }
    }

    fn import_vcard(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("vCard", &["vcf", "vcard"]).pick_file() else { return };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let list = crate::vcard::parse(&text);
                let mut n = 0;
                for c in list {
                    let existing = c.emails.first().and_then(|e| self.store.contact_by_email(e).ok().flatten());
                    let mut c = c;
                    if let Some(ex) = existing {
                        c.id = ex.id;
                        c.favorite = ex.favorite;
                    }
                    if self.store.save_contact(&c).is_ok() {
                        n += 1;
                    }
                }
                self.people.dirty = true;
                self.toast(format!("Imported {n} contact(s)"), ToastKind::Success);
            }
            Err(e) => {
                self.toast(format!("Couldn't read file: {e}"), ToastKind::Error);
            }
        }
    }

    fn export_vcard(&mut self) {
        let list: Vec<Contact> = self.store.contacts().unwrap_or_default().into_iter().filter(|c| !c.auto).collect();
        let Some(path) = rfd::FileDialog::new().set_file_name("contacts.vcf").save_file() else { return };
        match std::fs::write(&path, crate::vcard::to_vcard(&list)) {
            Ok(()) => self.toast(format!("Exported {} contact(s)", list.len()), ToastKind::Success),
            Err(e) => self.toast(format!("Couldn't save: {e}"), ToastKind::Error),
        };
    }

    pub fn people_ui(&mut self, ui: &mut Ui) {
        let p = pal();
        if self.people.dirty || !self.people.loaded {
            self.people.contacts = self.store.contacts().unwrap_or_default();
            self.people.dirty = false;
            self.people.loaded = true;
            self.people.recent = None;
        }
        let counts = (
            self.people.contacts.iter().filter(|c| !c.auto).count(),
            self.people.contacts.iter().filter(|c| c.favorite).count(),
            self.people.contacts.iter().filter(|c| c.auto).count(),
        );
        egui::Panel::left("people_nav")
            .exact_size(240.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(Frame::NONE.inner_margin(Margin { left: 8, right: 10, top: 2, bottom: 8 }))
            .show(ui, |ui| {
                ui.add_space(4.0);
                for (f, icon, label, n) in [
                    (PeopleFilter::All, icons::ADDRESS_BOOK, "All contacts", counts.0),
                    (PeopleFilter::Favorites, icons::STAR, "Favorites", counts.1),
                    (PeopleFilter::Frequent, icons::CLOCK_COUNTER_CLOCKWISE, "Suggested contacts", counts.2),
                ] {
                    if nav_item(ui, icon, label, n, self.people.filter == f).clicked() {
                        self.people.filter = f;
                    }
                }
                ui.add_space(14.0);
                ui.label(RichText::new("Suggested contacts are collected automatically from people you write to.").font(font(12.0)).color(p.text3));
            });
        egui::Panel::left("people_list")
            .default_size(340.0)
            .size_range(260.0..=520.0)
            .resizable(true)
            .show_separator_line(false)
            .frame(Frame::NONE.inner_margin(Margin { left: 0, right: 6, top: 0, bottom: 8 }))
            .show(ui, |ui| {
                widgets::card().show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    self.people_list(ui);
                });
            });
        egui::CentralPanel::no_frame()
            .frame(Frame::NONE.inner_margin(Margin { left: 0, right: 8, top: 0, bottom: 8 }))
            .show(ui, |ui| {
                widgets::card().show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    if self.people.editor.is_some() {
                        self.contact_editor(ui);
                    } else {
                        self.contact_details(ui);
                    }
                });
            });
    }

    fn people_list(&mut self, ui: &mut Ui) {
        let p = pal();
        let q = self.search.trim().to_lowercase();
        let filter = self.people.filter;
        let mut list: Vec<&Contact> = self
            .people
            .contacts
            .iter()
            .filter(|c| match filter {
                PeopleFilter::All => !c.auto,
                PeopleFilter::Favorites => c.favorite,
                PeopleFilter::Frequent => c.auto,
            })
            .filter(|c| {
                q.is_empty()
                    || c.name.to_lowercase().contains(&q)
                    || c.emails.iter().any(|e| e.to_lowercase().contains(&q))
                    || c.company.to_lowercase().contains(&q)
            })
            .collect();
        if filter == PeopleFilter::Frequent {
            list.sort_by(|a, b| b.use_count.cmp(&a.use_count));
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            let title = match filter {
                PeopleFilter::All => "All contacts",
                PeopleFilter::Favorites => "Favorites",
                PeopleFilter::Frequent => "Suggested contacts",
            };
            ui.label(RichText::new(title).font(font_semibold(18.0)));
            ui.label(RichText::new(format!("{}", list.len())).font(font(13.0)).color(p.text3));
        });
        ui.add_space(6.0);
        if list.is_empty() {
            widgets::empty_state(ui, icons::USERS, if q.is_empty() { "No contacts yet" } else { "No matches" }, if q.is_empty() { "Create one or import a vCard file." } else { "" });
            return;
        }
        let mut clicked = None;
        ScrollArea::vertical().id_salt("people").auto_shrink([false, false]).show(ui, |ui| {
            let mut last_letter = None;
            for c in list {
                let name = c.display_name();
                let letter = name.chars().next().map(|ch| ch.to_uppercase().next().unwrap_or(ch)).unwrap_or('#');
                let letter = if letter.is_alphabetic() { letter } else { '#' };
                if filter != PeopleFilter::Frequent && last_letter != Some(letter) {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.label(RichText::new(letter.to_string()).font(font_semibold(13.0)).color(p.accent_text));
                    });
                    last_letter = Some(letter);
                }
                let selected = self.people.selected == Some(c.id);
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
                let inner = rect.shrink2(vec2(6.0, 1.0));
                if selected || resp.hovered() {
                    ui.painter().rect_filled(inner, CornerRadius::same(6), if selected { p.selected } else { p.hover });
                }
                widgets::paint_avatar(ui.painter(), Pos2::new(inner.left() + 26.0, inner.center().y), 17.0, &c.name, c.primary_email());
                ui.painter().text(Pos2::new(inner.left() + 52.0, inner.top() + 16.0), Align2::LEFT_CENTER, &name, font_semibold(14.0), p.text);
                let sub = if !c.company.is_empty() && !c.job_title.is_empty() {
                    format!("{} · {}", c.job_title, c.company)
                } else {
                    c.primary_email().to_string()
                };
                ui.painter().text(Pos2::new(inner.left() + 52.0, inner.bottom() - 15.0), Align2::LEFT_CENTER, util::truncate_chars(&sub, 44), font(12.5), p.text3);
                if c.favorite {
                    ui.painter().text(Pos2::new(inner.right() - 16.0, inner.center().y), Align2::CENTER_CENTER, icons::STAR, icon_fill_font(14.0), hex(0xE8A317));
                }
                if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                    clicked = Some(c.id);
                }
            }
        });
        if let Some(id) = clicked {
            self.people.selected = Some(id);
            self.people.editor = None;
        }
    }

    fn contact_details(&mut self, ui: &mut Ui) {
        let p = pal();
        let Some(c) = self.people.selected.and_then(|id| self.people.contacts.iter().find(|c| c.id == id).cloned()) else {
            widgets::empty_state(ui, icons::USER_CIRCLE, "Select a contact", "Their details and your recent conversations appear here.");
            return;
        };
        let recent = match &self.people.recent {
            Some((id, v)) if *id == c.id => v.clone(),
            _ => {
                let mut v = Vec::new();
                for e in &c.emails {
                    v.extend(self.store.messages_with(e, 8).unwrap_or_default());
                }
                v.sort_by(|a, b| b.date.cmp(&a.date));
                v.dedup_by_key(|m| m.id);
                v.truncate(8);
                self.people.recent = Some((c.id, v.clone()));
                v
            }
        };
        let mut open_msg = None;
        ScrollArea::vertical().id_salt("contact").auto_shrink([false, false]).show(ui, |ui| {
            Frame::NONE.inner_margin(Margin::same(28)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    widgets::avatar(ui, 84.0, &c.name, c.primary_email());
                    ui.add_space(12.0);
                    ui.vertical(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(c.display_name()).font(font_semibold(24.0)));
                        let sub = [c.job_title.as_str(), c.company.as_str()].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
                        if !sub.is_empty() {
                            ui.label(RichText::new(sub).font(font(14.0)).color(p.text2));
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if !c.primary_email().is_empty() && widgets::primary_button(ui, Some(icons::ENVELOPE_SIMPLE), "Email").clicked() {
                                let to = Addr::new(c.name.clone(), c.primary_email().to_string()).to_header();
                                self.compose_to(&to);
                            }
                            if widgets::secondary_button(ui, Some(icons::PENCIL_SIMPLE), "Edit").clicked() {
                                self.people.editor = Some(ContactEditor::new(c.clone()));
                            }
                            let fav_label = if c.favorite { "Favorite" } else { "Add to favorites" };
                            if widgets::secondary_button(ui, Some(icons::STAR), fav_label).clicked() {
                                let mut c2 = c.clone();
                                c2.favorite = !c2.favorite;
                                c2.auto = false;
                                let _ = self.store.save_contact(&c2);
                                self.people.dirty = true;
                            }
                        });
                    });
                });
                ui.add_space(22.0);
                section(ui, "Contact information");
                for e in &c.emails {
                    detail_row(ui, icons::ENVELOPE_SIMPLE, "Email", e, true).then(|| {
                        let to = Addr::new(c.name.clone(), e.clone()).to_header();
                        self.compose_to(&to);
                    });
                }
                for ph in &c.phones {
                    if detail_row(ui, icons::PHONE, "Phone", ph, false) {
                        ui.ctx().copy_text(ph.clone());
                    }
                }
                if !c.address.is_empty() {
                    detail_row(ui, icons::MAP_PIN, "Address", &c.address, false);
                }
                if !c.website.is_empty() && detail_row(ui, icons::GLOBE, "Website", &c.website, true) {
                    let url = if c.website.starts_with("http") { c.website.clone() } else { format!("https://{}", c.website) };
                    let _ = open::that(url);
                }
                if !c.birthday.is_empty() {
                    let label = chrono::NaiveDate::parse_from_str(&c.birthday, "%Y-%m-%d")
                        .map(|d| d.format("%B %-d").to_string())
                        .unwrap_or_else(|_| c.birthday.clone());
                    detail_row(ui, icons::CAKE, "Birthday", &label, false);
                }
                if !c.notes.is_empty() {
                    ui.add_space(12.0);
                    section(ui, "Notes");
                    ui.label(RichText::new(&c.notes).font(font(14.0)));
                }
                ui.add_space(18.0);
                section(ui, "Recent conversations");
                if recent.is_empty() {
                    ui.label(RichText::new("No messages with this person yet.").font(font(13.0)).color(p.text3));
                }
                for m in &recent {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 44.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(6), p.hover);
                    }
                    ui.painter().text(Pos2::new(rect.left() + 18.0, rect.center().y), Align2::CENTER_CENTER, icons::ENVELOPE_SIMPLE, icon_font(15.0), p.accent_text);
                    ui.painter().text(Pos2::new(rect.left() + 38.0, rect.top() + 13.0), Align2::LEFT_CENTER, util::truncate_chars(m.subject_or_placeholder(), 70), font_semibold(13.5), p.text);
                    ui.painter().text(Pos2::new(rect.left() + 38.0, rect.bottom() - 12.0), Align2::LEFT_CENTER, util::truncate_chars(&m.preview, 80), font(12.0), p.text3);
                    ui.painter().text(Pos2::new(rect.right() - 10.0, rect.top() + 13.0), Align2::RIGHT_CENTER, util::fmt_list_date(m.date), font(12.0), p.text3);
                    if resp.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                        open_msg = Some(m.clone());
                    }
                }
                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    if widgets::subtle_button(ui, icons::TRASH, "Delete contact").clicked() {
                        let _ = self.store.delete_contact(c.id);
                        self.people.selected = None;
                        self.people.dirty = true;
                        self.toast("Contact deleted", ToastKind::Info);
                    }
                });
            });
        });
        if let Some(m) = open_msg {
            self.open_in_mail(&m);
        }
    }

    pub fn open_in_mail(&mut self, m: &MessageSummary) {
        self.module = crate::app::Module::Mail;
        self.search.clear();
        self.mail.view = crate::ui::mail::MailView::Folder(m.folder_id);
        self.mail.focused_tab = if m.focused { 0 } else { 1 };
        self.mail.list_dirty = true;
        self.mail.selection = vec![m.id];
        self.mail.scroll_to_selected = true;
        self.open_reading(m.id);
    }

    fn contact_editor(&mut self, ui: &mut Ui) {
        let p = pal();
        let Some(mut ed) = self.people.editor.take() else { return };
        let mut keep = true;
        let mut save = false;
        ScrollArea::vertical().id_salt("contact-edit").auto_shrink([false, false]).show(ui, |ui| {
            Frame::NONE.inner_margin(Margin::same(28)).show(ui, |ui| {
                ui.set_width(ui.available_width().min(640.0));
                ui.horizontal(|ui| {
                    widgets::avatar(ui, 64.0, &ed.c.name, ed.emails.split(',').next().unwrap_or("").trim());
                    ui.add_space(10.0);
                    widgets::heading(ui, if ed.c.id > 0 { "Edit contact" } else { "New contact" }, 20.0);
                });
                ui.add_space(18.0);
                let w = ui.available_width();
                egui::Grid::new("contact-grid").num_columns(2).spacing([14.0, 12.0]).show(ui, |ui| {
                    let row = |ui: &mut Ui, label: &str, value: &mut String, hint: &str| {
                        ui.label(RichText::new(label).color(p.text2));
                        widgets::text_input(ui, value, hint, w - 140.0);
                        ui.end_row();
                    };
                    row(ui, "Name", &mut ed.c.name, "Full name");
                    row(ui, "Email", &mut ed.emails, "name@example.com, other@example.com");
                    row(ui, "Phone", &mut ed.phones, "+1 555 0100");
                    row(ui, "Company", &mut ed.c.company, "Company");
                    row(ui, "Job title", &mut ed.c.job_title, "Job title");
                    row(ui, "Address", &mut ed.c.address, "Street, City, Country");
                    row(ui, "Website", &mut ed.c.website, "example.com");
                    row(ui, "Birthday", &mut ed.c.birthday, "YYYY-MM-DD");
                    ui.label(RichText::new("Notes").color(p.text2));
                    ui.add(egui::TextEdit::multiline(&mut ed.c.notes).desired_rows(4).desired_width(w - 140.0).margin(Margin::same(8)));
                    ui.end_row();
                    ui.label(RichText::new("Favorite").color(p.text2));
                    widgets::toggle(ui, &mut ed.c.favorite);
                    ui.end_row();
                });
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    if widgets::primary_button(ui, Some(icons::CHECK), "Save").clicked() {
                        save = true;
                    }
                    if widgets::secondary_button(ui, None, "Cancel").clicked() {
                        keep = false;
                    }
                });
            });
        });
        if save {
            ed.c.emails = ed.emails.split([',', ';']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            ed.c.phones = ed.phones.split([',', ';']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            ed.c.auto = false;
            if ed.c.name.trim().is_empty() && ed.c.emails.is_empty() {
                self.toast("Enter a name or an e-mail address", ToastKind::Error);
            } else if !ed.c.birthday.is_empty() && chrono::NaiveDate::parse_from_str(&ed.c.birthday, "%Y-%m-%d").is_err() {
                self.toast("Birthday must look like 1990-04-12", ToastKind::Error);
            } else {
                match self.store.save_contact(&ed.c) {
                    Ok(id) => {
                        self.people.selected = Some(id);
                        self.people.dirty = true;
                        self.cal.dirty = true;
                        self.toast("Contact saved", ToastKind::Success);
                        keep = false;
                    }
                    Err(e) => {
                        self.toast(format!("Couldn't save: {e}"), ToastKind::Error);
                    }
                }
            }
        }
        if keep {
            self.people.editor = Some(ed);
        }
        let _ = Align::Center;
        let _ = Layout::left_to_right(Align::Center);
    }
}

fn nav_item(ui: &mut Ui, icon: &str, label: &str, count: usize, selected: bool) -> egui::Response {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click());
    if selected || resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(4), if selected { p.selected } else { p.hover });
    }
    ui.painter().text(
        Pos2::new(rect.left() + 18.0, rect.center().y),
        Align2::CENTER_CENTER,
        icon,
        if selected { icon_fill_font(17.0) } else { icon_font(17.0) },
        if selected { p.accent } else { p.text2 },
    );
    ui.painter().text(Pos2::new(rect.left() + 36.0, rect.center().y), Align2::LEFT_CENTER, label, if selected { font_semibold(14.0) } else { font(14.0) }, p.text);
    if count > 0 {
        ui.painter().text(Pos2::new(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, count.to_string(), font(12.5), p.text3);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

fn section(ui: &mut Ui, title: &str) {
    ui.label(RichText::new(title).font(font_semibold(15.0)).color(pal().text));
    ui.add_space(4.0);
}

/// Returns true when the row was clicked.
fn detail_row(ui: &mut Ui, icon: &str, label: &str, value: &str, clickable: bool) -> bool {
    let p = pal();
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(6), p.hover);
    }
    ui.painter().text(Pos2::new(rect.left() + 18.0, rect.center().y), Align2::CENTER_CENTER, icon, icon_font(17.0), p.accent_text);
    ui.painter().text(Pos2::new(rect.left() + 42.0, rect.top() + 13.0), Align2::LEFT_CENTER, label, font(12.0), p.text3);
    ui.painter().text(
        Pos2::new(rect.left() + 42.0, rect.bottom() - 14.0),
        Align2::LEFT_CENTER,
        value,
        font(14.0),
        if clickable { p.accent_text } else { p.text },
    );
    let resp = if clickable { resp.on_hover_cursor(CursorIcon::PointingHand) } else { resp.on_hover_text("Click to copy") };
    resp.clicked()
}
