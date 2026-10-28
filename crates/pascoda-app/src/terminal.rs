//! The integrated terminal, without any UI: a shell on a pseudo-terminal, a `vt100` emulator
//! that follows its output on a reader thread, and the pure helpers around them (key events to
//! bytes, paste, selection text, grid size).

use eframe::egui;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::num::NonZeroU16;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};

/// Lines of history the emulator keeps above the screen.
pub const SCROLLBACK: usize = 5000;

// ---- pure helpers --------------------------------------------------------------------------

/// The user's login shell: `$SHELL` when it names an existing file, else `/bin/zsh`.
pub fn default_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty() && Path::new(s).exists())
        .unwrap_or_else(|| "/bin/zsh".into())
}

/// Rows and columns that fit in a pane of the given size, at least 2 x 10.
pub fn grid_for(width: f32, height: f32, cell_w: f32, cell_h: f32) -> (u16, u16) {
    let fit = |extent: f32, cell: f32, min: u16| -> u16 {
        if cell <= 0.0 || !extent.is_finite() {
            return min;
        }
        ((extent / cell).floor().max(0.0) as u32).clamp(u32::from(min), 1000) as u16
    };
    (fit(height, cell_h, 2), fit(width, cell_w, 10))
}

/// xterm's modifier parameter: 1 + shift (1) + alt (2) + ctrl (4).
fn modifier_param(m: egui::Modifiers) -> u8 {
    1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.ctrl)
}

/// The bytes a key press sends to the shell, or `None` when the key is not the terminal's:
/// plain printable keys (they arrive as text events), and anything with Cmd held, which belongs
/// to the menu bar and to the IDE. `app_cursor` is the application cursor-keys mode (DECCKM).
pub fn encode_key(key: egui::Key, m: egui::Modifiers, app_cursor: bool) -> Option<Vec<u8>> {
    use egui::Key::*;
    if m.command || m.mac_cmd {
        return None;
    }
    let param = modifier_param(m);
    let plain = param == 1;
    let csi = |final_byte: char| -> Vec<u8> {
        if plain {
            format!("\x1b[{final_byte}").into_bytes()
        } else {
            format!("\x1b[1;{param}{final_byte}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if plain {
            format!("\x1b[{n}~").into_bytes()
        } else {
            format!("\x1b[{n};{param}~").into_bytes()
        }
    };
    let cursor = |final_byte: char| -> Vec<u8> {
        if plain && app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            csi(final_byte)
        }
    };
    // Option+arrows move by word, as in Terminal.app; Option+Backspace deletes a word.
    let word = |left: bool| -> Vec<u8> {
        if left {
            b"\x1bb".to_vec()
        } else {
            b"\x1bf".to_vec()
        }
    };
    let only_alt = m.alt && !m.ctrl && !m.shift;
    let bytes = match key {
        Enter => vec![b'\r'],
        Tab if m.shift => b"\x1b[Z".to_vec(),
        Tab => vec![b'\t'],
        Escape => vec![0x1b],
        Backspace if m.alt => b"\x1b\x7f".to_vec(),
        Backspace if m.ctrl => vec![0x08],
        Backspace => vec![0x7f],
        Delete => tilde(3),
        Insert => tilde(2),
        PageUp => tilde(5),
        PageDown => tilde(6),
        ArrowLeft if only_alt => word(true),
        ArrowRight if only_alt => word(false),
        ArrowUp => cursor('A'),
        ArrowDown => cursor('B'),
        ArrowRight => cursor('C'),
        ArrowLeft => cursor('D'),
        Home => cursor('H'),
        End => cursor('F'),
        F1 | F2 | F3 | F4 => {
            let c = match key {
                F1 => 'P',
                F2 => 'Q',
                F3 => 'R',
                _ => 'S',
            };
            if plain {
                format!("\x1bO{c}").into_bytes()
            } else {
                format!("\x1b[1;{param}{c}").into_bytes()
            }
        }
        F5 => tilde(15),
        F6 => tilde(17),
        F7 => tilde(18),
        F8 => tilde(19),
        F9 => tilde(20),
        F10 => tilde(21),
        F11 => tilde(23),
        F12 => tilde(24),
        _ if m.ctrl && !m.alt => vec![ctrl_byte(key)?],
        _ => return None,
    };
    Some(bytes)
}

