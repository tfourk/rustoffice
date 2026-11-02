//! Mail module: folder pane, message list and reading pane.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use egui::text::{LayoutJob, TextWrapping};
use egui::{
    vec2, Align, Align2, Color32, CornerRadius, CursorIcon, Frame, Id, Layout, Margin, Modifiers,
    Pos2, Rect, RichText, ScrollArea, Sense, Stroke, TextFormat, Ui,
};

use crate::app::{App, Dialog, ToastKind};
use crate::cal;
use crate::config::{AppConfig, FavoriteRef, ImagePolicy, ReadingPanePos};
use crate::engine::{Command, FlagKind, MsgRef};
use crate::icons;
use crate::mime;
use crate::model::*;
use crate::store::{ListQuery, SortField, Store};
use crate::theme::{self, density, font, font_bold, font_semibold, icon_fill_font, icon_font, pal};
use crate::ui::html::{self, HtmlAction, HtmlDoc};
use crate::ui::widgets;
use crate::util;

pub struct DragMsgs(pub Vec<MsgId>);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MailView {
    Folder(FolderId),
    AllInboxes,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ListFilter {
    All,
    Unread,
    Flagged,
    ToMe,
    Attachments,
}

impl ListFilter {
    pub const ALL: [ListFilter; 5] = [
        ListFilter::All,
        ListFilter::Unread,
        ListFilter::Flagged,
        ListFilter::ToMe,
        ListFilter::Attachments,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ListFilter::All => "All",
            ListFilter::Unread => "Unread",
            ListFilter::Flagged => "Flagged",
            ListFilter::ToMe => "To me",
            ListFilter::Attachments => "Has files",
        }
    }
}

pub enum Row {
    Header(String),
    Msg {
        idx: usize,
        count: usize,
        child: bool,
        expanded: bool,
        unread_any: bool,
    },
}

pub struct ReadItem {
    pub summary: MessageSummary,
    pub parsed: Option<Arc<ParsedMessage>>,
    pub doc: Option<Arc<HtmlDoc>>,
    pub cid: HashMap<String, String>,
    pub loading: bool,
    pub error: Option<String>,
    pub expanded: bool,
    pub invite: Option<cal::IcsImport>,
}

pub struct Reading {
    pub msg_id: MsgId,
    pub items: Vec<ReadItem>,
    pub show_images: bool,
    pub subject: String,
}

/// Commands shared by context menus, the ribbon and the reading pane.
#[derive(Clone, Debug)]
pub enum MenuCmd {
    Reply,
    ReplyAll,
    Forward,
    MarkRead(bool),
    Flag(bool),
    Pin(bool),
    Category(String),
    ClearCategories,
    Snooze(Option<i64>),
    MoveTo(FolderId),
    MoveDialog,
    Archive,
    Delete,
    Junk(bool),
    BlockSender,
    AlwaysFocused(bool),
    ViewSource,
    SaveEml,
    EditDraft,
    OpenFull,
}

pub struct MailState {
    pub view: MailView,
    pub folders: BTreeMap<String, Vec<Folder>>,
    pub folders_dirty: bool,
    pub messages: Vec<MessageSummary>,
    pub rows: Vec<Row>,
    pub list_dirty: bool,
    pub selection: Vec<MsgId>,
    pub anchor: Option<MsgId>,
    pub focused_tab: usize,
    pub filter: ListFilter,
    pub sort: SortField,
    pub ascending: bool,
    pub reading: Option<Reading>,
    pub reading_flags_dirty: bool,
    pub collapsed: HashSet<String>,
    pub collapsed_folders: HashSet<FolderId>,
    pub expanded_threads: HashSet<String>,
    pub mark_read_at: Option<(MsgId, f64)>,
    pub scroll_to_selected: bool,
    pub full_view: bool,
    pub limit: usize,
    pub search_all: bool,
    pub compose_inline: Option<crate::ui::compose::Compose>,
    pub select_mode: bool,
    pub other_unread: usize,
    pub searching: bool,
}

impl MailState {
    pub fn new(_cfg: &AppConfig) -> Self {
        MailState {
            view: MailView::AllInboxes,
            folders: BTreeMap::new(),
            folders_dirty: true,
            messages: Vec::new(),
            rows: Vec::new(),
            list_dirty: true,
            selection: Vec::new(),
            anchor: None,
            focused_tab: 0,
            filter: ListFilter::All,
            sort: SortField::Date,
            ascending: false,
            reading: None,
            reading_flags_dirty: false,
            collapsed: HashSet::new(),
            collapsed_folders: HashSet::new(),
            expanded_threads: HashSet::new(),
            mark_read_at: None,
            scroll_to_selected: false,
            full_view: false,
            limit: 300,
            search_all: true,
            compose_inline: None,
            select_mode: false,
            other_unread: 0,
            searching: false,
        }
    }

    pub fn reload_folders(&mut self, store: &Store, cfg: &AppConfig) {
        self.folders.clear();
        for a in &cfg.accounts {
            self.folders
                .insert(a.id.clone(), store.folders(&a.id).unwrap_or_default());
        }
        self.folders_dirty = false;
        update_outgoing_folders(self);
    }

    pub fn select_default_folder(&mut self, store: &Store, cfg: &AppConfig) {
        self.reload_folders(store, cfg);
        let enabled: Vec<&crate::config::AccountConfig> = cfg.accounts.iter().filter(|a| a.enabled).collect();
        if enabled.len() > 1 && cfg.mail.unified_inbox {
            self.view = MailView::AllInboxes;
        } else if let Some(acc) = cfg.default_account() {
            if let Ok(Some(f)) = store.folder_by_role(&acc.id, FolderRole::Inbox) {
                self.view = MailView::Folder(f.id);
            } else {
                self.view = MailView::AllInboxes;
            }
        } else {
            self.view = MailView::AllInboxes;
        }
        self.list_dirty = true;
        self.selection.clear();
        self.reading = None;
    }

    pub fn find_folder(&self, id: FolderId) -> Option<&Folder> {
        self.folders.values().flatten().find(|f| f.id == id)
    }

    pub fn current_folder_id(&self) -> Option<FolderId> {
        match self.view {
            MailView::Folder(id) => Some(id),
            MailView::AllInboxes => None,
        }
    }

    pub fn current_role(&self) -> Option<FolderRole> {
        match self.view {
            MailView::Folder(id) => self.find_folder(id).map(|f| f.role),
            MailView::AllInboxes => Some(FolderRole::Inbox),
        }
    }

    fn inbox_ids(&self) -> Vec<FolderId> {
        self.folders
            .values()
            .flatten()
            .filter(|f| f.role == FolderRole::Inbox)
            .map(|f| f.id)
            .collect()
    }

    pub fn show_tabs(&self, cfg: &AppConfig) -> bool {
        cfg.mail.focused_inbox && self.current_role() == Some(FolderRole::Inbox) && !self.searching
    }

    pub fn reload(&mut self, store: &Store, cfg: &AppConfig, search: &str) {
        if self.folders_dirty {
            self.reload_folders(store, cfg);
        }
        self.searching = !search.trim().is_empty();
        let folders = if self.searching && self.search_all {
            Vec::new()
        } else {
            match self.view {
                MailView::Folder(id) => vec![id],
                MailView::AllInboxes => {
                    let ids = self.inbox_ids();
                    if ids.is_empty() {
                        vec![-1]
                    } else {
                        ids
                    }
                }
            }
        };
        let show_tabs = self.show_tabs(cfg);
        let mut q = ListQuery {
            folders: folders.clone(),
            focused: if show_tabs { Some(self.focused_tab == 0) } else { None },
            unread_only: self.filter == ListFilter::Unread,
            flagged_only: self.filter == ListFilter::Flagged,
            attachments_only: self.filter == ListFilter::Attachments,
            to_me: if self.filter == ListFilter::ToMe {
                cfg.accounts.iter().map(|a| a.email.clone()).collect()
            } else {
                Vec::new()
            },
            search: if self.searching { Some(search.to_string()) } else { None },
            sort: self.sort,
            ascending: self.ascending,
            pinned_first: true,
            limit: self.limit,
        };
        self.messages = store.list_messages(&q).unwrap_or_default();
        if show_tabs {
            q.focused = Some(self.focused_tab != 0);
            q.unread_only = true;
            q.limit = 999;
            self.other_unread = store.list_messages(&q).map(|v| v.len()).unwrap_or(0);
        }
        self.build_rows(cfg);
        self.list_dirty = false;
        let present: HashSet<MsgId> = self.messages.iter().map(|m| m.id).collect();
        self.selection.retain(|id| present.contains(id));
    }

    fn build_rows(&mut self, cfg: &AppConfig) {
        self.rows.clear();
        let group_dates = cfg.mail.group_by_date && self.sort == SortField::Date && !self.searching;
        let conv = cfg.mail.conversation_view && !self.searching;
        let mut members: HashMap<&str, Vec<usize>> = HashMap::new();
        if conv {
            for (i, m) in self.messages.iter().enumerate() {
                members.entry(m.thread_id.as_str()).or_default().push(i);
            }
        }
        let mut last_header: Option<String> = None;
        for (i, m) in self.messages.iter().enumerate() {
            let group = if conv { members.get(m.thread_id.as_str()) } else { None };
            if let Some(g) = group {
                if g[0] != i {
                    continue;
                }
            }
            let header = if m.pinned && !self.searching {
                "Pinned".to_string()
            } else if group_dates {
                util::date_group(m.snoozed_until.unwrap_or(m.date).max(m.date)).to_string()
            } else {
                String::new()
            };
            if !header.is_empty() && last_header.as_deref() != Some(header.as_str()) {
                self.rows.push(Row::Header(header.clone()));
                last_header = Some(header);
            }
            let count = group.map(|g| g.len()).unwrap_or(1);
            let unread_any = group
                .map(|g| g.iter().any(|&j| !self.messages[j].seen))
                .unwrap_or(!m.seen);
            let expanded = count > 1 && self.expanded_threads.contains(&m.thread_id);
            self.rows.push(Row::Msg {
                idx: i,
                count,
                child: false,
                expanded,
                unread_any,
            });
            if expanded {
                if let Some(g) = group {
                    for &j in &g[1..] {
                        self.rows.push(Row::Msg {
                            idx: j,
                            count: 1,
                            child: true,
                            expanded: false,
                            unread_any: !self.messages[j].seen,
                        });
                    }
                }
            }
        }
    }

    pub fn visible_ids(&self) -> Vec<MsgId> {
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Msg { idx, .. } => self.messages.get(*idx).map(|m| m.id),
                _ => None,
            })
            .collect()
    }

    pub fn first_message_id(&self) -> Option<MsgId> {
        self.visible_ids().first().copied()
    }
}

