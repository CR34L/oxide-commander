//! The window: sidebar with drives, two file panels, status line and F-key bar.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local};
use eframe::egui::{
    self, text::CCursor, text_selection::CCursorRange, vec2, Align, Align2, Color32, CornerRadius, DragAndDrop,
    Event, Id, Key, Layout, Margin, Modal, Modifiers, Rect, RichText, ScrollArea, Sense, Stroke, StrokeKind,
    TextEdit, Ui,
};

use crate::jobs::{self, Job, Kind};
use crate::ops::{self, human_size, Clash};
use crate::panel::{Panel, SortKey};
use crate::places::{self, paint_capacity, paint_icon, Place};
use crate::settings::{self, TerminalMode};
use crate::terminal::{self, TermPane};
use crate::sizes::DirSizes;
use crate::theme::{self, pal, Theme};

const ROW_H: f32 = 22.0;

/// A copy/move/delete waiting for the running one: kind, (source, target) pairs, clash mode.
type QueuedJob = (Kind, Vec<(PathBuf, PathBuf)>, Clash);

/// Items being dragged between panels or onto the sidebar.
#[derive(Clone)]
struct DragPayload { from: usize, paths: Vec<PathBuf> }

#[derive(Clone, Copy)]
enum Cmd {
    Help, Terminal, OpenIn, Settings, View, QuickLook, Edit, EditSystem, Copy, Duplicate, Move, Rename, NewFolder, NewFile, Trash, Delete, Quit,
    Open, Up, Back, Forward, Refresh, ToggleHidden, SelectAll, Swap, CopyPath, Reveal,
}

enum InputKind { Copy(Vec<PathBuf>), Move(Vec<PathBuf>), Rename(PathBuf), New { file: bool } }

