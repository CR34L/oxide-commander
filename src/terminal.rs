//! Built-in terminal pane: a shell in a pseudo terminal, emulated by `alacritty_terminal`
//! and drawn with egui. Used when Settings → Terminal is set to "Built-in".

use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use alacritty_terminal::Term;
use eframe::egui::{self, pos2, vec2, Align2, Color32, Event, FontId, Key, Modifiers, Rect, Sense, Stroke, Ui};

const FONT_SIZE: f32 = 13.0;
const PAD: f32 = 6.0;

/// Receives events from the terminal thread; the UI thread drains them each frame.
#[derive(Clone)]
struct Listener {
    ctx: egui::Context,
    queue: Arc<Mutex<Vec<TermEvent>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        if !matches!(event, TermEvent::Wakeup | TermEvent::MouseCursorDirty | TermEvent::CursorBlinkingChange) {
            self.queue.lock().unwrap().push(event);
        }
        self.ctx.request_repaint();
    }
}

pub struct TermPane {
    term: Arc<FairMutex<Term<Listener>>>,
    tx: EventLoopSender,
    queue: Arc<Mutex<Vec<TermEvent>>>,
    /// Window title set by the shell (usually user@host:folder).
    pub title: String,
    pub exited: bool,
    grid: (usize, usize),
    cell: egui::Vec2,
    /// Wheel movement not yet turned into whole lines.
    scroll_rest: f32,
}