fn elided(ui: &Ui, text: &str, font_id: egui::FontId, color: Color32, max_w: f32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(
        text.replace(['\n', '\r'], " "),
        TextFormat {
            font_id,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: max_w.max(10.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

enum ListAction {
    Select(MsgId, Modifiers),
    Open(MsgId),
    ToggleThread(String),
    ToggleCheck(MsgId),
    Cmd(MenuCmd, Vec<MsgId>),
    LoadMore,
    LoadOlder,
}

enum ReadAction {
    Cmd(MenuCmd, Vec<MsgId>),
    OpenLink(String),
    Unsubscribe(String),
    SaveAttachment(MsgId, usize),
    OpenAttachment(MsgId, usize),
    SaveAll(MsgId),
    AddToCalendar(MsgId),
    Rsvp(MsgId, cal::Rsvp),
    ImportContacts(MsgId, usize),
    TrustSender(String),
    Retry(MsgId),
    SendReceipt(MsgId, String),
    DismissReceipt(String),
}

impl App {
    // ================================================================ layout

    pub fn mail_ui(&mut self, ui: &mut Ui) {
        if self.mail.folders_dirty {
            self.mail.reload_folders(&self.store, &self.cfg);
            // A single new account starts on "All inboxes" before its folders
            // exist; switch to its Inbox once they arrive.
            let enabled: Vec<&crate::config::AccountConfig> = self.cfg.accounts.iter().filter(|a| a.enabled).collect();
            if self.mail.view == MailView::AllInboxes && (enabled.len() == 1 || !self.cfg.mail.unified_inbox) {
                if let Some(acc) = self.cfg.default_account() {
                    if let Ok(Some(f)) = self.store.folder_by_role(&acc.id, FolderRole::Inbox) {
                        self.mail.view = MailView::Folder(f.id);
                        self.mail.list_dirty = true;
                    }
                }
            }
        }
        if self.mail.list_dirty {
            let search = self.search.clone();
            self.mail.reload(&self.store, &self.cfg, &search);
        }
        if self.mail.reading_flags_dirty {
            self.refresh_reading_flags();
        }
        let ctx = ui.ctx().clone();
        // Files dropped on the window start a new message with attachments.
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if !dropped.is_empty() {
            self.attach_dropped(dropped);
        }

        if self.cfg.window.folder_pane_open {
            let r = egui::Panel::left("folder_pane")
                .resizable(true)
                .default_size(self.cfg.window.folder_width)
                .size_range(180.0..=420.0)
                .show_separator_line(false)
                .frame(Frame::NONE.inner_margin(Margin {
                    left: 4,
                    right: 8,
                    top: 0,
                    bottom: 8,
                }))
                .show(ui, |ui| self.folder_pane(ui));
            let w = r.response.rect.width();
            if (w - self.cfg.window.folder_width).abs() > 1.0 {
                self.cfg.window.folder_width = w;
                self.mark_config_dirty();
            }
        }
        let pos = self.cfg.mail.reading_pane;
        match pos {
            ReadingPanePos::Right | ReadingPanePos::Bottom => {
                let panel = if pos == ReadingPanePos::Right {
                    egui::Panel::left("list_pane")
                        .default_size(self.cfg.window.list_width)
                        .size_range(300.0..=760.0)
                } else {
                    egui::Panel::top("list_pane")
                        .default_size(self.cfg.window.list_height)
                        .size_range(160.0..=900.0)
                };
                let r = panel
                    .resizable(true)
                    .show_separator_line(false)
                    .frame(Frame::NONE.inner_margin(Margin {
                        left: 0,
                        right: if pos == ReadingPanePos::Right { 6 } else { 8 },
                        top: 0,
                        bottom: if pos == ReadingPanePos::Right { 8 } else { 6 },
                    }))
                    .show(ui, |ui| {
                        widgets::card().show(ui, |ui| {
                            ui.set_min_size(ui.available_size());
                            self.message_list(ui);
                        });
                    });
                let size = r.response.rect.size();
                if pos == ReadingPanePos::Right && (size.x - self.cfg.window.list_width).abs() > 1.0 {
                    self.cfg.window.list_width = size.x;
                    self.mark_config_dirty();
                } else if pos == ReadingPanePos::Bottom && (size.y - self.cfg.window.list_height).abs() > 1.0 {
                    self.cfg.window.list_height = size.y;
                    self.mark_config_dirty();
                }
                egui::CentralPanel::no_frame()
                    .frame(Frame::NONE.inner_margin(Margin {
                        left: 0,
                        right: 8,
                        top: 0,
                        bottom: 8,
                    }))
                    .show(ui, |ui| {
                        widgets::card().show(ui, |ui| {
                            ui.set_min_size(ui.available_size());
                            self.reading_pane(ui);
                        });
                    });
            }
            ReadingPanePos::Off => {
                egui::CentralPanel::no_frame()
                    .frame(Frame::NONE.inner_margin(Margin {
                        left: 0,
                        right: 8,
                        top: 0,
                        bottom: 8,
                    }))
                    .show(ui, |ui| {
                        widgets::card().show(ui, |ui| {
                            ui.set_min_size(ui.available_size());
                            if self.mail.full_view || self.mail.compose_inline.is_some() {
                                if self.mail.compose_inline.is_none() {
                                    ui.horizontal(|ui| {
                                        ui.add_space(8.0);
                                        if widgets::subtle_button(ui, icons::ARROW_LEFT, "Back to list").clicked() {
                                            self.mail.full_view = false;
                                        }
                                    });
                                }
                                self.reading_pane(ui);
                            } else {
                                self.message_list(ui);
                            }
                        });
                    });
            }
        }
    }

    // =========================================================== folder pane

    fn folder_pane(&mut self, ui: &mut Ui) {
        let p = pal();
        ScrollArea::vertical()
            .id_salt("folders")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                // Collapse toggle row.
                ui.horizontal(|ui| {
                    if widgets::icon_button(ui, icons::LIST, "Hide folder pane").clicked() {
                        self.cfg.window.folder_pane_open = false;
                        self.mark_config_dirty();
                    }
                });
                ui.add_space(4.0);

                // Favorites
                let fav_open = !self.mail.collapsed.contains("favorites");
                if self.section_header(ui, "Favorites", fav_open, "favorites") {
                    toggle_set(&mut self.mail.collapsed, "favorites");
                }
                if fav_open {
                    let enabled = self.cfg.accounts.iter().filter(|a| a.enabled).count();
                    if enabled > 1 && self.cfg.mail.unified_inbox {
                        let unread: i64 = self
                            .mail
                            .folders
                            .values()
                            .flatten()
                            .filter(|f| f.role == FolderRole::Inbox)
                            .map(|f| f.unread)
                            .sum();
                        let sel = self.mail.view == MailView::AllInboxes;
                        let r = self.folder_row(ui, icons::TRAY, "All inboxes", 0, unread, true, sel, None, false, false);
                        if r.clicked() {
                            self.select_view(MailView::AllInboxes);
                        }
                    }
                    let favs = self.cfg.favorites.clone();
                    for fav in favs {
                        let Some(f) = self
                            .mail
                            .folders
                            .get(&fav.account_id)
                            .and_then(|v| v.iter().find(|f| f.path == fav.path))
                            .cloned()
                        else {
                            continue;
                        };
                        let mut label = f.label();
                        if enabled > 1 {
                            label = format!("{} · {}", label, self.account_name(&f.account_id));
                        }
                        let count = if f.role == FolderRole::Drafts { f.total } else { f.unread };
                        let sel = self.mail.view == MailView::Folder(f.id);
                        let r = self.folder_row(ui, f.role.icon(), &label, 0, count, f.role != FolderRole::Drafts, sel, Some(&f), false, false);
                        self.folder_interactions(ui, &r, &f);
                    }
                    if self.cfg.favorites.is_empty() && enabled <= 1 {
                        ui.horizontal(|ui| {
                            ui.add_space(30.0);
                            ui.label(RichText::new("Right-click a folder to add it here").font(font(12.0)).color(p.text3));
                        });
                    }
                }
                ui.add_space(10.0);

                for acc in self.cfg.accounts.clone() {
                    if !acc.enabled {
                        continue;
                    }
                    let open = !self.mail.collapsed.contains(&acc.id);
                    let status = self.statuses.get(&acc.id).cloned();
                    let r = self.account_header(ui, &acc.name().to_string(), open, &status);
                    if r.clicked() {
                        toggle_set(&mut self.mail.collapsed, &acc.id);
                    }
                    r.context_menu(|ui| {
                        if widgets::menu_item(ui, icons::ARROWS_CLOCKWISE, "Sync now").clicked() {
                            self.engine.send(Command::SyncAccount { account_id: acc.id.clone(), full: true });
                        }
                        if widgets::menu_item(ui, icons::FOLDER_PLUS, "Create new folder").clicked() {
                            self.dialog = Some(Dialog::NewFolder { account_id: acc.id.clone(), parent: None, name: String::new() });
                        }
                        if widgets::menu_item(ui, icons::GEAR, "Account settings").clicked() {
                            self.settings = Some(crate::ui::settings::SettingsState::edit_account(&acc));
                        }
                        ui.separator();
                        if widgets::menu_item(ui, icons::SIGN_OUT, "Remove account").clicked() {
                            self.dialog = Some(Dialog::RemoveAccount { account_id: acc.id.clone(), name: acc.name().to_string() });
                        }
                    });
                    if !open {
                        ui.add_space(6.0);
                        continue;
                    }
                    let folders = self.mail.folders.get(&acc.id).cloned().unwrap_or_default();
                    if folders.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add_space(30.0);
                            let msg = match &status {
                                Some(SyncStatus::Error(_)) => "Couldn't load folders",
                                _ => "Loading folders…",
                            };
                            ui.label(RichText::new(msg).font(font(12.5)).color(p.text3));
                        });
                    }
                    for (f, depth, has_children) in folder_tree(&folders) {
                        if self.hidden_by_collapse(&folders, &f) {
                            continue;
                        }
                        let collapsed = self.mail.collapsed_folders.contains(&f.id);
                        let count = if f.role == FolderRole::Drafts { f.total } else if f.role == FolderRole::Trash { 0 } else { f.unread };
                        let sel = self.mail.view == MailView::Folder(f.id);
                        let r = self.folder_row(
                            ui,
                            f.role.icon(),
                            &f.label(),
                            depth,
                            count,
                            f.role != FolderRole::Drafts,
                            sel,
                            Some(&f),
                            has_children,
                            collapsed,
                        );
                        self.folder_interactions(ui, &r, &f);
                    }
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(26.0);
                        if widgets::link(ui, "Create new folder").clicked() {
                            self.dialog = Some(Dialog::NewFolder { account_id: acc.id.clone(), parent: None, name: String::new() });
                        }
                    });
                    ui.add_space(12.0);
                }
                if self.cfg.accounts.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        if widgets::primary_button(ui, Some(icons::USER_PLUS), "Add account").clicked() {
                            self.wizard = Some(crate::ui::wizard::WizardState::new(true));
                        }
                    });
                }
            });
    }

    fn hidden_by_collapse(&self, folders: &[Folder], f: &Folder) -> bool {
        let mut cur = f.parent_path();
        while let Some(pp) = cur {
            if let Some(parent) = folders.iter().find(|x| x.path == pp) {
                if self.mail.collapsed_folders.contains(&parent.id) {
                    return true;
                }
                cur = parent.parent_path();
            } else {
                break;
            }
        }
        false
    }

    fn section_header(&mut self, ui: &mut Ui, title: &str, open: bool, _key: &str) -> bool {
        let p = pal();
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(4), p.hover);
        }
        ui.painter().text(
            Pos2::new(rect.left() + 14.0, rect.center().y),
            Align2::CENTER_CENTER,
            if open { icons::CARET_DOWN } else { icons::CARET_RIGHT },
            icon_font(12.0),
            p.text2,
        );
        ui.painter().text(
            Pos2::new(rect.left() + 28.0, rect.center().y),
            Align2::LEFT_CENTER,
            title,
            font_semibold(14.0),
            p.text,
        );
        resp.clicked()
    }

    fn account_header(&mut self, ui: &mut Ui, name: &str, open: bool, status: &Option<SyncStatus>) -> egui::Response {
        let p = pal();
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(4), p.hover);
        }
        ui.painter().text(
            Pos2::new(rect.left() + 14.0, rect.center().y),
            Align2::CENTER_CENTER,
            if open { icons::CARET_DOWN } else { icons::CARET_RIGHT },
            icon_font(12.0),
            p.text2,
        );
        let g = elided(ui, name, font_semibold(14.0), p.text, rect.width() - 60.0);
        ui.painter().galley(Pos2::new(rect.left() + 28.0, rect.center().y - g.size().y / 2.0), g, p.text);
        let icon = match status {
            Some(SyncStatus::Error(_)) => Some((icons::WARNING, p.danger)),
            Some(SyncStatus::Syncing(_)) | Some(SyncStatus::Connecting) => Some((icons::ARROWS_CLOCKWISE, p.accent_text)),
            _ => None,
        };
        if let Some((ic, c)) = icon {
            ui.painter().text(Pos2::new(rect.right() - 14.0, rect.center().y), Align2::CENTER_CENTER, ic, icon_font(13.0), c);
        }
        let resp = match status {
            Some(SyncStatus::Error(e)) => resp.on_hover_text(e.as_str()),
            _ => resp,
        };
        resp.on_hover_cursor(CursorIcon::PointingHand)
    }

    #[allow(clippy::too_many_arguments)]
    fn folder_row(
        &mut self,
        ui: &mut Ui,
        icon: &str,
        label: &str,
        depth: usize,
        count: i64,
        count_strong: bool,
        selected: bool,
        folder: Option<&Folder>,
        has_children: bool,
        collapsed: bool,
    ) -> egui::Response {
        let p = pal();
        let h = match density() {
            theme::Density::Compact => 28.0,
            theme::Density::Cozy => 32.0,
            theme::Density::Roomy => 36.0,
        };
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
        let drop_hover = folder.is_some() && resp.dnd_hover_payload::<DragMsgs>().is_some();
        let bg = if drop_hover {
            p.selected_hover
        } else if selected {
            if resp.hovered() { p.selected_hover } else { p.selected }
        } else if resp.hovered() {
            p.hover
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(rect, CornerRadius::same(4), bg);
        if drop_hover {
            ui.painter().rect_stroke(rect, CornerRadius::same(4), Stroke::new(1.5, p.accent), egui::StrokeKind::Inside);
        }
        let x = rect.left() + 12.0 + depth as f32 * 16.0;
        if has_children {
            let chev = Rect::from_center_size(Pos2::new(x + 4.0, rect.center().y), vec2(16.0, 16.0));
            ui.painter().text(chev.center(), Align2::CENTER_CENTER, if collapsed { icons::CARET_RIGHT } else { icons::CARET_DOWN }, icon_font(11.0), p.text2);
            if let Some(f) = folder {
                let cr = ui.interact(chev.expand(4.0), Id::new(("chev", f.id)), Sense::click());
                if cr.clicked() {
                    if collapsed {
                        self.mail.collapsed_folders.remove(&f.id);
                    } else {
                        self.mail.collapsed_folders.insert(f.id);
                    }
                }
            }
        }
        let icon_x = x + 22.0;
        ui.painter().text(
            Pos2::new(icon_x, rect.center().y),
            Align2::CENTER_CENTER,
            icon,
            if selected { icon_fill_font(17.0) } else { icon_font(17.0) },
            if selected { p.accent } else { p.text2 },
        );
        let badge_w = if count > 0 { 34.0 } else { 6.0 };
        let strong = count > 0 && count_strong;
        let f = if strong || selected { font_semibold(14.0) } else { font(14.0) };
        let g = elided(ui, label, f, p.text, rect.right() - badge_w - (icon_x + 14.0));
        ui.painter().galley(Pos2::new(icon_x + 14.0, rect.center().y - g.size().y / 2.0), g, p.text);
        if count > 0 {
            widgets::paint_badge(ui.painter(), Pos2::new(rect.right() - 10.0, rect.center().y), count, count_strong);
        }
        resp.on_hover_cursor(CursorIcon::PointingHand)
    }

    fn folder_interactions(&mut self, _ui: &mut Ui, r: &egui::Response, f: &Folder) {
        if r.clicked() {
            self.select_view(MailView::Folder(f.id));
        }
        if let Some(payload) = r.dnd_release_payload::<DragMsgs>() {
            let ids = payload.0.clone();
            self.move_messages(&ids, f.id);
        }
        let f = f.clone();
        r.context_menu(|ui| {
            let fav = self.cfg.is_favorite(&f.account_id, &f.path);
            if widgets::menu_item(ui, icons::STAR, if fav { "Remove from Favorites" } else { "Add to Favorites" }).clicked() {
                if fav {
                    self.cfg.favorites.retain(|x| !(x.account_id == f.account_id && x.path == f.path));
                } else {
                    self.cfg.favorites.push(FavoriteRef { account_id: f.account_id.clone(), path: f.path.clone() });
                }
                self.mark_config_dirty();
            }
            if widgets::menu_item(ui, icons::ENVELOPE_SIMPLE_OPEN, "Mark all as read").clicked() {
                self.mark_folder_read(f.id);
            }
            if widgets::menu_item(ui, icons::ARROWS_CLOCKWISE, "Sync this folder").clicked() {
                self.engine.send(Command::SyncFolder { folder_id: f.id });
            }
            ui.separator();
            if widgets::menu_item(ui, icons::FOLDER_PLUS, "Create subfolder").clicked() {
                self.dialog = Some(Dialog::NewFolder { account_id: f.account_id.clone(), parent: Some(f.id), name: String::new() });
            }
            if f.role == FolderRole::Custom {
                if widgets::menu_item(ui, icons::PENCIL_SIMPLE, "Rename").clicked() {
                    self.dialog = Some(Dialog::RenameFolder { folder_id: f.id, name: f.name.clone() });
                }
                if widgets::menu_item(ui, icons::TRASH, "Delete folder").clicked() {
                    self.dialog = Some(Dialog::DeleteFolder { folder_id: f.id, name: f.label() });
                }
            }
            if matches!(f.role, FolderRole::Trash | FolderRole::Junk) && widgets::menu_item(ui, icons::BROOM, "Empty folder").clicked() {
                self.dialog = Some(Dialog::EmptyFolder { folder_id: f.id, name: f.label() });
            }
        });
    }

    pub fn select_view(&mut self, view: MailView) {
        self.module = crate::app::Module::Mail;
        if self.mail.view != view {
            self.mail.view = view;
            self.mail.selection.clear();
            self.mail.reading = None;
            self.mail.full_view = false;
            self.mail.limit = 300;
            self.mail.focused_tab = 0;
        }
        if !self.search.is_empty() {
            self.search.clear();
        }
        self.mail.list_dirty = true;
        if let MailView::Folder(id) = view {
            self.engine.send(Command::SyncFolder { folder_id: id });
        }
    }

    // ========================================================== message list

    fn message_list(&mut self, ui: &mut Ui) {
        let p = pal();
        let searching = self.mail.searching;
        let title = if searching {
            "Search results".to_string()
        } else {
            match self.mail.view {
                MailView::AllInboxes => "All inboxes".to_string(),
                MailView::Folder(id) => self.mail.find_folder(id).map(|f| f.label()).unwrap_or_default(),
            }
        };
        // Header
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(RichText::new(&title).font(font_semibold(18.0)).color(p.text));
            if let (false, MailView::Folder(id)) = (searching, self.mail.view) {
                if let Some(f) = self.mail.find_folder(id).cloned() {
                    let fav = self.cfg.is_favorite(&f.account_id, &f.path);
                    let r = widgets::icon_button_ex(ui, icons::STAR, 26.0, 15.0, if fav { "Remove from Favorites" } else { "Add to Favorites" }, fav, if fav { Some(theme::hex(0xE8A317)) } else { None });
                    if r.clicked() {
                        if fav {
                            self.cfg.favorites.retain(|x| !(x.account_id == f.account_id && x.path == f.path));
                        } else {
                            self.cfg.favorites.push(FavoriteRef { account_id: f.account_id.clone(), path: f.path.clone() });
                        }
                        self.mark_config_dirty();
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                let r = widgets::icon_button_ex(ui, icons::CHECKS, 30.0, 17.0, "Select messages", self.mail.select_mode, None);
                if r.clicked() {
                    self.mail.select_mode = !self.mail.select_mode;
                }
                let sort = widgets::icon_button_ex(ui, if self.mail.ascending { icons::SORT_ASCENDING } else { icons::SORT_DESCENDING }, 30.0, 17.0, "Sort", false, None);
                egui::Popup::menu(&sort).show(|ui| {
                    for s in SortField::ALL {
                        if widgets::menu_item(ui, if self.mail.sort == s { icons::CHECK } else { "" }, s.label()).clicked() {
                            self.mail.sort = s;
                            self.mail.list_dirty = true;
                        }
                    }
                    ui.separator();
                    if widgets::menu_item(ui, if !self.mail.ascending { icons::CHECK } else { "" }, "Newest / Z on top").clicked() {
                        self.mail.ascending = false;
                        self.mail.list_dirty = true;
                    }
                    if widgets::menu_item(ui, if self.mail.ascending { icons::CHECK } else { "" }, "Oldest / A on top").clicked() {
                        self.mail.ascending = true;
                        self.mail.list_dirty = true;
                    }
                });
                let active = self.mail.filter != ListFilter::All;
                let filter = widgets::icon_button_ex(ui, icons::FUNNEL, 30.0, 17.0, "Filter", active, if active { Some(p.accent) } else { None });
                egui::Popup::menu(&filter).show(|ui| {
                    for f in ListFilter::ALL {
                        if widgets::menu_item(ui, if self.mail.filter == f { icons::CHECK } else { "" }, f.label()).clicked() {
                            self.mail.filter = f;
                            self.mail.list_dirty = true;
                        }
                    }
                });
                if active {
                    ui.label(RichText::new(self.mail.filter.label()).font(font(12.5)).color(p.accent_text));
                }
            });
        });
        if searching {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                let mut idx = if self.mail.search_all { 0 } else { 1 };
                if widgets::tabs(ui, &["All folders", "Current folder"], &mut idx, 13.0) {
                    self.mail.search_all = idx == 0;
                    self.mail.list_dirty = true;
                }
                let n = self.mail.messages.len();
                ui.label(RichText::new(format!("{n} result{}", if n == 1 { "" } else { "s" })).font(font(12.5)).color(p.text3));
            });
        } else if self.mail.show_tabs(&self.cfg) {
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let mut t = self.mail.focused_tab;
                if widgets::tabs(ui, &["Focused", "Other"], &mut t, 14.0) {
                    self.mail.focused_tab = t;
                    self.mail.list_dirty = true;
                    self.mail.selection.clear();
                    self.mail.reading = None;
                }
                if self.mail.other_unread > 0 {
                    let label = if t == 0 { "Other" } else { "Focused" };
                    ui.label(RichText::new(format!("{} new in {label}", self.mail.other_unread)).font(font(12.0)).color(p.text3));
                }
            });
        } else {
            ui.add_space(6.0);
        }
        let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(1.0, p.stroke));
        self.outbox_banners(ui);

        if self.mail.rows.is_empty() {
            let (icon, t, s) = if searching {
                (icons::MAGNIFYING_GLASS, "No results", "Try different words, or search all folders.")
            } else if self.mail.filter != ListFilter::All {
                (icons::FUNNEL, "Nothing matches this filter", "Choose another filter to see more messages.")
            } else if self.mail.show_tabs(&self.cfg) && self.mail.focused_tab == 0 {
                (icons::CHECK_CIRCLE, "All caught up", "Nothing in Focused. Check Other for newsletters and notifications.")
            } else {
                (icons::TRAY, "This folder is empty", "")
            };
            widgets::empty_state(ui, icon, t, s);
            return;
        }

        let rows = std::mem::take(&mut self.mail.rows);
        let messages = std::mem::take(&mut self.mail.messages);
        let mut actions: Vec<ListAction> = Vec::new();
        let show_preview = self.cfg.mail.show_preview;
        let show_avatars = self.cfg.mail.show_avatars;
        let pad = density().pad();
        let heights: Vec<f32> = rows
            .iter()
            .map(|r| match r {
                Row::Header(_) => 30.0,
                Row::Msg { child, .. } => {
                    let lines = if show_preview && !child { 3.0 } else { 2.0 };
                    lines * 19.0 + 2.0 * pad + 6.0
                }
            })
            .collect();
        let has_more = messages.len() >= self.mail.limit;
        let can_older = matches!(self.mail.view, MailView::Folder(_)) && !searching;
        let footer = if has_more || can_older { 56.0 } else { 12.0 };
        let total: f32 = heights.iter().sum::<f32>() + footer;
        let scroll_target = if self.mail.scroll_to_selected {
            self.mail.scroll_to_selected = false;
            self.mail.selection.last().copied()
        } else {
            None
        };
        let selection: HashSet<MsgId> = self.mail.selection.iter().copied().collect();
        let select_mode = self.mail.select_mode;

        ScrollArea::vertical()
            .id_salt(("msglist", format!("{:?}", self.mail.view)))
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                ui.set_height(total);
                let origin = ui.max_rect().min;
                let width = ui.available_width();
                let mut y = 0.0;
                for (ri, row) in rows.iter().enumerate() {
                    let h = heights[ri];
                    let rect = Rect::from_min_size(origin + vec2(0.0, y), vec2(width, h));
                    if let (Some(t), Row::Msg { idx, .. }) = (scroll_target, row) {
                        if messages[*idx].id == t {
                            ui.scroll_to_rect(rect, None);
                        }
                    }
                    if y + h >= viewport.min.y && y <= viewport.max.y {
                        match row {
                            Row::Header(text) => {
                                ui.painter().text(
                                    Pos2::new(rect.left() + 16.0, rect.center().y + 3.0),
                                    Align2::LEFT_CENTER,
                                    text,
                                    font_semibold(13.0),
                                    p.text2,
                                );
                            }
                            Row::Msg { idx, count, child, expanded, unread_any } => {
                                let m = &messages[*idx];
                                self.draw_message_row(
                                    ui,
                                    rect,
                                    m,
                                    *count,
                                    *child,
                                    *expanded,
                                    *unread_any,
                                    selection.contains(&m.id),
                                    show_preview,
                                    show_avatars,
                                    select_mode,
                                    &selection,
                                    &mut actions,
                                );
                            }
                        }
                    }
                    y += h;
                }
                if has_more || can_older {
                    let r = Rect::from_min_size(origin + vec2(0.0, y + 10.0), vec2(width, 36.0));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Center)), |ui| {
                        if has_more {
                            if widgets::subtle_button(ui, icons::CARET_DOWN, "Show more messages").clicked() {
                                actions.push(ListAction::LoadMore);
                            }
                        } else if widgets::subtle_button(ui, icons::CLOUD_ARROW_DOWN, "Download older messages from the server").clicked() {
                            actions.push(ListAction::LoadOlder);
                        }
                    });
                }
            });

        self.mail.rows = rows;
        self.mail.messages = messages;
        for a in actions {
            match a {
                ListAction::Select(id, mods) => self.select_message(id, mods),
                ListAction::Open(id) => self.open_message_full(id),
                ListAction::ToggleThread(t) => {
                    toggle_set(&mut self.mail.expanded_threads, &t);
                    self.mail.list_dirty = true;
                }
                ListAction::ToggleCheck(id) => {
                    if let Some(pos) = self.mail.selection.iter().position(|x| *x == id) {
                        self.mail.selection.remove(pos);
                    } else {
                        self.mail.selection.push(id);
                    }
                    if self.mail.selection.len() == 1 {
                        let only = self.mail.selection[0];
                        self.open_reading(only);
                    }
                }
                ListAction::Cmd(cmd, ids) => self.apply_cmd(cmd, &ids),
                ListAction::LoadMore => {
                    self.mail.limit += 500;
                    self.mail.list_dirty = true;
                }
                ListAction::LoadOlder => {
                    if let MailView::Folder(id) = self.mail.view {
                        self.engine.send(Command::LoadOlder { folder_id: id });
                        self.toast("Downloading older messages…", ToastKind::Info);
                    }
                }
            }
        }
    }

    /// Messages that failed to send stay in the outbox until retried or discarded.
    fn outbox_banners(&mut self, ui: &mut Ui) {
        let p = pal();
        let failed: Vec<OutboxItem> = self
            .store
            .outbox()
            .unwrap_or_default()
            .into_iter()
            .filter(|o| o.attempts > 0)
            .collect();
        for o in failed.iter().take(3) {
            Frame::NONE.inner_margin(Margin { left: 8, right: 8, top: 6, bottom: 0 }).show(ui, |ui| {
                let text = format!(
                    "Couldn't send \"{}\": {}",
                    util::truncate_chars(&o.subject, 40),
                    util::truncate_chars(o.last_error.as_deref().unwrap_or("unknown error"), 120)
                );
                widgets::banner(ui, icons::WARNING, &text, p.danger, p.danger_soft, |ui| {
                    if widgets::link(ui, "Retry").clicked() {
                        let _ = self.store.outbox_retry(o.id);
                        self.engine.send(Command::ProcessOutbox);
                    }
                    ui.label(RichText::new("·").color(p.text3));
                    if widgets::link(ui, "Discard").clicked() {
                        let _ = self.store.remove_outbox(o.id);
                    }
                });
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_message_row(
        &self,
        ui: &mut Ui,
        rect: Rect,
        m: &MessageSummary,
        count: usize,
        child: bool,
        expanded: bool,
        unread_any: bool,
        selected: bool,
        show_preview: bool,
        show_avatars: bool,
        select_mode: bool,
        selection: &HashSet<MsgId>,
        actions: &mut Vec<ListAction>,
    ) {
        let p = pal();
        let id = Id::new(("msgrow", m.id));
        let resp = ui.interact(rect.shrink2(vec2(4.0, 1.0)), id, Sense::click_and_drag());
        let hovered = resp.hovered() || resp.context_menu_opened();
        let unread = !m.seen || (count > 1 && unread_any && !expanded);
        let inner = rect.shrink2(vec2(6.0, 2.0));
        let bg = if selected {
            if hovered { p.selected_hover } else { p.selected }
        } else if hovered {
            p.hover
        } else if m.flagged {
            p.flagged_row
        } else if m.pinned {
            p.pinned_row
        } else {
            Color32::TRANSPARENT
        };
        ui.painter().rect_filled(inner, CornerRadius::same(6), bg);
        if unread {
            let bar = Rect::from_min_size(inner.left_top() + vec2(0.0, 6.0), vec2(3.0, inner.height() - 12.0));
            ui.painter().rect_filled(bar, CornerRadius::same(2), p.accent);
        }
        let pad = density().pad();
        let mut x = inner.left() + 12.0 + if child { 22.0 } else { 0.0 };
        let top = inner.top() + pad;

        // Thread chevron.
        if count > 1 && !child {
            let chev = Rect::from_min_size(Pos2::new(inner.left() + 4.0, top + 2.0), vec2(16.0, 16.0));
            let cr = ui.interact(chev.expand(3.0), id.with("chev"), Sense::click());
            ui.painter().text(chev.center(), Align2::CENTER_CENTER, if expanded { icons::CARET_DOWN } else { icons::CARET_RIGHT }, icon_font(12.0), p.text2);
            if cr.clicked() {
                actions.push(ListAction::ToggleThread(m.thread_id.clone()));
            }
            x += 6.0;
        }

        // Avatar / selection circle.
        if (show_avatars || select_mode) && !child {
            let c = Pos2::new(x + 16.0, top + 17.0);
            let ar = Rect::from_center_size(c, vec2(32.0, 32.0));
            let ahover = ui.rect_contains_pointer(ar);
            if select_mode || (ahover && hovered) || (selected && selection.len() > 1) {
                widgets::paint_round_check(ui.painter(), c, 11.0, selected, ahover, p.accent, p.text3);
                let cr = ui.interact(ar, id.with("check"), Sense::click());
                if cr.clicked() {
                    actions.push(ListAction::ToggleCheck(m.id));
                }
            } else {
                let (n, e) = if m.folder_is_outgoing() { first_recipient(&m.to) } else { (m.from_name.clone(), m.from_addr.clone()) };
                widgets::paint_avatar(ui.painter(), c, 16.0, &n, &e);
            }
            x += 44.0;
        }

        let right = inner.right() - 10.0;
        // Line 1: sender + date / hover actions.
        let date = util::fmt_list_date(m.snoozed_until.filter(|s| *s > m.date).unwrap_or(m.date));
        let date_g = ui.painter().layout_no_wrap(date, font(12.0), if unread { p.accent_text } else { p.text3 });
        let show_actions = hovered && !select_mode;
        let actions_w = if show_actions { 4.0 * 26.0 } else { date_g.size().x };
        let who = if m.folder_is_outgoing() {
            format!("To: {}", first_recipient(&m.to).0)
        } else {
            m.sender().to_string()
        };
        let who = if count > 1 && !child { format!("{who}  ({count})") } else { who };
        let sender_font = if unread { font_semibold(14.5) } else { font(14.5) };
        let g = elided(ui, &who, sender_font, p.text, right - x - actions_w - 8.0);
        let line1_y = top + 9.0;
        ui.painter().galley(Pos2::new(x, line1_y - g.size().y / 2.0), g, p.text);
        if show_actions {
            let mut bx = right - 4.0 * 26.0;
            let buttons = [
                (icons::TRASH, "Delete", 0),
                (if m.seen { icons::ENVELOPE_SIMPLE } else { icons::ENVELOPE_SIMPLE_OPEN }, if m.seen { "Mark as unread" } else { "Mark as read" }, 1),
                (icons::FLAG, if m.flagged { "Unflag" } else { "Flag" }, 2),
                (icons::PUSH_PIN, if m.pinned { "Unpin" } else { "Pin" }, 3),
            ];
            for (icon, tip, k) in buttons {
                let r = Rect::from_min_size(Pos2::new(bx, line1_y - 13.0), vec2(26.0, 26.0));
                let br = ui.interact(r, id.with(("act", k)), Sense::click()).on_hover_text(tip);
                if br.hovered() {
                    ui.painter().rect_filled(r, CornerRadius::same(4), if selected { p.selected } else { p.pressed });
                }
                let active = (k == 2 && m.flagged) || (k == 3 && m.pinned);
                let color = if k == 2 && m.flagged { p.flag } else if active { p.accent } else { p.text2 };
                ui.painter().text(r.center(), Align2::CENTER_CENTER, icon, if active { icon_fill_font(15.0) } else { icon_font(15.0) }, color);
                if br.clicked() {
                    let ids = vec![m.id];
                    actions.push(match k {
                        0 => ListAction::Cmd(MenuCmd::Delete, ids),
                        1 => ListAction::Cmd(MenuCmd::MarkRead(!m.seen), ids),
                        2 => ListAction::Cmd(MenuCmd::Flag(!m.flagged), ids),
                        _ => ListAction::Cmd(MenuCmd::Pin(!m.pinned), ids),
                    });
                }
                bx += 26.0;
            }
        } else {
            let w = date_g.size().x;
            ui.painter().galley(Pos2::new(right - w, line1_y - date_g.size().y / 2.0), date_g, p.text3);
        }

        // Line 2: subject + status icons.
        let line2_y = line1_y + 19.0;
        let mut icons_right = right;
        let mut status_icons: Vec<(&str, Color32, bool)> = Vec::new();
        if m.flagged {
            status_icons.push((icons::FLAG, p.flag, true));
        }
        if m.pinned {
            status_icons.push((icons::PUSH_PIN, p.accent, true));
        }
        if m.has_attachments {
            status_icons.push((icons::PAPERCLIP, p.text2, false));
        }
        if m.importance == Importance::High {
            status_icons.push((icons::WARNING_CIRCLE, p.danger, true));
        }
        if m.answered {
            status_icons.push((icons::ARROW_BEND_UP_LEFT, p.text3, false));
        } else if m.forwarded {
            status_icons.push((icons::ARROW_BEND_UP_RIGHT, p.text3, false));
        }
        for (icon, color, fill) in status_icons.iter().rev() {
            ui.painter().text(
                Pos2::new(icons_right - 8.0, line2_y),
                Align2::CENTER_CENTER,
                *icon,
                if *fill { icon_fill_font(14.0) } else { icon_font(14.0) },
                *color,
            );
            icons_right -= 19.0;
        }
        for cat in m.categories.iter().take(3) {
            let c = self.cfg.category_color(cat);
            ui.painter().circle_filled(Pos2::new(icons_right - 6.0, line2_y), 4.5, c);
            icons_right -= 13.0;
        }
        let subj_color = if unread { p.accent_text } else { p.text };
        let subj_font = if unread { font_semibold(13.5) } else { font(13.5) };
        let mut subject = m.subject_or_placeholder().to_string();
        if m.draft {
            subject = format!("[Draft] {subject}");
        }
        let g = elided(ui, &subject, subj_font, subj_color, icons_right - x - 6.0);
        ui.painter().galley(Pos2::new(x, line2_y - g.size().y / 2.0), g, subj_color);

        // Line 3: preview.
        if show_preview && !child {
            let line3_y = line2_y + 19.0;
            let preview = if m.preview.is_empty() {
                if m.has_body { "(no text)".to_string() } else { String::new() }
            } else {
                m.preview.clone()
            };
            let g = elided(ui, &preview, font(13.0), p.text3, right - x);
            ui.painter().galley(Pos2::new(x, line3_y - g.size().y / 2.0), g, p.text3);
        }

        // Interactions.
        if resp.drag_started() {
            let ids = if selection.contains(&m.id) { selection.iter().copied().collect() } else { vec![m.id] };
            resp.dnd_set_drag_payload(DragMsgs(ids));
        }
        if resp.double_clicked() {
            actions.push(if m.draft { ListAction::Cmd(MenuCmd::EditDraft, vec![m.id]) } else { ListAction::Open(m.id) });
        } else if resp.clicked() {
            let mods = ui.input(|i| i.modifiers);
            actions.push(ListAction::Select(m.id, mods));
        }
        resp.context_menu(|ui| {
            let ids: Vec<MsgId> = if selection.contains(&m.id) { selection.iter().copied().collect() } else { vec![m.id] };
            let mut out = Vec::new();
            self.message_menu_items(ui, m, &mut out);
            for c in out {
                actions.push(ListAction::Cmd(c, ids.clone()));
            }
        });
    }

    /// Context menu content; pushes chosen commands into `out`.
    fn message_menu_items(&self, ui: &mut Ui, m: &MessageSummary, out: &mut Vec<MenuCmd>) {
        ui.set_min_width(220.0);
        if m.draft {
            if widgets::menu_item(ui, icons::PENCIL_SIMPLE, "Edit draft").clicked() {
                out.push(MenuCmd::EditDraft);
            }
        } else {
            if widgets::menu_item(ui, icons::ARROW_BEND_UP_LEFT, "Reply").clicked() {
                out.push(MenuCmd::Reply);
            }
            if widgets::menu_item(ui, icons::ARROW_BEND_DOUBLE_UP_LEFT, "Reply all").clicked() {
                out.push(MenuCmd::ReplyAll);
            }
            if widgets::menu_item(ui, icons::ARROW_BEND_UP_RIGHT, "Forward").clicked() {
                out.push(MenuCmd::Forward);
            }
        }
        ui.separator();
        if widgets::menu_item(ui, if m.seen { icons::ENVELOPE_SIMPLE } else { icons::ENVELOPE_SIMPLE_OPEN }, if m.seen { "Mark as unread" } else { "Mark as read" }).clicked() {
            out.push(MenuCmd::MarkRead(!m.seen));
        }
        if widgets::menu_item(ui, icons::FLAG, if m.flagged { "Clear flag" } else { "Flag" }).clicked() {
            out.push(MenuCmd::Flag(!m.flagged));
        }
        if widgets::menu_item(ui, icons::PUSH_PIN, if m.pinned { "Unpin" } else { "Pin" }).clicked() {
            out.push(MenuCmd::Pin(!m.pinned));
        }
        ui.menu_button(format!("{}   Categorize", icons::TAG), |ui| self.category_items(ui, &m.categories, out));
        ui.menu_button(format!("{}   Snooze", icons::CLOCK), |ui| snooze_items(ui, out));
        ui.separator();
        ui.menu_button(format!("{}   Move", icons::FOLDER_SIMPLE), |ui| self.move_items(ui, &m.account_id, out));
        if widgets::menu_item(ui, icons::ARCHIVE, "Archive").clicked() {
            out.push(MenuCmd::Archive);
        }
        if widgets::menu_item(ui, icons::TRASH, "Delete").clicked() {
            out.push(MenuCmd::Delete);
        }
        let in_junk = self.mail.find_folder(m.folder_id).map(|f| f.role == FolderRole::Junk).unwrap_or(false);
        if widgets::menu_item(ui, icons::PROHIBIT, if in_junk { "Not junk" } else { "Report junk" }).clicked() {
            out.push(MenuCmd::Junk(!in_junk));
        }
        if widgets::menu_item(ui, icons::SHIELD_CHECK, "Block sender").clicked() {
            out.push(MenuCmd::BlockSender);
        }
        if self.cfg.mail.focused_inbox {
            let label = if m.focused { "Always move to Other" } else { "Always move to Focused" };
            if widgets::menu_item(ui, icons::TRAY, label).clicked() {
                out.push(MenuCmd::AlwaysFocused(!m.focused));
            }
        }
        ui.separator();
        if widgets::menu_item(ui, icons::ARROW_SQUARE_OUT, "Open in full view").clicked() {
            out.push(MenuCmd::OpenFull);
        }
        if widgets::menu_item(ui, icons::CODE, "View message source").clicked() {
            out.push(MenuCmd::ViewSource);
        }
        if widgets::menu_item(ui, icons::DOWNLOAD_SIMPLE, "Save as .eml…").clicked() {
            out.push(MenuCmd::SaveEml);
        }
    }

    fn category_items(&self, ui: &mut Ui, current: &[String], out: &mut Vec<MenuCmd>) {
        let p = pal();
        for c in &self.cfg.categories {
            let on = current.contains(&c.name);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(200.0), 30.0), Sense::click());
            if resp.hovered() {
                ui.painter().rect_filled(rect, CornerRadius::same(4), p.hover);
            }
            ui.painter().circle_filled(Pos2::new(rect.left() + 16.0, rect.center().y), 6.0, theme::category_color(&c.color));
            ui.painter().text(Pos2::new(rect.left() + 34.0, rect.center().y), Align2::LEFT_CENTER, &c.name, font(14.0), p.text);
            if on {
                ui.painter().text(Pos2::new(rect.right() - 14.0, rect.center().y), Align2::CENTER_CENTER, icons::CHECK, icon_font(14.0), p.accent);
            }
            if resp.clicked() {
                out.push(MenuCmd::Category(c.name.clone()));
                ui.close();
            }
        }
        ui.separator();
        if widgets::menu_item(ui, icons::X, "Clear categories").clicked() {
            out.push(MenuCmd::ClearCategories);
        }
    }

    fn move_items(&self, ui: &mut Ui, account_id: &str, out: &mut Vec<MenuCmd>) {
        let folders = self.mail.folders.get(account_id).cloned().unwrap_or_default();
        ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for (f, depth, _) in folder_tree(&folders) {
                if !f.selectable {
                    continue;
                }
                let label = format!("{}{}", "   ".repeat(depth), f.label());
                if widgets::menu_item(ui, f.role.icon(), &label).clicked() {
                    out.push(MenuCmd::MoveTo(f.id));
                }
            }
        });
        ui.separator();
        if widgets::menu_item(ui, icons::MAGNIFYING_GLASS, "Other folder…").clicked() {
            out.push(MenuCmd::MoveDialog);
        }
    }

    pub fn snooze_menu(&mut self, ui: &mut Ui, ids: &[MsgId]) {
        let mut out = Vec::new();
        snooze_items(ui, &mut out);
        for c in out {
            self.apply_cmd(c, ids);
        }
    }

    pub fn categorize_menu(&mut self, ui: &mut Ui, ids: &[MsgId]) {
        let current = self
            .store
            .messages_by_ids(ids)
            .ok()
            .and_then(|m| m.first().map(|m| m.categories.clone()))
            .unwrap_or_default();
        let mut out = Vec::new();
        self.category_items(ui, &current, &mut out);
        for c in out {
            self.apply_cmd(c, ids);
        }
    }

    pub fn move_menu(&mut self, ui: &mut Ui, ids: &[MsgId]) {
        let account = self
            .store
            .messages_by_ids(ids)
            .ok()
            .and_then(|m| m.first().map(|m| m.account_id.clone()))
            .unwrap_or_default();
        let mut out = Vec::new();
        self.move_items(ui, &account, &mut out);
        for c in out {
            self.apply_cmd(c, ids);
        }
    }

    pub fn apply_cmd(&mut self, cmd: MenuCmd, ids: &[MsgId]) {
        if ids.is_empty() {
            return;
        }
        match cmd {
            MenuCmd::Reply => self.reply(ids[0], false),
            MenuCmd::ReplyAll => self.reply(ids[0], true),
            MenuCmd::Forward => self.forward(ids[0]),
            MenuCmd::MarkRead(v) => self.set_seen(ids, v),
            MenuCmd::Flag(v) => self.set_flag(ids, v),
            MenuCmd::Pin(v) => {
                let _ = self.store.set_bool(ids, "pinned", v);
                self.mail.list_dirty = true;
            }
            MenuCmd::Category(c) => {
                for m in self.store.messages_by_ids(ids).unwrap_or_default() {
                    let mut cats = m.categories.clone();
                    if let Some(pos) = cats.iter().position(|x| *x == c) {
                        cats.remove(pos);
                    } else {
                        cats.push(c.clone());
                    }
                    let _ = self.store.set_categories(m.id, &cats);
                }
                self.mail.list_dirty = true;
                self.mail.reading_flags_dirty = true;
            }
            MenuCmd::ClearCategories => {
                for id in ids {
                    let _ = self.store.set_categories(*id, &[]);
                }
                self.mail.list_dirty = true;
                self.mail.reading_flags_dirty = true;
            }
            MenuCmd::Snooze(until) => {
                let _ = self.store.set_snooze(ids, until);
                if until.is_some() {
                    self.mail.selection.retain(|x| !ids.contains(x));
                    if self.mail.reading.as_ref().map(|r| ids.contains(&r.msg_id)).unwrap_or(false) {
                        self.mail.reading = None;
                    }
                    let when = until.map(|u| util::fmt_full_date(u)).unwrap_or_default();
                    self.toast(format!("Snoozed until {when}"), ToastKind::Info);
                }
                self.mail.list_dirty = true;
                self.mail.folders_dirty = true;
            }
            MenuCmd::MoveTo(fid) => self.move_messages(ids, fid),
            MenuCmd::MoveDialog => {
                self.dialog = Some(Dialog::MoveTo { ids: ids.to_vec(), filter: String::new() });
            }
            MenuCmd::Archive => self.archive_messages(ids),
            MenuCmd::Delete => self.delete_messages(ids, false),
            MenuCmd::Junk(to_junk) => self.junk_messages(ids, !to_junk),
            MenuCmd::BlockSender => self.block_sender(ids),
            MenuCmd::AlwaysFocused(focused) => {
                for m in self.store.messages_by_ids(ids).unwrap_or_default() {
                    self.cfg.mail.focused_overrides.insert(m.from_addr.to_lowercase(), focused);
                    let _ = self.store.conn.execute(
                        "UPDATE messages SET focused = ?2 WHERE LOWER(from_addr) = ?1",
                        rusqlite::params![m.from_addr.to_lowercase(), focused],
                    );
                }
                self.mark_config_dirty();
                self.mail.list_dirty = true;
                self.toast(
                    if focused { "Messages from this sender will go to Focused" } else { "Messages from this sender will go to Other" },
                    ToastKind::Info,
                );
            }
            MenuCmd::ViewSource => {
                if let Ok(Some(raw)) = self.store.raw_body(ids[0]) {
                    let text = String::from_utf8_lossy(&raw).to_string();
                    let title = self.store.message(ids[0]).ok().flatten().map(|m| m.subject).unwrap_or_default();
                    self.dialog = Some(Dialog::Source { title: format!("Source: {}", util::truncate_chars(&title, 50)), text });
                } else {
                    self.engine.send(Command::FetchBody { msg_id: ids[0] });
                    self.toast("Downloading message… try again in a moment", ToastKind::Info);
                }
            }
            MenuCmd::SaveEml => self.save_eml(ids[0]),
            MenuCmd::EditDraft => self.edit_draft(ids[0]),
            MenuCmd::OpenFull => self.open_message_full(ids[0]),
        }
    }

    // ============================================================= selection

    pub fn select_message(&mut self, id: MsgId, mods: Modifiers) {
        if mods.command {
            if let Some(pos) = self.mail.selection.iter().position(|x| *x == id) {
                self.mail.selection.remove(pos);
            } else {
                self.mail.selection.push(id);
            }
            self.mail.anchor = Some(id);
            if self.mail.selection.len() == 1 {
                let only = self.mail.selection[0];
                self.open_reading(only);
            }
            return;
        }
        if mods.shift {
            if let Some(anchor) = self.mail.anchor {
                let vis = self.mail.visible_ids();
                if let (Some(a), Some(b)) = (vis.iter().position(|x| *x == anchor), vis.iter().position(|x| *x == id)) {
                    let (s, e) = if a <= b { (a, b) } else { (b, a) };
                    self.mail.selection = vis[s..=e].to_vec();
                    return;
                }
            }
        }
        self.mail.selection = vec![id];
        self.mail.anchor = Some(id);
        self.mail.compose_inline = self.mail.compose_inline.take().filter(|c| c.is_dirty());
        if let Some(c) = self.mail.compose_inline.take() {
            // Keep work: move a started message into its own window.
            let mut c = c;
            c.popped_out = true;
            self.composes.push(c);
        }
        self.open_reading(id);
    }

    pub fn move_selection(&mut self, delta: i32) {
        let vis = self.mail.visible_ids();
        if vis.is_empty() {
            return;
        }
        let cur = self.mail.selection.last().and_then(|id| vis.iter().position(|x| x == id));
        let next = match cur {
            Some(i) => (i as i32 + delta).clamp(0, vis.len() as i32 - 1) as usize,
            None => 0,
        };
        self.mail.scroll_to_selected = true;
        self.select_message(vis[next], Modifiers::NONE);
    }

    pub fn open_message_full(&mut self, id: MsgId) {
        if let Ok(Some(m)) = self.store.message(id) {
            if m.draft {
                self.edit_draft(id);
                return;
            }
        }
        self.mail.selection = vec![id];
        self.open_reading(id);
        if self.cfg.mail.reading_pane == ReadingPanePos::Off {
            self.mail.full_view = true;
        }
    }

    pub fn open_reading(&mut self, id: MsgId) {
        let Ok(Some(m)) = self.store.message(id) else { return };
        let mut items: Vec<MessageSummary> = if self.cfg.mail.conversation_view {
            let mut t = self.store.thread_messages(&m.account_id, &m.thread_id).unwrap_or_default();
            // Hide copies in Trash/Junk unless that's where we are.
            let here = self.mail.find_folder(m.folder_id).map(|f| f.role);
            t.retain(|x| {
                x.id == m.id
                    || !matches!(self.mail.find_folder(x.folder_id).map(|f| f.role), Some(FolderRole::Trash) | Some(FolderRole::Junk))
                    || here == self.mail.find_folder(x.folder_id).map(|f| f.role)
            });
            // De-duplicate by Message-ID, preferring the selected copy.
            let mut seen_ids: HashSet<String> = HashSet::new();
            let mut out = Vec::new();
            let mut sorted = t;
            sorted.sort_by_key(|x| if x.id == m.id { 0 } else { 1 });
            for x in sorted {
                if !x.message_id.is_empty() && !seen_ids.insert(x.message_id.clone()) {
                    continue;
                }
                out.push(x);
            }
            out.sort_by_key(|x| x.date);
            if out.is_empty() {
                vec![m.clone()]
            } else {
                out
            }
        } else {
            vec![m.clone()]
        };
        if items.len() > 60 {
            let keep = items.len() - 60;
            items.drain(0..keep);
        }
        let subject = m.subject_or_placeholder().to_string();
        let n = items.len();
        let mut reading = Reading {
            msg_id: id,
            items: items
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    let expanded = s.id == id || !s.seen || i + 1 == n;
                    ReadItem {
                        summary: s,
                        parsed: None,
                        doc: None,
                        cid: HashMap::new(),
                        loading: false,
                        error: None,
                        expanded,
                        invite: None,
                    }
                })
                .collect(),
            show_images: false,
            subject,
        };
        let sender = m.from_addr.to_lowercase();
        reading.show_images = match self.cfg.mail.images {
            ImagePolicy::Always => true,
            ImagePolicy::Contacts => {
                self.cfg.mail.safe_senders.iter().any(|s| s.eq_ignore_ascii_case(&sender))
                    || self.store.contact_by_email(&sender).ok().flatten().map(|c| !c.auto).unwrap_or(false)
            }
            ImagePolicy::Block => self.cfg.mail.safe_senders.iter().any(|s| s.eq_ignore_ascii_case(&sender)),
        };
        self.mail.reading = Some(reading);
        let ids: Vec<MsgId> = self
            .mail
            .reading
            .as_ref()
            .map(|r| r.items.iter().filter(|i| i.expanded).map(|i| i.summary.id).collect())
            .unwrap_or_default();
        for i in ids {
            self.ensure_body(i);
        }
        self.on_message_opened(&m);
    }

    fn ensure_body(&mut self, id: MsgId) {
        let raw = self.store.raw_body(id).ok().flatten();
        let Some(r) = self.mail.reading.as_mut() else { return };
        let Some(item) = r.items.iter_mut().find(|i| i.summary.id == id) else { return };
        if item.parsed.is_some() {
            return;
        }
        match raw {
            Some(raw) => {
                let parsed = mime::parse_full(&raw);
                match parsed {
                    Some(p) => {
                        let doc = match &p.html {
                            Some(h) => html::parse_html(h),
                            None => html::from_plain_text(p.text.as_deref().unwrap_or("")),
                        };
                        item.invite = p
                            .calendar
                            .as_deref()
                            .map(cal::parse_ics)
                            .filter(|i| !i.events.is_empty());
                        item.cid = register_images(&self.ctx, id, &raw, &doc);
                        item.doc = Some(Arc::new(doc));
                        item.parsed = Some(Arc::new(p));
                        item.loading = false;
                        item.error = None;
                    }
                    None => {
                        item.error = Some("This message could not be read.".into());
                    }
                }
            }
            None => {
                if !item.loading {
                    item.loading = true;
                    item.error = None;
                    self.engine.send(Command::FetchBody { msg_id: id });
                }
            }
        }
    }

    pub fn body_ready(&mut self, _ctx: &egui::Context, id: MsgId) {
        if let Some(r) = self.mail.reading.as_mut() {
            if let Some(item) = r.items.iter_mut().find(|i| i.summary.id == id) {
                item.parsed = None;
                item.loading = false;
                if let Ok(Some(s)) = self.store.message(id) {
                    item.summary = s;
                }
            }
        }
        self.ensure_body(id);
        if let Some(c) = self.mail.compose_inline.as_mut() {
            c.on_body_ready(&self.store, id);
        }
        for c in self.composes.iter_mut() {
            c.on_body_ready(&self.store, id);
        }
    }

    fn refresh_reading_flags(&mut self) {
        self.mail.reading_flags_dirty = false;
        let Some(r) = self.mail.reading.as_mut() else { return };
        let ids: Vec<MsgId> = r.items.iter().map(|i| i.summary.id).collect();
        let fresh = self.store.messages_by_ids(&ids).unwrap_or_default();
        r.items.retain(|i| fresh.iter().any(|f| f.id == i.summary.id));
        for it in r.items.iter_mut() {
            if let Some(f) = fresh.iter().find(|f| f.id == it.summary.id) {
                it.summary = f.clone();
            }
        }
        if r.items.is_empty() {
            self.mail.reading = None;
        }
    }

    // =============================================================== actions

    pub fn set_flag(&mut self, ids: &[MsgId], value: bool) {
        let msgs = self.store.messages_by_ids(ids).unwrap_or_default();
        let _ = self.store.set_bool(ids, "flagged", value);
        for m in &msgs {
            let _ = self.store.set_flag_due(m.id, if value { Some(util::local_midnight(util::today()) + 86400 * 2 - 1) } else { None });
        }
        let mut by_acc: BTreeMap<String, Vec<MsgRef>> = BTreeMap::new();
        for m in msgs.iter().filter(|m| m.flagged != value) {
            by_acc.entry(m.account_id.clone()).or_default().push(MsgRef::of(m));
        }
        for (account_id, items) in by_acc {
            self.engine.send(Command::SetFlag { account_id, items, flag: FlagKind::Flagged, value });
        }
        self.mail.list_dirty = true;
        self.mail.reading_flags_dirty = true;
        self.todo.dirty = true;
    }

    pub fn toggle_flag(&mut self, ids: &[MsgId]) {
        let msgs = self.store.messages_by_ids(ids).unwrap_or_default();
        let all = !msgs.is_empty() && msgs.iter().all(|m| m.flagged);
        self.set_flag(ids, !all);
    }

    pub fn toggle_pin(&mut self, ids: &[MsgId]) {
        let msgs = self.store.messages_by_ids(ids).unwrap_or_default();
        let all = !msgs.is_empty() && msgs.iter().all(|m| m.pinned);
        let _ = self.store.set_bool(ids, "pinned", !all);
        self.mail.list_dirty = true;
    }

    fn next_after(&self, ids: &[MsgId]) -> Option<MsgId> {
        let vis = self.mail.visible_ids();
        let last = vis.iter().rposition(|x| ids.contains(x))?;
        vis[last + 1..]
            .iter()
            .find(|x| !ids.contains(x))
            .copied()
            .or_else(|| vis[..last].iter().rev().find(|x| !ids.contains(x)).copied())
    }

    fn after_removal(&mut self, ids: &[MsgId]) {
        let next = self.next_after(ids);
        self.mail.selection.retain(|x| !ids.contains(x));
        let reading_removed = self.mail.reading.as_ref().map(|r| ids.contains(&r.msg_id)).unwrap_or(false);
        if reading_removed {
            self.mail.reading = None;
            if let Some(n) = next {
                self.mail.selection = vec![n];
                self.mail.anchor = Some(n);
                self.mail.scroll_to_selected = true;
                // Open without marking read in bulk; Outlook opens the next item.
                self.open_reading(n);
            }
        }
        self.mail.list_dirty = true;
        self.mail.folders_dirty = true;
    }

    /// Moves messages locally right away; the server move runs after the undo window.
    pub fn move_messages(&mut self, ids: &[MsgId], dest: FolderId) {
        self.flush_pending();
        let msgs: Vec<MessageSummary> = self
            .store
            .messages_by_ids(ids)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.folder_id != dest)
            .collect();
        if msgs.is_empty() {
            return;
        }
        let Some(dest_f) = self.store.folder(dest).ok().flatten() else { return };
        let msgs: Vec<MessageSummary> = msgs.into_iter().filter(|m| m.account_id == dest_f.account_id).collect();
        if msgs.is_empty() {
            self.toast("Messages can only be moved within the same account", ToastKind::Error);
            return;
        }
        let local = self.cfg.account(&dest_f.account_id).map(|a| a.is_local()).unwrap_or(false);
        let restore: Vec<(MsgId, FolderId, i64)> = msgs.iter().map(|m| (m.id, m.folder_id, m.uid)).collect();
        let items: Vec<MsgRef> = msgs.iter().map(MsgRef::of).collect();
        let mids: Vec<MsgId> = msgs.iter().map(|m| m.id).collect();
        self.after_removal(&mids);
        let _ = self.store.move_local(&mids, dest, local);
        let n = mids.len();
        self.defer(
            Command::Move { account_id: dest_f.account_id.clone(), items, dest },
            restore,
            format!("{} moved to {}", if n == 1 { "Message".to_string() } else { format!("{n} messages") }, dest_f.label()),
        );
    }

    pub fn delete_messages(&mut self, ids: &[MsgId], permanent: bool) {
        self.flush_pending();
        let msgs = self.store.messages_by_ids(ids).unwrap_or_default();
        if msgs.is_empty() {
            return;
        }
        let mut by_acc: BTreeMap<String, Vec<MessageSummary>> = BTreeMap::new();
        for m in msgs {
            by_acc.entry(m.account_id.clone()).or_default().push(m);
        }
        let mids: Vec<MsgId> = by_acc.values().flatten().map(|m| m.id).collect();
        self.after_removal(&mids);
        for (account_id, msgs) in by_acc {
            let trash = self.store.folder_by_role(&account_id, FolderRole::Trash).ok().flatten();
            let local = self.cfg.account(&account_id).map(|a| a.is_local()).unwrap_or(false);
            let restore: Vec<(MsgId, FolderId, i64)> = msgs.iter().map(|m| (m.id, m.folder_id, m.uid)).collect();
            let items: Vec<MsgRef> = msgs.iter().map(MsgRef::of).collect();
            // Without a Trash folder the engine creates one; never expunge by accident.
            let forever = permanent || trash.as_ref().map(|t| msgs.iter().all(|m| m.folder_id == t.id)).unwrap_or(local);
            let n = msgs.len();
            let ids: Vec<MsgId> = msgs.iter().map(|m| m.id).collect();
            if forever {
                if local {
                    let _ = self.store.delete_messages(&ids);
                } else {
                    // Hidden until the server confirms; restored if it fails.
                    let _ = self.store.hide(&ids);
                }
                self.engine.send(Command::Delete { account_id, items, permanent: true });
                self.toast(
                    format!("{} permanently deleted", if n == 1 { "Message".to_string() } else { format!("{n} messages") }),
                    ToastKind::Info,
                );
            } else {
                match &trash {
                    Some(t) => {
                        let _ = self.store.move_local(&ids, t.id, local);
                    }
                    None => {
                        let _ = self.store.hide(&ids);
                    }
                }
                self.defer(
                    Command::Delete { account_id, items, permanent: false },
                    restore,
                    format!("{} deleted", if n == 1 { "Message".to_string() } else { format!("{n} messages") }),
                );
            }
        }
    }

    pub fn archive_messages(&mut self, ids: &[MsgId]) {
        self.flush_pending();
        let msgs = self.store.messages_by_ids(ids).unwrap_or_default();
        if msgs.is_empty() {
            return;
        }
        let mut by_acc: BTreeMap<String, Vec<MessageSummary>> = BTreeMap::new();
        for m in msgs {
            by_acc.entry(m.account_id.clone()).or_default().push(m);
        }
        let mids: Vec<MsgId> = by_acc.values().flatten().map(|m| m.id).collect();
        self.after_removal(&mids);
        for (account_id, msgs) in by_acc {
            let archive = self
                .store
                .folder_by_role(&account_id, FolderRole::Archive)
                .ok()
                .flatten()
                .or_else(|| self.store.folder_by_role(&account_id, FolderRole::All).ok().flatten());
            let local = self.cfg.account(&account_id).map(|a| a.is_local()).unwrap_or(false);
            let msgs: Vec<MessageSummary> = msgs
                .into_iter()
                .filter(|m| archive.as_ref().map(|a| a.id != m.folder_id).unwrap_or(true))
                .collect();
            if msgs.is_empty() {
                continue;
            }
            let restore: Vec<(MsgId, FolderId, i64)> = msgs.iter().map(|m| (m.id, m.folder_id, m.uid)).collect();
            let items: Vec<MsgRef> = msgs.iter().map(MsgRef::of).collect();
            let ids: Vec<MsgId> = msgs.iter().map(|m| m.id).collect();
            match &archive {
                Some(a) => {
                    let _ = self.store.move_local(&ids, a.id, local);
                }
                None => {
                    // The engine creates an Archive folder; hide meanwhile.
                    let _ = self.store.hide(&ids);
                }
            }
            let n = ids.len();
            self.defer(
                Command::Archive { account_id, items },
                restore,
                format!("{} archived", if n == 1 { "Message".to_string() } else { format!("{n} messages") }),
            );
        }
    }

    pub fn junk_messages(&mut self, ids: &[MsgId], not_junk: bool) {
        self.flush_pending();
        let msgs = self.store.messages_by_ids(ids).unwrap_or_default();
        let Some(first) = msgs.first() else { return };
        let account_id = first.account_id.clone();
        let msgs: Vec<MessageSummary> = msgs.into_iter().filter(|m| m.account_id == account_id).collect();
        let dest_role = if not_junk { FolderRole::Inbox } else { FolderRole::Junk };
        let dest = self.store.folder_by_role(&account_id, dest_role).ok().flatten();
        let local = self.cfg.account(&account_id).map(|a| a.is_local()).unwrap_or(false);
        let restore: Vec<(MsgId, FolderId, i64)> = msgs.iter().map(|m| (m.id, m.folder_id, m.uid)).collect();
        let items: Vec<MsgRef> = msgs.iter().map(MsgRef::of).collect();
        let mids: Vec<MsgId> = msgs.iter().map(|m| m.id).collect();
        self.after_removal(&mids);
        match &dest {
            Some(d) => {
                let _ = self.store.move_local(&mids, d.id, local);
            }
            None => {
                let _ = self.store.hide(&mids);
            }
        }
        if not_junk {
            for m in &msgs {
                if !self.cfg.mail.safe_senders.iter().any(|s| s.eq_ignore_ascii_case(&m.from_addr)) {
                    self.cfg.mail.safe_senders.push(m.from_addr.to_lowercase());
                }
            }
            self.mark_config_dirty();
        }
        self.defer(
            Command::Junk { account_id, items, not_junk },
            restore,
            if not_junk { "Moved to Inbox".into() } else { "Reported as junk".into() },
        );
    }

    pub fn block_sender(&mut self, ids: &[MsgId]) {
        let msgs = self.store.messages_by_ids(ids).unwrap_or_default();
        for m in &msgs {
            let addr = m.from_addr.to_lowercase();
            if !addr.is_empty() && !self.cfg.mail.blocked_senders.contains(&addr) {
                self.cfg.mail.blocked_senders.push(addr);
            }
        }
        self.mark_config_dirty();
        self.junk_messages(ids, false);
        self.toast("Sender blocked. Future messages will be marked as read.", ToastKind::Info);
    }

    pub fn sweep(&mut self, ids: &[MsgId]) {
        let Some(m) = self.store.messages_by_ids(ids).ok().and_then(|v| v.into_iter().next()) else { return };
        let all: Vec<MsgId> = self
            .store
            .list_messages(&ListQuery {
                folders: vec![m.folder_id],
                limit: 5000,
                ..Default::default()
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|x| x.from_addr.eq_ignore_ascii_case(&m.from_addr))
            .map(|x| x.id)
            .collect();
        self.delete_messages(&all, false);
    }

    pub fn mark_folder_read(&mut self, fid: FolderId) {
        let ids: Vec<MsgId> = self
            .store
            .list_messages(&ListQuery {
                folders: vec![fid],
                unread_only: true,
                limit: 20000,
                ..Default::default()
            })
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.id)
            .collect();
        self.set_seen(&ids, true);
    }

    fn save_eml(&mut self, id: MsgId) {
        let Ok(Some(raw)) = self.store.raw_body(id) else {
            self.engine.send(Command::FetchBody { msg_id: id });
            self.toast("Downloading message… try again in a moment", ToastKind::Info);
            return;
        };
        let subject = self.store.message(id).ok().flatten().map(|m| m.subject).unwrap_or_default();
        let name = format!("{}.eml", util::sanitize_filename(if subject.is_empty() { "message" } else { &subject }));
        if let Some(path) = rfd::FileDialog::new().set_file_name(&name).save_file() {
            match std::fs::write(&path, raw) {
                Ok(()) => self.toast(format!("Saved {}", path.display()), ToastKind::Success),
                Err(e) => self.toast(format!("Could not save: {e}"), ToastKind::Error),
            };
        }
    }

    // ========================================================== reading pane

    fn reading_pane(&mut self, ui: &mut Ui) {
        if self.mail.compose_inline.is_some() {
            self.compose_inline_ui(ui);
            return;
        }
        let p = pal();
        if self.mail.selection.len() > 1 {
            let n = self.mail.selection.len();
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.25);
                let (r, _) = ui.allocate_exact_size(vec2(96.0, 96.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 46.0, p.accent_soft);
                ui.painter().text(r.center(), Align2::CENTER_CENTER, icons::CHECKS, icon_fill_font(40.0), p.accent);
                ui.add_space(12.0);
                widgets::heading(ui, &format!("{n} items selected"), 18.0);
                ui.add_space(14.0);
                let sel = self.mail.selection.clone();
                ui.horizontal(|ui| {
                    let w = 5.0 * 112.0;
                    ui.add_space((ui.available_width() - w).max(0.0) / 2.0);
                    if widgets::secondary_button(ui, Some(icons::TRASH), "Delete").clicked() {
                        self.delete_messages(&sel, false);
                    }
                    if widgets::secondary_button(ui, Some(icons::ARCHIVE), "Archive").clicked() {
                        self.archive_messages(&sel);
                    }
                    if widgets::secondary_button(ui, Some(icons::ENVELOPE_SIMPLE_OPEN), "Mark read").clicked() {
                        self.set_seen(&sel, true);
                    }
                    if widgets::secondary_button(ui, Some(icons::FLAG), "Flag").clicked() {
                        self.toggle_flag(&sel);
                    }
                    if widgets::secondary_button(ui, Some(icons::FOLDER_SIMPLE), "Move").clicked() {
                        self.dialog = Some(Dialog::MoveTo { ids: sel.clone(), filter: String::new() });
                    }
                });
                ui.add_space(10.0);
                if widgets::link(ui, "Clear selection").clicked() {
                    self.mail.selection.clear();
                }
            });
            return;
        }
        let Some(mut reading) = self.mail.reading.take() else {
            widgets::empty_state(ui, icons::ENVELOPE_SIMPLE_OPEN, "Select an item to read", "Nothing is selected");
            return;
        };
        let mut acts: Vec<ReadAction> = Vec::new();
        let mut expand_requests: Vec<MsgId> = Vec::new();
        ScrollArea::vertical()
            .id_salt(("reading", reading.msg_id))
            .auto_shrink([false, false])
            .show(ui, |ui| {
                Frame::NONE
                    .inner_margin(Margin { left: 24, right: 24, top: 18, bottom: 28 })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let selected = reading.items.iter().find(|i| i.summary.id == reading.msg_id).map(|i| i.summary.clone());
                        ui.add(egui::Label::new(RichText::new(&reading.subject).font(font_semibold(21.0)).color(p.text)).wrap());
                        if let Some(s) = &selected {
                            if !s.categories.is_empty() || s.importance == Importance::High || reading.items.len() > 1 {
                                ui.add_space(4.0);
                                ui.horizontal_wrapped(|ui| {
                                    for c in &s.categories {
                                        widgets::chip(ui, c, self.cfg.category_color(c));
                                    }
                                    if reading.items.len() > 1 {
                                        ui.label(RichText::new(format!("{}  {} messages", icons::CHAT_CIRCLE, reading.items.len())).font(icon_font(12.5)).color(p.text3));
                                    }
                                });
                            }
                        }
                        ui.add_space(12.0);
                        let n = reading.items.len();
                        let show_images = reading.show_images;
                        for k in (0..n).rev() {
                            let is_last = k + 1 == n;
                            let item = &mut reading.items[k];
                            if item.expanded {
                                self.expanded_card(ui, item, show_images, is_last, &mut acts);
                            } else if self.collapsed_card(ui, item) {
                                item.expanded = true;
                                expand_requests.push(item.summary.id);
                            }
                            ui.add_space(10.0);
                        }
                    });
            });
        let show_images_now = acts.iter().any(|a| matches!(a, ReadAction::TrustSender(_)));
        if show_images_now {
            reading.show_images = true;
        }
        if acts.iter().any(|a| matches!(a, ReadAction::OpenLink(l) if l == "#show-images")) {
            reading.show_images = true;
        }
        if self.mail.reading.is_none() {
            self.mail.reading = Some(reading);
        }
        for id in expand_requests {
            self.ensure_body(id);
        }
        for a in acts {
            self.apply_read_action(a);
        }
    }

    fn collapsed_card(&self, ui: &mut Ui, item: &ReadItem) -> bool {
        let p = pal();
        let s = &item.summary;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
        let bg = if resp.hovered() { p.hover } else { p.card_alt };
        ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
        ui.painter().rect_stroke(rect, CornerRadius::same(8), Stroke::new(1.0, p.stroke), egui::StrokeKind::Inside);
        widgets::paint_avatar(ui.painter(), Pos2::new(rect.left() + 28.0, rect.center().y), 15.0, &s.from_name, &s.from_addr);
        let date = util::fmt_list_date(s.date);
        let dg = ui.painter().layout_no_wrap(date, font(12.0), p.text3);
        let x = rect.left() + 54.0;
        let name = elided(ui, s.sender(), if s.seen { font_semibold(14.0) } else { font_bold(14.0) }, p.text, 220.0);
        let nw = name.size().x;
        ui.painter().galley(Pos2::new(x, rect.center().y - name.size().y / 2.0), name, p.text);
        let prev = elided(ui, &s.preview, font(13.0), p.text3, rect.right() - x - nw - dg.size().x - 40.0);
        ui.painter().galley(Pos2::new(x + nw + 12.0, rect.center().y - prev.size().y / 2.0), prev, p.text3);
        ui.painter().galley(Pos2::new(rect.right() - 14.0 - dg.size().x, rect.center().y - dg.size().y / 2.0), dg, p.text3);
        resp.on_hover_cursor(CursorIcon::PointingHand).clicked()
    }

    fn expanded_card(&self, ui: &mut Ui, item: &mut ReadItem, show_images: bool, is_last: bool, acts: &mut Vec<ReadAction>) {
        let p = pal();
        let s = item.summary.clone();
        let ids = vec![s.id];
        Frame::NONE
            .fill(p.card)
            .stroke(Stroke::new(1.0, p.stroke))
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin { left: 18, right: 14, top: 14, bottom: 18 })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                // Header
                ui.horizontal(|ui| {
                    let outgoing = s.folder_is_outgoing();
                    widgets::avatar(ui, 40.0, &s.from_name, &s.from_addr);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 1.0;
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(s.sender()).font(font_semibold(15.0)).color(p.text));
                            if !s.from_addr.is_empty() && s.from_addr != s.sender() {
                                let r = ui.add(egui::Label::new(RichText::new(format!("<{}>", s.from_addr)).font(font(13.0)).color(p.text3)).sense(Sense::click()));
                                if r.on_hover_text("Click to copy").clicked() {
                                    ui.ctx().copy_text(s.from_addr.clone());
                                }
                            }
                        });
                        let to = item.parsed.as_ref().map(|pm| Addr::list_to_string(&pm.to)).unwrap_or_else(|| s.to.clone());
                        let to_short = summarize_recipients(&to);
                        ui.label(RichText::new(format!("To: {to_short}")).font(font(12.5)).color(p.text2)).on_hover_text(&to);
                        if let Some(pm) = &item.parsed {
                            if !pm.cc.is_empty() {
                                let cc = Addr::list_to_string(&pm.cc);
                                ui.label(RichText::new(format!("Cc: {}", summarize_recipients(&cc))).font(font(12.5)).color(p.text2)).on_hover_text(&cc);
                            }
                        }
                        let _ = outgoing;
                    });
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        ui.vertical(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                let more = widgets::icon_button(ui, icons::DOTS_THREE, "More actions");
                                egui::Popup::menu(&more).width(240.0).show(|ui| {
                                    let mut out = Vec::new();
                                    self.message_menu_items(ui, &s, &mut out);
                                    for c in out {
                                        acts.push(ReadAction::Cmd(c, ids.clone()));
                                    }
                                });
                                if widgets::icon_button(ui, icons::ARROW_BEND_UP_RIGHT, "Forward").clicked() {
                                    acts.push(ReadAction::Cmd(MenuCmd::Forward, ids.clone()));
                                }
                                if widgets::icon_button(ui, icons::ARROW_BEND_DOUBLE_UP_LEFT, "Reply all").clicked() {
                                    acts.push(ReadAction::Cmd(MenuCmd::ReplyAll, ids.clone()));
                                }
                                if widgets::icon_button(ui, icons::ARROW_BEND_UP_LEFT, "Reply").clicked() {
                                    acts.push(ReadAction::Cmd(MenuCmd::Reply, ids.clone()));
                                }
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(util::fmt_full_date(s.date)).font(font(12.0)).color(p.text3));
                                if s.flagged {
                                    ui.label(RichText::new(icons::FLAG).font(icon_fill_font(13.0)).color(p.flag));
                                }
                            });
                        });
                    });
                });
                ui.add_space(10.0);

                // Banners
                if s.importance == Importance::High {
                    widgets::banner(ui, icons::WARNING_CIRCLE, "This message was sent with High importance.", p.danger, p.danger_soft, |_| {});
                    ui.add_space(6.0);
                }
                if s.answered {
                    widgets::banner(ui, icons::ARROW_BEND_UP_LEFT, "You replied to this message.", p.accent_text, p.accent_soft, |_| {});
                    ui.add_space(6.0);
                }
                if let Some(folder) = self.mail.find_folder(s.folder_id) {
                    if folder.role == FolderRole::Junk {
                        widgets::banner(ui, icons::PROHIBIT, "This message is in Junk Email. Links and images are blocked.", p.warning, p.warning_soft, |ui| {
                            if widgets::link(ui, "It's not junk").clicked() {
                                acts.push(ReadAction::Cmd(MenuCmd::Junk(false), ids.clone()));
                            }
                        });
                        ui.add_space(6.0);
                    }
                }
                let remote = item.doc.as_ref().map(|d| d.remote_images).unwrap_or(0);
                if remote > 0 && !show_images {
                    widgets::banner(ui, icons::SHIELD_CHECK, "To protect your privacy, external images in this message were blocked.", p.accent_text, p.accent_soft, |ui| {
                        if widgets::link(ui, "Show images").clicked() {
                            acts.push(ReadAction::OpenLink("#show-images".into()));
                        }
                        ui.label(RichText::new("·").color(p.text3));
                        if widgets::link(ui, "Always show from this sender").clicked() {
                            acts.push(ReadAction::TrustSender(s.from_addr.clone()));
                        }
                    });
                    ui.add_space(6.0);
                }
                if let Some(u) = &s.list_unsubscribe {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icons::MEGAPHONE).font(icon_font(13.0)).color(p.text3));
                        ui.label(RichText::new("This looks like a newsletter or mailing list.").font(font(12.5)).color(p.text3));
                        if widgets::link(ui, "Unsubscribe").clicked() {
                            acts.push(ReadAction::Unsubscribe(u.clone()));
                        }
                    });
                    ui.add_space(6.0);
                }

                // Read receipt request
                if let Some(pm) = &item.parsed {
                    if let Some(to) = pm.read_receipt_to.as_ref() {
                        let mine = self.my_addresses().contains(&s.from_addr.to_lowercase());
                        let key = format!("receipt:{}", s.message_id);
                        if !mine && !s.draft && self.store.kv_get(&key).is_none() {
                            let to = to.clone();
                            widgets::banner(ui, icons::ENVELOPE_SIMPLE_OPEN, &format!("{} asked for a read receipt.", s.sender()), p.accent_text, p.accent_soft, |ui| {
                                if widgets::link(ui, "Send receipt").clicked() {
                                    acts.push(ReadAction::SendReceipt(s.id, to.clone()));
                                }
                                ui.label(RichText::new("·").color(p.text3));
                                if widgets::link(ui, "Don't send").clicked() {
                                    acts.push(ReadAction::DismissReceipt(key.clone()));
                                }
                            });
                            ui.add_space(6.0);
                        }
                    }
                }

                // Invitation
                if let Some(inv) = &item.invite {
                    self.invite_card(ui, &s, inv, acts);
                    ui.add_space(10.0);
                }

                // Body
                if item.loading {
                    ui.horizontal(|ui| {
                        widgets::spinner(ui, 16.0);
                        ui.label(RichText::new("Downloading message…").color(p.text3));
                    });
                } else if let Some(err) = &item.error {
                    widgets::banner(ui, icons::WARNING, err, p.danger, p.danger_soft, |ui| {
                        if widgets::link(ui, "Try again").clicked() {
                            acts.push(ReadAction::Retry(s.id));
                        }
                    });
                } else if let Some(doc) = &item.doc {
                    let in_junk = self.mail.find_folder(s.folder_id).map(|f| f.role == FolderRole::Junk).unwrap_or(false);
                    let opts = html::RenderOpts {
                        show_images: show_images && !in_junk,
                        cid: &item.cid,
                        base_size: 14.5,
                    };
                    match html::show(ui, doc, &opts) {
                        Some(HtmlAction::OpenLink(l)) if !in_junk => acts.push(ReadAction::OpenLink(l)),
                        Some(HtmlAction::ShowImages) if !in_junk => acts.push(ReadAction::OpenLink("#show-images".into())),
                        _ => {}
                    }
                }

                // Attachments
                if let Some(pm) = &item.parsed {
                    let files: Vec<(usize, &Attachment)> = pm.attachments.iter().enumerate().filter(|(_, a)| !a.inline).collect();
                    if !files.is_empty() {
                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{} attachment{}", files.len(), if files.len() == 1 { "" } else { "s" })).font(font_semibold(13.0)).color(p.text2));
                            if files.len() > 1 && widgets::link(ui, "Save all").clicked() {
                                acts.push(ReadAction::SaveAll(s.id));
                            }
                        });
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
                            for (i, a) in files {
                                self.attachment_tile(ui, s.id, i, a, acts);
                            }
                        });
                    }
                }

                if is_last && !s.draft {
                    ui.add_space(18.0);
                    ui.horizontal(|ui| {
                        if widgets::secondary_button(ui, Some(icons::ARROW_BEND_UP_LEFT), "Reply").clicked() {
                            acts.push(ReadAction::Cmd(MenuCmd::Reply, ids.clone()));
                        }
                        if widgets::secondary_button(ui, Some(icons::ARROW_BEND_DOUBLE_UP_LEFT), "Reply all").clicked() {
                            acts.push(ReadAction::Cmd(MenuCmd::ReplyAll, ids.clone()));
                        }
                        if widgets::secondary_button(ui, Some(icons::ARROW_BEND_UP_RIGHT), "Forward").clicked() {
                            acts.push(ReadAction::Cmd(MenuCmd::Forward, ids.clone()));
                        }
                    });
                } else if s.draft {
                    ui.add_space(14.0);
                    if widgets::primary_button(ui, Some(icons::PENCIL_SIMPLE), "Continue editing").clicked() {
                        acts.push(ReadAction::Cmd(MenuCmd::EditDraft, ids.clone()));
                    }
                }
            });
    }

    fn invite_card(&self, ui: &mut Ui, s: &MessageSummary, inv: &cal::IcsImport, acts: &mut Vec<ReadAction>) {
        let p = pal();
        let Some(e) = inv.events.first() else { return };
        let existing = self.store.event_by_uid(&e.uid).ok().flatten();
        Frame::NONE
            .fill(p.accent_soft)
            .corner_radius(CornerRadius::same(8))
            .inner_margin(Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(48.0, 52.0), Sense::hover());
                    let start = util::local(e.start);
                    ui.painter().rect_filled(r, CornerRadius::same(6), p.card);
                    let band = Rect::from_min_size(r.min, vec2(r.width(), 16.0));
                    ui.painter().rect_filled(band, CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 }, p.accent);
                    ui.painter().text(band.center(), Align2::CENTER_CENTER, start.format("%b").to_string().to_uppercase(), font_semibold(10.0), Color32::WHITE);
                    ui.painter().text(Pos2::new(r.center().x, r.top() + 34.0), Align2::CENTER_CENTER, start.format("%-d").to_string(), font_semibold(20.0), p.text);
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.label(RichText::new(&e.title).font(font_semibold(15.0)).color(p.text));
                        let when = if e.all_day {
                            format!("{} (all day)", util::fmt_long_date(start.date_naive()))
                        } else {
                            format!("{}, {} – {}", util::fmt_long_date(start.date_naive()), util::fmt_time(&start), util::fmt_time(&util::local(e.end)))
                        };
                        ui.label(RichText::new(format!("{}  {when}", icons::CLOCK)).font(icon_font(13.0)).color(p.text2));
                        if !e.location.is_empty() {
                            ui.label(RichText::new(format!("{}  {}", icons::MAP_PIN, e.location)).font(icon_font(13.0)).color(p.text2));
                        }
                        if !e.organizer.is_empty() {
                            ui.label(RichText::new(format!("Organizer: {}", e.organizer)).font(font(12.5)).color(p.text3));
                        }
                    });
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if inv.method == "REQUEST" && !inv.organizer_email.is_empty() {
                        if widgets::primary_button(ui, Some(icons::CHECK), "Accept").clicked() {
                            acts.push(ReadAction::Rsvp(s.id, cal::Rsvp::Accepted));
                        }
                        if widgets::secondary_button(ui, Some(icons::QUESTION), "Tentative").clicked() {
                            acts.push(ReadAction::Rsvp(s.id, cal::Rsvp::Tentative));
                        }
                        if widgets::secondary_button(ui, Some(icons::X), "Decline").clicked() {
                            acts.push(ReadAction::Rsvp(s.id, cal::Rsvp::Declined));
                        }
                    }
                    if existing.is_some() {
                        ui.label(RichText::new(format!("{}  In your calendar", icons::CALENDAR_CHECK)).font(icon_font(13.0)).color(p.success));
                    } else if widgets::subtle_button(ui, icons::CALENDAR_PLUS, "Add to calendar").clicked() {
                        acts.push(ReadAction::AddToCalendar(s.id));
                    }
                });
            });
    }

    fn attachment_tile(&self, ui: &mut Ui, msg: MsgId, idx: usize, a: &Attachment, acts: &mut Vec<ReadAction>) {
        let p = pal();
        let (rect, resp) = ui.allocate_exact_size(vec2(236.0, 48.0), Sense::click());
        let bg = if resp.hovered() { p.hover } else { p.card };
        ui.painter().rect_filled(rect, CornerRadius::same(6), bg);
        ui.painter().rect_stroke(rect, CornerRadius::same(6), Stroke::new(1.0, p.stroke), egui::StrokeKind::Inside);
        let lower = a.name.to_lowercase();
        let (icon, color) = if a.mime.contains("pdf") || lower.ends_with(".pdf") {
            (icons::FILE_PDF, theme::hex(0xD13438))
        } else if a.mime.starts_with("image/") {
            (icons::FILE_IMAGE, theme::hex(0x0F7B0F))
        } else if a.mime.contains("zip") || lower.ends_with(".zip") || lower.ends_with(".gz") {
            (icons::FILE_ZIP, theme::hex(0x986F0B))
        } else if a.mime == "text/calendar" || lower.ends_with(".ics") {
            (icons::CALENDAR_BLANK, p.accent)
        } else if a.mime.contains("vcard") || lower.ends_with(".vcf") {
            (icons::ADDRESS_BOOK, p.accent)
        } else if a.mime.contains("word") || lower.ends_with(".docx") || lower.ends_with(".doc") {
            (icons::FILE_TEXT, theme::hex(0x185ABD))
        } else if a.mime.contains("sheet") || lower.ends_with(".xlsx") || lower.ends_with(".csv") {
            (icons::FILE_TEXT, theme::hex(0x107C41))
        } else {
            (icons::FILE, p.text2)
        };
        ui.painter().text(Pos2::new(rect.left() + 24.0, rect.center().y), Align2::CENTER_CENTER, icon, icon_fill_font(24.0), color);
        let name = elided(ui, &a.name, font(13.0), p.text, rect.width() - 64.0);
        ui.painter().galley(Pos2::new(rect.left() + 44.0, rect.top() + 7.0), name, p.text);
        ui.painter().text(Pos2::new(rect.left() + 44.0, rect.bottom() - 9.0), Align2::LEFT_BOTTOM, util::human_size(a.size), font(11.5), p.text3);
        ui.painter().text(Pos2::new(rect.right() - 14.0, rect.center().y), Align2::CENTER_CENTER, icons::CARET_DOWN, icon_font(12.0), p.text3);
        let resp = resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(&a.name);
        if resp.double_clicked() {
            acts.push(ReadAction::OpenAttachment(msg, idx));
        }
        egui::Popup::menu(&resp).width(200.0).show(|ui| {
            if widgets::menu_item(ui, icons::ARROW_SQUARE_OUT, "Open").clicked() {
                acts.push(ReadAction::OpenAttachment(msg, idx));
            }
            if widgets::menu_item(ui, icons::DOWNLOAD_SIMPLE, "Save as…").clicked() {
                acts.push(ReadAction::SaveAttachment(msg, idx));
            }
            if (a.mime == "text/calendar" || lower.ends_with(".ics")) && widgets::menu_item(ui, icons::CALENDAR_PLUS, "Add to calendar").clicked() {
                acts.push(ReadAction::AddToCalendar(msg));
            }
            if (a.mime.contains("vcard") || lower.ends_with(".vcf")) && widgets::menu_item(ui, icons::USER_PLUS, "Add to contacts").clicked() {
                acts.push(ReadAction::ImportContacts(msg, idx));
            }
        });
    }

    fn apply_read_action(&mut self, a: ReadAction) {
        match a {
            ReadAction::Cmd(c, ids) => self.apply_cmd(c, &ids),
            ReadAction::OpenLink(l) => {
                if l == "#show-images" {
                    if let Some(r) = self.mail.reading.as_mut() {
                        r.show_images = true;
                    }
                } else if let Some(addr) = l.strip_prefix("mailto:") {
                    self.compose_to(addr);
                } else if let Err(e) = open::that(&l) {
                    self.toast(format!("Could not open link: {e}"), ToastKind::Error);
                }
            }
            ReadAction::Unsubscribe(u) => {
                if let Some(addr) = u.strip_prefix("mailto:") {
                    self.compose_to(addr);
                } else {
                    let _ = open::that(&u);
                }
            }
            ReadAction::TrustSender(addr) => {
                let addr = addr.to_lowercase();
                if !self.cfg.mail.safe_senders.contains(&addr) {
                    self.cfg.mail.safe_senders.push(addr);
                    self.mark_config_dirty();
                }
                if let Some(r) = self.mail.reading.as_mut() {
                    r.show_images = true;
                }
            }
            ReadAction::Retry(id) => {
                if let Some(r) = self.mail.reading.as_mut() {
                    for it in r.items.iter_mut().filter(|i| i.summary.id == id) {
                        it.error = None;
                        it.loading = false;
                    }
                }
                self.ensure_body(id);
            }
            ReadAction::SaveAttachment(msg, idx) => self.save_attachment(msg, idx),
            ReadAction::OpenAttachment(msg, idx) => self.open_attachment(msg, idx),
            ReadAction::SaveAll(msg) => self.save_all_attachments(msg),
            ReadAction::AddToCalendar(msg) => {
                self.add_invite_to_calendar(msg, None);
            }
            ReadAction::Rsvp(msg, rsvp) => self.rsvp(msg, rsvp),
            ReadAction::ImportContacts(msg, idx) => {
                if let Some((_, data)) = self.attachment_data(msg, idx) {
                    let contacts = crate::vcard::parse(&String::from_utf8_lossy(&data));
                    let n = contacts.len();
                    for c in contacts {
                        let _ = self.store.save_contact(&c);
                    }
                    self.people.dirty = true;
                    self.toast(format!("Added {n} contact(s)"), ToastKind::Success);
                }
            }
            ReadAction::SendReceipt(id, to) => self.send_read_receipt(id, &to),
            ReadAction::DismissReceipt(key) => self.store.kv_set(&key, "declined"),
        }
    }

    fn send_read_receipt(&mut self, id: MsgId, to: &str) {
        let Ok(Some(m)) = self.store.message(id) else { return };
        let Some(acc) = self.cfg.account(&m.account_id).cloned() else { return };
        let Some(rcpt) = Addr::parse_list(to).into_iter().next() else { return };
        let text = format!(
            "Your message\n\n  To: {}\n  Subject: {}\n  Sent: {}\n\nwas read on {}.",
            acc.email,
            m.subject,
            util::fmt_full_date(m.date),
            util::fmt_full_date(util::now_ts())
        );
        let mail = mime::OutgoingMail {
            from: Addr::new(acc.display_name.clone(), acc.email.clone()),
            to: vec![rcpt],
            subject: format!("Read: {}", m.subject),
            text,
            in_reply_to: Some(m.message_id.clone()).filter(|s| !s.is_empty()),
            ..Default::default()
        };
        match mime::build(&mail) {
            Ok(built) => {
                let _ = self.store.add_outbox(&OutboxItem {
                    id: 0,
                    account_id: acc.id.clone(),
                    raw: built.raw,
                    from: acc.email.clone(),
                    recipients: built.recipients,
                    send_at: util::now_ts(),
                    attempts: 0,
                    last_error: None,
                    subject: mail.subject.clone(),
                    reply_to_msg: None,
                    forward_of: None,
                    draft_msg: None,
                });
                self.store.kv_set(&format!("receipt:{}", m.message_id), "sent");
                self.engine.send(Command::ProcessOutbox);
                self.toast("Read receipt sent", ToastKind::Success);
            }
            Err(e) => {
                self.toast(format!("Couldn't send receipt: {e}"), ToastKind::Error);
            }
        }
    }

    fn attachment_data(&mut self, msg: MsgId, idx: usize) -> Option<(Attachment, Vec<u8>)> {
        let raw = self.store.raw_body(msg).ok().flatten()?;
        let parsed = mime::parse_full(&raw)?;
        let att = parsed.attachments.get(idx)?.clone();
        let data = mime::part_bytes(&raw, att.part)?;
        Some((att, data))
    }

    fn save_attachment(&mut self, msg: MsgId, idx: usize) {
        let Some((att, data)) = self.attachment_data(msg, idx) else {
            self.toast("Attachment not available", ToastKind::Error);
            return;
        };
        if let Some(path) = rfd::FileDialog::new().set_file_name(util::sanitize_filename(&att.name)).save_file() {
            match std::fs::write(&path, data) {
                Ok(()) => self.toast(format!("Saved {}", path.display()), ToastKind::Success),
                Err(e) => self.toast(format!("Could not save: {e}"), ToastKind::Error),
            };
        }
    }

    fn save_all_attachments(&mut self, msg: MsgId) {
        let Ok(Some(raw)) = self.store.raw_body(msg) else { return };
        let Some(parsed) = mime::parse_full(&raw) else { return };
        let Some(dir) = rfd::FileDialog::new().pick_folder() else { return };
        let mut n = 0;
        for a in parsed.attachments.iter().filter(|a| !a.inline) {
            if let Some(data) = mime::part_bytes(&raw, a.part) {
                if std::fs::write(dir.join(util::sanitize_filename(&a.name)), data).is_ok() {
                    n += 1;
                }
            }
        }
        self.toast(format!("Saved {n} attachment(s) to {}", dir.display()), ToastKind::Success);
    }

    fn open_attachment(&mut self, msg: MsgId, idx: usize) {
        let Some((att, data)) = self.attachment_data(msg, idx) else {
            self.toast("Attachment not available", ToastKind::Error);
            return;
        };
        let dir = std::env::temp_dir().join("rustmail").join(msg.to_string());
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(util::sanitize_filename(&att.name));
        match std::fs::write(&path, data).and_then(|_| open::that(&path)) {
            Ok(()) => {}
            Err(e) => {
                self.toast(format!("Could not open attachment: {e}"), ToastKind::Error);
            }
        }
    }

    pub fn add_invite_to_calendar(&mut self, msg: MsgId, show_as: Option<ShowAs>) -> bool {
        let Ok(Some(raw)) = self.store.raw_body(msg) else { return false };
        let Some(parsed) = mime::parse_full(&raw) else { return false };
        let Some(ics) = parsed.calendar else {
            self.toast("No calendar information found", ToastKind::Error);
            return false;
        };
        let imp = cal::parse_ics(&ics);
        let mut n = 0;
        for mut e in imp.events {
            if let Some(sa) = show_as {
                e.show_as = sa;
            }
            if e.reminder_min.is_none() {
                e.reminder_min = Some(self.cfg.calendar.default_reminder_min);
            }
            if let Ok(Some(existing)) = self.store.event_by_uid(&e.uid) {
                e.id = existing.id;
            }
            if self.store.save_event(&e).is_ok() {
                n += 1;
            }
        }
        self.cal.dirty = true;
        if n > 0 {
            self.toast(format!("Added {n} event(s) to your calendar"), ToastKind::Success);
        }
        n > 0
    }

    fn rsvp(&mut self, msg: MsgId, rsvp: cal::Rsvp) {
        let Ok(Some(m)) = self.store.message(msg) else { return };
        let Ok(Some(raw)) = self.store.raw_body(msg) else { return };
        let Some(parsed) = mime::parse_full(&raw) else { return };
        let Some(ics) = parsed.calendar.clone() else { return };
        let imp = cal::parse_ics(&ics);
        let Some(ev) = imp.events.first().cloned() else { return };
        match rsvp {
            cal::Rsvp::Accepted => {
                self.add_invite_to_calendar(msg, Some(ShowAs::Busy));
            }
            cal::Rsvp::Tentative => {
                self.add_invite_to_calendar(msg, Some(ShowAs::Tentative));
            }
            cal::Rsvp::Declined => {
                if let Ok(Some(e)) = self.store.event_by_uid(&ev.uid) {
                    let _ = self.store.delete_event(e.id);
                    self.cal.dirty = true;
                }
            }
        }
        let Some(acc) = self.cfg.account(&m.account_id).cloned() else { return };
        let reply_ics = cal::rsvp_reply(&ev, &imp.organizer_email, &acc.display_name, &acc.email, rsvp);
        let mail = mime::OutgoingMail {
            from: Addr::new(acc.display_name.clone(), acc.email.clone()),
            to: vec![Addr::new("", imp.organizer_email.clone())],
            subject: format!("{}: {}", rsvp.label(), ev.title),
            text: format!("{} has {} this invitation.", acc.display_name, rsvp.label().to_lowercase()),
            attachments: vec![mime::OutAttachment {
                name: "reply.ics".into(),
                mime: "text/calendar; method=REPLY".into(),
                data: reply_ics.into_bytes(),
            }],
            in_reply_to: Some(parsed.message_id.clone()).filter(|s| !s.is_empty()),
            ..Default::default()
        };
        match mime::build(&mail) {
            Ok(built) => {
                let item = OutboxItem {
                    id: 0,
                    account_id: acc.id.clone(),
                    raw: built.raw,
                    from: acc.email.clone(),
                    recipients: built.recipients,
                    send_at: util::now_ts(),
                    attempts: 0,
                    last_error: None,
                    subject: mail.subject.clone(),
                    reply_to_msg: None,
                    forward_of: None,
                    draft_msg: None,
                };
                let _ = self.store.add_outbox(&item);
                self.engine.send(Command::ProcessOutbox);
                self.toast(format!("Response sent: {}", rsvp.label()), ToastKind::Success);
            }
            Err(e) => {
                self.toast(format!("Could not send response: {e}"), ToastKind::Error);
            }
        }
    }

    fn attach_dropped(&mut self, files: Vec<egui::DroppedFileHandle>) {
        let mut atts = Vec::new();
        for f in files {
            let data = f.bytes().ok().or_else(|| std::fs::read(f.path()).ok());
            if let Some(data) = data {
                let name = f
                    .path()
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "attachment".to_string());
                atts.push(mime::OutAttachment { mime: mime::guess_mime(&name), name, data });
            }
        }
        if atts.is_empty() {
            return;
        }
        if self.mail.compose_inline.is_none() {
            self.new_compose();
        }
        if let Some(c) = self.mail.compose_inline.as_mut() {
            c.attachments.extend(atts);
        }
    }
}

