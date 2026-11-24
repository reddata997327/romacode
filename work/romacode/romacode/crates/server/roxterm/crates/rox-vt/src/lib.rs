//! rox-vt — the grid database.
//!
//! The screen is a grid, not text: every cell carries char + colors +
//! attribute flags, every row carries the `seq` of the output chunk that last
//! touched it, and the OSC 133 command timeline records prompt→command→exit
//! provenance for every command the shell ran. This is what makes the
//! terminal queryable (`read --diff`, `wait text`, `expect exit-code`).

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use rox_graphics::{DeleteScope, GraphicsStore, KittyEvent};
use rox_protocol::flags;
use rox_protocol::{GridLine, ImageMeta, ImagePlacement, SessionMode};
use unicode_width::UnicodeWidthChar;
use vte::{Params, Parser, Perform};

use base64::Engine as _;

/// Tap state for kitty APC detection (vte ignores SosPmApcString payloads,
/// so we capture them ourselves while the same bytes still flow to vte).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tap {
    Ground,
    Esc,
    Csi,
    Osc,
    OscEsc,
    Dcs,
    DcsPassthrough,
    DcsEsc,
    String { apc: bool },
    StringEsc { apc: bool },
}

pub const DEFAULT_SCROLLBACK: usize = 5000;
/// Upper bound for a buffered Kitty APC image/control payload in the embedded
/// terminal; oversized strings are discarded until their terminator.
const MAX_APC_BYTES: usize = 4 * 1024 * 1024;
/// Bounded ring of OSC 52 clipboard payloads (1.0.0 fix: the previous
/// `Option<String>` last-write-wins model meant an agent had no way to know
/// what the app copied before the most recent copy — a real footgun when an
/// app publishes multiple selections during a session). The ring keeps the
/// last 16 payloads; `clipboard()` returns the back (most recent) so existing
/// callers behave exactly as before. History is queryable via
/// `clipboard_history()` for the timeline / audit paths.
const CLIPBOARD_RING_CAP: usize = 16;
/// Raised from 1000 (1.0.0 fix): the previous cap rotated oldest marks out
/// silently — an agent running a long dev session would lose provenance for
/// commands older than the last 1000. The journal keeps the full byte stream,
/// but the live `timeline` query used to have a hole. 10k still bounds
/// memory (~600 KB worst case at ~60 bytes per mark) and covers days of
/// continuous agent work.
const TIMELINE_CAP: usize = 10_000;

/// Internal cell flag: second half of a double-width char.
const WIDE_SPACER: u8 = 1 << 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy)]
pub struct Cell {
    pub ch: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: u8,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// seq of the output chunk that last touched this row.
    pub touch: u64,
    /// True when this row ended in a soft wrap (auto-wrap at the right
    /// margin): the next row continues the same logical line. Drives the
    /// 0.7.0 reflow on resize — hard breaks (\r\n) clear it.
    pub wrapped: bool,
}

/// One completed OSC 133 command (prompt A → command B → output C → exit D).
#[derive(Debug, Clone)]
pub struct CommandMark {
    pub seq_prompt: u64,
    pub seq_end: u64,
    pub command: Option<String>,
    pub exit: Option<i32>,
    pub started_unix_ms: u64,
    pub ended_unix_ms: u64,
    pub row_start: u32,
    /// 0.8.0: optional sentinel tag (`OSC 133;D;<exit>;<tag>`). The daemon
    /// tags every `run` sentinel so exit waits are content-addressed instead
    /// of positional — a stale prompt-time D from shell startup in flight
    /// when the run began can no longer be mistaken for the run's exit.
    pub tag: Option<String>,
}

#[derive(Debug, Clone)]
struct Mark {
    seq_prompt: u64,
    /// Absolute scrollback-space row where the command line starts.
    abs_row_b: i64,
    started_unix_ms: u64,
    command: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LineOpts {
    /// Include scrollback rows before the live grid.
    pub include_scrollback: bool,
    /// Keep only the last N lines.
    pub tail: Option<u32>,
    /// Attach image placements to their lines.
    pub images: bool,
}

pub struct Screen {
    cols: usize,
    rows: usize,
    grid: Vec<Row>,
    alt_grid: Vec<Row>,
    in_alt: bool,
    scrollback: VecDeque<Row>,
    max_scrollback: usize,
    cursor: (usize, usize), // (col, row)
    saved: Option<((usize, usize), Color, Color, u8)>,
    pub seq: u64,
    pub title: String,
    pub cwd: String,
    pub last_exit: Option<i32>,
    pub last_prompt_seq: u64,
    pub last_output_unix_ms: u64,
    pub mode_bits: u32,
    cur_fg: Color,
    cur_bg: Color,
    cur_flags: u8,
    pending_wrap: bool,
    insert_mode: bool,
    auto_wrap: bool,
    origin_mode: bool,
    scroll_region: (usize, usize),
    tabs: Vec<bool>,
    parser: Parser,
    pub graphics: GraphicsStore,
    /// Kitty image placements, ABSOLUTE-row anchored (0.7.0): `row` is the
    /// scrollback-space line the placement starts on (scrollback row `i` is
    /// abs `i`, live row `y` is abs `scrollback_len + y`). Anchoring in this
    /// space means scrolling content into scrollback never moves or kills a
    /// placement — images survive in history exactly like text does. Only
    /// scrollback eviction (cap reached) renormalizes the coordinates.
    placements: Vec<ImagePlacement>,
    /// Alternate-screen placements share the same abs space but live in
    /// their own list so entering/leaving vim/less never wipes the primary
    /// screen's images (0.7.0 fix — exit_alt used to clear them all).
    alt_placements: Vec<ImagePlacement>,
    timeline: VecDeque<CommandMark>,
    mark: Option<Mark>,
    tap: Tap,
    apc_buf: Vec<u8>,
    /// 0.8.0: last OSC 52 clipboard payload the app published into the grid
    /// (`ESC ] 52 ; c ; <base64> ST`), decoded to text. 1.0.0 fix: now a
    /// bounded ring of the last [`CLIPBOARD_RING_CAP`] payloads instead of
    /// last-write-only — `clipboard()` still returns the most recent (the
    /// previous API contract), `clipboard_history()` exposes the full ring
    /// oldest→newest for audit / agent introspection. A queryable cell of the
    /// grid database — agents read what the running app copied without owning
    /// a display server.
    pub clipboard: VecDeque<String>,
    pub cell_px: (u32, u32),
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Self {
        let mut s = Screen {
            cols,
            rows,
            grid: empty_grid(rows, cols, 0),
            alt_grid: Vec::new(),
            in_alt: false,
            scrollback: VecDeque::new(),
            max_scrollback: DEFAULT_SCROLLBACK,
            cursor: (0, 0),
            saved: None,
            seq: 0,
            title: String::new(),
            cwd: String::new(),
            last_exit: None,
            last_prompt_seq: 0,
            last_output_unix_ms: 0,
            mode_bits: 1, // cursor visible
            cur_fg: Color::Default,
            cur_bg: Color::Default,
            cur_flags: 0,
            pending_wrap: false,
            insert_mode: false,
            auto_wrap: true,
            origin_mode: false,
            scroll_region: (0, rows.saturating_sub(1)),
            tabs: (0..cols).map(|i| i % 8 == 0).collect(),
            parser: Parser::new(),
            graphics: GraphicsStore::new(),
            placements: Vec::new(),
            alt_placements: Vec::new(),
            timeline: VecDeque::new(),
            mark: None,
            tap: Tap::Ground,
            apc_buf: Vec::new(),
            clipboard: VecDeque::new(),
            cell_px: (8, 16),
        };
        s.graphics.cell_px = s.cell_px;
        s
    }

    // ------------------------------------------------------------- accessors

    pub fn cols(&self) -> usize {
        self.cols
    }
    pub fn rows(&self) -> usize {
        self.rows
    }
    pub fn seq(&self) -> u64 {
        self.seq
    }
    pub fn cursor(&self) -> (u32, u32) {
        (self.cursor.0 as u32, self.cursor.1 as u32)
    }
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }
    pub fn session_mode(&self) -> SessionMode {
        if self.in_alt {
            SessionMode::Alternate
        } else {
            SessionMode::Normal
        }
    }
    pub fn timeline(&self) -> impl Iterator<Item = &CommandMark> {
        self.timeline.iter()
    }
    pub fn placements(&self) -> &[ImagePlacement] {
        if self.in_alt {
            &self.alt_placements
        } else {
            &self.placements
        }
    }

    fn placements_mut(&mut self) -> &mut Vec<ImagePlacement> {
        if self.in_alt {
            &mut self.alt_placements
        } else {
            &mut self.placements
        }
    }
    pub fn bracketed_paste(&self) -> bool {
        self.mode_bits & 0b100 != 0
    }
    /// 0.8.0: the last OSC 52 clipboard text the app published (None until
    /// an app copies something through the grid). 1.0.0: returns the back
    /// of the ring (most recent payload) — preserves the previous API
    /// contract, so callers like the `Clipboard` RPC reply shape and the
    /// existing tests work unchanged.
    pub fn clipboard(&self) -> Option<&str> {
        self.clipboard.back().map(String::as_str)
    }
    /// 1.0.0: the full OSC 52 clipboard ring, oldest → newest. Use this for
    /// audit / introspection ("what did the app copy across the session?").
    /// The most recent payload is also reachable via [`clipboard`](Self::clipboard)
    /// for back-compat with 0.8.x callers.
    pub fn clipboard_history(&self) -> impl Iterator<Item = &str> {
        self.clipboard.iter().map(String::as_str)
    }
    /// 1.0.0: number of OSC 52 payloads currently held in the ring.
    pub fn clipboard_history_len(&self) -> usize {
        self.clipboard.len()
    }
    pub fn images(&self) -> Vec<ImageMeta> {
        self.graphics
            .iter()
            .map(|im| ImageMeta {
                image_id: im.id,
                pixel_w: im.w,
                pixel_h: im.h,
                size_bytes: im.rgba.len() as u32,
            })
            .collect()
    }