#[derive(Clone, PartialEq)]
enum OpenItem { Terminal, App(&'static str, PathBuf), Finder }

impl OpenItem {
    fn key(&self) -> &str {
        match self { OpenItem::Terminal => "terminal", OpenItem::App(name, _) => name, OpenItem::Finder => "finder" }
    }
}

enum Dialog {
    None,
    Input { kind: InputKind, title: String, prompt: String, buf: String, ok: &'static str, first_frame: bool, select_stem: bool },
    Confirm { title: String, text: String, ok: &'static str, kind: Kind, pairs: Vec<(PathBuf, PathBuf)> },
    /// Copy/move targets exist: replace, merge (folders) or rename (keep both).
    Clash { kind: Kind, pairs: Vec<(PathBuf, PathBuf)>, count: usize, names: String, can_merge: bool },
    Drop { paths: Vec<PathBuf>, dest: PathBuf },
    View { title: String, text: String },
    /// F1: open the current folder in the terminal, an editor or Finder.
    OpenIn { dir: PathBuf, items: Vec<OpenItem>, sel: usize },
    Help,
    Settings,
}

pub struct App {
    panels: [Panel; 2],
    active: usize,
    dialog: Dialog,
    job: Option<Job>,
    message: Option<(String, bool, Instant)>,
    favorites: Vec<Place>,
    /// Finder favourites delivered by the background loader.
    finder_favorites: FinderFavorites,
    drives: Vec<Place>,
    /// Background drive-list reader (reading free space can take 200 ms or more).
    drive_watch: DriveWatch,
    type_ahead: (String, Instant),
    list_rects: [Rect; 2],
    focus_filter: Option<usize>,
    /// Recent (time, bytes done) samples of the running job, for MB/s.
    speed: SpeedMeter,
    /// Tab presses taken out of the raw input (egui would use Tab to move widget focus).
    tab_presses: usize,
    /// Current settings, and the copy last written to disk (saved again when they differ).
    settings: settings::Settings,
    saved_settings: settings::Settings,
    /// Copy / move / delete operations waiting for the running one: they never run in parallel.
    queue: std::collections::VecDeque<QueuedJob>,
    /// Built-in terminals (Settings → Terminal), one per panel, each below its file list.
    terms: [Option<SideTerm>; 2],
    /// Side whose terminal has the keyboard; key events go to that shell instead of the panels.
    term_focus: Option<usize>,
    /// Each terminal's share of its column height (kept when the terminal is closed).
    term_frac: [f32; 2],
    /// Key and text events taken from the raw input for the shell.
    term_input: Vec<Event>,
    /// Last choice in the F1 "Open folder in…" popup (preselected next time).
    last_open_in: Option<String>,
    /// Window geometry last seen and since when (saved once it has been still for a moment).
    window_seen: Option<([f32; 4], Instant)>,
}

/// A panel's terminal. The shell keeps running while the pane is hidden.
struct SideTerm {
    pane: TermPane,
    open: bool,
    /// Folder the shell started in (shown until the shell sets a title).
    dir: PathBuf,
    rect: Rect,
}

impl App {
    /// `args`: folders from the command line; `defaults`: used when nothing else applies.
    pub fn new(cc: &eframe::CreationContext<'_>, args: [Option<PathBuf>; 2], defaults: [PathBuf; 2]) -> Self {
        let settings = settings::load();
        theme::apply(&cc.egui_ctx, settings.theme);
        let start = |i: usize| -> PathBuf {
            let remembered = settings.folders[i].clone().filter(|p| settings.remember_folders && p.is_dir());
            args[i].clone().or(remembered).unwrap_or_else(|| defaults[i].clone())
        };
        let sizes = DirSizes::new(Some(cc.egui_ctx.clone()));
        let mut panels = [Panel::new(start(0), sizes.clone()), Panel::new(start(1), sizes)];
        for (p, v) in panels.iter_mut().zip(settings.views) { p.set_view(v); }
        App {
            panels,
            saved_settings: settings.clone(),
            settings,
            queue: Default::default(),
            tab_presses: 0,
            speed: SpeedMeter::default(),
            active: 0,
            dialog: Dialog::None,
            job: None,
            message: None,
            favorites: places::favorites(),
            finder_favorites: spawn_finder_favorites(&cc.egui_ctx),
            drives: Vec::new(),
            drive_watch: spawn_drive_watch(&cc.egui_ctx),
            type_ahead: (String::new(), Instant::now()),
            list_rects: [Rect::NOTHING; 2],
            focus_filter: None,
            terms: [None, None],
            term_focus: None,
            term_frac: [0.45; 2],
            term_input: Vec::new(),
            last_open_in: None,
            window_seen: None,
        }
    }

    /// Remember the window position and size, once a move or resize has finished
    /// (not while dragging: that would write the settings file many times a second).
    fn track_window(&mut self, ctx: &egui::Context) {
        let vp = ctx.input(|i| i.viewport().clone());
        if vp.fullscreen == Some(true) || vp.minimized == Some(true) { return; }
        let (Some(outer), Some(inner)) = (vp.outer_rect, vp.inner_rect) else { return };
        let g = [outer.min.x, outer.min.y, inner.width(), inner.height()].map(f32::round);
        match self.window_seen {
            Some((last, since)) if last == g => {
                if since.elapsed() > Duration::from_millis(500) && self.settings.window != Some(g) {
                    self.settings.window = Some(g);
                }
            }
            _ => self.window_seen = Some((g, Instant::now())),
        }
    }

    fn builtin_terminal(&self) -> bool { self.settings.terminal_mode != TerminalMode::External }

    fn term_visible(&self, side: usize) -> bool {
        self.builtin_terminal() && self.terms[side].as_ref().is_some_and(|t| t.open)
    }

    /// Keystrokes currently belong to a built-in terminal (not after its shell has ended).
    fn term_has_keys(&self) -> bool {
        self.term_focus.is_some_and(|s| self.term_visible(s) && self.terms[s].as_ref().is_some_and(|t| !t.pane.exited))
            && matches!(self.dialog, Dialog::None)
    }

    /// F1: show the terminal under the active panel (starting a shell in its folder if
    /// needed) and give it the keyboard; hide it when it already has the keyboard.
    fn toggle_terminal(&mut self, ctx: &egui::Context) {
        let side = self.active;
        let alive = self.terms[side].as_ref().is_some_and(|t| !t.pane.exited);
        if alive && self.term_visible(side) && self.term_focus == Some(side) {
            if let Some(t) = self.terms[side].as_mut() { t.open = false; }
            self.term_focus = None;
            return;
        }
        if !alive {
            let dir = self.act().path.clone();
            match TermPane::spawn(&dir, ctx) {
                Ok(pane) => self.terms[side] = Some(SideTerm { pane, open: true, dir, rect: Rect::NOTHING }),
                Err(e) => { self.error(format!("Could not start the shell: {e}")); return; }
            }
        }
        if let Some(t) = self.terms[side].as_mut() { t.open = true; }
        self.term_focus = Some(side);
        ctx.memory_mut(|m| if let Some(id) = m.focused() { m.surrender_focus(id) });
    }

    fn terminal_ui(&mut self, ui: &mut Ui, side: usize) {
        let focused = self.term_focus == Some(side);
        let panel_dir = self.panels[side].path.clone();
        let Some(st) = self.terms[side].as_mut() else { return };
        let t = &mut st.pane;
        let frame = egui::Frame::new()
            .fill(pal().list_bg)
            .stroke(Stroke::new(if focused { 2.0 } else { 1.0 }, if focused { pal().accent } else { pal().border }))
            .corner_radius(CornerRadius::same(6));
        let mut hide = false;
        let resp = frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                // Buttons first (from the right), so a long title is cut instead of widening the pane.
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(6.0);
                    if ui.add(egui::Button::new("×").min_size(vec2(24.0, 20.0))).on_hover_text("Hide the terminal (F1). The shell keeps running.").clicked() { hide = true; }
                    if !t.exited && ui.add(egui::Button::new(RichText::new("cd to panel folder").size(12.0)).min_size(vec2(0.0, 20.0)))
                        .on_hover_text(dir_string(&panel_dir)).clicked() {
                        t.cd(&panel_dir);
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add_space(8.0);
                        let title = if t.title.is_empty() { terminal::short_dir(&st.dir) } else { t.title.clone() };
                        ui.label(RichText::new("Terminal").size(12.5).strong().color(if focused { pal().header_active } else { pal().text_dim }));
                        ui.add(egui::Label::new(RichText::new(title).size(12.5).color(pal().text_dim)).truncate());
                    });
                });
            });
            ui.add_space(4.0);
            ui.add(egui::Separator::default().spacing(0.0));
            t.show(ui, focused)
        });
        st.rect = resp.response.rect;
        if hide { st.open = false; }
        if resp.inner.clicked() || resp.inner.drag_started() {
            self.term_focus = Some(side);
            self.active = side;
            ui.ctx().memory_mut(|m| if let Some(id) = m.focused() { m.surrender_focus(id) });
        }
        if hide && self.term_focus == Some(side) { self.term_focus = None; }
    }

    fn act(&mut self) -> &mut Panel { &mut self.panels[self.active] }

    fn info(&mut self, text: impl Into<String>) { self.message = Some((text.into(), false, Instant::now())); }
    fn error(&mut self, text: impl Into<String>) { self.message = Some((text.into(), true, Instant::now())); }

    fn open_with(&mut self, path: &Path, name: &str, app: PathBuf) {
        if let Err(e) = ops::open_in_app(path, &app) { self.error(format!("Could not open {name}: {e}")); }
    }

    fn report<T>(&mut self, r: std::io::Result<T>) {
        if let Err(e) = r { self.error(e.to_string()); }
    }

    // ---- per-frame housekeeping ------------------------------------------

    fn tick(&mut self, ctx: &egui::Context) {
        // Re-read Finder's favourites when the window comes back to the front (at most every
        // 10 minutes): they only change in Finder.
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(false);
        let ff = &mut self.finder_favorites;
        if focused && !ff.was_focused && ff.requested.elapsed() > Duration::from_secs(600) {
            ff.requested = Instant::now();
            let _ = ff.refresh.send(());
        }
        ff.was_focused = focused;
        if let Some(favs) = self.finder_favorites.slot.lock().unwrap().take() {
            self.favorites = favs;
        }
        if let Some(drives) = self.drive_watch.slot.lock().unwrap().take() {
            self.drives = drives;
        }
        for p in &mut self.panels { p.poll_changes(); }
        for i in 0..2 {
            if let Some(e) = self.panels[i].take_error() { self.error(e.to_string()); }
        }

        if let Some(job) = &self.job && job.progress.finished.load(Ordering::Relaxed) {
            {
                let job = self.job.take().unwrap();
                let done = job.progress.items_done.load(Ordering::Relaxed);
                let err = job.progress.error.lock().unwrap().take();
                let cancelled = job.progress.cancel.load(Ordering::Relaxed);
                for p in &mut self.panels { p.selected.clear(); p.rescan(); }
                let msg = format!("{} {done} of {} item(s)", job.kind.done_verb(), job.items);
                match err {
                    _ if cancelled => self.info(format!("Cancelled. {msg}")),
                    Some(e) => self.error(format!("{msg}. Error: {e}")),
                    None => self.info(format!("{} {done} item(s)", job.kind.done_verb())),
                }
                if let Some((kind, pairs, clash)) = self.queue.pop_front() {
                    self.start(kind, pairs, clash, ctx);
                }
            }
        }

        // Files dropped from Finder / Explorer onto a panel.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            let pos = ctx.input(|i| i.pointer.latest_pos());
            let idx = pos.and_then(|p| self.list_rects.iter().position(|r| r.contains(p))).unwrap_or(self.active);
            self.dialog = Dialog::Drop { paths: dropped, dest: self.panels[idx].path.clone() };
        }
        self.track_window(ctx);
        self.settings.views = [self.panels[0].view(), self.panels[1].view()];
        self.settings.folders = [Some(self.panels[0].path.clone()), Some(self.panels[1].path.clone())];
        if self.settings != self.saved_settings {
            settings::save(&self.settings);
            self.saved_settings = self.settings.clone();
        }

        let busy = self.job.is_some() || self.panels.iter().any(Panel::is_calculating);
        let loading = self.panels.iter().any(Panel::is_loading);
        ctx.request_repaint_after(Duration::from_millis(if loading { 40 } else if busy { 150 } else { 1000 }));

        // Tab / Shift+Tab: switch between the left and right panel.
        // In the New dialog it switches between folder and file.
        if std::mem::take(&mut self.tab_presses) % 2 == 1 {
            match &mut self.dialog {
                Dialog::None => self.active = 1 - self.active,
                Dialog::Input { kind: InputKind::New { file }, .. } => { let f = !*file; set_new_kind(&mut self.dialog, f); }
                _ => {}
            }
        }
    }

    // ---- keyboard ---------------------------------------------------------

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if !matches!(self.dialog, Dialog::None) { return; }
        let typing = ctx.egui_wants_keyboard_input();
        // Take the key events we handle so egui does not also use them (e.g. Tab focus cycling).
        let (keys, texts) = ctx.input_mut(|i| {
            let mut keys = Vec::new();
            let mut texts = Vec::new();
            i.events.retain(|e| match e {
                Event::Key { key, physical_key, pressed: true, modifiers, .. } if !typing || is_global_key(*key) => {
                    // ⇧⌘. is layout dependent (Shift+. is ':' on German keyboards): use the physical key.
                    let key = if *physical_key == Some(Key::Period) { Key::Period } else { *key };
                    keys.push((key, *modifiers));
                    false
                }
                Event::Text(t) if !typing => { texts.push(t.clone()); false }
                _ => true,
            });
            (keys, texts)
        });
        // egui may run a frame twice with the same input (multi-pass layout). The events are
        // taken out above in every pass, but acted on only in the first.
        if ctx.current_pass_index() > 0 { return; }
        if typing {
            // In the filter box: Esc / Enter / arrows return to the list.
            for (k, _) in &keys {
                if matches!(k, Key::Escape | Key::Enter | Key::ArrowDown | Key::ArrowUp) {
                    ctx.memory_mut(|m| m.stop_text_input());
                }
            }
            for (k, m) in keys.into_iter().filter(|(k, _)| is_fkey(*k)) { self.on_key(k, m, ctx); }
            return;
        }
        for (k, m) in keys { self.on_key(k, m, ctx); }
        for t in texts {
            if t.trim().is_empty() { continue; }
            let (buf, at) = &mut self.type_ahead;
            if at.elapsed() > Duration::from_millis(900) { buf.clear(); }
            buf.push_str(&t.to_lowercase());
            *at = Instant::now();
            let needle = buf.clone();
            let p = self.act();
            if let Some(row) = (0..p.len()).find(|&r| p.row(r).name.to_lowercase().starts_with(&needle)) {
                p.click(row, false, false);
                p.scroll_to_cursor = true;
            }
        }
    }

    fn on_key(&mut self, key: Key, m: Modifiers, ctx: &egui::Context) {
        let page = (self.act().view_height / ROW_H).max(1.0) as isize;
        match key {
            // F-keys: plain, or the second layer with ⇧ or ⌘ (see `fkeys`, shown in the bottom bar).
            k if is_fkey(k) => {
                if let Some((_, _, Some(cmd))) = fkeys(m.shift || m.command, self.settings.use_trash).into_iter().find(|(n, _, _)| fkey_of(n) == Some(k)) {
                    self.run(cmd, ctx);
                }
            }
            Key::T if m.command => self.run(Cmd::Terminal, ctx),
            Key::Questionmark | Key::Slash if m.command => self.run(Cmd::Help, ctx),
            Key::Delete if m.shift => self.run(Cmd::Delete, ctx),
            Key::Delete => self.run(Cmd::Trash, ctx),
            Key::Comma if m.command => self.run(Cmd::Settings, ctx),
            Key::Q if m.command => self.run(Cmd::Quit, ctx),
            Key::ArrowUp if m.command => self.run(Cmd::Up, ctx),
            Key::ArrowDown if m.command => self.run(Cmd::Open, ctx),
            // ⌥↑ / ⌥↓ move one screen; PageUp / PageDown go to the edge of the visible page,
            // then page by page; with ⌘ (and Home / End) to the very top / bottom.
            Key::ArrowUp if m.alt => self.act().move_cursor(-page, m.shift),
            Key::ArrowDown if m.alt => self.act().move_cursor(page, m.shift),
            Key::ArrowUp => self.act().move_cursor(-1, m.shift),
            Key::ArrowDown => self.act().move_cursor(1, m.shift),
            Key::ArrowLeft if m.command => self.run(Cmd::Back, ctx),
            Key::ArrowRight if m.command => self.run(Cmd::Forward, ctx),
            Key::PageUp if m.command => self.act().move_cursor(-isize::MAX / 2, m.shift),
            Key::PageDown if m.command => self.act().move_cursor(isize::MAX / 2, m.shift),
            Key::PageUp => self.act().page(false, ROW_H, m.shift),
            Key::PageDown => self.act().page(true, ROW_H, m.shift),
            Key::Home => self.act().move_cursor(-isize::MAX / 2, m.shift),
            Key::End => self.act().move_cursor(isize::MAX / 2, m.shift),
            Key::Enter => self.run(Cmd::Open, ctx),
            Key::Backspace if m.command => self.run(Cmd::Trash, ctx),
            Key::Backspace => self.run(Cmd::Up, ctx),
            Key::Space | Key::Insert => self.act().toggle_mark(),
            Key::OpenBracket if m.command => self.run(Cmd::Back, ctx),
            Key::CloseBracket if m.command => self.run(Cmd::Forward, ctx),
            Key::A if m.command => self.run(Cmd::SelectAll, ctx),
            Key::R if m.command => self.run(Cmd::Refresh, ctx),
            Key::N if m.command && m.shift => self.run(Cmd::NewFolder, ctx),
            Key::N if m.command && m.alt => self.run(Cmd::NewFile, ctx),
            Key::Period if m.command && m.shift => self.run(Cmd::ToggleHidden, ctx),
            Key::H if m.command && m.shift => self.run(Cmd::ToggleHidden, ctx),
            Key::U if m.ctrl => self.run(Cmd::Swap, ctx),
            Key::C if m.command && m.alt => self.run(Cmd::CopyPath, ctx),
            Key::F if m.command => self.focus_filter = Some(self.active),
            Key::Escape => {
                let p = self.act();
                if !p.filter.is_empty() { p.filter.clear(); p.rebuild_visible(); } else { p.selected.clear(); }
            }
            _ => {}
        }
    }

    // ---- commands ---------------------------------------------------------

    fn run(&mut self, cmd: Cmd, ctx: &egui::Context) {
        // A repeated pass (see handle_keys) must not run a command, e.g. a button click, twice.
        if ctx.current_pass_index() > 0 { return; }
        // F8 follows the "Move to Trash" setting; ⇧F8 always deletes immediately.
        let cmd = if matches!(cmd, Cmd::Trash) && !self.settings.use_trash { Cmd::Delete } else { cmd };
        let targets = self.act().targets();
        let one_name = targets.first().map(|p| file_name(p)).unwrap_or_default();
        let what = if targets.len() == 1 { format!("“{one_name}”") } else { format!("{} items", targets.len()) };
        let other_dir = self.panels[1 - self.active].path.clone();
        match cmd {
            Cmd::Help => self.dialog = Dialog::Help,
            Cmd::Settings => self.dialog = Dialog::Settings,
            // F1 inside the focused terminal hides it; otherwise ask where to open the folder.
            Cmd::OpenIn if self.term_has_keys() => self.toggle_terminal(ctx),
            Cmd::OpenIn => {
                let mut items = vec![OpenItem::Terminal];
                items.extend(ops::installed_editors().into_iter().map(|(name, app)| OpenItem::App(name, app)));
                if cfg!(target_os = "macos") { items.push(OpenItem::Finder); }
                let sel = items.iter().position(|i| Some(i.key()) == self.last_open_in.as_deref()).unwrap_or(0);
                self.dialog = Dialog::OpenIn { dir: self.act().path.clone(), items, sel };
            }
            Cmd::Terminal if self.builtin_terminal() => self.toggle_terminal(ctx),
            Cmd::Terminal => {
                let dir = self.act().path.clone();
                let r = ops::open_terminal(&dir, &self.settings.terminal);
                self.report(r);
            }
            Cmd::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Cmd::Open => {
                let Some(e) = self.act().current() else { return };
                let (folder, path) = (e.is_folder(), e.path.clone());
                let r = if e.is_parent { self.act().go_up() } else if folder { self.act().navigate(&path, None) } else { ops::open_with_system(&path) };
                self.report(r);
            }
            Cmd::Up => { let r = self.act().go_up(); self.report(r); }
            Cmd::Back => { let r = self.act().go_back(); self.report(r); }
            Cmd::Forward => { let r = self.act().go_forward(); self.report(r); }
            Cmd::Refresh => {
                for p in &mut self.panels { p.rescan(); }
                let _ = self.drive_watch.refresh.send(());
            }
            Cmd::ToggleHidden => {
                let p = self.act();
                p.show_hidden = !p.show_hidden;
                p.rebuild_visible();
                let show = p.show_hidden;
                self.info(if show { "Showing hidden files in this panel" } else { "Hiding hidden files in this panel" });
            }
            Cmd::SelectAll => self.act().select_all(),
            Cmd::Swap => {
                let (l, r) = (self.panels[0].path.clone(), self.panels[1].path.clone());
                let a = self.panels[0].navigate(&r, None);
                let b = self.panels[1].navigate(&l, None);
                self.report(a.and(b));
            }
            Cmd::CopyPath => {
                let text = targets.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
                ctx.copy_text(text);
                self.info("Path copied");
            }
            Cmd::Reveal => {
                let dir = self.act().path.clone();
                self.report(ops::open_with_system(&dir));
            }
            Cmd::View => {
                let Some(e) = self.act().current() else { return };
                if e.is_folder() { return; }
                let path = e.path.clone();
                let viewer = self.settings.viewer.clone();
                match viewer.as_str() {
                    settings::VIEWER_QUICKLOOK => self.report(ops::preview(&path)),
                    name if name != settings::VIEWER_BUILTIN && ops::editor_app(name).is_some() => {
                        self.open_with(&path, name, ops::editor_app(name).unwrap());
                    }
                    // Built-in (also when the chosen app is gone).
                    _ => match read_text(&path) {
                        Some(text) => self.dialog = Dialog::View { title: file_name(&path), text },
                        None => self.report(ops::preview(&path)),
                    },
                }
            }
            Cmd::QuickLook => {
                let Some(e) = self.act().current().filter(|e| !e.is_parent) else { return };
                let path = e.path.clone();
                self.report(ops::preview(&path));
            }
            Cmd::EditSystem => {
                let Some(e) = self.act().current() else { return };
                if e.is_dir { return; }
                let path = e.path.clone();
                self.report(ops::open_in_editor(&path));
            }
            // Copies next to the originals: "name (1).ext", "name (2).ext", …
            Cmd::Duplicate if targets.is_empty() => {}
            Cmd::Duplicate => {
                let mut taken = std::collections::HashSet::new();
                let pairs = targets.into_iter().map(|s| {
                    let d = ops::unique_target(&s, &taken);
                    taken.insert(d.clone());
                    (s, d)
                }).collect();
                self.start(Kind::Copy, pairs, Clash::Replace, ctx);
            }
            Cmd::Edit => {
                let Some(e) = self.act().current() else { return };
                if e.is_dir { return; }
                let path = e.path.clone();
                let editor = self.settings.editor.clone();
                let app = match editor.as_str() {
                    settings::EDITOR_SYSTEM => None,
                    settings::EDITOR_AUTO => ops::vscode(),
                    // The chosen app, or VS Code / the system editor when it has been removed.
                    name => ops::editor_app(name).map(|p| (name, p)).or_else(|| ops::vscode()),
                };
                match app {
                    Some((name, app)) => self.open_with(&path, name, app),
                    None => self.report(ops::open_in_editor(&path)),
                }
            }
            Cmd::Copy | Cmd::Move if targets.is_empty() => {}
            // One item: the full target path with its name (stem selected), so it can be renamed
            // on the way; several items: just the folder.
            Cmd::Copy | Cmd::Move => {
                let one = targets.len() == 1;
                let dest = if one { other_dir.join(&one_name).display().to_string() } else { dir_string(&other_dir) };
                let (verb, kind) = if matches!(cmd, Cmd::Copy) { ("Copy", InputKind::Copy(targets)) } else { ("Move", InputKind::Move(targets)) };
                self.dialog = input(kind, verb, format!("{verb} {what} to:"), dest, verb, one);
            }
            Cmd::Rename => {
                let Some(e) = self.act().current().filter(|e| !e.is_parent) else { return };
                let path = e.path.clone();
                self.dialog = input(InputKind::Rename(path), "Rename", format!("New name for “{one_name}”:"), file_name(&e.path), "Rename", true);
            }
            Cmd::NewFolder => self.dialog = new_item(false),
            Cmd::NewFile => self.dialog = new_item(true),
            Cmd::Trash if !targets.is_empty() => {
                let pairs = targets.into_iter().map(|p| (p, PathBuf::new())).collect();
                self.start(Kind::Trash, pairs, Clash::Replace, ctx);
            }
            Cmd::Delete if !targets.is_empty() => {
                let pairs = targets.into_iter().map(|p| (p, PathBuf::new())).collect();
                self.dialog = Dialog::Confirm {
                    title: "Delete permanently?".into(),
                    text: format!("{what} will be deleted immediately. This cannot be undone."),
                    ok: "Delete", kind: Kind::Delete, pairs,
                };
            }
            Cmd::Trash | Cmd::Delete => {}
        }
    }

    fn submit_input(&mut self, kind: InputKind, buf: String, ctx: &egui::Context) {
        let buf = buf.trim().to_string();
        if buf.is_empty() { return; }
        let base = self.act().path.clone();
        match kind {
            InputKind::New { file } => {
                let path = ops::resolve_input(&buf, &base);
                let r = if file { ops::create_file(&path) } else { std::fs::create_dir_all(&path) };
                let first = Path::new(&buf).components().next().map(|c| c.as_os_str().to_string_lossy().into_owned());
                self.act().refresh(first.as_deref());
                self.report(r);
            }
            InputKind::Rename(src) => {
                let dst = src.with_file_name(&buf);
                if dst == src { return; }
                if ops::exists(&dst) && !ops::same_file(&src, &dst) {
                    return self.error(format!("“{buf}” already exists"));
                }
                let r = ops::move_path(&src, &dst, &mut |_, _| Ok(()));
                self.act().refresh(Some(&buf));
                self.report(r);
            }
            InputKind::Copy(src) => self.prepare(Kind::Copy, ops::plan_targets(&src, &buf, &base), ctx),
            InputKind::Move(src) => self.prepare(Kind::Move, ops::plan_targets(&src, &buf, &base), ctx),
        }
    }

    /// Check a copy/move, ask before overwriting, then run it in the background.
    fn prepare(&mut self, kind: Kind, pairs: Vec<(PathBuf, PathBuf)>, ctx: &egui::Context) {
        let pairs: Vec<_> = pairs.into_iter().filter(|(s, d)| !(kind == Kind::Move && s == d)).collect();
        if pairs.is_empty() { return; }
        for (s, d) in &pairs {
            if let Err(e) = ops::check_pair(s, d) { return self.error(e.to_string()); }
            if kind == Kind::Copy && ops::same_file(s, d) {
                return self.error(format!("“{}” is already in that folder", file_name(s)));
            }
        }
        let clashing: Vec<&PathBuf> = pairs.iter().filter(|(s, d)| ops::exists(d) && !ops::same_file(s, d)).map(|(_, d)| d).collect();
        if clashing.is_empty() {
            return self.start(kind, pairs, Clash::Replace, ctx);
        }
        let can_merge = pairs.iter().any(|(s, d)| s.is_dir() && d.is_dir() && !ops::same_file(s, d));
        let mut names: Vec<String> = clashing.iter().take(3).map(|d| format!("“{}”", file_name(d))).collect();
        if clashing.len() > 3 { names.push(format!("and {} more", clashing.len() - 3)); }
        let count = clashing.len();
        self.dialog = Dialog::Clash { kind, pairs, count, names: names.join(", "), can_merge };
    }

    /// Rename: every clashing target gets the first free "name (n)".
    fn keep_both(pairs: Vec<(PathBuf, PathBuf)>) -> Vec<(PathBuf, PathBuf)> {
        let mut taken = std::collections::HashSet::new();
        pairs.into_iter().map(|(s, d)| {
            let d = if (ops::exists(&d) && !ops::same_file(&s, &d)) || taken.contains(&d) { ops::unique_target(&d, &taken) } else { d };
            taken.insert(d.clone());
            (s, d)
        }).collect()
    }

    fn start(&mut self, kind: Kind, pairs: Vec<(PathBuf, PathBuf)>, clash: Clash, ctx: &egui::Context) {
        if self.job.is_some() {
            // One operation at a time: wait in line behind the running one.
            self.queue.push_back((kind, pairs, clash));
            let n = self.queue.len();
            return self.info(format!("{} queued: starts when the current operation is done ({n} waiting)", kind.ing()));
        }
        self.speed = SpeedMeter::default();
        self.job = Some(jobs::spawn(kind, pairs, clash, ctx.clone()));
    }

    fn drop_into(&mut self, payload: &DragPayload, dest: &Path) {
        let paths: Vec<PathBuf> = payload.paths.iter().filter(|p| p.parent() != Some(dest) && p.as_path() != dest).cloned().collect();
        if paths.is_empty() { return; }
        self.dialog = Dialog::Drop { paths, dest: dest.to_path_buf() };
    }

    // ---- layout -----------------------------------------------------------

    fn sidebar(&mut self, ui: &mut Ui) {
        let mut go: Option<PathBuf> = None;
        let mut drop: Option<(DragPayload, PathBuf)> = None;
        let mut open_settings = false;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(6.0);
            for (title, list) in [("Favorites", &self.favorites), ("Devices", &self.drives)] {
                ui.label(RichText::new(title.to_uppercase()).size(11.0).strong().color(pal().text_dim));
                ui.add_space(2.0);
                for place in list {
                    let current = self.panels[self.active].path == place.path;
                    let h = if place.space.is_some() { 34.0 } else { 24.0 };
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
                    let hovered_drop = resp.dnd_hover_payload::<DragPayload>().is_some();
                    let p = ui.painter();
                    if current || hovered_drop {
                        p.rect_filled(rect, 5.0, if hovered_drop { pal().accent_soft } else { pal().sidebar_current });
                    } else if resp.hovered() {
                        p.rect_filled(rect, 5.0, pal().sidebar_hover);
                    }
                    let icon_r = Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2(16.0, 16.0));
                    paint_icon(p, icon_r, place.icon);
                    p.with_clip_rect(rect.shrink(2.0)).text(rect.min + vec2(32.0, 12.0), Align2::LEFT_CENTER, &place.name, pal().font(13.0), if current && pal().marks_as_text { pal().accent_text } else { pal().sidebar_text });
                    if let Some((free, total)) = place.space {
                        let bar = Rect::from_min_size(rect.min + vec2(32.0, 22.0), vec2((rect.width() - 40.0).max(10.0), 4.0));
                        paint_capacity(p, bar, 1.0 - free as f32 / total.max(1) as f32);
                        resp.clone().on_hover_text(format!("{} free of {}\n{}", human_size(free), human_size(total), place.path.display()));
                    }
                    if resp.clicked() { go = Some(place.path.clone()); }
                    if let Some(pl) = resp.dnd_release_payload::<DragPayload>() { drop = Some(((*pl).clone(), place.path.clone())); }
                }
                if title == "Favorites" && self.finder_favorites.no_access.load(Ordering::Relaxed) {
                    // Without Full Disk Access macOS asks for every protected folder separately.
                    ui.add_space(2.0);
                    let link = ui.add(egui::Button::new(RichText::new("Allow access to all folders…").size(12.0).color(pal().accent)).frame(false))
                        .on_hover_text("Opens System Settings → Privacy & Security → Full Disk Access. Turn on Oxide Commander \
                            there, then restart the app: macOS stops asking about Desktop, Documents, Downloads and drives, \
                            and the sidebar shows your Finder favourites.");
                    if link.clicked() { open_settings = true; }
                }
                ui.add_space(10.0);
            }
        });
        if let Some(dir) = go { let r = self.act().navigate(&dir, None); self.report(r); }
        if let Some((pl, dest)) = drop { self.drop_into(&pl, &dest); }
        if open_settings {
            let r = ops::open_with_system(Path::new("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"));
            self.report(r);
        }
    }

    fn panel_ui(&mut self, ui: &mut Ui, idx: usize) {
        let active = idx == self.active && self.term_focus.is_none();
        let frame = egui::Frame::new()
            .fill(pal().list_bg)
            .stroke(Stroke::new(if active { 2.0 } else { 1.0 }, if active { pal().accent } else { pal().border }))
            .corner_radius(CornerRadius::same(6))
            .inner_margin(Margin::same(0));
        frame.show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.spacing_mut().item_spacing = vec2(4.0, 0.0);
            self.panel_toolbar(ui, idx);
            ui.add(egui::Separator::default().spacing(0.0));
            self.panel_list(ui, idx);
        });
    }

    fn panel_toolbar(&mut self, ui: &mut Ui, idx: usize) {
        let mut nav: Option<Cmd> = None;
        let mut go: Option<PathBuf> = None;
        ui.add_space(5.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            let p = &self.panels[idx];
            if ui.add_enabled(p.can_back(), egui::Button::new("←").min_size(vec2(26.0, 22.0))).on_hover_text("Back (⌘[)").clicked() { nav = Some(Cmd::Back); }
            if ui.add_enabled(p.can_forward(), egui::Button::new("→").min_size(vec2(26.0, 22.0))).on_hover_text("Forward (⌘])").clicked() { nav = Some(Cmd::Forward); }
            if ui.add_enabled(p.path.parent().is_some(), egui::Button::new("↑").min_size(vec2(26.0, 22.0))).on_hover_text("Enclosing folder (Backspace)").clicked() { nav = Some(Cmd::Up); }
            ui.add_space(4.0);

            // Filter box on the right, breadcrumb path in the remaining space.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(6.0);
                let p = &mut self.panels[idx];
                let resp = ui.add(TextEdit::singleline(&mut p.filter).hint_text("🔍 Filter").desired_width(120.0));
                if self.focus_filter == Some(idx) { resp.request_focus(); self.focus_filter = None; }
                if resp.changed() { p.rebuild_visible(); }
                if resp.gained_focus() { self.active = idx; }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ScrollArea::horizontal().id_salt(("crumbs", idx)).stick_to_right(true).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            let mut acc = PathBuf::new();
                            let comps: Vec<_> = p.path.components().collect();
                            for (i, c) in comps.iter().enumerate() {
                                acc.push(c.as_os_str());
                                let label = match c {
                                    // On Windows the drive letter (Prefix) is already shown.
                                    std::path::Component::RootDir if cfg!(windows) => continue,
                                    std::path::Component::RootDir if cfg!(target_os = "macos") => "Macintosh HD".to_string(),
                                    std::path::Component::RootDir => "/".to_string(),
                                    other => other.as_os_str().to_string_lossy().to_string(),
                                };
                                let last = i + 1 == comps.len();
                                let text = RichText::new(label).size(12.5).color(if last { pal().header_active } else { pal().text_dim });
                                if ui.add(egui::Button::new(if last { text.strong() } else { text }).frame(false)).clicked() { go = Some(acc.clone()); }
                                if !last { ui.label(RichText::new("›").color(pal().text_dim)); }
                            }
                        });
                    });
                });
            });
        });
        ui.add_space(5.0);
        if nav.is_some() || go.is_some() { self.active = idx; }
        if let Some(c) = nav { self.run(c, &ui.ctx().clone()); }
        if let Some(dir) = go { let r = self.panels[idx].navigate(&dir, None); self.report(r); }
    }

    fn panel_list(&mut self, ui: &mut Ui, idx: usize) {
        let active = idx == self.active;
        let ctx = ui.ctx().clone();
        let full = ui.available_rect_before_wrap();
        // Columns: name gets the remaining width.
        let w = full.width();
        let date_w = if pal().monospace { 165.0 } else { 140.0 };
        let size_w = 80.0;
        let kind_w = if w > 520.0 { 110.0 } else { 0.0 };
        let name_w = (w - date_w - size_w - kind_w).max(80.0);
        let cols = [(0.0, name_w), (name_w, size_w), (name_w + size_w, kind_w), (name_w + size_w + kind_w, date_w)];

        // Header with clickable sort columns.
        let (hdr, _) = ui.allocate_exact_size(vec2(w, 22.0), Sense::hover());
        ui.painter().rect_filled(hdr, 0.0, pal().header_bg);
        let headers = [("Name", SortKey::Name), ("Size", SortKey::Size), ("Kind", SortKey::Kind), ("Date Modified", SortKey::Date)];
        for (i, (label, key)) in headers.iter().enumerate() {
            let (x, cw) = cols[i];
            if cw <= 0.0 { continue; }
            let r = Rect::from_min_size(hdr.min + vec2(x, 0.0), vec2(cw, hdr.height()));
            let resp = ui.interact(r, Id::unique(("hdr", idx, i)), Sense::click());
            let p = &self.panels[idx];
            let arrow = if p.sort == *key { if p.ascending { " ▲" } else { " ▼" } } else { "" };
            let align = if i == 1 { Align2::RIGHT_CENTER } else { Align2::LEFT_CENTER };
            let pos = if i == 1 { r.right_center() - vec2(10.0, 0.0) } else { r.left_center() + vec2(if i == 0 { 30.0 } else { 6.0 }, 0.0) };
            ui.painter().with_clip_rect(r).text(pos, align, format!("{label}{arrow}"), pal().font(12.0), if p.sort == *key || pal().marks_as_text { pal().header_active } else { pal().text_dim });
            if i > 0 { ui.painter().vline(r.left(), r.y_range().shrink(4.0), Stroke::new(1.0, pal().line)); }
            if resp.clicked() { self.active = idx; self.panels[idx].set_sort(*key); }
        }
        ui.painter().hline(hdr.x_range(), hdr.bottom(), Stroke::new(1.0, pal().line));

        let status_h = 24.0;
        let list_h = (ui.available_height() - status_h).max(ROW_H);
        let mut events: Vec<(usize, RowEvent)> = Vec::new();

        let p = &mut self.panels[idx];
        let mut area = ScrollArea::vertical().id_salt(("list", idx)).auto_shrink(false).max_height(list_h);
        if p.scroll_to_cursor && p.view_height > 0.0 {
            let top = p.cursor as f32 * ROW_H;
            let off = p.scroll_offset;
            let target = if top < off { top } else if top + ROW_H > off + p.view_height { top + ROW_H - p.view_height } else { off };
            area = area.vertical_scroll_offset(target);
        }
        p.scroll_to_cursor = false;
        let n = p.len();
        let p = &self.panels[idx];
        let out = ui.allocate_ui(vec2(w, list_h), |ui| {
            area.show_rows(ui, ROW_H, n, |ui, range| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                for row in range {
                    let e = p.row(row);
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, ROW_H), Sense::click_and_drag());
                    let painter = ui.painter();
                    let selected = p.is_selected(row);
                    let drop_hover = e.is_folder() && resp.dnd_hover_payload::<DragPayload>().is_some_and(|pl| !pl.paths.contains(&e.path));
                    let pl = pal();
                    let is_cursor = row == p.cursor;
                    let (bg, fg, dim) = if pl.marks_as_text {
                        // Norton: the cyan bar is the cursor, marked files have yellow text.
                        let bar = is_cursor && active;
                        let marked = p.selected.contains(&e.path);
                        let bg = if drop_hover { pl.accent_soft } else if bar { pl.accent } else if resp.hovered() { pl.hover } else { pl.list_bg };
                        let fg = if bar { pl.accent_text } else if marked { pl.marked_text } else if e.hidden { pl.text_hidden } else if e.is_dir { pl.folder_text } else { pl.text };
                        (bg, fg, if bar { pl.accent_text } else if marked { pl.marked_text } else { pl.text_dim })
                    } else {
                        let bg = if drop_hover { pl.accent_soft }
                            else if selected && active { pl.accent }
                            else if selected { pl.selected_inactive }
                            else if resp.hovered() { pl.hover }
                            else if row % 2 == 1 { pl.stripe } else { pl.list_bg };
                        let fg = if selected && active { pl.accent_text } else if e.hidden { pl.text_hidden } else { pl.text };
                        (bg, fg, if selected && active { pl.accent_text.gamma_multiply(0.85) } else { pl.text_dim })
                    };
                    painter.rect_filled(rect, 0.0, bg);
                    if !pl.marks_as_text && is_cursor && !p.selected.is_empty() && active {
                        painter.rect_stroke(rect.shrink(1.0), 2.0, Stroke::new(1.0, pl.accent), StrokeKind::Inside);
                    }
                    paint_icon(painter, Rect::from_min_size(rect.min + vec2(8.0, 3.0), vec2(16.0, 16.0)), places::icon_for(e));
                    let font = pl.font(13.0);
                    let clip = |i: usize| Rect::from_min_size(rect.min + vec2(cols[i].0, 0.0), vec2(cols[i].1 - 6.0, ROW_H));
                    painter.with_clip_rect(clip(0)).text(rect.left_center() + vec2(30.0, 0.0), Align2::LEFT_CENTER, &e.name, font.clone(), fg);
                    let size = match p.size_of(e) {
                        Some(bytes) => human_size(bytes),
                        None if e.is_parent => String::new(),
                        None if p.is_calculating() => "…".to_string(),
                        None => "--".to_string(),
                    };
                    painter.with_clip_rect(clip(1)).text(rect.left_center() + vec2(cols[1].0 + cols[1].1 - 10.0, 0.0), Align2::RIGHT_CENTER, size, font.clone(), dim);
                    if kind_w > 0.0 {
                        painter.with_clip_rect(clip(2)).text(rect.left_center() + vec2(cols[2].0 + 6.0, 0.0), Align2::LEFT_CENTER, e.kind(), font.clone(), dim);
                    }
                    let date = e.modified.map(|t| DateTime::<Local>::from(t).format("%-d %b %Y, %H:%M").to_string()).unwrap_or_default();
                    painter.with_clip_rect(clip(3)).text(rect.left_center() + vec2(cols[3].0 + 6.0, 0.0), Align2::LEFT_CENTER, date, font, dim);

                    let mods = ui.input(|i| i.modifiers);
                    if resp.double_clicked() { events.push((row, RowEvent::Open)); }
                    else if resp.clicked() { events.push((row, RowEvent::Click(mods.command, mods.shift))); }
                    if resp.secondary_clicked() { events.push((row, RowEvent::Context)); }
                    if resp.drag_started() && !e.is_parent {
                        let paths = if p.is_selected(row) { p.targets() } else { vec![e.path.clone()] };
                        events.push((row, RowEvent::DragStart(paths)));
                    }
                    if e.is_folder() && let Some(pl) = resp.dnd_release_payload::<DragPayload>() {
                        events.push((row, RowEvent::DropOn((*pl).clone(), e.path.clone())));
                    }
                    resp.context_menu(|ui| {
                        let items: [(&str, Cmd); 8] = [("Open", Cmd::Open), ("Quick Look  F3", Cmd::View), ("Rename…  F2", Cmd::Rename),
                            ("Copy to other side…  F5", Cmd::Copy), ("Move to other side…  F6", Cmd::Move),
                            ("Copy Path", Cmd::CopyPath), ("Open Folder in System", Cmd::Reveal), ("Move to Trash  F8", Cmd::Trash)];
                        for (label, cmd) in items {
                            if ui.button(label).clicked() { events.push((row, RowEvent::Cmd(cmd))); ui.close(); }
                        }
                    });
                }
            })
        });
        let out = out.inner;
        let p = &mut self.panels[idx];
        p.scroll_offset = out.state.clamped_offset().y;
        p.view_height = out.inner_rect.height();
        self.list_rects[idx] = out.inner_rect;

        // Clicking the empty space below the last row activates the panel and clears the
        // selection (as in Finder). Only that area: a widget covering the rows would take their clicks.
        let rows_bottom = out.inner_rect.top() + n as f32 * ROW_H - out.state.clamped_offset().y;
        let empty = Rect::from_min_max(egui::pos2(out.inner_rect.left(), rows_bottom.max(out.inner_rect.top())), out.inner_rect.max);
        let bg = if empty.height() > 1.0 { ui.interact(empty, Id::unique(("listbg", idx)), Sense::click()) } else { ui.interact(Rect::NOTHING, Id::unique(("listbg", idx)), Sense::hover()) };
        if bg.clicked() {
            self.active = idx;
            self.panels[idx].selected.clear();
        }
        let released = ctx.input(|i| i.pointer.any_released());
        let over = ctx.input(|i| i.pointer.latest_pos()).is_some_and(|pos| out.inner_rect.contains(pos));

        for (row, ev) in events {
            self.active = idx;
            let p = &mut self.panels[idx];
            match ev {
                RowEvent::Click(toggle, range) => p.click(row, toggle, range),
                RowEvent::Open => { p.click(row, false, false); self.run(Cmd::Open, &ctx); }
                RowEvent::Context => { if !p.is_selected(row) { p.click(row, false, false); } }
                RowEvent::Cmd(c) => self.run(c, &ctx),
                RowEvent::DragStart(paths) => {
                    if !p.is_selected(row) { p.click(row, false, false); }
                    DragAndDrop::set_payload(&ctx, DragPayload { from: idx, paths });
                }
                RowEvent::DropOn(pl, dest) => self.drop_into(&pl, &dest),
            }
        }
        if released && over && let Some(pl) = DragAndDrop::take_payload::<DragPayload>(&ctx) && pl.from != idx {
            let dest = self.panels[idx].path.clone();
            self.drop_into(&pl, &dest);
        }

        // Status line.
        let (items, sel, bytes) = self.panels[idx].summary();
        let free = self.free_space(&self.panels[idx].path.clone());
        let loading = self.panels[idx].is_loading();
        let mut text = if loading { "Loading folder…".to_string() } else { format!("{items} items") };
        if sel > 0 { text += &format!(" · {sel} selected ({})", human_size(bytes)); }
        if let Some(f) = free { text += &format!(" · {} available", human_size(f)); }
        if self.panels[idx].is_calculating() { text += " · calculating folder sizes…"; }
        if loading {
            ui.painter().text(self.list_rects[idx].center(), Align2::CENTER_CENTER, "Loading…", pal().font(14.0), pal().text_dim);
        }
        let (sr, _) = ui.allocate_exact_size(vec2(w, status_h), Sense::hover());
        ui.painter().hline(sr.x_range(), sr.top(), Stroke::new(1.0, pal().line));
        ui.painter().with_clip_rect(sr.with_max_x(sr.right() - 120.0)).text(sr.left_center() + vec2(10.0, 0.0), Align2::LEFT_CENTER, text, pal().font(11.5), pal().text_dim);
        // Per-panel "hidden files" switch (remembered between launches).
        let p = &mut self.panels[idx];
        let mut hidden = p.show_hidden;
        let cb = ui.put(Rect::from_min_max(sr.right_top() - vec2(116.0, -2.0), sr.right_bottom()),
            egui::Checkbox::new(&mut hidden, RichText::new("Hidden files").size(11.5).color(pal().text_dim)));
        if cb.changed() {
            p.show_hidden = hidden;
            p.rebuild_visible();
            self.active = idx;
        }
    }

    fn free_space(&self, path: &Path) -> Option<u64> {
        self.drives.iter()
            .filter(|d| path.starts_with(&d.path))
            .max_by_key(|d| d.path.as_os_str().len())
            .and_then(|d| d.space.map(|s| s.0))
    }

    fn bottom_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        // Progress / message line.
        ui.horizontal(|ui| {
            ui.set_height(22.0);
            if let Some(job) = &self.job {
                let pr = &job.progress;
                let total = pr.total.load(Ordering::Relaxed).max(1);
                let done = pr.done.load(Ordering::Relaxed).min(total);
                let name = pr.current.lock().unwrap().clone();
                ui.label(format!("{} {name}", job.kind.ing()));
                if pr.measuring.load(Ordering::Relaxed) {
                    ui.add(egui::ProgressBar::new(0.0).desired_width(240.0).animate(true).text("Calculating size…"));
                } else if pr.in_bytes.load(Ordering::Relaxed) {
                    let bar = egui::ProgressBar::new(done as f32 / total as f32).desired_width(240.0)
                        .text(format!("{} of {}", human_size(done), human_size(total)));
                    ui.add(bar);
                    if let Some(rate) = self.speed.sample(done) {
                        let left = (total - done) as f64 / rate;
                        ui.label(RichText::new(format!("{}/s · {} left", human_size(rate as u64), human_duration(left))).color(pal().text_dim));
                    }
                } else {
                    ui.add(egui::ProgressBar::new(done as f32 / total as f32).desired_width(240.0).show_percentage());
                }
                if ui.button("Cancel").clicked() { pr.cancel.store(true, Ordering::Relaxed); }
                if !self.queue.is_empty() {
                    ui.label(RichText::new(format!("+{} queued", self.queue.len())).color(pal().text_dim));
                    if ui.button("Clear queue").clicked() { self.queue.clear(); }
                }
            } else if let Some((text, is_err, at)) = &self.message
                && at.elapsed() < Duration::from_secs(6) {
                    ui.label(RichText::new(text).color(if *is_err { pal().error } else { pal().text_dim }));
                }
        });
        ui.add_space(4.0);
        // Function key buttons.
        // Holding ⇧ or ⌘ shows (and clicking runs) the second layer.
        let alt = ui.input(|i| i.modifiers.shift || i.modifiers.command);
        let keys = fkeys(alt, self.settings.use_trash);
        let gap = 6.0;
        let bw = ((ui.available_width() - gap * (keys.len() as f32 - 1.0)) / keys.len() as f32).max(60.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (k, label, cmd) in keys {
                let mut job = egui::text::LayoutJob::default();
                let label_color = if pal().marks_as_text { Color32::BLACK } else { ui.visuals().widgets.inactive.fg_stroke.color };
                if pal().marks_as_text {
                    // Norton: white number on black, then the label on a cyan button.
                    job.append(k, 0.0, egui::TextFormat { font_id: pal().font(12.0), color: pal().fkey_number, background: Color32::BLACK, ..Default::default() });
                } else {
                    job.append(k, 0.0, egui::TextFormat { font_id: pal().font(11.5), color: pal().fkey_number, ..Default::default() });
                }
                job.append(label, 6.0, egui::TextFormat { font_id: pal().font(13.0), color: label_color, ..Default::default() });
                let button = ui.add_enabled(cmd.is_some(), egui::Button::new(job).min_size(vec2(bw, 30.0)));
                if let Some(cmd) = cmd && button.clicked() { self.run(cmd, &ctx); }
            }
        });
    }

    // ---- dialogs ----------------------------------------------------------

    fn dialogs(&mut self, ctx: &egui::Context) {
        let mut dialog = std::mem::replace(&mut self.dialog, Dialog::None);
        let keep = self.show_dialog(ctx, &mut dialog);
        if keep && matches!(self.dialog, Dialog::None) { self.dialog = dialog; }
    }

    /// Returns true while the dialog should stay open.
    fn show_dialog(&mut self, ctx: &egui::Context, dialog: &mut Dialog) -> bool {
        let mut keep = true;
        match dialog {
            Dialog::None => return false,
            Dialog::Input { kind, title, prompt, buf, ok, first_frame, select_stem } => {
                let mut submit = false;
                let mut switch_to = None;
                let resp = Modal::new(Id::unique("input")).show(ctx, |ui| {
                    ui.set_width(440.0);
                    ui.label(RichText::new(title.as_str()).size(16.0).strong());
                    ui.add_space(6.0);
                    if let InputKind::New { file } = kind {
                        ui.horizontal(|ui| {
                            let mut f = *file;
                            ui.radio_value(&mut f, false, RichText::new("Folder").color(pal().text));
                            ui.radio_value(&mut f, true, RichText::new("File").color(pal().text));
                            ui.label(RichText::new("Tab switches").weak());
                            if f != *file { switch_to = Some(f); }
                        });
                        ui.add_space(4.0);
                    }
                    ui.label(prompt.as_str());
                    ui.add_space(4.0);
                    let mut out = TextEdit::singleline(buf).desired_width(f32::INFINITY).show(ui);
                    if *first_frame {
                        out.response.request_focus();
                        let (start, end) = if *select_stem { stem_range(buf) } else { (0, buf.chars().count()) };
                        out.state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(start), CCursor::new(end))));
                        out.state.store(ui.ctx(), out.response.id);
                        *first_frame = false;
                    }
                    if out.response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) { submit = true; }
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new(*ok).color(pal().accent_text)).fill(pal().accent).min_size(vec2(80.0, 26.0))).clicked() { submit = true; }
                        if ui.add(egui::Button::new("Cancel").min_size(vec2(80.0, 26.0))).clicked() { keep = false; }
                    });
                });
                if resp.should_close() { keep = false; }
                if let Some(f) = switch_to { set_new_kind(dialog, f); }
                if submit {
                    keep = false;
                    if let Dialog::Input { kind, buf, .. } = std::mem::replace(dialog, Dialog::None) {
                        self.submit_input(kind, buf, ctx);
                    }
                }
            }
            Dialog::Confirm { title, text, ok, .. } => {
                let mut yes = false;
                let resp = Modal::new(Id::unique("confirm")).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.label(RichText::new(title.as_str()).size(16.0).strong());
                    ui.add_space(6.0);
                    ui.label(text.as_str());
                    ui.add_space(12.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new(*ok).color(Color32::WHITE)).fill(pal().danger).min_size(vec2(80.0, 26.0))).clicked() { yes = true; }
                        if ui.add(egui::Button::new("Cancel").min_size(vec2(80.0, 26.0))).clicked() { keep = false; }
                    });
                    if ui.input(|i| i.key_pressed(Key::Enter)) { yes = true; }
                });
                if resp.should_close() { keep = false; }
                if yes {
                    keep = false;
                    if let Dialog::Confirm { kind, pairs, .. } = std::mem::replace(dialog, Dialog::None) {
                        self.start(kind, pairs, Clash::Replace, ctx);
                    }
                }
            }
            Dialog::Clash { kind, count, names, can_merge, .. } => {
                let mut choice: Option<Option<Clash>> = None; // Some(None) = rename
                let resp = Modal::new(Id::unique("clash")).show(ctx, |ui| {
                    ui.set_width(460.0);
                    let title = if *count == 1 { "An item with this name already exists" } else { "Items with these names already exist" };
                    ui.label(RichText::new(title).size(16.0).strong());
                    ui.add_space(6.0);
                    ui.label(format!("{names} {} in the destination.", if *count == 1 { "is already" } else { "are already" }));
                    ui.add_space(4.0);
                    let verb = if *kind == Kind::Move { "moved" } else { "copied" };
                    let mut help = format!("Replace: remove the existing {}.  Rename: keep both, the {verb} {} gets a number, e.g. “name (1)”.",
                        if *count == 1 { "item" } else { "items" }, if *count == 1 { "item" } else { "items" });
                    if *can_merge { help += "  Merge: combine folder contents (same names inside are replaced)."; }
                    ui.label(RichText::new(help).size(12.0).color(pal().text_dim));
                    ui.add_space(12.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new("Replace").color(Color32::WHITE)).fill(pal().danger).min_size(vec2(80.0, 26.0))).clicked() { choice = Some(Some(Clash::Replace)); }
                        if *can_merge && ui.add(egui::Button::new("Merge").min_size(vec2(80.0, 26.0))).clicked() { choice = Some(Some(Clash::Merge)); }
                        if ui.add(egui::Button::new(RichText::new("Rename").color(pal().accent_text)).fill(pal().accent).min_size(vec2(80.0, 26.0))).clicked() { choice = Some(None); }
                        if ui.add(egui::Button::new("Cancel").min_size(vec2(80.0, 26.0))).clicked() { keep = false; }
                    });
                    // Enter takes the harmless choice (nothing is lost).
                    if ui.input(|i| i.key_pressed(Key::Enter)) { choice = Some(None); }
                });
                if resp.should_close() { keep = false; }
                if let Some(c) = choice {
                    keep = false;
                    if let Dialog::Clash { kind, pairs, .. } = std::mem::replace(dialog, Dialog::None) {
                        match c {
                            Some(clash) => self.start(kind, pairs, clash, ctx),
                            None => self.start(kind, Self::keep_both(pairs), Clash::Replace, ctx),
                        }
                    }
                }
            }
            Dialog::Drop { paths, dest } => {
                let mut choice: Option<Kind> = None;
                let resp = Modal::new(Id::unique("drop")).show(ctx, |ui| {
                    ui.set_width(420.0);
                    let what = if paths.len() == 1 { format!("“{}”", file_name(&paths[0])) } else { format!("{} items", paths.len()) };
                    ui.label(RichText::new(format!("Copy or move {what}?")).size(16.0).strong());
                    ui.add_space(6.0);
                    ui.label(format!("To: {}", dest.display()));
                    ui.add_space(12.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new(RichText::new("Copy").color(pal().accent_text)).fill(pal().accent).min_size(vec2(80.0, 26.0))).clicked() { choice = Some(Kind::Copy); }
                        if ui.add(egui::Button::new("Move").min_size(vec2(80.0, 26.0))).clicked() { choice = Some(Kind::Move); }
                        if ui.add(egui::Button::new("Cancel").min_size(vec2(80.0, 26.0))).clicked() { keep = false; }
                    });
                    if ui.input(|i| i.key_pressed(Key::Enter)) { choice = Some(Kind::Copy); }
                });
                if resp.should_close() { keep = false; }
                if let Some(kind) = choice {
                    keep = false;
                    let pairs = paths.iter().map(|p| (p.clone(), dest.join(p.file_name().unwrap_or_default()))).collect();
                    self.prepare(kind, pairs, ctx);
                }
            }
            Dialog::View { title, text } => {
                let screen = ctx.content_rect();
                let resp = Modal::new(Id::unique("view")).show(ctx, |ui| {
                    ui.set_width(screen.width() * 0.75);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(title.as_str()).size(15.0).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Close").clicked() || ui.input(|i| i.key_pressed(Key::F3)) { keep = false; }
                        });
                    });
                    ui.separator();
                    ScrollArea::both().max_height(screen.height() * 0.75).auto_shrink(false).show(ui, |ui| {
                        ui.add(egui::Label::new(RichText::new(text.as_str()).monospace().size(12.5)).extend());
                    });
                });
                if resp.should_close() { keep = false; }
            }
            Dialog::OpenIn { dir, items, sel } => {
                let mut choice: Option<usize> = None;
                let terminal_label = if self.builtin_terminal() { "Terminal (below the panel)".to_string() }
                    else { format!("{} window", self.settings.terminal) };
                // Keys act once per frame (egui may run a frame twice with the same input).
                if ctx.current_pass_index() == 0 {
                    ctx.input_mut(|i| {
                        let n = items.len();
                        if i.consume_key(Modifiers::NONE, Key::ArrowDown) { *sel = (*sel + 1) % n; }
                        if i.consume_key(Modifiers::NONE, Key::ArrowUp) { *sel = (*sel + n - 1) % n; }
                        if i.consume_key(Modifiers::NONE, Key::Enter) { choice = Some(*sel); }
                        if i.consume_key(Modifiers::NONE, Key::F1) { choice = Some(0); } // F1 F1 = terminal
                        const DIGITS: [Key; 9] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
                        for (k, key) in DIGITS.into_iter().enumerate().take(n) {
                            if i.consume_key(Modifiers::NONE, key) { choice = Some(k); }
                        }
                    });
                }
                let resp = Modal::new(Id::unique("open_in")).show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.label(RichText::new("Open folder in…").size(16.0).strong());
                    ui.label(RichText::new(terminal::short_dir(dir)).size(12.0).color(pal().text_dim));
                    ui.add_space(8.0);
                    for (k, item) in items.iter().enumerate() {
                        let name = match item {
                            OpenItem::Terminal => terminal_label.clone(),
                            OpenItem::App(name, _) => name.to_string(),
                            OpenItem::Finder => "Finder".to_string(),
                        };
                        let text = if k < 9 { format!("{}   {name}", k + 1) } else { format!("    {name}") };
                        let r = ui.add_sized([ui.available_width(), 26.0], egui::Button::selectable(k == *sel, RichText::new(text).size(14.0)));
                        if r.clicked() { choice = Some(k); }
                        if r.hovered() { *sel = k; }
                    }
                    ui.add_space(8.0);
                    ui.label(RichText::new("↑↓ and Enter, or the number. F1 again: terminal. Esc: cancel.").size(11.5).color(pal().text_dim));
                });
                if resp.should_close() { keep = false; }
                if let Some(k) = choice {
                    keep = false;
                    let (dir, item) = (dir.clone(), items[k].clone());
                    self.last_open_in = Some(item.key().to_string());
                    match item {
                        OpenItem::Terminal => self.run(Cmd::Terminal, ctx),
                        OpenItem::App(name, app) => {
                            match ops::open_in_app(&dir, &app) {
                                Ok(()) => self.info(format!("Opened in {name}")),
                                Err(e) => self.error(format!("Could not open {name}: {e}")),
                            }
                        }
                        OpenItem::Finder => { let r = ops::open_with_system(&dir); self.report(r); }
                    }
                }
            }
            Dialog::Settings => {
                let resp = Modal::new(Id::unique("settings")).show(ctx, |ui| {
                    ui.set_width(440.0);
                    ui.label(RichText::new("Settings").size(16.0).strong());
                    ui.add_space(10.0);
                    ui.label(RichText::new("Appearance").strong());
                    ui.horizontal(|ui| {
                        for t in Theme::ALL {
                            if ui.radio_value(&mut self.settings.theme, t, RichText::new(t.label()).color(pal().text)).changed() {
                                theme::apply(ctx, t);
                            }
                        }
                    });
                    ui.add_space(10.0);
                    ui.label(RichText::new("Deleting (F8)").strong());
                    ui.radio_value(&mut self.settings.use_trash, true, RichText::new("Move to Trash").color(pal().text));
                    ui.radio_value(&mut self.settings.use_trash, false, RichText::new("Delete immediately (asks first, cannot be undone)").color(pal().text));
                    ui.add_space(10.0);
                    ui.label(RichText::new("Terminal (F1)").strong());
                    for m in TerminalMode::ALL {
                        ui.radio_value(&mut self.settings.terminal_mode, m, RichText::new(m.label()).color(pal().text));
                    }
                    let terminals = ops::installed_terminals();
                    if !terminals.is_empty() && self.settings.terminal_mode == TerminalMode::External {
                        egui::ComboBox::from_id_salt("terminal")
                            .selected_text(RichText::new(self.settings.terminal.as_str()).color(pal().text))
                            .show_ui(ui, |ui| {
                                for t in terminals {
                                    ui.selectable_value(&mut self.settings.terminal, t.to_string(), t);
                                }
                            });
                    }
                    ui.add_space(10.0);
                    let editors: Vec<&'static str> = {
                        let mut v: Vec<_> = ops::installed_editors().into_iter().map(|(n, _)| n).collect();
                        if let Some((n, _)) = ops::vscode() && !v.contains(&n) { v.insert(0, n); }
                        v
                    };
                    ui.horizontal(|ui| {
                        ui.add_sized(vec2(90.0, 20.0), egui::Label::new(RichText::new("View (F3)").strong()));
                        let mut opts = vec![(settings::VIEWER_BUILTIN, "Built-in text viewer")];
                        if cfg!(target_os = "macos") { opts.push((settings::VIEWER_QUICKLOOK, "Quick Look")); }
                        opts.extend(editors.iter().map(|n| (*n, *n)));
                        choice(ui, "viewer", &mut self.settings.viewer, &opts);
                    });
                    ui.horizontal(|ui| {
                        ui.add_sized(vec2(90.0, 20.0), egui::Label::new(RichText::new("Edit (F4)").strong()));
                        let mut opts = vec![(settings::EDITOR_AUTO, "Automatic (VS Code if installed)"), (settings::EDITOR_SYSTEM, "System text editor")];
                        opts.extend(editors.iter().map(|n| (*n, *n)));
                        choice(ui, "editor", &mut self.settings.editor, &opts);
                    });
                    ui.add_space(10.0);
                    ui.label(RichText::new("Startup").strong());
                    ui.checkbox(&mut self.settings.remember_folders, RichText::new("Reopen the last folders when the app starts").color(pal().text));
                    ui.add_space(10.0);
                    if let Some(f) = settings::file() {
                        ui.label(RichText::new(format!("Saved in {}", f.display())).size(11.0).color(pal().text_dim));
                    }
                    ui.add_space(8.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.add(egui::Button::new("Done").min_size(vec2(80.0, 26.0))).clicked() { keep = false; }
                    });
                });
                if resp.should_close() { keep = false; }
            }
            Dialog::Help => {
                let resp = Modal::new(Id::unique("help")).show(ctx, |ui| {
                    ui.set_width(460.0);
                    ui.label(RichText::new("Keyboard").size(16.0).strong());
                    ui.add_space(6.0);
                    egui::Grid::new("help").num_columns(2).spacing(vec2(18.0, 4.0)).show(ui, |ui| {
                        for (k, d) in HELP { ui.label(RichText::new(*k).strong()); ui.label(*d); ui.end_row(); }
                    });
                    ui.add_space(8.0);
                    ui.label(RichText::new("Drag files between panels or onto a drive in the sidebar. Right-click a file for more actions.").color(pal().text_dim));
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() { keep = false; }
                });
                if resp.should_close() { keep = false; }
            }
        }
        keep
    }
}