/// Registers inline (cid:) and data: images with egui and returns the map
/// used by the renderer.
fn register_images(ctx: &egui::Context, msg: MsgId, raw: &[u8], doc: &HtmlDoc) -> HashMap<String, String> {
    use base64::Engine;
    let mut map = HashMap::new();
    for (n, (cid, bytes)) in mime::inline_parts(raw).into_iter().enumerate() {
        let ext = image::guess_format(&bytes)
            .ok()
            .and_then(|f| f.extensions_str().first().copied())
            .unwrap_or("png");
        let uri = format!("bytes://cid/{msg}/{n}.{ext}");
        ctx.include_bytes(uri.clone(), bytes);
        map.insert(cid, uri);
    }
    for (i, b) in doc.blocks.iter().enumerate() {
        if let html::Block::Image { src, .. } = b {
            if let Some(rest) = src.strip_prefix("data:") {
                if let Some((meta, data)) = rest.split_once(',') {
                    if meta.ends_with(";base64") {
                        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.trim()) {
                            let ext = image::guess_format(&bytes)
                                .ok()
                                .and_then(|f| f.extensions_str().first().copied())
                                .unwrap_or("png");
                            let uri = format!("bytes://data/{msg}/{i}.{ext}");
                            ctx.include_bytes(uri.clone(), bytes);
                            map.insert(format!("data:{i}"), uri);
                        }
                    }
                }
            }
        }
    }
    map
}

