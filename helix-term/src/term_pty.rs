//! PTY-backed terminal buffers.
//!
//! A terminal buffer is a normal `Document`/`View` like any other: its rope
//! is kept in sync with a child process's PTY output instead of a file. This
//! module owns the PTY/child process and the `vt100` screen state; it never
//! touches the compositor, so a terminal buffer gets bufferline, splits, and
//! buffer-next/previous for free from the existing document machinery.
//!
//! Sessions are tracked in a global registry keyed by `DocumentId` (the same
//! pattern `job::JOB_QUEUE` uses for the job-dispatch channel) rather than
//! threaded through `Application`, since the PTY needs to be reachable from
//! both command handlers and the key-routing code in `ui/editor.rs`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use helix_view::document::Mode;
use helix_view::editor::Action;
use helix_view::input::{KeyCode, KeyEvent, KeyModifiers};
use helix_view::{DocumentId, ViewId};

use crate::job;

/// Scrollback kept by the vt100 parser, in lines, beyond the visible screen.
const SCROLLBACK_LINES: usize = 10_000;

pub struct PtySession {
    doc_id: DocumentId,
    view_id: ViewId,
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Detects terminal capability queries (DA/DSR) that `vt100` silently
    /// drops, so we can answer them instead of the child program hanging.
    query_detector: Mutex<(vte::Parser, QueryResponder)>,
    /// Set whenever the parser processes new bytes, cleared by the ticker
    /// thread once it flushes to the document. A fixed-cadence ticker (not
    /// "refresh after each read") is what actually guarantees delivery: a
    /// refresh keyed off read arrivals can throttle away the *last* chunk of
    /// a burst with nothing left to trigger the deferred flush once the
    /// child goes quiet (e.g. a shell that draws its prompt right after
    /// startup output, then just waits for input) — the prompt would then
    /// only show up once some *later* read happened to come in.
    dirty: AtomicBool,
    /// Last plain character written (outside of any escape sequence), for
    /// expanding ECMA-48 REP (see `preprocess`).
    last_char: Mutex<Option<char>>,
    /// Whether the buffer has been switched to/shown yet. `doc_id` is
    /// created but deliberately not displayed anywhere at spawn time —
    /// `refresh_document` reveals it the first time there's real content to
    /// show, so the child process's own startup latency (shell fork/exec,
    /// its own init) never flashes an empty buffer first.
    revealed: AtomicBool,
}

/// Whether terminal buffers hide the line-number gutter (default: yes — a
/// full-width blank/loading terminal reads as "a terminal", whereas one
/// with Helix's usual gutter furniture reads as "an empty file").
static HIDE_GUTTER: AtomicBool = AtomicBool::new(true);

pub fn hide_gutter() -> bool {
    HIDE_GUTTER.load(Ordering::Relaxed)
}

pub fn set_hide_gutter(hide: bool) {
    HIDE_GUTTER.store(hide, Ordering::Relaxed);
}

/// Minimal `vte::Perform` that only cares about the handful of CSI queries
/// an interactive shell is likely to issue at startup, run over the same raw
/// bytes fed to the main `vt100::Parser`. Everything else is a no-op via the
/// trait's defaults.
#[derive(Default)]
struct QueryResponder {
    cursor_row: u16,
    cursor_col: u16,
    responses: Vec<u8>,
}

