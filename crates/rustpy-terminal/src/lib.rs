//! RustPy IDE — embedded terminal: PTY-backed shell with VT100/xterm
//! emulation (roadmap §6 "Terminal").

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use portable_pty::{MasterPty, PtySize};

pub use vt100;
pub mod bell;
pub mod integration;
pub mod session;

/// A key press translated to terminal input bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    Insert,
    F(u8),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// Encodes a key press as the byte sequence an xterm would send.
pub fn key_to_bytes(key: Key, mods: Mods, app_cursor: bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    if mods.alt
        && matches!(
            key,
            Key::Char(_) | Key::Enter | Key::Backspace | Key::Tab | Key::Escape
        )
    {
        out.push(0x1b);
    }
    let arrow = |letter: u8, out: &mut Vec<u8>| {
        if mods.ctrl || mods.shift || mods.alt {
            let m = 1 + (mods.shift as u8) + (mods.alt as u8) * 2 + (mods.ctrl as u8) * 4;
            out.extend_from_slice(format!("\x1b[1;{m}{}", letter as char).as_bytes());
        } else if app_cursor {
            out.extend_from_slice(&[0x1b, b'O', letter]);
        } else {
            out.extend_from_slice(&[0x1b, b'[', letter]);
        }
    };
    match key {
        Key::Char(c) => {
            if mods.ctrl {
                let lower = c.to_ascii_lowercase();
                if lower.is_ascii_lowercase() {
                    out.push(lower as u8 - b'a' + 1);
                } else {
                    match c {
                        '[' | '3' => out.push(0x1b),
                        '\\' | '4' => out.push(0x1c),
                        ']' | '5' => out.push(0x1d),
                        '^' | '6' => out.push(0x1e),
                        '_' | '7' | '/' => out.push(0x1f),
                        ' ' | '2' | '@' => out.push(0),
                        _ => return None,
                    }
                }
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
        Key::Enter => out.push(b'\r'),
        Key::Backspace => out.push(if mods.ctrl { 0x08 } else { 0x7f }),
        Key::Tab => {
            if mods.shift {
                out.extend_from_slice(b"\x1b[Z");
            } else {
                out.push(b'\t');
            }
        }
        Key::Escape => out.push(0x1b),
        Key::Up => arrow(b'A', &mut out),
        Key::Down => arrow(b'B', &mut out),
        Key::Right => arrow(b'C', &mut out),
        Key::Left => arrow(b'D', &mut out),
        Key::Home => arrow(b'H', &mut out),
        Key::End => arrow(b'F', &mut out),
        Key::PageUp | Key::PageDown | Key::Delete | Key::Insert => {
            let code = match key {
                Key::PageUp => 5,
                Key::PageDown => 6,
                Key::Delete => 3,
                _ => 2,
            };
            let modifier =
                1 + u8::from(mods.shift) + u8::from(mods.alt) * 2 + u8::from(mods.ctrl) * 4;
            out.extend_from_slice(
                if modifier > 1 {
                    format!("\x1b[{code};{modifier}~")
                } else {
                    format!("\x1b[{code}~")
                }
                .as_bytes(),
            );
        }
        Key::F(n) => {
            if mods.ctrl || mods.alt || mods.shift {
                let modifier =
                    1 + u8::from(mods.shift) + u8::from(mods.alt) * 2 + u8::from(mods.ctrl) * 4;
                let sequence = match n {
                    1..=4 => format!("\x1b[1;{modifier}{}", (b'P' + n - 1) as char),
                    5..=12 => {
                        let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
                        format!("\x1b[{code};{modifier}~")
                    }
                    _ => return None,
                };
                out.extend_from_slice(sequence.as_bytes());
                return Some(out);
            }
            let seq: &[u8] = match n {
                1 => b"\x1bOP",
                2 => b"\x1bOQ",
                3 => b"\x1bOR",
                4 => b"\x1bOS",
                5 => b"\x1b[15~",
                6 => b"\x1b[17~",
                7 => b"\x1b[18~",
                8 => b"\x1b[19~",
                9 => b"\x1b[20~",
                10 => b"\x1b[21~",
                11 => b"\x1b[23~",
                12 => b"\x1b[24~",
                _ => return None,
            };
            out.extend_from_slice(seq);
        }
    }
    Some(out)
}

/// Rows of scrollback kept above the live screen.
pub const SCROLLBACK_LINES: usize = 10_000;

/// A cell position in the whole terminal buffer: `line` counts rows from the
/// oldest scrollback row (0) down through the live screen, so it does not
/// change when the view is scrolled; `col` is a cell column (a caret position
/// in `0..=cols` when used as a range bound).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BufferPos {
    pub line: usize,
    pub col: u16,
}

impl BufferPos {
    pub const fn new(line: usize, col: u16) -> Self {
        Self { line, col }
    }
}

/// Number of rows currently held in the scrollback above the live screen
/// (always 0 on the alternate screen). The view's scroll offset is kept.
pub fn history_len(screen: &mut vt100::Screen) -> usize {
    let offset = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let len = screen.scrollback();
    screen.set_scrollback(offset);
    len
}

/// Plain text between two buffer positions (`end` exclusive), e.g. a mouse
/// selection that may reach into the scrollback. Rows the terminal wrapped
/// are joined without a newline; trailing blanks are dropped where a
/// selected row runs to its end. The view's scroll offset is restored.
pub fn buffer_text(screen: &mut vt100::Screen, start: BufferPos, end: BufferPos) -> String {
    let mut out = String::new();
    if end <= start {
        return out;
    }
    let saved = screen.scrollback();
    let hist = history_len(screen);
    let (rows, cols) = screen.size();
    let total = hist + usize::from(rows);
    let mut offset = saved;
    let last = end.line.min(total.saturating_sub(1));
    for line in start.line..=last {
        // Scroll just enough that `line` is on screen, then read it as a visible row.
        let top = hist - offset;
        if line < top || line >= top + usize::from(rows) {
            offset = hist.saturating_sub(line);
            screen.set_scrollback(offset);
        }
        let row = (line - (hist - offset)) as u16;
        let c0 = if line == start.line {
            start.col.min(cols)
        } else {
            0
        };
        let c1 = if line == end.line {
            end.col.min(cols)
        } else {
            cols
        };
        let wrapped = screen.row_wrapped(row);
        let text = screen.contents_between(row, c0, row, c1);
        if line == last {
            out.push_str(if c1 >= cols && !wrapped {
                text.trim_end()
            } else {
                &text
            });
        } else if wrapped {
            out.push_str(&text);
        } else {
            out.push_str(text.trim_end());
            out.push('\n');
        }
    }
    screen.set_scrollback(saved);
    out
}

/// The default interactive shell for this platform.
pub fn default_shell() -> String {
    if cfg!(windows) {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
    } else {
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                if Path::new("/bin/bash").exists() {
                    "/bin/bash".into()
                } else {
                    "/bin/sh".into()
                }
            })
    }
}