fn snooze_items(ui: &mut Ui, out: &mut Vec<MenuCmd>) {
    use chrono::{Datelike, Duration, Weekday};
    let today = util::today();
    let now = util::now_ts();
    let later = now + 3 * 3600;
    let tomorrow = util::ts_from_date_time(today + Duration::days(1), 8, 0);
    let days_to_sat = (Weekday::Sat.num_days_from_monday() as i64 - today.weekday().num_days_from_monday() as i64).rem_euclid(7);
    let weekend = util::ts_from_date_time(today + Duration::days(if days_to_sat == 0 { 7 } else { days_to_sat }), 8, 0);
    let days_to_mon = (7 - today.weekday().num_days_from_monday() as i64).rem_euclid(7);
    let next_week = util::ts_from_date_time(today + Duration::days(if days_to_mon == 0 { 7 } else { days_to_mon }), 8, 0);
    let opts = [
        ("Later today", later),
        ("Tomorrow", tomorrow),
        ("This weekend", weekend),
        ("Next week", next_week),
    ];
    let p = pal();
    for (label, ts) in opts {
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(240.0), 30.0), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(4), p.hover);
        }
        ui.painter().text(Pos2::new(rect.left() + 12.0, rect.center().y), Align2::LEFT_CENTER, label, font(14.0), p.text);
        let dt = util::local(ts);
        ui.painter().text(
            Pos2::new(rect.right() - 10.0, rect.center().y),
            Align2::RIGHT_CENTER,
            format!("{} {}", dt.format("%a"), util::fmt_time(&dt)),
            font(12.5),
            p.text3,
        );
        if resp.clicked() {
            out.push(MenuCmd::Snooze(Some(ts)));
            ui.close();
        }
    }
    ui.separator();
    if widgets::menu_item(ui, icons::X, "Remove snooze").clicked() {
        out.push(MenuCmd::Snooze(None));
    }
}