impl TermPane {
    /// Start the user's login shell in `dir`.
    pub fn spawn(dir: &Path, ctx: &egui::Context) -> io::Result<TermPane> {
        let (cols, lines) = (80, 24);
        let mut env = HashMap::from([
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
            ("TERM_PROGRAM".to_string(), "MacCommander".to_string()),
        ]);
        // Started from Finder there is no locale: without one zsh shows UTF-8 names as escapes.
        if ["LANG", "LC_ALL", "LC_CTYPE"].iter().all(|v| std::env::var_os(v).is_none()) {
            env.insert("LC_CTYPE".into(), "UTF-8".into());
        }
        let options = tty::Options { working_directory: Some(dir.to_path_buf()), drain_on_exit: true, env, ..Default::default() };
        let window = WindowSize { num_lines: lines as u16, num_cols: cols as u16, cell_width: 8, cell_height: 16 };
        let pty = tty::new(&options, window, 0)?;
        let queue = Arc::new(Mutex::new(Vec::new()));
        let listener = Listener { ctx: ctx.clone(), queue: queue.clone() };
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &TermSize::new(cols, lines), listener.clone())));
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let tx = event_loop.channel();
        event_loop.spawn();
        Ok(TermPane {
            term, tx, queue, title: String::new(), exited: false,
            grid: (cols, lines), cell: vec2(8.0, 16.0), scroll_rest: 0.0,
        })
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.tx.send(Msg::Input(bytes.into()));
    }

    /// Type `cd '<dir>'` and Enter into the shell.
    pub fn cd(&self, dir: &Path) {
        let quoted = dir.to_string_lossy().replace('\'', r"'\''");
        self.write(format!(" cd '{quoted}'\r").into_bytes());
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        let events = std::mem::take(&mut *self.queue.lock().unwrap());
        for e in events {
            match e {
                TermEvent::PtyWrite(text) => self.write(text.into_bytes()),
                TermEvent::Title(t) => self.title = t,
                TermEvent::ResetTitle => self.title.clear(),
                TermEvent::ClipboardStore(_, text) => ctx.copy_text(text),
                TermEvent::ColorRequest(index, format) => {
                    let c = self.color(Color::Indexed(index.min(255) as u8), &{ *self.term.lock().colors() });
                    let c = if index == NamedColor::Background as usize { palette().bg }
                        else if index >= 256 { palette().fg } else { c };
                    self.write(format(Rgb { r: c.r(), g: c.g(), b: c.b() }).into_bytes());
                }
                TermEvent::TextAreaSizeRequest(format) => {
                    let (cols, lines) = self.grid;
                    let size = WindowSize { num_lines: lines as u16, num_cols: cols as u16, cell_width: self.cell.x as u16, cell_height: self.cell.y as u16 };
                    self.write(format(size).into_bytes());
                }
                TermEvent::Exit | TermEvent::ChildExit(_) => self.exited = true,
                _ => {}
            }
        }
    }

    /// Feed keyboard / clipboard events while the pane has keyboard focus.
    pub fn input(&mut self, events: Vec<Event>, ctx: &egui::Context) {
        let mode = *self.term.lock().mode();
        for e in events {
            let bytes: Option<Vec<u8>> = match e {
                Event::Text(t) => Some(t.into_bytes()),
                Event::Ime(egui::ImeEvent::Commit(t)) => Some(t.into_bytes()),
                Event::Paste(t) => Some(paste_bytes(&t, mode.contains(TermMode::BRACKETED_PASTE))),
                Event::Copy | Event::Cut => {
                    if let Some(text) = self.term.lock().selection_to_string() { ctx.copy_text(text); }
                    None
                }
                Event::Key { key, pressed: true, modifiers, .. } => key_bytes(key, modifiers, mode.contains(TermMode::APP_CURSOR)),
                _ => None,
            };
            if let Some(b) = bytes.filter(|b| !b.is_empty()) {
                let mut term = self.term.lock();
                term.selection = None;
                term.scroll_display(Scroll::Bottom);
                drop(term);
                self.write(b);
            }
        }
    }

    /// Draw the terminal into the rest of `ui` and handle the mouse. Returns the response
    /// of the whole terminal area (clicked = wants keyboard focus).
    pub fn show(&mut self, ui: &mut Ui, focused: bool) -> egui::Response {
        let ctx = ui.ctx().clone();
        self.drain_events(&ctx);
        let font = FontId::monospace(FONT_SIZE);
        let (cw, ch) = ctx.fonts_mut(|f| (f.glyph_width(&font, 'M'), f.row_height(&font)));
        self.cell = vec2(cw, ch);
        let rect = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let inner = rect.shrink(PAD);
        let cols = ((inner.width() / cw).floor() as usize).max(2);
        let lines = ((inner.height() / ch).floor() as usize).max(1);
        if (cols, lines) != self.grid {
            self.grid = (cols, lines);
            self.term.lock().resize(TermSize::new(cols, lines));
            let _ = self.tx.send(Msg::Resize(WindowSize { num_lines: lines as u16, num_cols: cols as u16, cell_width: cw as u16, cell_height: ch as u16 }));
        }
        self.mouse(ui, &resp, inner.min);

        let pal = palette();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, pal.bg);
        let term = self.term.lock();
        let content = term.renderable_content();
        let colors = *content.colors;
        let offset = content.display_offset as i32;
        let selection = content.selection;
        let cursor = content.cursor;
        let cell_rect = |line: i32, col: usize, width: usize| {
            Rect::from_min_size(inner.min + vec2(col as f32 * cw, (line + offset) as f32 * ch), vec2(cw * width as f32, ch))
        };
        // Characters of one row with the same colour are drawn as one run; anything
        // outside ASCII is drawn on its own so a wider fallback glyph cannot shift the row.
        let mut run: Option<(Point, Color32, String)> = None;
        let flush = |run: &mut Option<(Point, Color32, String)>| {
            if let Some((p, fg, text)) = run.take() {
                painter.text(cell_rect(p.line.0, p.column.0, 1).min, Align2::LEFT_TOP, text, font.clone(), fg);
            }
        };
        for indexed in content.display_iter {
            let (p, cell) = (indexed.point, indexed.cell);
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) { continue; }
            let mut fg = self.color(cell.fg, &colors);
            let mut bg = self.color(cell.bg, &colors);
            if cell.flags.contains(Flags::DIM) { fg = fg.gamma_multiply(0.66); }
            if cell.flags.contains(Flags::INVERSE) { std::mem::swap(&mut fg, &mut bg); }
            if selection.is_some_and(|s| s.contains(p)) { bg = pal.selection; fg = pal.fg; }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            let r = cell_rect(p.line.0, p.column.0, width);
            if bg != pal.bg { painter.rect_filled(r, 0.0, bg); }
            if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                painter.hline(r.x_range(), r.bottom() - 1.5, Stroke::new(1.0, fg));
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                painter.hline(r.x_range(), r.center().y, Stroke::new(1.0, fg));
            }
            let c = cell.c;
            let visible = c != ' ' && c != '\0' && !cell.flags.contains(Flags::HIDDEN);
            let joins = run.as_ref().is_some_and(|(rp, rfg, text)| {
                rp.line == p.line && *rfg == fg && rp.column.0 + text.chars().count() == p.column.0
            });
            if !visible || !c.is_ascii() || !joins { flush(&mut run); }
            if !visible { continue; }
            if c.is_ascii() {
                match &mut run { Some((_, _, text)) => text.push(c), None => run = Some((p, fg, c.to_string())) }
            } else {
                let mut text = c.to_string();
                if let Some(extra) = cell.zerowidth() { text.extend(extra); }
                painter.text(r.center(), Align2::CENTER_CENTER, text, font.clone(), fg);
            }
        }
        flush(&mut run);

        // Cursor: filled when focused, outlined otherwise.
        if cursor.shape != CursorShape::Hidden && (cursor.point.line.0 + offset) < lines as i32 {
            let wide = term.grid()[cursor.point].flags.contains(Flags::WIDE_CHAR);
            let r = cell_rect(cursor.point.line.0, cursor.point.column.0, if wide { 2 } else { 1 });
            let color = colors[NamedColor::Cursor].map(c32).unwrap_or(pal.cursor);
            match cursor.shape {
                CursorShape::Beam => { painter.rect_filled(Rect::from_min_size(r.min, vec2(2.0, ch)), 0.0, color); }
                CursorShape::Underline => { painter.rect_filled(Rect::from_min_max(pos2(r.left(), r.bottom() - 2.0), r.max), 0.0, color); }
                _ if focused => {
                    painter.rect_filled(r, 0.0, color);
                    let c = term.grid()[cursor.point].c;
                    if c != ' ' && c != '\0' { painter.text(r.center(), Align2::CENTER_CENTER, c.to_string(), font.clone(), pal.bg); }
                }
                _ => { painter.rect_stroke(r.shrink(0.5), 0.0, Stroke::new(1.0, color), egui::StrokeKind::Inside); }
            }
        }
        drop(term);

        if self.exited {
            let msg = "The shell has ended. Press F1 to start a new one.";
            let r = Rect::from_center_size(rect.center(), vec2(360.0, 30.0));
            painter.rect_filled(r, 6.0, pal.selection);
            painter.text(r.center(), Align2::CENTER_CENTER, msg, FontId::proportional(13.0), pal.fg);
        }
        resp
    }

    fn mouse(&mut self, ui: &Ui, resp: &egui::Response, origin: egui::Pos2) {
        let (cw, ch) = (self.cell.x, self.cell.y);
        let (cols, lines) = self.grid;
        let mut term = self.term.lock();
        let offset = term.grid().display_offset() as i32;
        let to_point = |pos: egui::Pos2| {
            let x = ((pos.x - origin.x) / cw).max(0.0);
            let col = (x as usize).min(cols - 1);
            let row = (((pos.y - origin.y) / ch).max(0.0) as i32).min(lines as i32 - 1);
            let side = if x.fract() < 0.5 { Side::Left } else { Side::Right };
            (Point::new(Line(row - offset), Column(col)), side)
        };
        if resp.hovered() {
            let dy = ui.input(|i| i.smooth_scroll_delta.y);
            self.scroll_rest += dy / ch;
            let n = self.scroll_rest.trunc() as i32;
            if n != 0 {
                self.scroll_rest -= n as f32;
                let mode = *term.mode();
                if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
                    // Full-screen programs (less, vim): the wheel sends arrow keys.
                    let arrow: &[u8] = match (n > 0, mode.contains(TermMode::APP_CURSOR)) {
                        (true, true) => b"\x1bOA", (true, false) => b"\x1b[A", (false, true) => b"\x1bOB", (false, false) => b"\x1b[B",
                    };
                    self.write(arrow.repeat(n.unsigned_abs() as usize));
                } else {
                    term.scroll_display(Scroll::Delta(n));
                }
            }
        }
        if let Some(pos) = resp.interact_pointer_pos() {
            let (p, side) = to_point(pos);
            if resp.double_clicked() {
                term.selection = Some(Selection::new(SelectionType::Semantic, p, side));
            } else if resp.drag_started() {
                term.selection = Some(Selection::new(SelectionType::Simple, p, side));
            } else if resp.dragged() {
                if let Some(s) = term.selection.as_mut() { s.update(p, side); }
            } else if resp.clicked() {
                term.selection = None;
            }
        }
    }

    fn color(&self, c: Color, colors: &Colors) -> Color32 {
        let pal = palette();
        match c {
            Color::Spec(rgb) => c32(rgb),
            Color::Indexed(i) => colors[i as usize].map(c32).unwrap_or_else(|| indexed_color(i, pal)),
            Color::Named(n) => colors[n].map(c32).unwrap_or_else(|| named_color(n, pal)),
        }
    }
}