enum RowEvent {
    Click(bool, bool),
    Open,
    Context,
    Cmd(Cmd),
    DragStart(Vec<PathBuf>),
    DropOn(DragPayload, PathBuf),
}

impl eframe::App for App {
    /// Take Tab out of the input before egui sees it: egui would otherwise use it to
    /// cycle keyboard focus through buttons and the filter box instead of switching panels.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if self.term_has_keys() {
            // Everything typed goes to the shell, except the keys that work the app itself.
            let mut taken = Vec::new();
            raw_input.events.retain(|e| {
                let take = match e {
                    Event::Key { key, modifiers, .. } => !app_key_in_terminal(*key, *modifiers),
                    Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut | Event::Ime(_) => true,
                    _ => false,
                };
                if take { taken.push(e.clone()); }
                !take
            });
            self.term_input.extend(taken);
            return;
        }
        let before = raw_input.events.len();
        raw_input.events.retain(|e| !matches!(e, Event::Key { key: Key::Tab, pressed: true, .. }));
        let tabs = before - raw_input.events.len();
        raw_input.events.retain(|e| !matches!(e, Event::Key { key: Key::Tab, .. }));
        if tabs > 0 {
            self.tab_presses += tabs;
            if matches!(self.dialog, Dialog::None) {
                // Leave the filter box (or a focused button) so keys go to the list again.
                ctx.memory_mut(|m| if let Some(id) = m.focused() { m.surrender_focus(id) });
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.tick(&ctx);
        self.handle_keys(&ctx);

        egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(200.0)
            .size_range(150.0..=320.0)
            .frame(egui::Frame::new().fill(pal().sidebar_bg).inner_margin(Margin::symmetric(8, 6)))
            .show(ui, |ui| self.sidebar(ui));
        egui::Panel::bottom("bottom")
            .frame(egui::Frame::new().fill(pal().sidebar_bg).inner_margin(Margin::symmetric(10, 6)))
            .show(ui, |ui| self.bottom_bar(ui));
        // Answer the shells' queries and notice exits even while a terminal is hidden.
        for t in self.terms.iter_mut().flatten() { t.pane.drain_events(&ctx); }
        if !self.term_focus.is_some_and(|s| self.term_visible(s)) { self.term_focus = None; }
        let input = std::mem::take(&mut self.term_input);
        if let Some(s) = self.term_focus && let Some(t) = self.terms[s].as_mut() {
            t.pane.input(input, &ctx);
            // A click anywhere else gives the keyboard back to the panels (or the other terminal).
            let rect = t.rect;
            if ctx.input(|i| i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|p| !rect.contains(p))) {
                self.term_focus = None;
            }
        }
        egui::CentralPanel::no_frame().frame(egui::Frame::new().fill(pal().app_bg)).show(ui, |ui| {
            let full = ui.available_rect_before_wrap().shrink(8.0);
            let gap = 8.0;
            let half = (full.width() - gap) / 2.0;
            for idx in 0..2 {
                let r = Rect::from_min_size(full.min + vec2(idx as f32 * (half + gap), 0.0), vec2(half, full.height()));
                if !self.term_visible(idx) {
                    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).id_salt(("panel", idx)));
                    self.panel_ui(&mut child, idx);
                    continue;
                }
                // The terminal splits this panel's column: file list on top, terminal below,
                // with a draggable divider in the gap. Both keep at least 120 points.
                let min = 120.0;
                let term_h = (r.height() * self.term_frac[idx]).clamp(min, (r.height() - gap - min).max(min));
                let top = Rect::from_min_max(r.min, egui::pos2(r.max.x, r.max.y - term_h - gap));
                let bottom = Rect::from_min_max(egui::pos2(r.min.x, r.max.y - term_h), r.max);
                let divider = Rect::from_min_max(egui::pos2(r.min.x, top.max.y), egui::pos2(r.max.x, bottom.min.y));
                let drag = ui.interact(divider, Id::unique(("term_divider", idx)), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeVertical);
                if drag.dragged() { ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical); }
                if drag.dragged() && let Some(p) = drag.interact_pointer_pos() {
                    self.term_frac[idx] = ((r.max.y - p.y - gap / 2.0) / r.height()).clamp(0.12, 0.88);
                }
                if drag.hovered() || drag.dragged() {
                    ui.painter().hline(divider.x_range().shrink(divider.width() * 0.4), divider.center().y, Stroke::new(3.0, pal().border));
                }
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(top).id_salt(("panel", idx)));
                self.panel_ui(&mut child, idx);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(bottom).id_salt(("terminal", idx)));
                self.terminal_ui(&mut child, idx);
            }
        });

        // Show what is being dragged next to the pointer.
        if let Some(pl) = DragAndDrop::payload::<DragPayload>(&ctx)
            && let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                let text = if pl.paths.len() == 1 { file_name(&pl.paths[0]) } else { format!("{} items", pl.paths.len()) };
                let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::unique("drag")));
                let galley = painter.layout_no_wrap(text, pal().font(13.0), pal().accent_text);
                let r = Rect::from_min_size(pos + vec2(14.0, 10.0), galley.size() + vec2(16.0, 8.0));
                painter.rect_filled(r, 5.0, pal().accent);
                painter.galley(r.min + vec2(8.0, 4.0), galley, pal().accent_text);
            }

        self.dialogs(&ctx);
    }
}