/// Ctrl+letter is the letter's code minus 64; a few punctuation keys have codes too.
fn ctrl_byte(key: egui::Key) -> Option<u8> {
    let name = key.name();
    let mut chars = name.chars();
    if let (Some(c), None) = (chars.next(), chars.next())
        && c.is_ascii_alphabetic()
    {
        return Some(c.to_ascii_uppercase() as u8 - b'A' + 1);
    }
    Some(match key {
        egui::Key::Space | egui::Key::Num2 => 0x00,
        egui::Key::OpenBracket => 0x1b,
        egui::Key::Backslash => 0x1c,
        egui::Key::CloseBracket => 0x1d,
        egui::Key::Minus | egui::Key::Slash => 0x1f,
        _ => return None,
    })
}

/// What a paste sends: the text wrapped in bracketed-paste markers when the shell asked for
/// them, else with line breaks turned into carriage returns, as typing Enter would.
pub fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    let text = text.replace("\r\n", "\n");
    if bracketed {
        // A paste must not be able to end itself early.
        let clean = text.replace("\x1b[201~", "");
        let mut out = b"\x1b[200~".to_vec();
        out.extend_from_slice(clean.as_bytes());
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}

/// What the pointer did, as a program that asked for mouse reports wants to hear it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MouseEvent {
    /// Button 0 = left, 1 = middle, 2 = right.
    Press(u8),
    Release(u8),
    /// Pointer moved with this button down.
    Drag(u8),
    /// Pointer moved with no button down.
    Move,
    WheelUp,
    WheelDown,
}