    fn active_grid(&self) -> &Vec<Row> {
        if self.in_alt {
            &self.alt_grid
        } else {
            &self.grid
        }
    }
    fn active_grid_mut(&mut self) -> &mut Vec<Row> {
        if self.in_alt {
            &mut self.alt_grid
        } else {
            &mut self.grid
        }
    }

    // ------------------------------------------------------------------ feed

    /// Feed raw PTY output. Every call bumps `seq`; all cells written by this
    /// chunk are tagged with that seq (this drives `read --diff`).
    ///
    /// The stream is tapped for kitty APC sequences (`ESC _ G ... ST`) — vte
    /// discards those payloads, we route them to the graphics store while the
    /// identical bytes still drive the vte state machine, so placement cursors
    /// stay exact.
    pub fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.seq += 1;
        self.last_output_unix_ms = now_ms();
        let mut parser = std::mem::replace(&mut self.parser, Parser::new());
        for &b in bytes {
            if self.tap_byte(b) {
                let payload = std::mem::take(&mut self.apc_buf);
                self.handle_apc(&payload);
            }
            parser.advance(self, b);
        }
        self.parser = parser;
    }

    /// Advance the APC tap one byte; returns true when an APC payload closed.
    fn tap_byte(&mut self, b: u8) -> bool {
        match self.tap {
            Tap::Ground => {
                match b {
                    0x1b => self.tap = Tap::Esc,
                    0x98 | 0x9e | 0x9f => self.tap = Tap::String { apc: false },
                    _ => {}
                }
                false
            }
            Tap::Esc => {
                self.tap = match b {
                    0x1b => Tap::Esc,
                    b'[' | 0x9b => Tap::Csi,
                    b']' => Tap::Osc,
                    b'P' | 0x90 => Tap::Dcs,
                    b'X' | b'^' => Tap::String { apc: false },
                    b'_' => {
                        self.apc_buf.clear();
                        Tap::String { apc: true }
                    }
                    _ => Tap::Ground,
                };
                false
            }
            Tap::Csi => {
                match b {
                    0x20..=0x3f => {}
                    0x40..=0x7e => self.tap = Tap::Ground,
                    0x1b => self.tap = Tap::Esc,
                    _ => self.tap = Tap::Ground, // C1 / garbage — bail safely
                }
                false
            }
            Tap::Osc => {
                match b {
                    0x07 | 0x9c => self.tap = Tap::Ground,
                    0x1b => self.tap = Tap::OscEsc,
                    _ => {}
                }
                false
            }
            Tap::OscEsc => {
                self.tap = if b == b'\\' { Tap::Ground } else { Tap::Osc };
                false
            }
            Tap::Dcs => {
                match b {
                    0x40..=0x7e => self.tap = Tap::DcsPassthrough,
                    0x1b => self.tap = Tap::DcsEsc,
                    0x9c => self.tap = Tap::Ground,
                    _ => {}
                }
                false
            }
            Tap::DcsPassthrough => {
                match b {
                    0x1b => self.tap = Tap::DcsEsc,
                    0x9c => self.tap = Tap::Ground,
                    _ => {}
                }
                false
            }
            Tap::DcsEsc => {
                self.tap = if b == b'\\' {
                    Tap::Ground
                } else {
                    Tap::DcsPassthrough
                };
                false
            }
            Tap::String { apc } => match b {
                0x07 | 0x9c => {
                    self.tap = Tap::Ground;
                    if apc {
                        true
                    } else {
                        self.apc_buf.clear();
                        false
                    }
                }
                0x1b => {
                    self.tap = Tap::StringEsc { apc };
                    false
                }
                _ => {
                    if apc {
                        if self.apc_buf.len() < MAX_APC_BYTES {
                            self.apc_buf.push(b);
                        } else {
                            self.apc_buf.clear();
                            self.tap = Tap::String { apc: false };
                        }
                    }
                    false
                }
            },
            Tap::StringEsc { apc } => {
                if b == b'\\' {
                    self.tap = Tap::Ground;
                    if apc {
                        true
                    } else {
                        self.apc_buf.clear();
                        false
                    }
                } else {
                    self.tap = Tap::String { apc };
                    if apc {
                        self.apc_buf.push(0x1b);
                        self.apc_buf.push(b);
                    }
                    false
                }
            }
        }
    }

    fn abs_row(&self, y: usize) -> i64 {
        self.scrollback.len() as i64 + y as i64
    }

    fn touch_all(&mut self) {
        let seq = self.seq;
        for row in self.active_grid_mut().iter_mut() {
            row.touch = seq;
        }
    }

    // ------------------------------------------------------------- mutation

    fn put_char(&mut self, c: char) {
        let width = UnicodeWidthChar::width(c).unwrap_or(0);
        if width == 0 {
            return; // combining marks are dropped for grid simplicity
        }
        if self.pending_wrap && self.auto_wrap {
            // Soft wrap: mark the row we are leaving so reflow knows the
            // next row is a continuation of this logical line.
            let y = self.cursor.1;
            if let Some(row) = self.active_grid_mut().get_mut(y) {
                row.wrapped = true;
            }
            self.cursor.0 = 0;
            self.linefeed_internal();
        }
        self.pending_wrap = false;

        if self.insert_mode {
            let (x, y) = self.cursor;
            let grid = self.active_grid_mut();
            if let Some(row) = grid.get_mut(y) {
                for _ in 0..width {
                    if row.cells.len() > 1 {
                        row.cells.pop();
                        row.cells.insert(x, Cell::default());
                    }
                }
            }
        }

        let (fg, bg, fl) = (self.cur_fg, self.cur_bg, self.cur_flags);
        let (x, y) = self.cursor;
        if let Some(row) = self.active_grid_mut().get_mut(y) {
            if let Some(cell) = row.cells.get_mut(x) {
                *cell = Cell {
                    ch: c,
                    fg,
                    bg,
                    flags: fl,
                };
            }
            if width == 2 && x + 1 < row.cells.len() {
                row.cells[x + 1] = Cell {
                    ch: ' ',
                    fg,
                    bg,
                    flags: WIDE_SPACER,
                };
            }
        }
        // Advance.
        if x + width as usize >= self.cols {
            self.cursor.0 = self.cols.saturating_sub(1);
            if self.auto_wrap {
                self.pending_wrap = true;
            }
        } else {
            self.cursor.0 = x + width as usize;
        }
        self.touch_row(y);
    }

    fn touch_row(&mut self, y: usize) {
        let seq = self.seq;
        if let Some(row) = self.active_grid_mut().get_mut(y) {
            row.touch = seq;
        }
    }

    fn linefeed_internal(&mut self) {
        let bottom = self.scroll_region.1.min(self.rows - 1);
        if self.cursor.1 == bottom {
            self.scroll_up(1);
        } else if self.cursor.1 + 1 < self.rows {
            self.cursor.1 += 1;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        let (top, bottom) = (
            self.scroll_region.0,
            self.scroll_region.1.min(self.rows - 1),
        );
        if top > bottom || n == 0 {
            return;
        }
        let n = n.min(bottom - top + 1);
        let seq = self.seq;
        // Rows leaving the region top go to scrollback when the region starts
        // at screen top and we are on the primary screen. With absolute-row
        // placement anchors, pushing to scrollback moves NOTHING: the pushed
        // content keeps its abs row, so placements ride along untouched.
        if top == 0 && !self.in_alt {
            for y in 0..n {
                let row = self.grid[y].clone();
                self.scrollback.push_back(row);
            }
            self.evict_scrollback();
        } else {
            // No scrollback push (alt screen or a narrowed region): content
            // moves up within the region, so placements anchored inside it
            // move up too; those that scroll off the region top are killed.
            let (t, b) = (top as u32, bottom as u32);
            let k = n as u32;
            self.placements_mut().retain_mut(|p| {
                if p.row < t || p.row > b {
                    return true;
                }
                if p.row - t < k {
                    return false;
                }
                p.row -= k;
                true
            });
        }
        for y in top..=(bottom - n) {
            let src = self.active_grid()[y + n].clone();
            self.active_grid_mut()[y] = src;
        }
        for y in (bottom - n + 1)..=bottom {
            self.active_grid_mut()[y] = Row {
                cells: vec![Cell::default(); self.cols],
                touch: seq,
                wrapped: false,
            };
        }
        self.touch_all();
    }

    /// Drop scrollback rows past the cap. Evicting `k` front rows shifts
    /// EVERY abs row down by `k` (scrollback row `i` becomes row `i - k`),
    /// so placement anchors renormalize with it; images anchored to evicted
    /// history die with their rows.
    fn evict_scrollback(&mut self) {
        let excess = self.scrollback.len().saturating_sub(self.max_scrollback);
        if excess == 0 {
            return;
        }
        for _ in 0..excess {
            self.scrollback.pop_front();
        }
        let k = excess as u32;
        for list in [&mut self.placements, &mut self.alt_placements] {
            list.retain_mut(|p| {
                if p.row < k {
                    return false;
                }
                p.row -= k;
                true
            });
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let (top, bottom) = (
            self.scroll_region.0,
            self.scroll_region.1.min(self.rows - 1),
        );
        if top > bottom || n == 0 {
            return;
        }
        let n = n.min(bottom - top + 1);
        let seq = self.seq;
        for y in ((top + n)..=bottom).rev() {
            let src = self.active_grid()[y - n].clone();
            self.active_grid_mut()[y] = src;
        }
        for y in top..(top + n) {
            self.active_grid_mut()[y] = Row {
                cells: vec![Cell::default(); self.cols],
                touch: seq,
                wrapped: false,
            };
        }
        self.touch_all();
        // Inserted lines push region content down; placements beyond the
        // region bottom are pushed out and die (abs anchors move with the
        // content they sit on).
        let (t, b) = (top as u32, bottom as u32);
        let k = n as u32;
        self.placements_mut().retain_mut(|p| {
            if p.row < t || p.row > b {
                return true;
            }
            if p.row + k > b {
                return false;
            }
            p.row += k;
            true
        });
    }

    fn erase_in_row(&mut self, y: usize, from: usize, to: usize) {
        let seq = self.seq;
        let bg = self.cur_bg;
        if let Some(row) = self.active_grid_mut().get_mut(y) {
            for x in from..to.min(row.cells.len()) {
                row.cells[x] = Cell {
                    ch: ' ',
                    fg: Color::Default,
                    bg,
                    flags: 0,
                };
            }
            row.touch = seq;
        }
    }

    fn clear_screen(&mut self) {
        let seq = self.seq;
        let (cols, rows) = (self.cols, self.rows);
        let grid = self.active_grid_mut();
        *grid = (0..rows)
            .map(|_| Row {
                cells: vec![Cell::default(); cols],
                touch: seq,
                wrapped: false,
            })
            .collect();
        self.cursor = (0, 0);
        self.pending_wrap = false;
        self.placements_mut().clear();
    }

    fn enter_alt(&mut self, save_cursor: bool) {
        if self.in_alt {
            return;
        }
        if save_cursor {
            self.save_cursor();
        }
        let seq = self.seq;
        self.alt_grid = empty_grid(self.rows, self.cols, seq);
        self.in_alt = true;
        self.alt_placements.clear();
        self.mode_bits |= 0b10;
        self.cursor = (0, 0);
        self.pending_wrap = false;
    }

    fn exit_alt(&mut self) {
        if !self.in_alt {
            return;
        }
        self.in_alt = false;
        self.mode_bits &= !0b10;
        self.cursor = (0, 0);
        self.pending_wrap = false;
        // 0.7.0: primary placements are kept — the alt screen had its own
        // list, so leaving vim no longer wipes images placed on the shell.
        self.touch_all();
    }

    fn save_cursor(&mut self) {
        self.saved = Some((self.cursor, self.cur_fg, self.cur_bg, self.cur_flags));
    }

    fn restore_cursor(&mut self) {
        if let Some((pos, fg, bg, fl)) = self.saved {
            // the saved position predates a possible resize; clamp it to the
            // current grid so later ICH/DCH can't index out of range
            self.cursor = (
                pos.0.min(self.cols.saturating_sub(1)),
                pos.1.min(self.rows.saturating_sub(1)),
            );
            self.cur_fg = fg;
            self.cur_bg = bg;
            self.cur_flags = fl;
            self.pending_wrap = false;
        }
    }

    fn full_reset(&mut self) {
        let seq = self.seq;
        self.grid = empty_grid(self.rows, self.cols, seq);
        self.scrollback.clear();
        self.in_alt = false;
        self.mode_bits = 1;
        self.cursor = (0, 0);
        self.cur_fg = Color::Default;
        self.cur_bg = Color::Default;
        self.cur_flags = 0;
        self.scroll_region = (0, self.rows.saturating_sub(1));
        self.origin_mode = false;
        self.auto_wrap = true;
        self.insert_mode = false;
        self.timeline.clear();
        self.mark = None;
        self.clipboard.clear();
        self.placements.clear();
        self.alt_placements.clear();
        self.graphics = GraphicsStore::new();
        self.graphics.cell_px = self.cell_px;
    }

    /// 0.7.0: resize REFLOWS the primary screen. Soft-wrapped logical lines
    /// (rows carrying `wrapped: true`) are re-wrapped at the new width, so
    /// narrowing the terminal pushes the continuation into scrollback and
    /// widening it pulls the line back together — the standard terminal
    /// behaviour, now grid-native. Scrollback is preserved; growing the
    /// height pulls history back into the viewport. The alternate screen is
    /// padded/truncated without reflow (xterm semantics — no scrollback
    /// there to rebuild from), and image placements are re-anchored onto
    /// the chunk that now holds their content cell.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(2), rows.max(2));
        let seq = self.seq;
        if cols == self.cols && rows == self.rows {
            return;
        }

        // ---- alternate screen: pad/truncate, no reflow ----
        self.alt_grid.truncate(rows);
        for row in self.alt_grid.iter_mut() {
            row.cells.resize(cols, Cell::default());
            row.touch = seq;
        }
        while self.alt_grid.len() < rows {
            self.alt_grid.push(Row {
                cells: vec![Cell::default(); cols],
                touch: seq,
                wrapped: false,
            });
        }
        let alt_max_abs = self.scrollback.len() as u32 + rows as u32;
        self.alt_placements.retain(|p| p.row < alt_max_abs);
        for p in self.alt_placements.iter_mut() {
            p.col = p.col.min(cols as u32 - 1);
        }

        // ---- primary screen: full reflow ----
        self.reflow_primary(cols, rows, seq);

        self.cols = cols;
        self.rows = rows;
        self.cursor.0 = self.cursor.0.min(cols - 1);
        self.cursor.1 = self.cursor.1.min(rows - 1);
        self.scroll_region = (0, rows - 1);
        self.tabs = (0..cols).map(|i| i % 8 == 0).collect();
        self.pending_wrap = false;
    }

    /// The reflow engine. Flat old coordinates: scrollback row `i` is flat
    /// row `i`, live row `y` is flat row `old_sb + y` — exactly the abs row
    /// placements are anchored to, which is what makes the remap trivial.
    fn reflow_primary(&mut self, cols: usize, rows: usize, seq: u64) {
        let old_cols = self.cols;
        let old_sb = self.scrollback.len();
        let old_cursor = self.cursor;

        // 1. Flatten scrollback + primary grid.
        let mut all: Vec<Row> = Vec::with_capacity(old_sb + self.grid.len());
        all.extend(self.scrollback.drain(..));
        all.extend(self.grid.drain(..));

        // 2. Split into logical lines at soft-wrap boundaries.
        struct Logical {
            start: usize,
            rows: usize,
            cells: Vec<Cell>,
        }
        let mut logicals: Vec<Logical> = Vec::new();
        let mut pending: Option<(usize, Vec<Cell>)> = None;
        for (i, row) in all.iter().enumerate() {
            match pending.as_mut() {
                Some((start, cells)) => cells.extend_from_slice(&row.cells),
                None => pending = Some((i, row.cells.clone())),
            }
            if !row.wrapped {
                let (start, cells) = pending.take().unwrap();
                logicals.push(Logical {
                    start,
                    rows: i + 1 - start,
                    cells,
                });
            }
        }
        if let Some((start, cells)) = pending.take() {
            let rows_n = all.len() - start;
            logicals.push(Logical {
                start,
                rows: rows_n,
                cells,
            });
        }

        // 3. Re-wrap every logical line at the new width (wide-char aware).
        //    chunk_meta: (cell_start, cell_len) per chunk; row_start: index
        //    of the line's first chunk in the final row list.
        struct Chunk {
            cells: Vec<Cell>,
            wrapped: bool,
            cell_start: usize,
            row_index: usize,
        }
        let mut new_rows: Vec<Row> = Vec::new();
        let mut line_chunks: Vec<Vec<Chunk>> = Vec::with_capacity(logicals.len());
        let old_cursor_flat = old_sb as i64 + old_cursor.1 as i64;

        for lg in &logicals {
            let mut chunks: Vec<Chunk> = Vec::new();
            let mut cur: Vec<Cell> = Vec::with_capacity(cols);
            let mut cur_cell_start = 0usize;
            let mut i = 0usize;
            while i < lg.cells.len() {
                let cell = lg.cells[i];
                if cell.flags & WIDE_SPACER != 0 {
                    // Spacer rides with its char cell (same chunk, 1:1 cols).
                    cur.push(cell);
                    i += 1;
                    continue;
                }
                let wide = UnicodeWidthChar::width(cell.ch).unwrap_or(0) == 2;
                let need = if wide { 2 } else { 1 };
                if cur.len() + need > cols && !cur.is_empty() {
                    // Wrap: pad the row to full width, hard-start the next.
                    while cur.len() < cols {
                        cur.push(Cell::default());
                    }
                    chunks.push(Chunk {
                        cells: std::mem::take(&mut cur),
                        wrapped: true,
                        cell_start: cur_cell_start,
                        row_index: 0,
                    });
                    cur_cell_start = i;
                }
                cur.push(cell);
                if wide {
                    if i + 1 < lg.cells.len() && lg.cells[i + 1].flags & WIDE_SPACER != 0 {
                        cur.push(lg.cells[i + 1]);
                        i += 1;
                    }
                }
                i += 1;
            }
            if cur.is_empty() && chunks.is_empty() {
                // Wholly blank logical line: keep one blank row.
                cur.push(Cell::default());
            }
            if !cur.is_empty() {
                while cur.len() < cols {
                    cur.push(Cell::default());
                }
                chunks.push(Chunk {
                    cells: cur,
                    wrapped: false,
                    cell_start: cur_cell_start,
                    row_index: 0,
                });
            }
            for (k, chunk) in chunks.iter_mut().enumerate() {
                chunk.row_index = new_rows.len() + k;
            }
            for chunk in &chunks {
                new_rows.push(Row {
                    cells: chunk.cells.clone(),
                    touch: seq,
                    wrapped: chunk.wrapped,
                });
            }
            line_chunks.push(chunks);
        }

        // 4. Remap the cursor through the chunk that holds its cell.
        let mut new_cursor = (0usize, 0usize);
        'find_cursor: for (li, lg) in logicals.iter().enumerate() {
            let line_rows = lg.rows as i64;
            let line_start = lg.start as i64;
            if old_cursor_flat < line_start || old_cursor_flat >= line_start + line_rows {
                continue;
            }
            let coff = ((old_cursor_flat - line_start) as usize) * old_cols + old_cursor.0;
            let chunks = &line_chunks[li];
            let last = chunks.len().saturating_sub(1);
            let (k, col) = chunks
                .iter()
                .enumerate()
                .find(|(_, c)| coff >= c.cell_start && coff < c.cell_start + c.cells.len())
                .map(|(k, c)| (k, coff - c.cell_start))
                .unwrap_or((last, coff.saturating_sub(chunks[last].cell_start)));
            let flat = chunks[k].row_index;
            new_cursor = (col.min(cols - 1), flat);
            break 'find_cursor;
        }

        // 5. Distribute. The viewport keeps the CURSOR visible (xterm rule):
        //    overflow above the cursor goes to scrollback; content below the
        //    viewport bottom is cut, exactly like a real shrink. Growing the
        //    height pulls history back into the viewport.
        let total = new_rows.len();
        let cur_flat = new_cursor.1;
        let push_n = if total > rows && cur_flat >= rows {
            cur_flat + 1 - rows // cursor becomes the bottom viewport row
        } else {
            0
        };
        let end = (push_n + rows).min(total);
        let sb_target = push_n;
        let evict = sb_target.saturating_sub(self.max_scrollback);
        self.scrollback = new_rows[evict..sb_target.min(end)].to_vec().into();
        let mut grid: Vec<Row> = new_rows[sb_target..end].to_vec();
        while grid.len() < rows {
            grid.push(Row {
                cells: vec![Cell::default(); cols],
                touch: seq,
                wrapped: false,
            });
        }
        self.grid = grid;

        // 6. Re-anchor image placements: find the chunk holding their old
        //    content cell, drop anchors that fell into evicted or cut rows.
        self.placements.retain_mut(|p| {
            let old = p.row as usize;
            let Some((li, lg)) = logicals
                .iter()
                .enumerate()
                .find(|(_, l)| old >= l.start && old < l.start + l.rows)
            else {
                return false;
            };
            let coff = (old - lg.start) * old_cols + (p.col as usize).min(old_cols - 1);
            let chunks = &line_chunks[li];
            let last = chunks.len().saturating_sub(1);
            let k = chunks
                .iter()
                .position(|c| coff >= c.cell_start && coff < c.cell_start + c.cells.len())
                .unwrap_or(last);
            let flat = chunks[k].row_index;
            if flat < evict || flat >= end {
                return false;
            }
            p.row = (flat - evict) as u32;
            p.col = (p.col as usize).min(cols - 1) as u32;
            true
        });

        // 7. Cursor lands where its content landed (clamped to the viewport).
        self.cursor = (
            new_cursor.0,
            new_cursor.1.saturating_sub(sb_target).min(rows - 1),
        );
    }

    fn move_to(&mut self, col: usize, row: usize) {
        let row = if self.origin_mode {
            self.scroll_region.0 + row.min(self.scroll_region.1 - self.scroll_region.0)
        } else {
            row
        };
        self.cursor = (col.min(self.cols - 1), row.min(self.rows - 1));
        self.pending_wrap = false;
    }

    fn tab(&mut self) {
        let x = self.cursor.0;
        for i in x + 1..self.cols {
            if self.tabs[i] {
                self.cursor.0 = i;
                return;
            }
        }
        self.cursor.0 = self.cols - 1;
    }

    fn back_tab(&mut self, n: usize) {
        let mut x = self.cursor.0;
        for _ in 0..n {
            let mut found = 0;
            for i in (0..x).rev() {
                if self.tabs[i] {
                    found = i;
                    break;
                }
            }
            x = found;
        }
        self.cursor.0 = x;
    }

    fn sgr(&mut self, params: &[&[u16]]) {
        let empty: &[u16] = &[0];
        let params: Vec<&[u16]> = if params.is_empty() {
            vec![empty]
        } else {
            params.to_vec()
        };
        let mut i = 0;
        while i < params.len() {
            let p = params[i];
            let first = p.first().copied().unwrap_or(0);
            match first {
                0 => {
                    self.cur_fg = Color::Default;
                    self.cur_bg = Color::Default;
                    self.cur_flags = 0;
                }
                1 => self.cur_flags |= flags::BOLD,
                2 => self.cur_flags |= flags::DIM,
                3 => self.cur_flags |= flags::ITALIC,
                4 => {
                    if p.len() > 1 && p[1] == 0 {
                        self.cur_flags &= !flags::UNDERLINE;
                    } else {
                        self.cur_flags |= flags::UNDERLINE;
                    }
                }
                5 => self.cur_flags |= flags::DIM, // blink ≈ dim
                7 => self.cur_flags |= flags::REVERSE,
                8 => self.cur_flags |= flags::HIDDEN,
                9 => self.cur_flags |= flags::STRIKEOUT,
                21 | 22 => self.cur_flags &= !(flags::BOLD | flags::DIM),
                23 => self.cur_flags &= !flags::ITALIC,
                24 => self.cur_flags &= !flags::UNDERLINE,
                27 => self.cur_flags &= !flags::REVERSE,
                28 => self.cur_flags &= !flags::HIDDEN,
                29 => self.cur_flags &= !flags::STRIKEOUT,
                30..=37 => self.cur_fg = Color::Indexed((first - 30) as u8),
                38 | 48 => {
                    let (color, consumed) = parse_color_ext(&params, i);
                    if first == 38 {
                        self.cur_fg = color;
                    } else {
                        self.cur_bg = color;
                    }
                    i += consumed;
                }
                39 => self.cur_fg = Color::Default,
                40..=47 => self.cur_bg = Color::Indexed((first - 40) as u8),
                49 => self.cur_bg = Color::Default,
                90..=97 => self.cur_fg = Color::Indexed((first - 90 + 8) as u8),
                100..=107 => self.cur_bg = Color::Indexed((first - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }

    // ------------------------------------------------------------- kitty APC

    fn handle_apc(&mut self, payload: &[u8]) {
        if !payload.starts_with(b"G") {
            return;
        }
        match self.graphics.feed(&payload[1..]) {
            Ok(KittyEvent::Placed {
                image_id,
                placement,
            }) => {
                let (wc, hc) = self.graphics.cells_for(image_id, &placement);
                let (im_w, im_h) = self
                    .graphics
                    .get(image_id)
                    .map(|im| (im.w, im.h))
                    .unwrap_or((0, 0));
                let number = self.graphics.get(image_id).and_then(|im| im.number);
                let col = (self.cursor.0 as u32) + placement.x_cells.unwrap_or(0);
                // Absolute row anchor: placements survive scrolling into
                // scrollback and land on the same content line after reflow.
                let row = self.abs_row(self.cursor.1).max(0) as u32;
                self.placements_mut().push(ImagePlacement {
                    image_id,
                    image_number: number.unwrap_or(image_id),
                    col,
                    row,
                    width_cells: wc,
                    height_cells: hc,
                    pixel_w: im_w,
                    pixel_h: im_h,
                });
            }
            Ok(KittyEvent::Deleted(scope)) => match scope {
                DeleteScope::AllPlacements => self.placements.clear(),
                DeleteScope::ByImageId(id) => self.placements.retain(|p| p.image_id != id),
                DeleteScope::ByImageNumber(n) => self.placements.retain(|p| p.image_number != n),
                DeleteScope::ByNumberEverywhere(n) => {
                    self.placements.retain(|p| p.image_number != n)
                }
                DeleteScope::AllImages => {
                    // the store dropped every image (d=3/d=7); drop the
                    // placements pointing at the deleted ids — both screens
                    self.placements.clear();
                    self.alt_placements.clear();
                }
                _ => {}
            },
            Ok(_) => {}
            Err(_) => {}
        }
    }

    // ------------------------------------------------------------- OSC 133

    fn osc_dispatch_impl(&mut self, params: &[&[u8]]) {
        if params.is_empty() {
            return;
        }
        match params[0] {
            b"0" | b"2" => {
                self.title = params[1..]
                    .iter()
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect::<Vec<_>>()
                    .join(";");
            }
            b"7" => {
                if let Some(f) = params.get(1) {
                    let s = String::from_utf8_lossy(f).into_owned();
                    if let Some(path) = s.strip_prefix("file://").map(str::to_string) {
                        self.cwd = url_decode(&path);
                    } else {
                        self.cwd = s;
                    }
                }
            }
            b"133" => {
                let kind = params.get(1).map(|s| s.to_vec()).unwrap_or_default();
                match kind.first().copied() {
                    Some(b'A') => {
                        self.mark = Some(Mark {
                            seq_prompt: self.seq,
                            abs_row_b: self.abs_row(self.cursor.1),
                            started_unix_ms: now_ms(),
                            command: None,
                        });
                        self.last_prompt_seq = self.seq;
                    }
                    Some(b'B') => {
                        let (seq, abs_row) = (self.seq, self.abs_row(self.cursor.1));
                        let m = self.mark.get_or_insert_with(|| Mark {
                            seq_prompt: seq,
                            abs_row_b: abs_row,
                            started_unix_ms: now_ms(),
                            command: None,
                        });
                        m.abs_row_b = abs_row;
                    }
                    Some(b'C') => {
                        let row = self.mark.as_ref().map(|m| m.abs_row_b);
                        if let Some(abs_row_b) = row {
                            let cmd = self.capture_command_text(abs_row_b);
                            if let Some(m) = self.mark.as_mut() {
                                m.command = cmd;
                            }
                        }
                    }
                    Some(b'D') => {
                        let exit = params
                            .get(2)
                            .and_then(|s| std::str::from_utf8(s).ok())
                            .and_then(|s| s.trim().parse::<i32>().ok());
                        let seq = self.seq;
                        let ts = now_ms();
                        let tag = params
                            .get(3)
                            .and_then(|s| std::str::from_utf8(s).ok())
                            .map(str::to_string);
                        if let Some(m) = self.mark.take() {
                            self.timeline.push_back(CommandMark {
                                seq_prompt: m.seq_prompt,
                                seq_end: seq,
                                command: m.command,
                                exit,
                                started_unix_ms: m.started_unix_ms,
                                ended_unix_ms: ts,
                                row_start: (m.abs_row_b - self.scrollback_len() as i64).max(0)
                                    as u32,
                                tag,
                            });
                        } else {
                            // 0.8.0 fix: shells WITHOUT OSC 133 integration
                            // (dash, plain sh) never open an A/B mark, so the
                            // run sentinel's D used to be dropped entirely and
                            // `run` waited forever for an exit mark that never
                            // got recorded. Synthesize the timeline entry.
                            self.timeline.push_back(CommandMark {
                                seq_prompt: self.last_prompt_seq,
                                seq_end: seq,
                                command: None,
                                exit,
                                started_unix_ms: ts,
                                ended_unix_ms: ts,
                                row_start: (self.abs_row(self.cursor.1)
                                    - self.scrollback_len() as i64)
                                    .max(0) as u32,
                                tag,
                            });
                        }
                        if self.timeline.len() > TIMELINE_CAP {
                            self.timeline.pop_front();
                        }
                        self.last_exit = exit;
                    }
                    _ => {}
                }
            }
            // 0.8.0: OSC 52 clipboard set. `52 ; c ; <base64>` stores the
            // decoded text as a queryable grid cell; `52 ; c ; ?` is a
            // clipboard QUERY (the emulator would answer with the same
            // sequence — a headless grid has nothing to answer with, so it
            // is ignored rather than echoed back into the app's stdin).
            //
            // 1.0.0: push to a bounded ring of the last
            // [`CLIPBOARD_RING_CAP`] payloads (instead of overwriting). The
            // latest entry is what `clipboard()` returns, so callers see the
            // same `Some(...)` shape as before — but the older payloads are
            // no longer destroyed, and `clipboard_history()` exposes them.
            b"52" => {
                let payload = params.get(2).copied().unwrap_or_default();
                if payload == b"?" || payload.is_empty() {
                    return;
                }
                const CLIPBOARD_CAP: usize = 64 * 1024;
                if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload) {
                    if bytes.len() <= CLIPBOARD_CAP {
                        // Store lossy: a clipboard is text in practice, and a
                        // binary blob should never wedge the query path.
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        if self.clipboard.len() >= CLIPBOARD_RING_CAP {
                            self.clipboard.pop_front();
                        }
                        self.clipboard.push_back(text);
                    }
                }
            }
            _ => {}
        }
    }

    fn capture_command_text(&self, abs_row_b: i64) -> Option<String> {
        let sb = self.scrollback.len() as i64;
        let start_y = (abs_row_b - sb).max(0) as usize;
        let end_y = self.cursor.1; // exclusive: cursor sits on first output row
        if start_y >= end_y && start_y > 0 {
            return None;
        }
        let grid = self.active_grid();
        let end_y = end_y.min(grid.len().saturating_sub(1));
        let mut lines = Vec::new();
        for y in start_y..end_y {
            lines.push(row_text(&grid[y]));
        }
        let joined = lines.join("\n").trim().to_string();
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    }

    // ------------------------------------------------------------- reading

    fn line_from_row(&self, row: &Row, index: i64, images: bool) -> GridLine {
        let mut text = String::new();
        let mut attrs = Vec::new();
        let mut colored = false;
        let mut chars = Vec::new();
        for cell in &row.cells {
            if cell.flags & WIDE_SPACER != 0 {
                continue;
            }
            chars.push((cell.ch, cell.flags & !WIDE_SPACER));
        }
        // Compute trimmed length in chars.
        let mut keep = chars.len();
        while keep > 0 && chars[keep - 1].0 == ' ' {
            keep -= 1;
        }
        for (ch, a) in &chars[..keep] {
            text.push(*ch);
            attrs.push(*a);
            if *a != 0 {
                colored = true;
            }
        }
        // Color presence (fg/bg) also marks the line colored; check the first
        // kept cells conservatively.
        if !colored {
            for cell in &row.cells[..keep.min(row.cells.len())] {
                if cell.fg != Color::Default || cell.bg != Color::Default {
                    colored = true;
                    break;
                }
            }
        }
        let image = if images {
            self.placement_at(index)
        } else {
            None
        };
        GridLine {
            seq: row.touch,
            index,
            text,
            colored,
            attrs,
            image,
        }
    }

    fn placement_at(&self, abs_row_index: i64) -> Option<ImagePlacement> {
        // 0.7.0: placements are absolute-row anchored, so scrollback rows
        // carry their images exactly like viewport rows do.
        self.placements()
            .iter()
            .find(|p| p.row as i64 == abs_row_index)
            .cloned()
    }

    fn grid_rows(&self, include_scrollback: bool) -> Vec<(i64, &Row)> {
        let sb = self.scrollback_len();
        let mut out = Vec::with_capacity(sb + self.rows);
        if include_scrollback {
            for (i, r) in self.scrollback.iter().enumerate() {
                out.push((i as i64, r));
            }
        }
        for (y, r) in self.active_grid().iter().enumerate() {
            out.push((sb as i64 + y as i64, r));
        }
        out
    }

    pub fn lines(&self, opts: LineOpts) -> Vec<GridLine> {
        let mut rows = self.grid_rows(opts.include_scrollback);
        if let Some(tail) = opts.tail {
            let tail = tail as usize;
            if rows.len() > tail {
                rows = rows.split_off(rows.len() - tail);
            }
        }
        rows.into_iter()
            .map(|(i, r)| self.line_from_row(r, i, opts.images))
            .collect()
    }

    /// Rows touched by any output chunk with seq > `since`, newest last.
    pub fn lines_since(&self, since: u64, tail: Option<u32>) -> Vec<GridLine> {
        let mut rows: Vec<(i64, &Row)> = self
            .grid_rows(true)
            .into_iter()
            .filter(|(_, r)| r.touch > since)
            .collect();
        if let Some(tail) = tail {
            let tail = tail as usize;
            if rows.len() > tail {
                rows = rows.split_off(rows.len() - tail);
            }
        }
        rows.into_iter()
            .map(|(i, r)| self.line_from_row(r, i, true))
            .collect()
    }

    /// Plain-text view of the live screen (used by `snapshot --format text`).
    pub fn snapshot_text(&self) -> String {
        let mut out = String::new();
        for (_, r) in self.grid_rows(false) {
            out.push_str(&row_text(r));
            out.push('\n');
        }
        out
    }

    // ------------------------------------------------------- viewer wire

    /// Encode a row for the native viewer (full cell colors).
    fn row_wire(&self, index: i64, row: &Row) -> rox_protocol::RowWire {
        use rox_protocol::{CellWire, RowWire};
        let mut cells = Vec::with_capacity(row.cells.len());
        for c in &row.cells {
            if c.flags & WIDE_SPACER != 0 {
                continue;
            }
            cells.push(CellWire {
                ch: c.ch,
                fg: encode_color(c.fg),
                bg: encode_color(c.bg),
                flags: c.flags & !WIDE_SPACER,
            });
        }
        RowWire {
            index,
            touch: row.touch,
            cells,
        }
    }

    /// Full grid state for a fresh viewer (viewport + scrollback).
    pub fn rows_all(&self) -> Vec<rox_protocol::RowWire> {
        self.grid_rows(true)
            .into_iter()
            .map(|(i, r)| self.row_wire(i, r))
            .collect()
    }

    /// Current viewport rows only, for embedded renderers that do not need to
    /// clone the potentially large scrollback buffer on every redraw.
    pub fn live_rows(&self) -> Vec<rox_protocol::RowWire> {
        self.active_grid()
            .iter()
            .enumerate()
            .map(|(i, row)| self.row_wire(i as i64, row))
            .collect()
    }

    /// Rows visible at a scrollback offset (0 is the live bottom). Only the
    /// requested viewport is cloned; a full-screen program remains on its
    /// alternate grid and never exposes the shell's primary scrollback.
    pub fn viewport_rows(&self, scrollback_offset: usize) -> Vec<rox_protocol::RowWire> {
        if self.in_alt || scrollback_offset == 0 {
            return self.live_rows();
        }
        let sb_len = self.scrollback.len();
        let top = sb_len.saturating_sub(scrollback_offset.min(sb_len));
        let mut rows = Vec::with_capacity(self.rows);
        for y in 0..self.rows {
            let index = top + y;
            let row = if index < sb_len {
                self.scrollback.get(index)
            } else {
                self.active_grid().get(index - sb_len)
            };
            if let Some(row) = row {
                rows.push(self.row_wire(index as i64, row));
            }
        }
        rows
    }

    /// Incremental viewer state: rows touched after `since`, newest last.
    pub fn rows_since(&self, since: u64, limit: usize) -> Vec<rox_protocol::RowWire> {
        let rows: Vec<(i64, &Row)> = self
            .grid_rows(true)
            .into_iter()
            .filter(|(_, r)| r.touch > since)
            .collect();
        let start = rows.len().saturating_sub(limit);
        rows[start..]
            .iter()
            .map(|(i, r)| self.row_wire(*i, r))
            .collect()
    }
}

/// Viewer color encoding: 0 default, 0x4000_0000|n indexed, 0x8000_0000|rgb.
pub fn encode_color(c: Color) -> u32 {
    match c {
        Color::Default => 0,
        Color::Indexed(i) => 0x4000_0000 | i as u32,
        Color::Rgb(r, g, b) => 0x8000_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
    }
}

fn empty_grid(rows: usize, cols: usize, touch: u64) -> Vec<Row> {
    (0..rows)
        .map(|_| Row {
            cells: vec![Cell::default(); cols],
            touch,
            wrapped: false,
        })
        .collect()
}

fn row_text(row: &Row) -> String {
    let mut s = String::new();
    for c in &row.cells {
        if c.flags & WIDE_SPACER != 0 {
            continue;
        }
        s.push(c.ch);
    }
    s.trim_end().to_string()
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3])
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(byte) = hex {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Parse extended colors (38/48) at param index `i`; returns color + consumed.
fn parse_color_ext(params: &[&[u16]], i: usize) -> (Color, usize) {
    let p = params[i];
    // Colon form: single param [38, 5, n] or [38, 2, r, g, b].
    if p.len() >= 3 && p[1] == 5 {
        return (Color::Indexed(p[2] as u8), 0);
    }
    if p.len() >= 5 && p[1] == 2 {
        return (Color::Rgb(p[2] as u8, p[3] as u8, p[4] as u8), 0);
    }
    // Semicolon form: params[i]=[38], params[i+1]=[5], params[i+2]=[n].
    let semi = |k: usize| params.get(i + k).and_then(|s| s.first().copied());
    match semi(1) {
        Some(5) => {
            let c = semi(2).unwrap_or(0);
            (Color::Indexed(c as u8), 2)
        }
        Some(2) => {
            let (r, g, b) = (
                semi(2).unwrap_or(0),
                semi(3).unwrap_or(0),
                semi(4).unwrap_or(0),
            );
            (Color::Rgb(r as u8, g as u8, b as u8), 4)
        }
        _ => (Color::Default, 0),
    }
}

impl Perform for Screen {
    fn print(&mut self, c: char) {
        self.put_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => {} // BEL
            0x08 => {
                if self.cursor.0 > 0 {
                    self.cursor.0 -= 1;
                    self.pending_wrap = false;
                }
            }
            0x09 => self.tab(),
            0x0A | 0x0B | 0x0C => {
                self.pending_wrap = false;
                self.linefeed_internal();
            }
            0x0D => {
                self.cursor.0 = 0;
                self.pending_wrap = false;
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        self.osc_dispatch_impl(params);
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        let params: Vec<&[u16]> = params.iter().collect();
        let n = |k: usize, default: u32| -> u32 {
            params
                .get(k)
                .and_then(|s| s.first())
                .copied()
                .map(|v| if v == 0 { default } else { v as u32 })
                .unwrap_or(default)
        };
        let private = intermediates.contains(&b'?');

        match action {
            '@' => {
                let cnt = n(0, 1) as usize;
                let seq = self.seq;
                let (x, y) = self.cursor;
                if let Some(row) = self.active_grid_mut().get_mut(y) {
                    for _ in 0..cnt {
                        if row.cells.len() > 1 {
                            row.cells.pop();
                            row.cells.insert(x, Cell::default());
                        }
                    }
                    row.touch = seq;
                }
            }
            'A' => {
                self.cursor.1 = self.cursor.1.saturating_sub(n(0, 1) as usize);
            }
            'B' | 'e' => {
                self.cursor.1 = (self.cursor.1 + n(0, 1) as usize).min(self.rows - 1);
            }
            'C' | 'a' => {
                self.cursor.0 = (self.cursor.0 + n(0, 1) as usize).min(self.cols - 1);
                self.pending_wrap = false;
            }
            'D' => {
                self.cursor.0 = self.cursor.0.saturating_sub(n(0, 1) as usize);
                self.pending_wrap = false;
            }
            'E' => {
                self.cursor.0 = 0;
                self.cursor.1 = (self.cursor.1 + n(0, 1) as usize).min(self.rows - 1);
            }
            'F' => {
                self.cursor.0 = 0;
                self.cursor.1 = self.cursor.1.saturating_sub(n(0, 1) as usize);
            }
            'G' | '`' => {
                self.cursor.0 = (n(0, 1) as usize).saturating_sub(1).min(self.cols - 1);
                self.pending_wrap = false;
            }
            'H' | 'f' => {
                let row = n(0, 1).saturating_sub(1) as usize;
                let col = n(1, 1).saturating_sub(1) as usize;
                self.move_to(col, row);
            }
            'I' => {
                for _ in 0..n(0, 1) {
                    self.tab();
                }
            }
            'J' => {
                let mode = n(0, 0);
                let (cols, rows) = (self.cols, self.rows);
                match mode {
                    0 => {
                        let (x, y) = self.cursor;
                        self.erase_in_row(y, x, cols);
                        for yy in y + 1..rows {
                            self.erase_in_row(yy, 0, cols);
                        }
                        let anchor = self.abs_row(y);
                        self.placements_mut().retain(|p| (p.row as i64) <= anchor);
                    }
                    1 => {
                        let (x, y) = self.cursor;
                        self.erase_in_row(y, 0, x + 1);
                        for yy in 0..y {
                            self.erase_in_row(yy, 0, cols);
                        }
                    }
                    2 | 3 => {
                        self.clear_screen();
                        if mode == 3 && !self.in_alt {
                            self.scrollback.clear();
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                let mode = n(0, 0);
                let (x, y) = self.cursor;
                let cols = self.cols;
                match mode {
                    0 => self.erase_in_row(y, x, cols),
                    1 => self.erase_in_row(y, 0, x + 1),
                    _ => self.erase_in_row(y, 0, cols),
                }
            }
            'L' => {
                let cnt = n(0, 1) as usize;
                let y = self.cursor.1;
                if y >= self.scroll_region.0 && y <= self.scroll_region.1.min(self.rows - 1) {
                    let (t, b) = self.scroll_region;
                    self.scroll_region = (y, b);
                    self.scroll_down(cnt);
                    self.scroll_region = (t, b);
                }
            }
            'M' => {
                let cnt = n(0, 1) as usize;
                let y = self.cursor.1;
                if y >= self.scroll_region.0 && y <= self.scroll_region.1.min(self.rows - 1) {
                    let (t, b) = self.scroll_region;
                    self.scroll_region = (y, b);
                    self.scroll_up(cnt);
                    self.scroll_region = (t, b);
                }
            }
            'P' => {
                let cnt = n(0, 1) as usize;
                let seq = self.seq;
                let (x, y) = self.cursor;
                if let Some(row) = self.active_grid_mut().get_mut(y) {
                    for _ in 0..cnt {
                        if row.cells.len() > 1 {
                            row.cells.remove(x);
                            row.cells.push(Cell::default());
                        }
                    }
                    row.touch = seq;
                }
            }
            'S' => self.scroll_up(n(0, 1) as usize),
            'T' => self.scroll_down(n(0, 1) as usize),
            'X' => {
                let cnt = n(0, 1) as usize;
                let (x, y) = self.cursor;
                self.erase_in_row(y, x, x + cnt);
            }
            'Z' => self.back_tab(n(0, 1) as usize),
            'b' => {} // REP — rare; skipped
            'd' => {
                let row = n(0, 1).saturating_sub(1) as usize;
                self.move_to(self.cursor.0, row);
            }
            'g' => {
                let mode = n(0, 0);
                if mode == 0 {
                    if let Some(t) = self.tabs.get_mut(self.cursor.0) {
                        *t = false;
                    }
                } else {
                    self.tabs = vec![false; self.cols];
                }
            }
            'h' | 'l' => {
                let set = action == 'h';
                if private {
                    self.private_mode(&params, set);
                } else {
                    let mode = n(0, 0);
                    if mode == 4 {
                        self.insert_mode = set;
                    }
                }
            }
            'm' => self.sgr(&params),
            'r' => {
                let top = n(0, 1).saturating_sub(1) as usize;
                let bottom = n(1, self.rows as u32).saturating_sub(1) as usize;
                if top < bottom && bottom < self.rows {
                    self.scroll_region = (top, bottom);
                    self.cursor.1 = top;
                    self.pending_wrap = false;
                }
            }
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        match (intermediates.first(), byte) {
            (None, b'D') => self.linefeed_internal(), // IND
            (None, b'E') => {
                self.cursor.0 = 0;
                self.linefeed_internal();
            }
            (None, b'M') => {
                let top = self.scroll_region.0;
                if self.cursor.1 == top {
                    self.scroll_down(1);
                } else {
                    self.cursor.1 = self.cursor.1.saturating_sub(1);
                }
            }
            (None, b'H') => {
                if let Some(t) = self.tabs.get_mut(self.cursor.0) {
                    *t = true;
                }
            }
            (None, b'7') => self.save_cursor(),
            (None, b'8') => self.restore_cursor(),
            (None, b'c') => self.full_reset(),
            (Some(b'('), _) | (Some(b')'), _) | (Some(b'*'), _) | (Some(b'+'), _) => {}
            (Some(b'#'), b'8') => {
                let cols = self.cols;
                let seq = self.seq;
                let grid = self.active_grid_mut();
                for row in grid.iter_mut() {
                    *row = Row {
                        cells: vec![
                            Cell {
                                ch: 'E',
                                fg: Color::Default,
                                bg: Color::Default,
                                flags: 0
                            };
                            cols
                        ],
                        touch: seq,
                        wrapped: false,
                    };
                }
            }
            _ => {}
        }
    }
}

impl Screen {
    fn private_mode(&mut self, params: &[&[u16]], set: bool) {
        for p in params {
            let Some(mode) = p.first().copied() else {
                continue;
            };
            match mode {
                1 => {
                    if set {
                        self.mode_bits |= 0b1000
                    } else {
                        self.mode_bits &= !0b1000
                    }
                }
                3 => self.clear_screen(), // DECCOLM — clear either way
                5 => {
                    if set {
                        self.mode_bits |= 1 << 5
                    } else {
                        self.mode_bits &= !(1 << 5)
                    }
                }
                6 => {
                    self.origin_mode = set;
                    if set {
                        self.mode_bits |= 1 << 6
                    } else {
                        self.mode_bits &= !(1 << 6)
                    }
                    self.move_to(0, 0);
                }
                7 => self.auto_wrap = set,
                25 => {
                    if set {
                        self.mode_bits |= 1
                    } else {
                        self.mode_bits &= !1
                    }
                }
                47 => {
                    if set {
                        self.enter_alt(false)
                    } else {
                        self.exit_alt()
                    }
                }
                1047 => {
                    if set {
                        self.enter_alt(false)
                    } else {
                        self.exit_alt()
                    }
                }
                1049 => {
                    if set {
                        self.enter_alt(true);
                    } else {
                        self.exit_alt();
                        self.restore_cursor();
                    }
                }
                1000 | 1002 | 1003 => {
                    if set {
                        self.mode_bits |= 1 << 7
                    } else {
                        self.mode_bits &= !(1 << 7)
                    }
                }
                1004 => {}
                2004 => {
                    if set {
                        self.mode_bits |= 0b100
                    } else {
                        self.mode_bits &= !0b100
                    }
                }
                2026 => {
                    if set {
                        self.mode_bits |= 1 << 8
                    } else {
                        self.mode_bits &= !(1 << 8)
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Screen {
        Screen::new(20, 6)
    }

    #[test]
    fn basic_print_and_wrap() {
        let mut s = screen();
        s.feed(b"hello world");
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[0].text, "hello world");
        assert_eq!(s.cursor(), (11, 0));

        s.feed(&b"x".repeat(25));
        assert_eq!(s.cursor().1, 1, "should have wrapped to row 1");
    }

    #[test]
    fn csi_cursor_and_sgr() {
        let mut s = screen();
        s.feed(b"\x1b[2;3H\x1b[1;31mRED");
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[1].text, "  RED");
        assert_eq!(lines[1].attrs[2] & flags::BOLD, flags::BOLD);
        assert_eq!(s.cursor(), (5, 1));
    }

    #[test]
    fn scrollback_and_diff() {
        let mut s = Screen::new(10, 3);
        let seq0 = s.seq();
        for i in 0..10 {
            s.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(s.scrollback_len(), 8);
        let lines = s.lines(LineOpts {
            include_scrollback: true,
            ..Default::default()
        });
        assert_eq!(lines.len(), 11);
        assert_eq!(lines[0].text, "line 0");
        assert_eq!(lines[9].text, "line 9");

        let before = s.seq();
        let diff = s.lines_since(before, None);
        assert!(diff.is_empty(), "nothing changed since {before}");

        s.feed(b"new stuff");
        let diff = s.lines_since(before, None);
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].text, "new stuff");
        assert!(diff[0].seq > seq0);
    }

    #[test]
    fn osc133_d_mark_without_integration_still_records() {
        // 0.8.0 fix: a shell WITHOUT OSC 133 integration (dash) never opens
        // an A mark, but the run sentinel's D must still land in the
        // timeline — `run` waits on it or it times out.
        let mut s = Screen::new(60, 6);
        s.feed(b"$ true\r\n");
        s.feed(b"\x1b]133;D;0\x1b\\");
        let marks: Vec<_> = s.timeline().collect();
        assert_eq!(
            marks.len(),
            1,
            "bare D mark synthesized into the timeline: {marks:?}"
        );
        assert_eq!(marks[0].exit, Some(0));
        assert_eq!(marks[0].command, None);
        assert_eq!(s.last_exit, Some(0));

        // Nonzero exit codes ride the same path.
        s.feed(b"\x1b]133;D;7\x1b\\");
        let marks: Vec<_> = s.timeline().collect();
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[1].exit, Some(7));
        assert_eq!(s.last_exit, Some(7));

        // Integration shells (A → D) keep the richer path: A opens, D closes.
        let mut s = Screen::new(60, 6);
        s.feed(b"\x1b]133;A\x1b\\");
        s.feed(b"$ npm run dev\r\n");
        s.feed(b"\x1b]133;C\x1b\\");
        s.feed(b"\x1b]133;D;2\x1b\\");
        let marks: Vec<_> = s.timeline().collect();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].exit, Some(2));
        assert_eq!(marks[0].command.as_deref(), Some("$ npm run dev"));
    }

    #[test]
    fn osc133_d_mark_tag_is_content_addressed() {
        // 0.8.0: the run sentinel emits `D;<exit>;<tag>`; the tag lands on
        // the CommandMark so exit waits can address THEIR run's D, wherever
        // stale prompt-time Ds happen to sit in the timeline.
        let mut s = Screen::new(60, 6);
        s.feed(b"$ echo hi\r\n");
        // Stale prompt-time D (no tag) — e.g. shell startup still in flight.
        s.feed(b"\x1b]133;D;1\x1b\\");
        // The run's tagged sentinel D.
        s.feed(b"\x1b]133;D;0;run-42\x1b\\");
        let marks: Vec<_> = s.timeline().collect();
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].exit, Some(1));
        assert_eq!(marks[0].tag, None, "bare prompt D carries no tag");
        assert_eq!(marks[1].exit, Some(0));
        assert_eq!(marks[1].tag.as_deref(), Some("run-42"));
        // Tagged D works through the integration path too (A → D;exit;tag).
        let mut s = Screen::new(60, 6);
        s.feed(b"\x1b]133;A\x1b\\$ cmd\r\n\x1b]133;C\x1b\\\x1b]133;D;7;run-43\x1b\\");
        let marks: Vec<_> = s.timeline().collect();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].exit, Some(7));
        assert_eq!(marks[0].tag.as_deref(), Some("run-43"));
    }

    #[test]
    fn osc133_marks_timeline() {
        let mut s = screen();
        s.feed(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hi\r\n\x1b]133;C\x07hi\r\n\x1b]133;D;0\x07");
        let marks: Vec<&CommandMark> = s.timeline().collect();
        assert_eq!(marks.len(), 1);
        assert_eq!(marks[0].exit, Some(0));
        assert_eq!(marks[0].command.as_deref(), Some("$ echo hi"));
        assert_eq!(s.last_exit, Some(0));
        assert!(s.last_prompt_seq > 0);
    }

    #[test]
    fn alt_screen_switch() {
        let mut s = screen();
        s.feed(b"main screen");
        s.feed(b"\x1b[?1049h");
        assert_eq!(s.session_mode(), SessionMode::Alternate);
        s.feed(b"\x1b[3;1HALT-SCREEN");
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[2].text, "ALT-SCREEN");
        s.feed(b"\x1b[?1049l");
        assert_eq!(s.session_mode(), SessionMode::Normal);
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[0].text, "main screen");
    }

    #[test]
    fn scroll_region() {
        let mut s = Screen::new(10, 5);
        s.feed(b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        s.feed(b"\x1b[2;4r"); // region rows 2..4
        s.feed(b"\x1b[2;1H"); // cursor to region top
        s.feed(b"\x1bM"); // RI inside region → region scrolls down
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[1].text, "");
        assert_eq!(lines[2].text, "two");
        assert_eq!(lines[4].text, "five");
    }

    #[test]
    fn erase_clear() {
        let mut s = screen();
        s.feed(b"\x1b[38;5;196mX\x1b[0mY");
        s.feed(b"\x1b[2J");
        let lines = s.lines(LineOpts::default());
        assert!(lines.iter().all(|l| l.text.trim().is_empty()));
    }

    #[test]
    fn wide_chars_occupy_two_cells() {
        let mut s = Screen::new(10, 2);
        s.feed("\u{4f60}\u{597d}".as_bytes()); // 你好
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[0].text, "你好");
        assert_eq!(lines[0].attrs.len(), 2);
    }

    #[test]
    fn kitty_image_apc_via_hook() {
        let mut s = Screen::new(40, 10);
        // 1x1 RGBA image, direct transmit.
        s.feed(b"\x1b_Gf=32,s=1,v=1,i=1,q=1;AAAAAAAAAAA\x1b\\");
        assert_eq!(s.graphics.len(), 1, "image store should hold the transmit");
        s.feed(b"\x1b_Ga=p,i=1,C=2,R=1,q=1\x1b\\");
        let lines = s.lines(LineOpts {
            images: true,
            ..Default::default()
        });
        assert_eq!(lines[0].image.as_ref().unwrap().image_id, 1);
        assert_eq!(lines[0].image.as_ref().unwrap().width_cells, 2);
        let metas = s.images();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].pixel_w, 1);
    }

    #[test]
    fn title_and_cwd_osc() {
        let mut s = screen();
        s.feed(b"\x1b]0;rox editor\x07");
        assert_eq!(s.title, "rox editor");
        s.feed(b"\x1b]7;file:///home/z/projects\x07");
        assert_eq!(s.cwd, "/home/z/projects");
    }

    #[test]
    fn resize_keeps_content() {
        let mut s = screen();
        s.feed(b"hello");
        s.resize(10, 4);
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[0].text, "hello");
        assert_eq!(s.cursor(), (5, 0));
    }

    #[test]
    fn insert_and_delete_chars() {
        let mut s = screen();
        s.feed(b"ABCD");
        s.feed(b"\x1b[1;1H\x1b[2P"); // delete 2 chars at start
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[0].text, "CD");
    }

    #[test]
    fn run_exit_sentinel_visible_in_grid() {
        let mut s = screen();
        s.feed(b"out\r\n\x1b]133;D;42\x1b\\");
        assert_eq!(s.last_exit, Some(42));
    }

    #[test]
    fn bracketed_paste_bit() {
        let mut s = screen();
        assert!(!s.bracketed_paste());
        s.feed(b"\x1b[?2004h");
        assert!(s.bracketed_paste());
        s.feed(b"\x1b[?2004l");
        assert!(!s.bracketed_paste());
    }

    // ------------------------------------------------- 0.8.0 OSC 52 clipboard

    #[test]
    fn osc52_clipboard_roundtrip() {
        let mut s = Screen::new(40, 6);
        assert_eq!(s.clipboard(), None, "empty until an app publishes");
        // ESC ] 52 ; c ; base64 ST — "clipboard-hello" in base64.
        let b64 = base64::engine::general_purpose::STANDARD.encode("clipboard-hello");
        let seq = format!("\x1b]52;c;{b64}\x1b\\");
        s.feed(seq.as_bytes());
        assert_eq!(s.clipboard(), Some("clipboard-hello"));
        // A later copy overwrites (last write wins, like a real clipboard).
        let b64b = base64::engine::general_purpose::STANDARD.encode("second");
        s.feed(format!("\x1b]52;c;{b64b}\x07").as_bytes()); // BEL terminator
        assert_eq!(s.clipboard(), Some("second"));
    }

    #[test]
    fn osc52_query_and_junk_ignored() {
        let mut s = Screen::new(40, 6);
        // `?` is a clipboard QUERY — headless grid answers nothing.
        s.feed(b"\x1b]52;c;?\x1b\\");
        assert_eq!(s.clipboard(), None);
        // Invalid base64 never wedges the parser.
        s.feed(b"\x1b]52;c;%%%not-base64%%%\x1b\\");
        assert_eq!(s.clipboard(), None);
        // Empty payload ignored.
        s.feed(b"\x1b]52;c;\x1b\\");
        assert_eq!(s.clipboard(), None);
        // The screen still parses normally after all of that.
        s.feed(b"alive\r\n");
        assert_eq!(s.lines(LineOpts::default())[0].text, "alive");
    }

    #[test]
    fn osc52_cleared_on_full_reset() {
        let mut s = Screen::new(40, 6);
        let b64 = base64::engine::general_purpose::STANDARD.encode("gone");
        s.feed(format!("\x1b]52;c;{b64}\x1b\\").as_bytes());
        assert_eq!(s.clipboard(), Some("gone"));
        s.feed(b"\x1bc"); // RIS full reset
        assert_eq!(
            s.clipboard(),
            None,
            "RIS wipes every grid cell incl. clipboard"
        );
    }

    #[test]
    fn osc52_oversize_payload_rejected() {
        let mut s = Screen::new(40, 6);
        // > 64 KiB decoded: refuse, keep the previous clipboard.
        let big = vec![b'x'; 70_000];
        let b64 = base64::engine::general_purpose::STANDARD.encode(&big);
        s.feed(format!("\x1b]52;c;{b64}\x1b\\").as_bytes());
        assert_eq!(s.clipboard(), None);
    }

    #[test]
    fn osc52_history_ring_keeps_last_16() {
        // 1.0.0 fix: the clipboard used to be last-write-only — an agent
        // had no way to know what the app copied before the most recent
        // OSC 52 publish. Now the grid keeps the last 16 payloads; the
        // most recent is still reachable via `clipboard()` (back-compat
        // with 0.8.x), and `clipboard_history()` exposes the full ring
        // oldest → newest.
        let mut s = Screen::new(40, 6);
        for i in 0..20 {
            let payload = format!("copy-{i}");
            let b64 = base64::engine::general_purpose::STANDARD.encode(&payload);
            s.feed(format!("\x1b]52;c;{b64}\x1b\\").as_bytes());
        }
        // The most recent payload is "copy-19" — the 0.8.x API.
        assert_eq!(s.clipboard(), Some("copy-19"));
        // History keeps the last 16 (oldest → newest): copy-4 .. copy-19.
        let history: Vec<&str> = s.clipboard_history().collect();
        assert_eq!(history.len(), 16, "ring caps at 16");
        assert_eq!(history[0], "copy-4", "oldest surviving payload is copy-4");
        assert_eq!(
            history[15], "copy-19",
            "newest is copy-19 (also clipboard())"
        );
        assert_eq!(s.clipboard_history_len(), 16);
    }

    // ------------------------------------------------- 0.7.0 reflow

    #[test]
    fn resize_reflow_narrow_then_wide() {
        let mut s = Screen::new(20, 6);
        s.feed(b"hello world, roxterm reflows");
        assert_eq!(s.lines(LineOpts::default())[0].text, "hello world, roxterm");
        // Narrow: the soft-wrapped line re-wraps at 10 cols.
        s.resize(10, 6);
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[0].text, "hello worl");
        assert_eq!(lines[1].text, "d, roxterm");
        assert_eq!(lines[2].text, " reflows");
        // Widen again: the logical line rejoins on one row.
        s.resize(40, 6);
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[0].text, "hello world, roxterm reflows");
        assert_eq!(s.cursor(), (28, 0), "cursor follows its content cell");
        // Resize to the same size is a no-op.
        s.resize(40, 6);
        assert_eq!(
            s.lines(LineOpts::default())[0].text,
            "hello world, roxterm reflows"
        );
    }

    #[test]
    fn resize_reflow_pulls_scrollback_into_view() {
        let mut s = Screen::new(10, 4);
        for i in 0..10 {
            s.feed(format!("row{i}\r\n").as_bytes());
        }
        assert!(s.scrollback_len() >= 7);
        // Grow the viewport: history is pulled back in, scrollback empties.
        s.resize(10, 12);
        assert_eq!(s.scrollback_len(), 0);
        let lines = s.lines(LineOpts::default());
        assert!(
            lines.iter().any(|l| l.text == "row0"),
            "oldest row pulled back: {lines:?}"
        );
        assert!(lines.iter().any(|l| l.text.contains("row9")));
    }

    #[test]
    fn wrapped_rows_rejoin_across_viewport_boundary() {
        let mut s = Screen::new(6, 3);
        s.feed(b"abcdefghij"); // row0 "abcdef" (wrapped) + row1 "ghij"
        s.feed(b"\r\n\r\n\r\n\r\n\r\n"); // push both into scrollback
        assert!(s.scrollback_len() >= 4);
        s.resize(30, 3);
        let lines = s.lines(LineOpts {
            include_scrollback: true,
            tail: None,
            images: false,
        });
        assert!(
            lines.iter().any(|l| l.text.contains("abcdefghij")),
            "soft-wrapped rows rejoin into one logical line even across the scrollback boundary: {lines:?}"
        );
    }

    #[test]
    fn hard_breaks_stay_hard_after_reflow() {
        let mut s = Screen::new(10, 4);
        s.feed(b"one\r\ntwo\r\nthree");
        s.resize(30, 4);
        let lines = s.lines(LineOpts::default());
        assert_eq!(lines[0].text, "one");
        assert_eq!(lines[1].text, "two");
        assert_eq!(lines[2].text, "three");
    }

    // --------------------------------- 0.7.0 scrollback placements

    #[test]
    fn placement_survives_scrollback_and_eviction() {
        let mut s = Screen::new(40, 6);
        s.feed(b"\x1b_Gf=32,s=1,v=1,i=1,q=1;AAAAAAAAAAA\x1b\\");
        s.feed(b"\x1b_Ga=p,i=1,C=2,R=2,q=1\x1b\\");
        assert!(s.placement_at(0).is_some(), "placed at abs row 0");
        for i in 0..10 {
            s.feed(format!("l{i}\r\n").as_bytes());
        }
        assert!(s.scrollback_len() > 0, "image row scrolled into history");
        assert!(s.placement_at(0).is_some(), "anchor survives the viewport");
        let lines = s.lines(LineOpts {
            include_scrollback: true,
            tail: None,
            images: true,
        });
        assert!(
            lines.iter().take(2).any(|l| l.image.is_some()),
            "read --images surfaces images from scrollback"
        );
        // Crossing the cap evicts the row — the image goes with it.
        s.max_scrollback = 3;
        for i in 10..20 {
            s.feed(format!("m{i}\r\n").as_bytes());
        }
        assert!(
            s.placements.is_empty(),
            "evicted history takes its image with it"
        );
    }

    #[test]
    fn alt_screen_keeps_primary_placements() {
        let mut s = Screen::new(40, 6);
        s.feed(b"\x1b_Gf=32,s=1,v=1,i=1,q=1;AAAAAAAAAAA\x1b\\");
        s.feed(b"\x1b_Ga=p,i=1,C=2,R=2,q=1\x1b\\");
        s.feed(b"\x1b[?1049h");
        assert!(s.placements().is_empty(), "alt screen starts image-free");
        s.feed(b"\x1b[?1049l");
        assert!(
            s.placement_at(0).is_some(),
            "primary placement survives the alt roundtrip"
        );
    }

    #[test]
    fn resize_keeps_placement_on_content() {
        let mut s = Screen::new(40, 6);
        s.feed(b"\x1b_Gf=32,s=1,v=1,i=1,q=1;AAAAAAAAAAA\x1b\\");
        s.feed(b"\x1b_Ga=p,i=1,C=2,R=2,q=1\x1b\\");
        s.resize(60, 6);
        let p = s.placement_at(0).expect("re-anchored after widen");
        assert_eq!(p.row, 0);
        s.resize(20, 6);
        assert_eq!(
            s.placements().len(),
            1,
            "placement survives the narrow reflow"
        );
        s.resize(40, 6);
        assert_eq!(s.placements().len(), 1);
        assert_eq!(s.placement_at(0).map(|p| p.row), Some(0));
    }

    #[test]
    fn viewport_rows_select_scrollback_and_isolate_alt_screen() {
        let mut s = Screen::new(12, 3);
        for i in 0..8 {
            s.feed(format!("history-{i}\r\n").as_bytes());
        }
        assert!(s.scrollback_len() > 2);
        let live = s.viewport_rows(0);
        let history = s.viewport_rows(2);
        assert_eq!(live.len(), 3);
        assert_eq!(history.len(), 3);
        assert_ne!(live[0].cells[8].ch, history[0].cells[8].ch);
        assert_eq!(history[0].index, (s.scrollback_len() - 2) as i64);

        s.feed(b"\x1b[?1049h");
        let alt = s.viewport_rows(usize::MAX);
        assert_eq!(alt.len(), 3);
        assert_eq!(alt[0].index, 0);
    }
}