fn toggle_set(set: &mut HashSet<String>, key: &str) {
    if !set.remove(key) {
        set.insert(key.to_string());
    }
}

fn first_recipient(list: &str) -> (String, String) {
    Addr::parse_list(list)
        .into_iter()
        .next()
        .map(|a| (a.display().to_string(), a.email))
        .unwrap_or_default()
}

fn summarize_recipients(list: &str) -> String {
    let addrs = Addr::parse_list(list);
    let names: Vec<&str> = addrs.iter().map(|a| a.display()).collect();
    if names.len() <= 3 {
        names.join("; ")
    } else {
        format!("{}; +{} others", names[..3].join("; "), names.len() - 3)
    }
}

/// Orders folders as a tree: special folders first, then custom folders,
/// children below parents. Returns (folder, depth, has_children).
pub fn folder_tree(folders: &[Folder]) -> Vec<(Folder, usize, bool)> {
    let by_path: HashMap<&str, &Folder> = folders.iter().map(|f| (f.path.as_str(), f)).collect();
    let parent_of = |f: &Folder| -> Option<String> {
        let mut cur = f.parent_path();
        while let Some(pp) = cur {
            if let Some(pf) = by_path.get(pp.as_str()) {
                if pf.role == FolderRole::Custom || pf.role == FolderRole::Inbox {
                    return Some(pp);
                }
                // Special-use parents (e.g. "[Gmail]") are not shown as parents.
                if !pf.selectable {
                    return None;
                }
                return Some(pp);
            }
            cur = parent_of_path(&pp, f.delimiter.as_deref());
        }
        None
    };
    let mut children: HashMap<String, Vec<&Folder>> = HashMap::new();
    let mut roots: Vec<&Folder> = Vec::new();
    for f in folders {
        // Hide non-selectable containers that only hold special folders.
        if !f.selectable {
            let has_custom_child = folders.iter().any(|c| c.role == FolderRole::Custom && c.selectable && c.path.starts_with(&f.path) && c.path != f.path);
            if !has_custom_child {
                continue;
            }
        }
        if f.role != FolderRole::Custom {
            roots.push(f);
            continue;
        }
        match parent_of(f) {
            Some(pp) => children.entry(pp).or_default().push(f),
            None => roots.push(f),
        }
    }
    roots.sort_by(|a, b| {
        a.role
            .sort_key()
            .cmp(&b.role.sort_key())
            .then_with(|| a.label().to_lowercase().cmp(&b.label().to_lowercase()))
    });
    let mut out = Vec::new();
    fn visit<'a>(
        f: &'a Folder,
        depth: usize,
        children: &HashMap<String, Vec<&'a Folder>>,
        out: &mut Vec<(Folder, usize, bool)>,
        guard: usize,
    ) {
        if guard > 30 {
            return;
        }
        let mut kids = children.get(&f.path).cloned().unwrap_or_default();
        kids.sort_by_key(|k| k.label().to_lowercase());
        out.push((f.clone(), depth, !kids.is_empty()));
        for k in kids {
            visit(k, depth + 1, children, out, guard + 1);
        }
    }
    for r in roots {
        visit(r, 0, &children, &mut out, 0);
    }
    out
}

fn parent_of_path(path: &str, delim: Option<&str>) -> Option<String> {
    let d = delim.filter(|d| !d.is_empty())?;
    path.rfind(d).map(|i| path[..i].to_string())
}

trait OutgoingExt {
    fn folder_is_outgoing(&self) -> bool;
}

impl OutgoingExt for MessageSummary {
    /// Sent/draft messages show recipients instead of the sender.
    fn folder_is_outgoing(&self) -> bool {
        self.draft || OUTGOING_FOLDERS.with(|s| s.borrow().contains(&self.folder_id))
    }
}

thread_local! {
    pub static OUTGOING_FOLDERS: std::cell::RefCell<HashSet<FolderId>> = std::cell::RefCell::new(HashSet::new());
}

pub fn update_outgoing_folders(state: &MailState) {
    let ids: HashSet<FolderId> = state
        .folders
        .values()
        .flatten()
        .filter(|f| matches!(f.role, FolderRole::Sent | FolderRole::Drafts))
        .map(|f| f.id)
        .collect();
    OUTGOING_FOLDERS.with(|s| *s.borrow_mut() = ids);
}