impl Drop for TermPane {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
    }
}

/// Bytes a key press sends to the shell (xterm conventions). Plain characters arrive
/// as text events instead, so letters only produce bytes together with Control.
pub fn key_bytes(key: Key, m: Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    let csi_mod = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let s = |t: &str| Some(t.as_bytes().to_vec());
    if m.mac_cmd {
        // macOS Terminal habits: ⌘← / ⌘→ start / end of line, ⌘⌫ deletes the line.
        return match key { Key::ArrowLeft => s("\x01"), Key::ArrowRight => s("\x05"), Key::Backspace => s("\x15"), _ => None };
    }
    let cursor = |c: char| {
        if csi_mod > 1 { Some(format!("\x1b[1;{csi_mod}{c}").into_bytes()) }
        else if app_cursor { Some(format!("\x1bO{c}").into_bytes()) }
        else { Some(format!("\x1b[{c}").into_bytes()) }
    };
    let tilde = |n: u8| {
        if csi_mod > 1 { Some(format!("\x1b[{n};{csi_mod}~").into_bytes()) } else { Some(format!("\x1b[{n}~").into_bytes()) }
    };
    match key {
        Key::Enter => s("\r"),
        Key::Tab if m.shift => s("\x1b[Z"),
        Key::Tab => s("\t"),
        Key::Backspace if m.alt => s("\x1b\x7f"),
        Key::Backspace => s("\x7f"),
        Key::Escape => s("\x1b"),
        // ⌥← / ⌥→ move by word, as in macOS Terminal.
        Key::ArrowLeft if m.alt && !m.ctrl && !m.shift => s("\x1bb"),
        Key::ArrowRight if m.alt && !m.ctrl && !m.shift => s("\x1bf"),
        Key::ArrowUp => cursor('A'),
        Key::ArrowDown => cursor('B'),
        Key::ArrowRight => cursor('C'),
        Key::ArrowLeft => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => s("\x1bOP"),
        Key::F2 => s("\x1bOQ"),
        Key::F3 => s("\x1bOR"),
        Key::F4 => s("\x1bOS"),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ if m.ctrl => {
            let name = key.name();
            let c = if name.len() == 1 { name.chars().next()? } else {
                match key { Key::Space => ' ', Key::OpenBracket => '[', Key::Backslash => '\\', Key::CloseBracket => ']', Key::Minus => '_', _ => return None }
            };
            match c {
                'A'..='Z' => Some(vec![c as u8 - b'A' + 1]),
                'a'..='z' => Some(vec![c as u8 - b'a' + 1]),
                ' ' | '2' => Some(vec![0]),
                '[' => Some(vec![0x1b]),
                '\\' => Some(vec![0x1c]),
                ']' => Some(vec![0x1d]),
                '_' => Some(vec![0x1f]),
                _ => None,
            }
        }
        _ => None,
    }
}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        // A pasted end marker would let the text escape the bracket: drop it.
        format!("\x1b[200~{}\x1b[201~", text.replace("\x1b[201~", "")).into_bytes()
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