/// Mounted drives, read by a background thread every 3 seconds (or at once when
/// `refresh` is signalled). Never on the UI thread: macOS answers free-space queries
/// through a system service, which takes 200 ms or more with network shares mounted.
struct DriveWatch {
    slot: Arc<Mutex<Option<Vec<Place>>>>,
    refresh: std::sync::mpsc::Sender<()>,
}

fn spawn_drive_watch(ctx: &egui::Context) -> DriveWatch {
    let slot = Arc::new(Mutex::new(None));
    let (refresh, wake) = std::sync::mpsc::channel::<()>();
    let (s, ctx) = (slot.clone(), ctx.clone());
    std::thread::spawn(move || loop {
        let drives = places::drives();
        *s.lock().unwrap() = Some(drives);
        ctx.request_repaint();
        match wake.recv_timeout(Duration::from_secs(3)) {
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            _ => while wake.try_recv().is_ok() {},
        }
    });
    DriveWatch { slot, refresh }
}

/// Finder sidebar favourites, read by a background thread: once at startup and again
/// whenever `refresh` is signalled (macOS only; elsewhere the built-in list stays).
struct FinderFavorites {
    slot: Arc<Mutex<Option<Vec<Place>>>>,
    /// The last read failed: no Full Disk Access (the sidebar then offers to set it up).
    no_access: Arc<std::sync::atomic::AtomicBool>,
    refresh: std::sync::mpsc::Sender<()>,
    requested: Instant,
    was_focused: bool,
}