/// The report for `ev` at the 0-based cell (`col`, `row`), in the encoding the program chose,
/// or `None` when its mode does not report this kind of event (or the cell does not fit the
/// encoding). Presses and the wheel are reported in every mode; releases from `PressRelease` on,
/// drags from `ButtonMotion`, plain moves only in `AnyMotion`.
pub fn encode_mouse(
    ev: MouseEvent,
    col: u16,
    row: u16,
    m: egui::Modifiers,
    mode: vt100::MouseProtocolMode,
    enc: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    use vt100::{MouseProtocolEncoding as E, MouseProtocolMode as M};
    let wanted = match ev {
        MouseEvent::Press(_) | MouseEvent::WheelUp | MouseEvent::WheelDown => mode != M::None,
        MouseEvent::Release(_) => matches!(mode, M::PressRelease | M::ButtonMotion | M::AnyMotion),
        MouseEvent::Drag(_) => matches!(mode, M::ButtonMotion | M::AnyMotion),
        MouseEvent::Move => mode == M::AnyMotion,
    };
    if !wanted {
        return None;
    }
    let (mut code, release) = match ev {
        MouseEvent::Press(b) => (u32::from(b.min(2)), false),
        // The old encodings cannot say which button went up.
        MouseEvent::Release(b) => (
            if enc == E::Sgr {
                u32::from(b.min(2))
            } else {
                3
            },
            true,
        ),
        MouseEvent::Drag(b) => (32 + u32::from(b.min(2)), false),
        MouseEvent::Move => (35, false),
        MouseEvent::WheelUp => (64, false),
        MouseEvent::WheelDown => (65, false),
    };
    code += 4 * u32::from(m.shift) + 8 * u32::from(m.alt) + 16 * u32::from(m.ctrl);
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    match enc {
        E::Sgr => {
            Some(format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes())
        }
        E::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for v in [code + 32, x + 32, y + 32] {
                let c = char::from_u32(v)?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
        E::Default => {
            // One byte each: positions beyond 222 cannot be said.
            let b = |v: u32| u8::try_from(v + 32).ok();
            if x > 222 || y > 222 {
                return None;
            }
            Some(vec![0x1b, b'[', b'M', b(code)?, b(x)?, b(y)?])
        }
    }
}

/// Mouse-wheel movement turned into whole lines. `carry` keeps the fraction between frames.
/// Positive `delta` (scroll up in egui) gives positive lines (towards older output).
pub fn wheel_lines(carry: &mut f32, delta: f32, line_h: f32) -> i32 {
    *carry += delta / line_h.max(1.0);
    let whole = carry.trunc();
    *carry -= whole;
    whole as i32
}

/// The scroll bar's thumb for a view `rows` tall that is `offset` lines back in a history of
/// `max` lines, on a track `track` points high: `(top, height)` in points from the track's top.
pub fn thumb(max: usize, offset: usize, rows: u16, track: f32) -> (f32, f32) {
    let total = (max + usize::from(rows)).max(1) as f32;
    let height = (track * f32::from(rows) / total).clamp(24.0_f32.min(track), track);
    let offset = offset.min(max) as f32;
    // Offset 0 is the live screen, at the bottom of the track.
    let top = (track - height) * (1.0 - offset / (max.max(1) as f32));
    (top, height)
}

/// The inverse: where the thumb's centre `y` (points from the track's top) puts the view.
pub fn offset_for(y: f32, max: usize, rows: u16, track: f32) -> usize {
    let (_, height) = thumb(max, 0, rows, track);
    let span = (track - height).max(1.0);
    let frac = ((y - height / 2.0) / span).clamp(0.0, 1.0);
    ((1.0 - frac) * max as f32).round() as usize
}

/// A selected range of cells in the visible screen, as (row, column) pairs.
pub type Cell = (u16, u16);

/// The text between two cells (either order, both included), without trailing blanks per row.
pub fn selection_text(screen: &vt100::Screen, a: Cell, b: Cell) -> String {
    let (start, end) = if a <= b { (a, b) } else { (b, a) };
    let (_, cols) = screen.size();
    let end_col = end.1.saturating_add(1).min(cols.get());
    screen.contents_between(start.0, start.1, end.0, end_col)
}

// ---- the terminal -------------------------------------------------------------------------

/// Answers the emulator sends back to the shell (cursor position and device attributes), for
/// programs that ask before they draw.
pub struct Replies(Sender<Vec<u8>>);

impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let reply = match (i1, c) {
            (None, 'n') if first == 6 => {
                let (row, col) = screen.cursor_position();
                Some(format!("\x1b[{};{}R", row + 1, col + 1))
            }
            (None, 'n') if first == 5 => Some("\x1b[0n".to_string()),
            (None, 'c') if first == 0 => Some("\x1b[?1;2c".to_string()),
            _ => None,
        };
        if let Some(r) = reply {
            let _ = self.0.send(r.into_bytes());
        }
    }
}

struct Shared {
    parser: Mutex<vt100::Parser<Replies>>,
    exited: AtomicBool,
    /// Counts chunks of output, so the view can tell that the screen changed.
    generation: AtomicU64,
}

/// A running (or finished) shell. Dropping it hangs the shell up and kills it.
pub struct Terminal {
    shared: Arc<Shared>,
    master: Box<dyn MasterPty + Send>,
    input: Option<Sender<Vec<u8>>>,
    child: Box<dyn Child + Send + Sync>,
    size: (u16, u16),
    /// The user has sent input (keys, paste); the emulator's own query replies do not count.
    typed: AtomicBool,
}

fn nz(n: u16) -> NonZeroU16 {
    NonZeroU16::new(n.max(1)).unwrap_or(NonZeroU16::MIN)
}

impl Terminal {
    /// The user's shell, as a login shell, in `cwd`. `wake` is called whenever output arrived
    /// (the UI passes `ctx.request_repaint`).
    pub fn spawn_shell(
        cwd: &Path,
        rows: u16,
        cols: u16,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let mut cmd = CommandBuilder::new(default_shell());
        cmd.arg("-l");
        cmd.cwd(cwd);
        Self::spawn(cmd, rows, cols, wake)
    }