// ---- colours -------------------------------------------------------------

struct TermPalette { bg: Color32, fg: Color32, cursor: Color32, selection: Color32, ansi: [Color32; 16] }

/// Colours follow the app theme: its list background and text, with ANSI colours
/// that stay readable on light or dark backgrounds.
fn palette() -> TermPalette {
    let p = crate::theme::pal();
    let bg = p.list_bg;
    let dark = (bg.r() as u32 * 3 + bg.g() as u32 * 6 + bg.b() as u32) / 10 < 128;
    let rgb = Color32::from_rgb;
    let ansi = if dark {
        [rgb(0, 0, 0), rgb(230, 80, 80), rgb(110, 200, 90), rgb(225, 195, 80), rgb(90, 140, 240), rgb(200, 110, 210), rgb(80, 200, 210), rgb(205, 205, 205),
         rgb(110, 110, 110), rgb(255, 120, 120), rgb(150, 235, 120), rgb(250, 225, 120), rgb(140, 180, 255), rgb(235, 150, 240), rgb(130, 230, 240), rgb(255, 255, 255)]
    } else {
        [rgb(0, 0, 0), rgb(180, 30, 30), rgb(30, 130, 30), rgb(150, 110, 0), rgb(30, 70, 200), rgb(160, 40, 160), rgb(20, 130, 150), rgb(170, 170, 170),
         rgb(100, 100, 100), rgb(220, 50, 50), rgb(40, 160, 40), rgb(180, 140, 0), rgb(60, 100, 230), rgb(190, 60, 190), rgb(30, 160, 180), rgb(250, 250, 250)]
    };
    TermPalette { bg, fg: p.text, cursor: p.text, selection: p.accent_soft, ansi }
}