fn spawn_finder_favorites(ctx: &egui::Context) -> FinderFavorites {
    let slot = Arc::new(Mutex::new(None));
    let no_access = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (refresh, wake) = std::sync::mpsc::channel::<()>();
    if cfg!(target_os = "macos") {
        let (slot, no_access, ctx) = (slot.clone(), no_access.clone(), ctx.clone());
        // Reading can take a moment, so it stays off the UI thread.
        std::thread::spawn(move || loop {
            let favs = places::finder_favorites();
            no_access.store(favs.is_none(), Ordering::Relaxed);
            if let Some(favs) = favs { *slot.lock().unwrap() = Some(favs); }
            ctx.request_repaint();
            if wake.recv().is_err() { break; }
            while wake.try_recv().is_ok() {}
        });
    }
    FinderFavorites { slot, no_access, refresh, requested: Instant::now(), was_focused: true }
}

/// Transfer speed over the last few seconds (smooth, but still reacts to changes).
#[derive(Default)]
struct SpeedMeter { samples: std::collections::VecDeque<(Instant, u64)> }

impl SpeedMeter {
    const WINDOW: Duration = Duration::from_secs(4);

    /// Record bytes done so far; returns bytes per second once there is enough data.
    fn sample(&mut self, done: u64) -> Option<f64> {
        let now = Instant::now();
        if self.samples.back().is_none_or(|(t, _)| now.duration_since(*t) > Duration::from_millis(100)) {
            self.samples.push_back((now, done));
        }
        while self.samples.len() > 2 && self.samples.front().is_some_and(|(t, _)| now.duration_since(*t) > Self::WINDOW) {
            self.samples.pop_front();
        }
        let (&(t0, b0), &(t1, b1)) = (self.samples.front()?, self.samples.back()?);
        let secs = t1.duration_since(t0).as_secs_f64();
        (secs >= 0.5 && b1 > b0).then(|| (b1 - b0) as f64 / secs)
    }
}