impl vte::Perform for QueryResponder {
    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], _ignore: bool, c: char) {
        match (intermediates.first(), c) {
            // Primary Device Attributes: "what kind of terminal are you?"
            // fish (among others) blocks for ~10s waiting on this before
            // giving up and disabling the features that depend on it.
            (None, 'c') => self.responses.extend_from_slice(b"\x1b[?6c"),
            // Device Status Report.
            (None, 'n') => {
                let code = params
                    .iter()
                    .next()
                    .and_then(|p| p.first())
                    .copied()
                    .unwrap_or(0);
                match code {
                    5 => self.responses.extend_from_slice(b"\x1b[0n"),
                    6 => {
                        let report =
                            format!("\x1b[{};{}R", self.cursor_row + 1, self.cursor_col + 1);
                        self.responses.extend_from_slice(report.as_bytes());
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

static TERMINALS: Lazy<Mutex<HashMap<DocumentId, Arc<PtySession>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Look up the live PTY session backing a terminal buffer, if `doc_id` is one.
pub fn get(doc_id: DocumentId) -> Option<Arc<PtySession>> {
    TERMINALS.lock().get(&doc_id).cloned()
}

pub fn is_terminal(doc_id: DocumentId) -> bool {
    TERMINALS.lock().contains_key(&doc_id)
}

/// Kill the child process and drop the PTY for a closed terminal buffer.
/// Wired to the `DocumentDidClose` event in `events.rs` so it fires
/// regardless of which command path closed the buffer.
pub fn cleanup(doc_id: DocumentId) {
    if let Some(session) = TERMINALS.lock().remove(&doc_id) {
        let _ = session.child.lock().kill();
    }
}

impl PtySession {
    /// Spawn a PTY sized `rows`x`cols` and register it as the terminal
    /// session backing `doc_id` (displayed in `view_id`). A background
    /// thread feeds PTY output into a vt100 screen and re-flattens it into
    /// the document's rope on every read.
    ///
    /// `command`, if given, is run as `shell -c command` instead of an
    /// interactive shell — this is how plugins (lazygit.hx, sidekick.hx,
    /// ...) embed a specific program (`exec lazygit`, an AI CLI, ...) as a
    /// terminal buffer via `term-buffer-spawn!` rather than opening a shell
    /// the user has to type a command into themselves. `shell` defaults to
    /// `$SHELL` when not given; plugins whose command uses syntax that
    /// isn't portable across shells (e.g. POSIX `(...)` subshell grouping,
    /// which fish doesn't understand the same way) can pin one explicitly
    /// via `term-buffer-spawn-with-shell!` instead of guessing what the
    /// user's login shell is.
    pub fn spawn(
        doc_id: DocumentId,
        view_id: ViewId,
        rows: u16,
        cols: u16,
        command: Option<&str>,
        shell: Option<&str>,
    ) -> anyhow::Result<()> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;

        let mut cmd = match command {
            // `new_default_prog` builds a special marker that resolves the
            // shell at spawn time and explicitly forbids `.arg()` (panics
            // if called) — fine for a bare interactive shell, but a custom
            // command needs `shell -c command`, so resolve the shell path
            // ourselves the same way `new_default_prog` would.
            Some(command) => {
                let shell = shell.map(str::to_string).unwrap_or_else(|| {
                    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
                });
                let mut cmd = CommandBuilder::new(shell);
                cmd.arg("-c");
                cmd.arg(command);
                cmd
            }
            None => CommandBuilder::new_default_prog(),
        };
        // `vt100` only understands a basic xterm-ish subset. Inheriting
        // hx's own $TERM (e.g. tmux-256color, or whatever the host terminal
        // reports) advertises features vt100 can't parse, which is enough
        // to garble full-screen apps like vim; pin the child to a baseline
        // vt100 is actually able to keep up with.
        cmd.env("TERM", "xterm-256color");
        cmd.cwd(helix_stdx::env::current_working_dir());
        let child = pair.slave.spawn_command(cmd)?;
        // Our handle to the slave side only needs to live long enough to hand
        // it to the child; the child's own copy (stdin/stdout/stderr) is what
        // keeps the pty alive, and dropping ours here lets the master side see
        // EOF when the child actually exits instead of when we happen to.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK_LINES)));

        let session = Arc::new(Self {
            doc_id,
            view_id,
            master: Mutex::new(pair.master),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            parser,
            query_detector: Mutex::new((vte::Parser::new(), QueryResponder::default())),
            dirty: AtomicBool::new(false),
            last_char: Mutex::new(None),
            revealed: AtomicBool::new(false),
        });

        TERMINALS.lock().insert(doc_id, session.clone());

        std::thread::Builder::new()
            .name("term-pty-reader".to_string())
            .spawn({
                let session = session.clone();
                move || session.read_loop(reader)
            })?;

        // Flushes to the document on a fixed cadence rather than "after each
        // read": decoupling from read timing is what guarantees delivery
        // (see the `dirty` field doc comment). Exits on its own once the
        // session is dropped (weak ref fails to upgrade), so it doesn't need
        // to be told about the child exiting or the buffer closing.
        let ticker_session = Arc::downgrade(&session);
        std::thread::Builder::new()
            .name("term-pty-ticker".to_string())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_millis(16));
                let Some(session) = ticker_session.upgrade() else {
                    break;
                };
                if session.dirty.swap(false, Ordering::AcqRel) {
                    session.refresh_document();
                }
            })?;

        Ok(())
    }

    /// Patch up CSI sequences `vt100` handles incorrectly or not at all,
    /// before handing bytes to it. Operates on raw bytes (walking the
    /// ECMA-48 CSI grammar directly: `ESC [ params intermediates final`)
    /// rather than going through a `vte::Parser` + `Perform` pass, so that
    /// every sequence this doesn't specifically special-case can be copied
    /// through byte-for-byte — no risk of subtly mis-reconstructing a
    /// sequence from parsed params/intermediates.
    ///
    /// Two rewrites, both found by diffing btop's raw output against
    /// `vt100`'s CSI dispatch table (`vt100-0.15.2/src/screen.rs`):
    ///
    /// - `CSI Pb` (REP, "repeat the preceding character P times") has no
    ///   handler in `vt100` at all, so it's expanded here into literal
    ///   repeated characters.
    /// - `CSI row;col f` (HVP) is functionally identical to `CSI row;col H`
    ///   (CUP), but `vt100` only implements `H`; every `f` cursor move was
    ///   silently dropped, desyncing the cursor from where btop thought it
    ///   had positioned it and scrambling everything drawn after. This was
    ///   the dominant cause of btop looking scrambled (648 `f` sequences in
    ///   one capture vs. zero `b`/REP sequences) — rewritten to `H` here.
    fn preprocess(&self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut last_char = self.last_char.lock();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
                let seq_start = i;
                let mut j = i + 2;
                // Parameter bytes: 0x30-0x3F (digits, `;`, `:`, and the
                // private-marker prefixes `< = > ?`).
                let param_start = j;
                while j < bytes.len() && (0x30..=0x3f).contains(&bytes[j]) {
                    j += 1;
                }
                let param_end = j;
                // Intermediate bytes: 0x20-0x2F.
                while j < bytes.len() && (0x20..=0x2f).contains(&bytes[j]) {
                    j += 1;
                }
                if j < bytes.len() && (0x40..=0x7e).contains(&bytes[j]) {
                    let final_byte = bytes[j];
                    match final_byte {
                        b'b' => {
                            let count: usize = std::str::from_utf8(&bytes[param_start..param_end])
                                .ok()
                                .and_then(|s| s.parse().ok())
                                .unwrap_or(1)
                                .max(1);
                            if let Some(c) = *last_char {
                                let mut char_buf = [0u8; 4];
                                let encoded = c.encode_utf8(&mut char_buf);
                                for _ in 0..count {
                                    out.extend_from_slice(encoded.as_bytes());
                                }
                            }
                        }
                        b'f' => {
                            out.extend_from_slice(&bytes[seq_start..j]);
                            out.push(b'H');
                        }
                        _ => out.extend_from_slice(&bytes[seq_start..=j]),
                    }
                    i = j + 1;
                    continue;
                }
                // Sequence truncated at the end of this chunk (a CSI split
                // across two reads, which is rare). Not worth reassembling
                // across reads for a cosmetic-only edge case: fall through
                // and copy the ESC byte verbatim; the rest resumes as plain
                // bytes on the next read.
            }
            let b = bytes[i];
            if b < 0x20 || b == 0x7f {
                // Control byte: passed through, doesn't count as "printed".
                out.push(b);
                i += 1;
                continue;
            }
            let char_len = utf8_len(b);
            let end = (i + char_len).min(bytes.len());
            if let Ok(s) = std::str::from_utf8(&bytes[i..end]) {
                if let Some(c) = s.chars().next() {
                    *last_char = Some(c);
                }
            }
            out.extend_from_slice(&bytes[i..end]);
            i = end;
        }
        out
    }

    fn read_loop(self: Arc<Self>, mut reader: Box<dyn Read + Send>) {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let expanded = self.preprocess(&buf[..n]);
                    self.parser.lock().process(&expanded);
                    self.respond_to_queries(&expanded);
                    self.dirty.store(true, Ordering::Release);
                }
            }
        }
        // Flush the final screen state (e.g. an "exited" message) even
        // though the ticker will also pick this up on its next tick.
        self.refresh_document();
        // Reap the child so it doesn't sit around as a zombie now that the
        // pty has seen EOF, and close the buffer to match: a terminal
        // buffer showing a dead shell isn't useful to leave open.
        let _ = self.child.lock().wait();
        let doc_id = self.doc_id;
        let view_id = self.view_id;
        job::dispatch_blocking(move |editor, _compositor| {
            // Only if the terminal's own view still has focus and is still
            // showing it — otherwise the user has since moved on to editing
            // something else, and forcing Normal mode there would yank them
            // out of unrelated typing just because a background process
            // happened to exit at that moment.
            let was_focused = editor.tree.focus == view_id
                && editor.tree.try_get(view_id).map(|v| v.doc) == Some(doc_id);
            let _ = editor.close_document(doc_id, true);
            if was_focused {
                editor.mode = Mode::Normal;
            }
        });
    }

    fn refresh_document(&self) {
        let doc_id = self.doc_id;
        let spawn_view_id = self.view_id;
        let parser = self.parser.clone();
        // Computed on whichever thread calls refresh_document (read_loop or
        // the ticker) — the atomic swap means only the first caller, ever,
        // sees `true`, so exactly one queued callback ends up doing the
        // reveal, however many refreshes happen to race into the queue
        // around the same time.
        let just_revealed = !self.revealed.swap(true, Ordering::AcqRel);
        job::dispatch_blocking(move |editor, _compositor| {
            if !editor.documents.contains_key(&doc_id) {
                return;
            }
            let view_id = if just_revealed {
                // First real content: switch to the buffer now instead of
                // at spawn time, so the child process's own startup latency
                // (shell fork/exec, its own init) never flashes an empty
                // buffer before there's something to show.
                editor.switch(doc_id, Action::Replace);
                editor.mode = Mode::Insert;
                // `switch` reveals into whichever view is *currently*
                // focused, which may not be `spawn_view_id` if the user
                // moved on before the child's first output arrived — use
                // the view it actually landed in for the cursor sync below.
                editor.tree.focus
            } else {
                spawn_view_id
            };
            let Some(doc) = editor.document_mut(doc_id) else {
                return;
            };
            let (text, cursor_row, cursor_col) = {
                let parser = parser.lock();
                let screen = parser.screen();
                // `Screen::contents()` omits the newline between rows the
                // terminal auto-wrapped into each other, merging them into
                // one oversized logical line — which breaks the "rope line
                // N is vt100 row N" invariant `cell_style` depends on, and
                // then needs re-wrapping by Helix's own renderer on top of
                // vt100's original wrap. `rows()` keeps every row separate
                // regardless of wrap state, matching the fixed grid a real
                // terminal always shows.
                let cols = screen.size().1;
                let (cursor_row, cursor_col) = screen.cursor_position();
                // Curses-style apps (vim, htop, ...) often clear/redraw a
                // row by writing literal spaces across it rather than an
                // erase-to-end-of-line sequence, which (unlike a genuinely
                // untouched cell) counts as real row content. Left in, every
                // such row would sit exactly at (or a hair past) the view's
                // width and get soft-wrapped by Helix on top of vt100's own
                // grid — trailing whitespace is invisible either way, so
                // trimming it here is a no-op for what's actually on screen.
                //
                // Exception: the cursor's own row. The cursor position sync
                // below needs an actual character at `cursor_col` to place
                // the selection on — trimming a blank/short row out from
                // under it would clamp the cursor short and it'd appear one
                // cell left of where the terminal really has it, snapping
                // to the right place only once real text pushed the trim
                // point out that far. Pad that one row back out instead.
                let text = screen
                    .rows(0, cols)
                    .enumerate()
                    .map(|(i, row)| {
                        let trimmed = row.trim_end();
                        if i as u16 == cursor_row && trimmed.chars().count() < cursor_col as usize {
                            let pad = cursor_col as usize - trimmed.chars().count();
                            format!("{trimmed}{}", " ".repeat(pad))
                        } else {
                            trimmed.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                (text, cursor_row, cursor_col)
            };
            let new_text = helix_core::Rope::from(text.as_str());
            let transaction = helix_core::diff::compare_ropes(doc.text(), &new_text);
            doc.apply(&transaction, view_id);
            // A terminal buffer is never "saved" in any meaningful sense,
            // but its content changing every frame would otherwise leave it
            // permanently `is_modified() == true` (there's nothing else to
            // commit these changes to history and clear it, since
            // `append_changes_to_history` is skipped for terminal buffers -
            // see `ui/editor.rs`) - forcing `:q`/`:qa` to need `!` for a
            // buffer the user never asked to persist in the first place.
            doc.discard_pending_changes();

            // Helix draws its (block) cursor by highlighting whatever the
            // document's selection currently points at, not via the real
            // terminal cursor — so without this, the terminal buffer's
            // cursor just sits wherever the selection was left (buffer
            // creation, typically) instead of following the shell prompt
            // or program cursor around like a real terminal would.
            let text = doc.text();
            let len_lines = text.len_lines();
            let row = (cursor_row as usize).min(len_lines.saturating_sub(1));
            let line_start = text.line_to_char(row);
            let line_end = if row + 1 < len_lines {
                text.line_to_char(row + 1).saturating_sub(1)
            } else {
                text.len_chars()
            };
            let offset = (line_start + cursor_col as usize).min(line_end);
            doc.set_selection(view_id, helix_core::Selection::point(offset));
        });
    }

    /// Scan `bytes` for terminal capability queries (DA/DSR) and write back
    /// whatever response(s) they call for, using the cursor position `vt100`
    /// just computed for this same chunk.
    fn respond_to_queries(&self, bytes: &[u8]) {
        let (row, col) = self.parser.lock().screen().cursor_position();

        let responses = {
            let mut guard = self.query_detector.lock();
            let (parser, responder) = &mut *guard;
            responder.cursor_row = row;
            responder.cursor_col = col;
            for &byte in bytes {
                parser.advance(responder, byte);
            }
            std::mem::take(&mut responder.responses)
        };

        if !responses.is_empty() {
            self.write_input(&responses);
        }
    }

    /// Whether the child program has put the terminal in application-cursor
    /// mode (DECCKM) — determines which escape prefix arrow/Home/End keys use.
    pub fn application_cursor(&self) -> bool {
        self.parser.lock().screen().application_cursor()
    }

    /// The style a real terminal would render cell `(row, col)` with (SGR
    /// colors + bold/italic/underline/inverse), or `None` for a cell with no
    /// non-default attributes (nothing to override the document's plain text
    /// style with).
    pub fn cell_style(&self, row: u16, col: u16) -> Option<helix_view::graphics::Style> {
        use helix_view::graphics::{Color, Modifier, Style, UnderlineStyle};

        let parser = self.parser.lock();
        let cell = parser.screen().cell(row, col)?;

        let to_color = |c: vt100::Color| match c {
            vt100::Color::Default => None,
            vt100::Color::Idx(i) => Some(Color::Indexed(i)),
            vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
        };

        let mut fg = to_color(cell.fgcolor());
        let mut bg = to_color(cell.bgcolor());
        if cell.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        if fg.is_none() && bg.is_none() && !cell.bold() && !cell.italic() && !cell.underline() {
            return None;
        }

        let mut style = Style {
            fg,
            bg,
            ..Style::default()
        };
        if cell.bold() {
            style = style.add_modifier(Modifier::BOLD);
        }
        if cell.italic() {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if cell.underline() {
            style.underline_style = Some(UnderlineStyle::Line);
        }
        Some(style)
    }

    /// Forward raw bytes to the child process's stdin.
    pub fn write_input(&self, bytes: &[u8]) {
        let mut writer = self.writer.lock();
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
    }

    /// Resize the pty (and the vt100 screen tracking it) to match the view.
    /// Called on every render of a terminal buffer's view, so it no-ops
    /// unless the size actually changed.
    pub fn resize(&self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 {
            return;
        }
        {
            let mut parser = self.parser.lock();
            if parser.screen().size() == (rows, cols) {
                return;
            }
            parser.set_size(rows, cols);
        }
        let _ = self.master.lock().resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.refresh_document();
    }
}

/// Encode a key event into the byte sequence a real terminal would send for
/// it, or `None` for keys with no terminal representation (e.g. plain
/// modifier keys). `application_cursor` selects the SS3 (`ESC O`) vs. CSI
/// (`ESC [`) prefix for arrow/Home/End, matching whatever the running
/// program (e.g. vim) last requested via DECCKM.
pub fn encode_key(key: &KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let KeyEvent { code, modifiers } = *key;
    let alt = modifiers.contains(KeyModifiers::ALT);

    let mut bytes = match code {
        KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) => encode_ctrl_char(c)?,
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Delete => csi_seq(b"3~"),
        KeyCode::Insert => csi_seq(b"2~"),
        KeyCode::PageUp => csi_seq(b"5~"),
        KeyCode::PageDown => csi_seq(b"6~"),
        KeyCode::Home => cursor_seq(b'H', application_cursor),
        KeyCode::End => cursor_seq(b'F', application_cursor),
        KeyCode::Up => cursor_seq(b'A', application_cursor),
        KeyCode::Down => cursor_seq(b'B', application_cursor),
        KeyCode::Right => cursor_seq(b'C', application_cursor),
        KeyCode::Left => cursor_seq(b'D', application_cursor),
        KeyCode::F(n) => function_key_seq(n),
        _ => return None,
    };

    if alt {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn encode_ctrl_char(c: char) -> Option<Vec<u8>> {
    let upper = c.to_ascii_uppercase();
    let byte = if upper.is_ascii_alphabetic() {
        (upper as u8) & 0x1f
    } else {
        match c {
            '@' | ' ' => 0,
            '[' => 0x1b,
            '\\' => 0x1c,
            ']' => 0x1d,
            '^' => 0x1e,
            '_' | '?' => 0x1f,
            _ => return None,
        }
    };
    Some(vec![byte])
}

fn cursor_seq(final_byte: u8, application_cursor: bool) -> Vec<u8> {
    vec![
        0x1b,
        if application_cursor { b'O' } else { b'[' },
        final_byte,
    ]
}

fn csi_seq(seq: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1b, b'['];
    out.extend_from_slice(seq);
    out
}

/// Byte length of the UTF-8 character starting with leading byte `b`.
fn utf8_len(b: u8) -> usize {
    if b & 0x80 == 0 {
        1
    } else if b & 0xe0 == 0xc0 {
        2
    } else if b & 0xf0 == 0xe0 {
        3
    } else if b & 0xf8 == 0xf0 {
        4
    } else {
        1
    }
}

fn function_key_seq(n: u8) -> Vec<u8> {
    match n {
        1 => b"\x1bOP".to_vec(),
        2 => b"\x1bOQ".to_vec(),
        3 => b"\x1bOR".to_vec(),
        4 => b"\x1bOS".to_vec(),
        5 => b"\x1b[15~".to_vec(),
        6 => b"\x1b[17~".to_vec(),
        7 => b"\x1b[18~".to_vec(),
        8 => b"\x1b[19~".to_vec(),
        9 => b"\x1b[20~".to_vec(),
        10 => b"\x1b[21~".to_vec(),
        11 => b"\x1b[23~".to_vec(),
        12 => b"\x1b[24~".to_vec(),
        _ => Vec::new(),
    }
}