struct Retirement {
    stop: AtomicBool,
    complete: AtomicBool,
    stamp: Mutex<String>,
}
struct RetainedWaiter {
    state: Arc<Retirement>,
    thread: std::thread::JoinHandle<()>,
}
static RETIRING: Mutex<Vec<RetainedWaiter>> = Mutex::new(Vec::new());

/// Poll dropped terminal owners, including hidden tabs. Never joins a live worker.
pub fn poll_retiring_terminals() -> Vec<String> {
    let mut owners = RETIRING.lock();
    let mut pending = Vec::new();
    let mut index = 0;
    while index < owners.len() {
        if owners[index].state.complete.load(Ordering::Acquire)
            && owners[index].thread.is_finished()
        {
            let _ = owners.swap_remove(index).thread.join();
        } else {
            pending.push(format!(
                "{}:stop-requested={}:complete={}",
                owners[index].state.stamp.lock(),
                owners[index].state.stop.load(Ordering::Acquire),
                owners[index].state.complete.load(Ordering::Acquire)
            ));
            index += 1;
        }
    }
    pending
}

/// A terminal session whose PTY retirement is supervised off the UI thread.
pub struct Terminal {
    pub scrollback_limit: usize,
    waiter: Option<std::thread::JoinHandle<()>>,
    retirement: Arc<Retirement>,
    pid: Option<u32>,
    tracker: Arc<Mutex<integration::Tracker>>,
    pub cwd: std::path::PathBuf,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    exited: Arc<AtomicBool>,
    pub cols: u16,
    pub rows: u16,
    pub shell: String,
    /// Incremented whenever new output arrived (for repaint scheduling).
    generation: Arc<std::sync::atomic::AtomicU64>,
}