fn human_duration(secs: f64) -> String {
    let s = secs.round() as u64;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min {} s", s / 60, s % 60),
        _ => format!("{} h {} min", s / 3600, s % 3600 / 60),
    }
}

fn input(kind: InputKind, title: &str, prompt: String, buf: String, ok: &'static str, select_stem: bool) -> Dialog {
    Dialog::Input { kind, title: title.into(), prompt, buf, ok, first_frame: true, select_stem }
}

const NEW_FOLDER: &str = "untitled folder";
const NEW_FILE: &str = "untitled.txt";

/// F7 dialog: a new folder, or a new empty file (`file`).
fn new_item(file: bool) -> Dialog {
    let mut d = input(InputKind::New { file: false }, "New Folder", "Name of the new folder:".into(), NEW_FOLDER.into(), "Create", false);
    if file { set_new_kind(&mut d, true); }
    d
}

/// Switch the New dialog between folder and file; an untouched default name follows along.
fn set_new_kind(d: &mut Dialog, to_file: bool) {
    let Dialog::Input { kind: InputKind::New { file }, title, prompt, buf, first_frame, select_stem, .. } = d else { return };
    *file = to_file;
    let what = if to_file { "file" } else { "folder" };
    *title = format!("New {}", if to_file { "File" } else { "Folder" });
    *prompt = format!("Name of the new {what}:");
    if buf.as_str() == NEW_FOLDER || buf.as_str() == NEW_FILE { *buf = (if to_file { NEW_FILE } else { NEW_FOLDER }).into(); }
    *select_stem = to_file;
    // Give the name field the keyboard again (a radio click takes it) and reselect the name.
    *first_frame = true;
}