    /// Any command on a fresh pseudo-terminal. TERM is set to xterm-256color.
    pub fn spawn(
        mut cmd: CommandBuilder,
        rows: u16,
        cols: u16,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self> {
        let (rows, cols) = (rows.max(1), cols.max(1));
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(std::io::Error::other)?;
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "Pascoda");
        // An app started from Finder has no locale; shells then misdraw non-ASCII text.
        if std::env::var_os("LANG").is_none() {
            cmd.env("LANG", "en_US.UTF-8");
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(std::io::Error::other)?;
        // Only the child may hold the slave, so that its exit ends our reads.
        drop(pair.slave);
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(std::io::Error::other)?;
        let mut writer = pair.master.take_writer().map_err(std::io::Error::other)?;

        let (input, rx) = channel::<Vec<u8>>();
        let shared = Arc::new(Shared {
            parser: Mutex::new(vt100::Parser::new_with_callbacks(
                nz(rows),
                nz(cols),
                SCROLLBACK,
                Replies(input.clone()),
            )),
            exited: AtomicBool::new(false),
            generation: AtomicU64::new(0),
        });
        // Writes go through their own thread: a full pty buffer must not stop the UI.
        std::thread::spawn(move || {
            while let Ok(bytes) = rx.recv() {
                if writer
                    .write_all(&bytes)
                    .and_then(|()| writer.flush())
                    .is_err()
                {
                    break;
                }
            }
        });
        let state = shared.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut p) = state.parser.lock() {
                            p.process(&buf[..n]);
                        }
                        state.generation.fetch_add(1, Ordering::Release);
                        wake();
                    }
                }
            }
            state.exited.store(true, Ordering::Release);
            wake();
        });
        Ok(Self {
            shared,
            master: pair.master,
            input: Some(input),
            child,
            size: (rows, cols),
            typed: AtomicBool::new(false),
        })
    }

    /// Sends bytes to the shell (queued; never blocks).
    /// Reports a pointer event to the program when it asked for mouse reports. Returns whether
    /// it did (then the IDE must not also treat the event as its own, e.g. a selection).
    pub fn report_mouse(&self, ev: MouseEvent, cell: (u16, u16), m: egui::Modifiers) -> bool {
        let (mode, enc) = {
            let p = self.parser();
            (
                p.screen().mouse_protocol_mode(),
                p.screen().mouse_protocol_encoding(),
            )
        };
        if mode == vt100::MouseProtocolMode::None {
            return false;
        }
        if let Some(bytes) = encode_mouse(ev, cell.1, cell.0, m, mode, enc) {
            self.write(&bytes);
        }
        true
    }

    /// Does the program want mouse reports?
    pub fn wants_mouse(&self) -> bool {
        self.parser().screen().mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    pub fn write(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.typed.store(true, Ordering::Relaxed);
        }
        self.write_quiet(bytes);
    }

    /// Like `write`, for the IDE's own bytes (a redraw request): it does not count as the user
    /// having used the shell.
    pub fn write_quiet(&self, bytes: &[u8]) {
        if let Some(tx) = &self.input
            && !bytes.is_empty()
        {
            let _ = tx.send(bytes.to_vec());
        }
    }

    /// Whether the user has typed or pasted anything into this shell yet.
    pub fn typed(&self) -> bool {
        self.typed.load(Ordering::Relaxed)
    }

    /// Tells the pty and the emulator the new grid size; does nothing if it is unchanged.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        let (rows, cols) = (rows.max(1), cols.max(1));
        if (rows, cols) == self.size {
            return;
        }
        self.size = (rows, cols);
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        self.parser().set_size(nz(rows), nz(cols));
    }

    #[cfg(test)]
    pub fn size(&self) -> (u16, u16) {
        self.size
    }

    /// True once the shell has ended (its output is closed).
    pub fn exited(&self) -> bool {
        self.shared.exited.load(Ordering::Acquire)
    }

    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub fn pid(&self) -> Option<u32> {
        self.child.process_id()
    }

    /// The emulator, locked. Keep it short: the reader thread waits for it.
    pub fn parser(&self) -> MutexGuard<'_, vt100::Parser<Replies>> {
        self.shared
            .parser
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The visible screen as text.
    #[cfg(test)]
    pub fn screen_text(&self) -> String {
        self.parser().screen().contents()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // Closing the input ends the writer thread; hanging up is what a closed window does.
        self.input = None;
        let Some(pid) = self.child.process_id() else {
            return;
        };
        let pid = pid as i32;
        // The shell is a session leader, so its process group is its jobs too.
        // SAFETY: plain signals to a child we spawned and have not yet reaped.
        unsafe {
            libc::kill(-pid, libc::SIGHUP);
            libc::kill(pid, libc::SIGHUP);
        }
        for _ in 0..20 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // SAFETY: as above.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::kill(pid, libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Key, Modifiers};
    use std::time::{Duration, Instant};

    const NONE: Modifiers = Modifiers::NONE;

    fn enc(key: Key, m: Modifiers) -> Option<Vec<u8>> {
        encode_key(key, m, false)
    }

    #[test]
    fn the_thumb_and_its_inverse_agree() {
        // live screen: thumb at the bottom; the oldest line: at the top
        let (top, h) = thumb(100, 0, 20, 200.0);
        assert!((top + h - 200.0).abs() < 0.01 && h < 200.0);
        assert_eq!(thumb(100, 100, 20, 200.0).0, 0.0);
        // no history: the thumb fills the track
        assert_eq!(thumb(0, 0, 20, 200.0), (0.0, 200.0));
        for off in [0usize, 1, 37, 99, 100] {
            let (top, h) = thumb(100, off, 20, 200.0);
            let back = offset_for(top + h / 2.0, 100, 20, 200.0);
            assert!(back.abs_diff(off) <= 1, "{off} -> {back}");
        }
        // dragging beyond the ends clamps
        assert_eq!(offset_for(-50.0, 100, 20, 200.0), 100);
        assert_eq!(offset_for(999.0, 100, 20, 200.0), 0);
    }

    #[test]
    fn plain_keys() {
        assert_eq!(enc(Key::Enter, NONE).unwrap(), b"\r");
        assert_eq!(enc(Key::Tab, NONE).unwrap(), b"\t");
        assert_eq!(enc(Key::Tab, Modifiers::SHIFT).unwrap(), b"\x1b[Z");
        assert_eq!(enc(Key::Escape, NONE).unwrap(), b"\x1b");
        assert_eq!(enc(Key::Backspace, NONE).unwrap(), b"\x7f");
        assert_eq!(enc(Key::Delete, NONE).unwrap(), b"\x1b[3~");
        assert_eq!(enc(Key::Home, NONE).unwrap(), b"\x1b[H");
        assert_eq!(enc(Key::End, NONE).unwrap(), b"\x1b[F");
        assert_eq!(enc(Key::PageUp, NONE).unwrap(), b"\x1b[5~");
        assert_eq!(enc(Key::PageDown, NONE).unwrap(), b"\x1b[6~");
        assert_eq!(enc(Key::F1, NONE).unwrap(), b"\x1bOP");
        assert_eq!(enc(Key::F5, NONE).unwrap(), b"\x1b[15~");
        assert_eq!(enc(Key::F12, NONE).unwrap(), b"\x1b[24~");
    }

    #[test]
    fn arrows_follow_the_cursor_key_mode() {
        assert_eq!(enc(Key::ArrowUp, NONE).unwrap(), b"\x1b[A");
        assert_eq!(enc(Key::ArrowLeft, NONE).unwrap(), b"\x1b[D");
        assert_eq!(encode_key(Key::ArrowUp, NONE, true).unwrap(), b"\x1bOA");
        assert_eq!(encode_key(Key::Home, NONE, true).unwrap(), b"\x1bOH");
        // Modified arrows use the CSI form in both modes.
        assert_eq!(
            encode_key(Key::ArrowUp, Modifiers::CTRL, true).unwrap(),
            b"\x1b[1;5A"
        );
    }

    #[test]
    fn modified_keys() {
        assert_eq!(
            enc(Key::ArrowRight, Modifiers::SHIFT).unwrap(),
            b"\x1b[1;2C"
        );
        assert_eq!(enc(Key::ArrowDown, Modifiers::CTRL).unwrap(), b"\x1b[1;5B");
        assert_eq!(
            enc(Key::ArrowUp, Modifiers::CTRL | Modifiers::SHIFT).unwrap(),
            b"\x1b[1;6A"
        );
        // Option+arrows jump words.
        assert_eq!(enc(Key::ArrowLeft, Modifiers::ALT).unwrap(), b"\x1bb");
        assert_eq!(enc(Key::ArrowRight, Modifiers::ALT).unwrap(), b"\x1bf");
        assert_eq!(enc(Key::ArrowUp, Modifiers::ALT).unwrap(), b"\x1b[1;3A");
        assert_eq!(enc(Key::Backspace, Modifiers::ALT).unwrap(), b"\x1b\x7f");
        assert_eq!(enc(Key::Delete, Modifiers::CTRL).unwrap(), b"\x1b[3;5~");
    }

    #[test]
    fn control_letters() {
        assert_eq!(enc(Key::C, Modifiers::CTRL).unwrap(), vec![3]);
        assert_eq!(enc(Key::A, Modifiers::CTRL).unwrap(), vec![1]);
        assert_eq!(enc(Key::D, Modifiers::CTRL).unwrap(), vec![4]);
        assert_eq!(enc(Key::Z, Modifiers::CTRL).unwrap(), vec![26]);
        assert_eq!(enc(Key::OpenBracket, Modifiers::CTRL).unwrap(), vec![0x1b]);
        assert_eq!(enc(Key::Space, Modifiers::CTRL).unwrap(), vec![0]);
    }

    #[test]
    fn printable_keys_and_command_shortcuts_are_not_encoded() {
        // Plain letters come in as text events.
        assert!(enc(Key::A, NONE).is_none());
        assert!(enc(Key::Space, NONE).is_none());
        // Cmd shortcuts stay with the menu bar and the IDE.
        for key in [
            Key::R,
            Key::S,
            Key::C,
            Key::V,
            Key::K,
            Key::ArrowLeft,
            Key::Enter,
        ] {
            assert!(enc(key, Modifiers::COMMAND).is_none(), "{key:?}");
            assert!(enc(key, Modifiers::MAC_CMD).is_none(), "{key:?}");
        }
    }

    #[test]
    fn paste_is_bracketed_or_turns_newlines_into_returns() {
        assert_eq!(paste_bytes("ls\npwd", false), b"ls\rpwd");
        assert_eq!(paste_bytes("a\r\nb", false), b"a\rb");
        assert_eq!(paste_bytes("ls\npwd", true), b"\x1b[200~ls\npwd\x1b[201~");
        // A pasted end marker cannot leave bracketed mode early.
        assert_eq!(paste_bytes("x\x1b[201~y", true), b"\x1b[200~xy\x1b[201~");
    }

    #[test]
    fn grid_size_from_pixels() {
        assert_eq!(grid_for(800.0, 180.0, 8.0, 18.0), (10, 100));
        assert_eq!(grid_for(807.9, 197.0, 8.0, 18.0), (10, 100));
        // Never smaller than 2 x 10, never absurdly large.
        assert_eq!(grid_for(5.0, 5.0, 8.0, 18.0), (2, 10));
        assert_eq!(grid_for(1e9, 1e9, 8.0, 18.0), (1000, 1000));
        assert_eq!(grid_for(100.0, 100.0, 0.0, 0.0), (2, 10));
    }

    #[test]
    fn wheel_keeps_the_fraction() {
        let mut carry = 0.0;
        assert_eq!(wheel_lines(&mut carry, 10.0, 18.0), 0);
        assert_eq!(wheel_lines(&mut carry, 10.0, 18.0), 1);
        assert!((carry - 2.0 / 18.0).abs() < 1e-5);
        assert_eq!(wheel_lines(&mut carry, -54.0, 18.0), -2);
    }

    fn screen_of(text: &str) -> vt100::Parser {
        let mut p = vt100::Parser::new(nz(5), nz(20), 0);
        p.process(text.as_bytes());
        p
    }

    #[test]
    fn selection_text_covers_rows_in_either_direction() {
        let p = screen_of("hello world\r\nsecond line\r\nthird");
        let s = p.screen();
        assert_eq!(selection_text(s, (0, 0), (0, 4)), "hello");
        assert_eq!(selection_text(s, (0, 6), (0, 10)), "world");
        // Dragged backwards: same text.
        assert_eq!(selection_text(s, (0, 4), (0, 0)), "hello");
        assert_eq!(selection_text(s, (0, 6), (1, 5)), "world\nsecond");
        assert_eq!(
            selection_text(s, (0, 0), (2, 4)),
            "hello world\nsecond line\nthird"
        );
        // To the right edge: trailing blanks are not copied.
        assert_eq!(selection_text(s, (0, 6), (0, 19)), "world");
    }

    // ---- with a real pty ---------------------------------------------------------------

    fn sh(script: &str) -> Terminal {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg(script);
        Terminal::spawn(cmd, 10, 40, || {}).expect("spawn")
    }

    fn wait_for(t: &Terminal, what: impl Fn(&Terminal) -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(10);
        while Instant::now() < end {
            if what(t) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn alive(pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    #[test]
    fn output_reaches_the_emulator_with_colours() {
        let t = sh(r"printf 'plain \033[1;31mRED\033[0m done\n'");
        assert!(wait_for(&t, |t| t.screen_text().contains("done")));
        let p = t.parser();
        let cell = p.screen().cell(0, 6).unwrap();
        assert_eq!(cell.contents(), "R");
        assert_eq!(cell.fgcolor(), vt100::Color::Idx(1));
        assert!(cell.bold());
        assert_eq!(
            p.screen().cell(0, 0).unwrap().fgcolor(),
            vt100::Color::Default
        );
    }

    #[test]
    fn typed_input_is_echoed_and_read_by_the_program() {
        let t = sh("read line; echo got:$line");
        t.write(b"hello\r");
        assert!(wait_for(&t, |t| t.screen_text().contains("got:hello")));
        assert!(wait_for(&t, Terminal::exited));
    }

    #[test]
    fn resizing_reaches_the_program() {
        // `stty size` prints "rows cols".
        let mut t = sh("read x; stty size; read y; stty size");
        t.write(b"a\r");
        assert!(wait_for(&t, |t| t.screen_text().contains("10 40")));
        t.resize(20, 77);
        assert_eq!(t.size(), (20, 77));
        assert_eq!(t.parser().screen().size(), (nz(20), nz(77)));
        t.write(b"b\r");
        assert!(wait_for(&t, |t| t.screen_text().contains("20 77")));
    }

    #[test]
    fn the_exit_is_noticed() {
        let t = sh("echo bye; exit 3");
        assert!(wait_for(&t, Terminal::exited));
        assert!(t.screen_text().contains("bye"));
    }

    #[test]
    fn dropping_the_terminal_kills_the_shell_and_its_jobs() {
        let t = sh("sleep 600 & echo $! > /dev/null; exec sleep 601");
        let pid = t.pid().expect("pid");
        assert!(alive(pid));
        drop(t);
        assert!(!alive(pid), "the shell outlived its terminal");
    }

    #[test]
    fn a_shell_that_ignores_hangup_is_killed() {
        let t = sh("trap '' HUP; echo ready; while :; do sleep 1; done");
        assert!(wait_for(&t, |t| t.screen_text().contains("ready")));
        let pid = t.pid().expect("pid");
        drop(t);
        assert!(!alive(pid));
    }

    #[test]
    fn cursor_position_requests_are_answered() {
        // The terminal's answer is echoed by the pty, so it shows on screen as ^[[1;3R.
        let t = sh(r"printf 'ab\033[6n'; sleep 5");
        assert!(wait_for(&t, |t| t.screen_text().contains("1;3")));
    }

    #[test]
    fn a_login_shell_starts_in_the_given_folder() {
        let dir = std::env::temp_dir().canonicalize().unwrap();
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("pwd; echo $TERM");
        cmd.cwd(&dir);
        let t = Terminal::spawn(cmd, 10, 100, || {}).unwrap();
        assert!(wait_for(&t, |t| t.screen_text().contains("xterm-256color")));
        assert!(t.screen_text().contains(dir.to_str().unwrap()));
    }

    #[test]
    fn mouse_reports_in_the_three_encodings() {
        use vt100::{MouseProtocolEncoding as E, MouseProtocolMode as M};
        let none = egui::Modifiers::NONE;
        let sgr = |ev, c, r| encode_mouse(ev, c, r, none, M::AnyMotion, E::Sgr).unwrap();
        assert_eq!(sgr(MouseEvent::WheelUp, 4, 2), b"\x1b[<64;5;3M");
        assert_eq!(sgr(MouseEvent::WheelDown, 0, 0), b"\x1b[<65;1;1M");
        assert_eq!(sgr(MouseEvent::Press(0), 9, 9), b"\x1b[<0;10;10M");
        assert_eq!(
            sgr(MouseEvent::Release(2), 9, 9),
            b"\x1b[<2;10;10m",
            "SGR keeps the button"
        );
        assert_eq!(sgr(MouseEvent::Drag(0), 1, 1), b"\x1b[<32;2;2M");
        assert_eq!(sgr(MouseEvent::Move, 1, 1), b"\x1b[<35;2;2M");
        // modifiers: shift 4, alt 8, ctrl 16
        let m = egui::Modifiers {
            ctrl: true,
            shift: true,
            ..none
        };
        assert_eq!(
            encode_mouse(MouseEvent::WheelUp, 0, 0, m, M::Press, E::Sgr).unwrap(),
            b"\x1b[<84;1;1M"
        );
        // the old encoding: one byte each, release has no button
        assert_eq!(
            encode_mouse(MouseEvent::Press(0), 0, 0, none, M::Press, E::Default).unwrap(),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
        assert_eq!(
            encode_mouse(
                MouseEvent::Release(1),
                0,
                0,
                none,
                M::PressRelease,
                E::Default
            )
            .unwrap(),
            vec![0x1b, b'[', b'M', 35, 33, 33]
        );
        assert_eq!(
            encode_mouse(MouseEvent::Press(0), 250, 0, none, M::Press, E::Default),
            None
        );
        // UTF-8 encoding reaches further
        let far = encode_mouse(MouseEvent::Press(0), 300, 0, none, M::Press, E::Utf8).unwrap();
        assert_eq!(String::from_utf8(far).unwrap(), "\u{1b}[M \u{14d}!");
    }

    #[test]
    fn a_mode_only_reports_what_it_asked_for() {
        use vt100::{MouseProtocolEncoding as E, MouseProtocolMode as M};
        let none = egui::Modifiers::NONE;
        let ev = |ev, mode| encode_mouse(ev, 0, 0, none, mode, E::Sgr).is_some();
        assert!(!ev(MouseEvent::WheelUp, M::None));
        assert!(ev(MouseEvent::WheelUp, M::Press));
        assert!(ev(MouseEvent::Press(0), M::Press));
        assert!(!ev(MouseEvent::Release(0), M::Press));
        assert!(ev(MouseEvent::Release(0), M::PressRelease));
        assert!(!ev(MouseEvent::Drag(0), M::PressRelease));
        assert!(ev(MouseEvent::Drag(0), M::ButtonMotion));
        assert!(!ev(MouseEvent::Move, M::ButtonMotion));
        assert!(ev(MouseEvent::Move, M::AnyMotion));
    }

    #[test]
    fn a_program_in_mouse_mode_hears_the_wheel_and_one_that_is_not_does_not() {
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.args([
            "-c",
            "printf '\\033[?1000h\\033[?1006h'; stty -icanon -echo; cat -v",
        ]);
        let t = Terminal::spawn(cmd, 10, 40, || {}).expect("spawn");
        assert!(
            wait_for(&t, |t| t.wants_mouse()),
            "the program turned mouse reports on"
        );
        assert!(t.report_mouse(MouseEvent::WheelUp, (2, 4), egui::Modifiers::NONE));
        assert!(
            wait_for(&t, |t| t.screen_text().contains("^[[<64;5;3M")),
            "{}",
            t.screen_text()
        );
        // a shell in its normal state: nothing is reported, so the IDE keeps the event
        let plain = sh("true");
        assert!(!plain.wants_mouse());
        assert!(!plain.report_mouse(MouseEvent::WheelUp, (0, 0), egui::Modifiers::NONE));
    }
}