impl Terminal {
    /// Spawns `shell` (or the default) in a new PTY.
    pub fn spawn(
        shell: Option<&str>,
        cwd: &Path,
        env: &[(String, String)],
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<Terminal> {
        let shell = shell.map(str::to_owned).unwrap_or_else(default_shell);
        let args: Vec<String> = if !cfg!(windows)
            && (shell.ends_with("bash") || shell.ends_with("zsh") || shell.ends_with("fish"))
        {
            vec!["-l".into()]
        } else {
            Vec::new()
        };
        Self::spawn_command(&shell, &args, cwd, env, cols, rows)
    }

    /// Spawns an arbitrary program (e.g. `python -i`) in a new PTY.
    pub fn spawn_command(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
        cols: u16,
        rows: u16,
    ) -> anyhow::Result<Terminal> {
        Self::spawn_configured(program, args, cwd, env, cols, rows, SCROLLBACK_LINES)
    }

    pub fn spawn_configured(
        program: &str,
        args: &[String],
        cwd: &Path,
        env: &[(String, String)],
        cols: u16,
        rows: u16,
        scrollback: usize,
    ) -> anyhow::Result<Terminal> {
        anyhow::ensure!(cols > 0 && rows > 0, "Terminal dimensions must be positive");
        let shell = program.to_string();
        let parts = rustpy_run::pty_owner::OwnedPty::spawn(
            program,
            args,
            cwd,
            env,
            PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            },
        )?;
        let owner = parts.owner;
        let mut child = parts.child;
        let pid = child.process_id();
        let mut reader = parts.reader;
        let writer = parts.writer;
        let master = Arc::new(Mutex::new(parts.master));
        let parser = Arc::new(Mutex::new(vt100::Parser::new(
            rows,
            cols,
            scrollback.clamp(100, 1_000_000),
        )));
        let tracker = Arc::new(Mutex::new(integration::Tracker::default()));
        let exited = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let reader_thread = {
            let parser = parser.clone();
            let tracker = tracker.clone();
            let exited = exited.clone();
            let generation = generation.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::BrokenPipe
                                // Unix PTY masters report EIO after the last slave closes.
                                || cfg!(unix) && error.raw_os_error() == Some(5) =>
                        {
                            break
                        }
                        Err(_) => return,
                        Ok(n) => {
                            tracker.lock().process(&buf[..n]);
                            parser.lock().process(&buf[..n]);
                            generation.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                exited.store(true, Ordering::Release);
                generation.fetch_add(1, Ordering::Relaxed);
            })
        };
        let retirement = Arc::new(Retirement {
            stop: AtomicBool::new(false),
            complete: AtomicBool::new(false),
            stamp: Mutex::new(owner.stamp()),
        });
        let waiter = {
            let state = retirement.clone();
            let master = master.clone();
            let drained = exited.clone();
            let generation = generation.clone();
            std::thread::spawn(move || {
                let mut reaped = false;
                loop {
                    if state.stop.load(Ordering::Acquire) {
                        owner.request_stop();
                    }
                    if !reaped {
                        match owner.poll(child.as_mut()) {
                            Ok(Some(_)) => reaped = true,
                            Ok(None) => {}
                            Err(_) => owner.request_stop(),
                        }
                        *state.stamp.lock() = owner.stamp();
                    }
                    if reaped && reader_thread.is_finished() {
                        // Both physical retirement and pipe consumption precede
                        // completion. A reader panic cannot falsely acknowledge drain.
                        if reader_thread.join().is_err() || !drained.load(Ordering::Acquire) {
                            state.stamp.lock().push_str(":reader-drain-error=true");
                            return;
                        }
                        drop(master);
                        state.complete.store(true, Ordering::Release);
                        generation.fetch_add(1, Ordering::Relaxed);
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            })
        };
        Ok(Terminal {
            scrollback_limit: scrollback.clamp(100, 1_000_000),
            waiter: Some(waiter),
            retirement,
            pid,
            tracker,
            cwd: cwd.to_path_buf(),
            parser,
            writer,
            master,
            exited,
            cols,
            rows,
            shell,
            generation,
        })
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    pub fn metadata(&self) -> integration::Metadata {
        self.tracker.lock().metadata.clone()
    }
    pub fn note_command(&self, command: &str) {
        self.tracker.lock().command(command);
    }
    pub fn take_clipboard(&self) -> Option<String> {
        self.tracker.lock().metadata.clipboard.take()
    }
    pub fn current_directory(&self) -> std::path::PathBuf {
        self.tracker
            .lock()
            .metadata
            .cwd
            .clone()
            .unwrap_or_else(|| self.cwd.clone())
    }
    pub fn clear(&self) {
        clear_buffer(&mut self.parser.lock(), self.scrollback_limit);
        self.generation.fetch_add(1, Ordering::Relaxed);
    }
    pub fn all_text(&self) -> String {
        let mut parser = self.parser.lock();
        let screen = parser.screen_mut();
        let history = history_len(screen);
        let (rows, cols) = screen.size();
        buffer_text(
            screen,
            BufferPos::new(0, 0),
            BufferPos::new(history + usize::from(rows) - 1, cols),
        )
    }
    pub fn find(&self, query: &str) -> Vec<BufferPos> {
        find_buffer(self.parser.lock().screen_mut(), query)
    }
    pub fn reveal(&self, position: BufferPos) {
        let mut parser = self.parser.lock();
        let screen = parser.screen_mut();
        let hist = history_len(screen);
        screen.set_scrollback(hist.saturating_sub(position.line));
    }
    pub fn process_id(&self) -> Option<u32> {
        self.pid
    }

    /// Sends raw bytes to the shell.
    pub fn write(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    pub fn send_key(&mut self, key: Key, mods: Mods) {
        let app_cursor = self.parser.lock().screen().application_cursor();
        if let Some(bytes) = key_to_bytes(key, mods, app_cursor) {
            self.write(&bytes);
        }
    }
    pub fn mouse(&mut self, button: u8, col: u16, row: u16, pressed: bool, motion: bool) {
        let encoding = self.with_screen(|s| s.mouse_protocol_encoding());
        let code = button + if motion { 32 } else { 0 };
        if encoding == vt100::MouseProtocolEncoding::Sgr {
            self.write(
                format!(
                    "\x1b[<{code};{col};{row}{}",
                    if pressed { 'M' } else { 'm' }
                )
                .as_bytes(),
            );
        } else {
            let code = if pressed { code } else { 3 };
            if encoding == vt100::MouseProtocolEncoding::Utf8 {
                let text = [
                    char::from(code + 32),
                    char::from_u32(u32::from(col) + 32).unwrap_or(' '),
                    char::from_u32(u32::from(row) + 32).unwrap_or(' '),
                ]
                .into_iter()
                .collect::<String>();
                self.write(b"\x1b[M");
                self.write(text.as_bytes());
            } else if col <= 223 && row <= 223 {
                self.write(&[27, b'[', b'M', code + 32, col as u8 + 32, row as u8 + 32]);
            }
        }
    }

    /// Whether the running application enabled bracketed paste (multi-line
    /// pastes are then not executed line by line).
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.parser.lock().screen().bracketed_paste()
    }

    /// Pastes text (bracketed paste when the application asked for it).
    pub fn paste(&mut self, text: &str) {
        let bracketed = self.parser.lock().screen().bracketed_paste();
        if bracketed {
            self.write(b"\x1b[200~");
            self.write(text.as_bytes());
            self.write(b"\x1b[201~");
        } else {
            self.write(text.replace('\n', "\r").as_bytes());
        }
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if cols == self.cols && rows == self.rows || cols == 0 || rows == 0 {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.parser.lock().screen_mut().set_size(rows, cols);
        let _ = self.master.lock().resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Runs `f` with the current screen.
    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        let p = self.parser.lock();
        f(p.screen())
    }

    /// Runs `f` with the current screen and the number of scrollback rows
    /// above it, read consistently under one lock (see [`BufferPos`]).
    pub fn with_buffer<R>(&self, f: impl FnOnce(&vt100::Screen, usize) -> R) -> R {
        let mut p = self.parser.lock();
        let hist = history_len(p.screen_mut());
        f(p.screen(), hist)
    }

    /// Plain text between two buffer positions (see [`buffer_text`]).
    pub fn text_between(&self, start: BufferPos, end: BufferPos) -> String {
        buffer_text(self.parser.lock().screen_mut(), start, end)
    }

    /// Scrolls the viewport into the scrollback (`delta` lines, positive = older).
    pub fn scroll(&self, delta: i32) {
        let mut p = self.parser.lock();
        let cur = p.screen().scrollback() as i32;
        let next = (cur + delta).max(0) as usize;
        p.screen_mut().set_scrollback(next);
    }

    pub fn scroll_to_bottom(&self) {
        self.parser.lock().screen_mut().set_scrollback(0);
    }

    pub fn is_alive(&mut self) -> bool {
        !self.retirement.complete.load(Ordering::Acquire)
    }

    pub fn kill(&mut self) {
        self.retirement.stop.store(true, Ordering::Release);
    }
    pub fn lifecycle_stamp(&self) -> String {
        format!(
            "{}:reader-drained={}:stop-requested={}:complete={}",
            self.retirement.stamp.lock(),
            self.exited.load(Ordering::Acquire),
            self.retirement.stop.load(Ordering::Acquire),
            self.retirement.complete.load(Ordering::Acquire)
        )
    }

    /// Title for the tab: the shell's base name (OSC titles are not tracked by vt100).
    pub fn title(&self) -> String {
        let title = self.tracker.lock().metadata.title.clone();
        if !title.is_empty() {
            return title;
        }
        Path::new(&self.shell)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.shell.clone())
    }

    /// Plain-text contents of the visible screen (for tests / copy).
    pub fn contents(&self) -> String {
        self.parser.lock().screen().contents()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.kill();
        if let Some(thread) = self.waiter.take() {
            RETIRING.lock().push(RetainedWaiter {
                state: self.retirement.clone(),
                thread,
            });
        }
    }
}

pub fn clear_buffer(parser: &mut vt100::Parser, scrollback: usize) {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let modes = screen.input_mode_formatted();
    let attrs = screen.attributes_formatted();
    let alternate = screen.alternate_screen();
    *parser = vt100::Parser::new(rows, cols, scrollback);
    if alternate {
        parser.process(b"\x1b[?1049h");
    }
    parser.process(&modes);
    parser.process(&attrs);
}

pub fn find_buffer(screen: &mut vt100::Screen, query: &str) -> Vec<BufferPos> {
    if query.is_empty() {
        return Vec::new();
    }
    let saved = screen.scrollback();
    let history = history_len(screen);
    let (rows, cols) = screen.size();
    let mut text = String::new();
    let mut starts = Vec::new();
    for line in 0..history + usize::from(rows) {
        screen.set_scrollback(history.saturating_sub(line));
        let row = (line - (history - screen.scrollback())) as u16;
        starts.push((text.len(), line));
        let contents = screen.contents_between(row, 0, row, cols);
        text.push_str(if screen.row_wrapped(row) {
            &contents
        } else {
            contents.trim_end()
        });
        if !screen.row_wrapped(row) {
            text.push('\n');
        }
    }
    screen.set_scrollback(saved);
    text.match_indices(query)
        .take(10000)
        .map(|(byte, _)| {
            let index = starts
                .partition_point(|(start, _)| *start <= byte)
                .saturating_sub(1);
            let (start, line) = starts[index];
            BufferPos::new(
                line,
                text[start..byte].chars().count().min(usize::from(u16::MAX)) as u16,
            )
        })
        .collect()
}

#[cfg(test)]
mod workspace_regressions {
    use super::*;
    #[test]
    fn clearing_removes_real_scrollback_and_keeps_bracketed_paste() {
        let mut parser = vt100::Parser::new(2, 20, 100);
        parser.process(b"old\r\nolder\r\nlatest\r\n\x1b[?2004h");
        assert!(history_len(parser.screen_mut()) > 0);
        clear_buffer(&mut parser, 100);
        assert_eq!(history_len(parser.screen_mut()), 0);
        assert!(parser.screen().bracketed_paste());
        assert!(parser.screen().contents().is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn stopping_owned_terminal_reaps_leader_and_drains_reader() {
        let mut terminal = Terminal::spawn_command(
            "/bin/sh",
            &[
                "-c".into(),
                "sleep 30 & printf 'CHILD:%s\\n' \"$!\"; wait".into(),
            ],
            Path::new("/"),
            &[],
            40,
            5,
        )
        .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !terminal.contents().contains("CHILD:") && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(terminal.contents().contains("CHILD:"));
        let before = std::time::Instant::now();
        terminal.kill();
        assert!(before.elapsed() < std::time::Duration::from_secs(1));
        while terminal.is_alive() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!terminal.is_alive());
        let stamp = terminal.lifecycle_stamp();
        assert!(stamp.contains("reaped=true"));
        assert!(stamp.contains("reader-drained=true"));
    }

    #[cfg(unix)]
    #[test]
    fn dropping_terminal_retains_waiter_until_pty_retirement() {
        let terminal = Terminal::spawn_command(
            "/bin/sh",
            &["-c".into(), "sleep 30 & wait".into()],
            Path::new("/"),
            &[],
            40,
            5,
        )
        .unwrap();
        let state = terminal.retirement.clone();
        let before = std::time::Instant::now();
        drop(terminal);
        assert!(before.elapsed() < std::time::Duration::from_secs(1));
        assert!(state.stop.load(Ordering::Acquire));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !state.complete.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            poll_retiring_terminals();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(state.complete.load(Ordering::Acquire));
        assert!(state.stamp.lock().contains("reaped=true"));
        poll_retiring_terminals();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_encoding() {
        assert_eq!(
            key_to_bytes(Key::Char('a'), Mods::default(), false),
            Some(b"a".to_vec())
        );
        assert_eq!(
            key_to_bytes(
                Key::Char('c'),
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                false
            ),
            Some(vec![3])
        );
        assert_eq!(
            key_to_bytes(Key::Up, Mods::default(), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_to_bytes(Key::Up, Mods::default(), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_to_bytes(
                Key::Left,
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                false
            ),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            key_to_bytes(
                Key::Char('x'),
                Mods {
                    alt: true,
                    ..Default::default()
                },
                false
            ),
            Some(b"\x1bx".to_vec())
        );
        assert_eq!(
            key_to_bytes(Key::Enter, Mods::default(), false),
            Some(b"\r".to_vec())
        );
    }

    fn parser_with(rows: u16, cols: u16, bytes: &[u8]) -> vt100::Parser {
        let mut p = vt100::Parser::new(rows, cols, 100);
        p.process(bytes);
        p
    }

    #[test]
    fn buffer_text_reaches_into_scrollback() {
        let mut p = parser_with(3, 10, b"one\r\ntwo\r\nthree\r\nfour\r\nfive");
        let s = p.screen_mut();
        assert_eq!(history_len(s), 2, "`one` and `two` scrolled off");
        let all = buffer_text(s, BufferPos::new(0, 0), BufferPos::new(4, 10));
        assert_eq!(all, "one\ntwo\nthree\nfour\nfive");
        assert_eq!(
            buffer_text(s, BufferPos::new(1, 1), BufferPos::new(2, 3)),
            "wo\nthr"
        );
        assert_eq!(
            buffer_text(s, BufferPos::new(3, 1), BufferPos::new(3, 3)),
            "ou"
        );
        assert_eq!(
            buffer_text(s, BufferPos::new(3, 3), BufferPos::new(3, 1)),
            "",
            "reversed range"
        );
        assert_eq!(
            buffer_text(s, BufferPos::new(4, 0), BufferPos::new(9, 0)),
            "five",
            "clamped to the buffer"
        );
        assert_eq!(s.scrollback(), 0, "view offset restored");
        // The same positions address the same text while the view is scrolled back.
        s.set_scrollback(2);
        assert_eq!(
            buffer_text(s, BufferPos::new(1, 0), BufferPos::new(3, 4)),
            "two\nthree\nfour"
        );
        assert_eq!(s.scrollback(), 2);
        assert_eq!(history_len(s), 2);
        assert_eq!(s.scrollback(), 2);
    }

    #[test]
    fn buffer_text_joins_wrapped_rows_and_trims_line_ends() {
        let mut p = parser_with(4, 5, b"abcdefgh\r\nab   \r\ncd");
        let s = p.screen_mut();
        // "abcdefgh" wraps after 5 columns: one logical line over two rows.
        assert_eq!(
            buffer_text(s, BufferPos::new(0, 0), BufferPos::new(1, 5)),
            "abcdefgh"
        );
        assert_eq!(
            buffer_text(s, BufferPos::new(0, 2), BufferPos::new(3, 5)),
            "cdefgh\nab\ncd"
        );
        // A selection that stops inside a row keeps what it covers.
        assert_eq!(
            buffer_text(s, BufferPos::new(2, 0), BufferPos::new(2, 3)),
            "ab "
        );
    }

    #[test]
    fn runs_a_shell_command() {
        if cfg!(windows) || !Path::new("/bin/sh").exists() {
            return;
        }
        let mut t = Terminal::spawn(Some("/bin/sh"), Path::new("/"), &[], 80, 24).expect("spawn");
        t.write(b"echo RUSTPY_OK_$((1+1))\n");
        let start = std::time::Instant::now();
        while !t.contents().contains("RUSTPY_OK_2") && start.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        assert!(
            t.contents().contains("RUSTPY_OK_2"),
            "screen: {}",
            t.contents()
        );
        t.resize(100, 30);
        assert_eq!(t.cols, 100);
        t.write(b"exit\n");
        let start = std::time::Instant::now();
        while t.is_alive() && start.elapsed().as_secs() < 10 {
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        assert!(!t.is_alive());
    }
}
#[cfg(test)]
mod modified_key_fixtures {
    use super::*;
    #[test]
    fn alt_arrows_and_modified_function_keys_have_one_control_sequence() {
        assert_eq!(
            key_to_bytes(
                Key::Up,
                Mods {
                    alt: true,
                    ..Default::default()
                },
                false
            ),
            Some(b"\x1b[1;3A".to_vec())
        );
        assert_eq!(
            key_to_bytes(
                Key::F(5),
                Mods {
                    ctrl: true,
                    ..Default::default()
                },
                false
            ),
            Some(b"\x1b[15;5~".to_vec())
        );
        assert_eq!(
            key_to_bytes(
                Key::PageDown,
                Mods {
                    shift: true,
                    ..Default::default()
                },
                false
            ),
            Some(b"\x1b[6;2~".to_vec())
        );
    }
}
#[cfg(test)]
mod search_regressions {
    use super::*;
    #[test]
    fn wrapped_search_keeps_physical_rows_and_scroll_position() {
        let mut parser = vt100::Parser::new(3, 5, 100);
        parser.process(b"12345abcdeXYZ\r\nlast\r\n");
        parser.screen_mut().set_scrollback(1);
        let saved = parser.screen().scrollback();
        let results = find_buffer(parser.screen_mut(), "deXYZ");
        assert_eq!(results, vec![BufferPos::new(1, 3)]);
        assert_eq!(parser.screen().scrollback(), saved);
    }
}