/// Settings combo box over (stored key, label) pairs; an unknown stored key is shown as is.
fn choice(ui: &mut Ui, id: &str, value: &mut String, opts: &[(&str, &str)]) {
    let shown = opts.iter().find(|(k, _)| *k == value.as_str()).map_or(value.as_str(), |(_, l)| l).to_string();
    egui::ComboBox::from_id_salt(id).width(260.0)
        .selected_text(RichText::new(shown).color(pal().text))
        .show_ui(ui, |ui| {
            for (k, l) in opts {
                ui.selectable_value(value, k.to_string(), *l);
            }
        });
}

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string())
}

/// Character range of the last path part's name without its extension
/// ("/a/b/photo.jpg" → "photo"; "notes" → "notes"; ".zshrc" → ".zshrc").
fn stem_range(text: &str) -> (usize, usize) {
    let start = text.rfind(['/', std::path::MAIN_SEPARATOR]).map_or(0, |i| text[..=i].chars().count());
    let name: String = text.chars().skip(start).collect();
    let stem = Path::new(&name).file_stem().map_or(name.chars().count(), |s| s.to_string_lossy().chars().count());
    (start, start + stem)
}

fn dir_string(p: &Path) -> String {
    let mut s = p.display().to_string();
    if !s.ends_with(std::path::MAIN_SEPARATOR) { s.push(std::path::MAIN_SEPARATOR); }
    s
}