fn c32(c: Rgb) -> Color32 { Color32::from_rgb(c.r, c.g, c.b) }

fn indexed_color(i: u8, pal: TermPalette) -> Color32 {
    match i {
        0..=15 => pal.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        _ => Color32::from_gray(8 + (i - 232) * 10),
    }
}

fn named_color(n: NamedColor, pal: TermPalette) -> Color32 {
    let i = n as usize;
    match n {
        NamedColor::Foreground | NamedColor::BrightForeground => pal.fg,
        NamedColor::Background => pal.bg,
        NamedColor::Cursor => pal.cursor,
        NamedColor::DimForeground => pal.fg.gamma_multiply(0.66),
        _ if i < 16 => pal.ansi[i],
        // DimBlack … DimWhite follow Cursor.
        _ => pal.ansi[(i - NamedColor::DimBlack as usize).min(7)].gamma_multiply(0.66),
    }
}

/// Folder the pane was started in, shown until the shell sets a title.
pub fn short_dir(dir: &Path) -> String {
    let home = crate::ops::home_dir().unwrap_or_default();
    match dir.strip_prefix(&home) {
        Ok(rest) if !home.as_os_str().is_empty() => format!("~/{}", rest.display()).trim_end_matches('/').to_string(),
        _ => dir.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_xterm_sequences() {
        let none = Modifiers::NONE;
        let ctrl = Modifiers { ctrl: true, ..none };
        assert_eq!(key_bytes(Key::Enter, none, false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(Key::C, ctrl, false), Some(vec![3]));
        assert_eq!(key_bytes(Key::ArrowUp, none, false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(Key::ArrowUp, none, true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(Key::ArrowLeft, Modifiers { shift: true, ..none }, false), Some(b"\x1b[1;2D".to_vec()));
        assert_eq!(key_bytes(Key::F5, none, false), Some(b"\x1b[15~".to_vec()));
        // Letters without Control come as text, and ⌥ letters are composed characters (e.g. ⌥L = @).
        assert_eq!(key_bytes(Key::L, Modifiers { alt: true, ..none }, false), None);
        assert_eq!(key_bytes(Key::A, none, false), None);
    }

    #[test]
    fn paste_is_bracketed_when_asked() {
        assert_eq!(paste_bytes("a\nb", false), b"a\rb".to_vec());
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~".to_vec());
    }
}