/// Keys the app keeps while the built-in terminal has the keyboard.
fn app_key_in_terminal(k: Key, m: Modifiers) -> bool {
    k == Key::F1 || (m.mac_cmd && matches!(k, Key::T | Key::Q | Key::Comma))
}

/// The function-key bar: (key, label, command) for the plain layer, or with `alt` (⇧ or ⌘ held)
/// the second one. ⌘F5 never reaches the app (macOS: VoiceOver), so Duplicate is ⇧F5 from the keyboard.
fn fkeys(alt: bool, use_trash: bool) -> [(&'static str, &'static str, Option<Cmd>); 10] {
    if alt {
        [("F1", "Help", Some(Cmd::Help)), ("F2", "", None), ("F3", "Quick Look", Some(Cmd::QuickLook)),
         ("F4", "Text Editor", Some(Cmd::EditSystem)), ("F5", "Duplicate", Some(Cmd::Duplicate)), ("F6", "Rename", Some(Cmd::Rename)),
         ("F7", "New File", Some(Cmd::NewFile)), ("F8", "Delete", Some(Cmd::Delete)), ("F9", "", None), ("F10", "", None)]
    } else {
        [("F1", "Open in…", Some(Cmd::OpenIn)), ("F2", "Rename", Some(Cmd::Rename)), ("F3", "View", Some(Cmd::View)),
         ("F4", "Edit", Some(Cmd::Edit)), ("F5", "Copy", Some(Cmd::Copy)), ("F6", "Move", Some(Cmd::Move)),
         ("F7", "New Folder", Some(Cmd::NewFolder)), ("F8", if use_trash { "Trash" } else { "Delete" }, Some(Cmd::Trash)),
         ("F9", "Settings", Some(Cmd::Settings)), ("F10", "Quit", Some(Cmd::Quit))]
    }
}

fn fkey_of(name: &str) -> Option<Key> {
    Some(match name {
        "F1" => Key::F1, "F2" => Key::F2, "F3" => Key::F3, "F4" => Key::F4, "F5" => Key::F5,
        "F6" => Key::F6, "F7" => Key::F7, "F8" => Key::F8, "F9" => Key::F9, "F10" => Key::F10,
        _ => return None,
    })
}

fn is_fkey(k: Key) -> bool {
    matches!(k, Key::F1 | Key::F2 | Key::F3 | Key::F4 | Key::F5 | Key::F6 | Key::F7 | Key::F8 | Key::F9 | Key::F10)
}

/// Keys that work even while the filter box has focus.
fn is_global_key(k: Key) -> bool {
    is_fkey(k) || matches!(k, Key::Escape | Key::Enter | Key::ArrowDown | Key::ArrowUp)
}

/// Text preview for F3: the first 2 MB if it looks like text.
fn read_text(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut data = Vec::new();
    std::fs::File::open(path).ok()?.take(2 * 1024 * 1024).read_to_end(&mut data).ok()?;
    let sample = &data[..data.len().min(8192)];
    if sample.contains(&0) { return None; }
    let text = String::from_utf8_lossy(&data);
    if text.chars().take(8192).filter(|&c| c == '\u{FFFD}').count() > 8 { return None; }
    Some(text.replace('\t', "    "))
}

const HELP: &[(&str, &str)] = &[
    ("Tab / ⇧Tab", "Switch between left and right panel"),
    ("↑ ↓", "Move (hold Shift to select)"),
    ("PgUp / PgDn  (fn↑ / fn↓)", "To the first / last visible row, then page by page"),
    ("⌘PgUp / ⌘PgDn, Home / End", "Jump to top / bottom of the list"),
    ("⌥↑ / ⌥↓", "Move one screen up / down"),
    ("Enter / double-click", "Open folder or file"),
    ("Backspace  /  ⌘↑", "Enclosing folder"),
    ("⌘[  ⌘]", "Back / forward"),
    ("Space / Insert", "Select item and move down"),
    ("⌘-click  ⇧-click", "Add to selection / select range"),
    ("Type letters", "Jump to a file by name"),
    ("⌘F", "Filter the current folder"),
    ("⇧ or ⌘ + F-key", "Second layer: hold ⇧ or ⌘ and the bottom bar shows it"),
    ("F2 / ⇧F6", "Rename"),
    ("F3  /  ⇧F3", "View (built-in viewer, Quick Look or an editor, see Settings) / Quick Look"),
    ("F4  /  ⇧F4", "Edit (VS Code if installed, or the editor chosen in Settings) / system text editor"),
    ("F5  /  ⇧F5", "Copy to the other panel / duplicate in the same folder"),
    ("F6", "Move to the other panel"),
    ("F7  /  ⇧⌘N", "New folder (Tab in the dialog: new file instead)"),
    ("⇧F7 ⌘F7  /  ⌥⌘N", "New file"),
    ("F8  /  ⌘⌫", "Move to Trash (or delete, see Settings)"),
    ("⇧F8 / ⌘F8", "Delete permanently"),
    ("⇧⌘.", "Show / hide hidden files in this panel"),
    ("⌃U", "Swap panels"),
    ("⌘R", "Refresh"),
    ("F1", "Open the folder in… the terminal, VS Code or another editor, Finder (F1 twice: terminal)"),
    ("⌘T", "Terminal below the current panel; again hides it (or a separate window, see Settings)"),
    ("⇧F1 ⌘F1 / ⌘?", "This help"),
    ("F9 / ⌘,", "Settings (look, terminal, viewer and editor, Trash or delete, startup folders)"),
    ("F10 / ⌘Q", "Quit"),
];

#[cfg(test)]
mod tests {
    use super::stem_range;

    #[test]
    fn stem_range_selects_name_without_extension() {
        assert_eq!(stem_range("/Volumes/photo/Ürlaub.2024.jpg"), (15, 26));
        assert_eq!(stem_range("notes"), (0, 5));
        assert_eq!(stem_range("/x/.zshrc"), (3, 9));
        assert_eq!(stem_range("photo.jpg"), (0, 5));
    }
}
