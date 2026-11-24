//! Built-in local terminal: Roxterm's VT grid/parser with a direct PTY.
//!
//! The local daemon, RPC server, runbook engine, and Roxterm shell-startup
//! injection are intentionally not used. Commands are run only in the user's
//! interactive shell inside this RomaCode process.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use rox_vt::Screen as VtScreen;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::{self, JoinHandle};

const READ_CHUNK: usize = 8192;

pub struct RoxtermTerminal {
    pub screen: VtScreen,
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    child: Option<Box<dyn Child + Send + Sync>>,
    output: Receiver<Vec<u8>>,
    reader_thread: Option<JoinHandle<()>>,
    exit_code: Option<u32>,
    scrollback_offset: usize,
}

impl RoxtermTerminal {
    pub fn spawn(cols: u16, rows: u16, cwd: &Path) -> Result<Self, String> {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                if cfg!(windows) {
                    "cmd.exe".into()
                } else {
                    "sh".into()
                }
            });
        let cols = cols.max(2);
        let rows = rows.max(2);
        let system = native_pty_system();
        let pair = system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("open terminal PTY: {e}"))?;
        let mut command = CommandBuilder::new(&shell);
        command.cwd(cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        #[cfg(unix)]
        command.arg("-i");
        let child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| format!("start shell {shell:?}: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("open PTY input: {e}"))?;
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("open PTY output: {e}"))?;
        drop(pair.slave);

        let (tx, output) = mpsc::channel::<Vec<u8>>();
        let reader_thread = thread::Builder::new()
            .name("romacode-roxterm-reader".into())
            .spawn(move || {
                let mut reader = reader;
                let mut buffer = [0u8; READ_CHUNK];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            if tx.send(buffer[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
            })
            .map_err(|e| format!("start PTY reader: {e}"))?;

        Ok(Self {
            screen: VtScreen::new(cols as usize, rows as usize),
            master: Some(pair.master),
            writer: Some(writer),
            child: Some(child),
            output,
            reader_thread: Some(reader_thread),
            exit_code: None,
            scrollback_offset: 0,
        })
    }

    /// Consume all available PTY bytes into Roxterm's VT parser.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        loop {
            match self.output.try_recv() {
                Ok(bytes) => {
                    self.screen.feed(&bytes);
                    changed = true;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        if self.exit_code.is_none() {
            if let Some(child) = self.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    self.exit_code = Some(status.exit_code());
                    changed = true;
                }
            }
        }
        changed
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> Result<(), String> {
        let cols = cols.max(2);
        let rows = rows.max(2);
        self.screen.resize(cols as usize, rows as usize);
        self.master
            .as_mut()
            .ok_or_else(|| "terminal PTY is closed".to_string())?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("resize terminal PTY: {e}"))
    }

    pub fn send_key(&mut self, key: KeyEvent) -> Result<(), String> {
        self.scroll_to_bottom();
        let bytes = encode_key(key);
        self.write_input(&bytes)
    }

    pub fn send_paste(&mut self, text: &str) -> Result<(), String> {
        self.scroll_to_bottom();
        let bytes = if self.screen.bracketed_paste() {
            let mut out = Vec::with_capacity(text.len() + 12);
            out.extend_from_slice(b"\x1b[200~");
            out.extend_from_slice(text.as_bytes());
            out.extend_from_slice(b"\x1b[201~");
            out
        } else {
            text.as_bytes().to_vec()
        };
        self.write_input(&bytes)
    }

    fn write_input(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| "terminal PTY is closed".to_string())?;
        writer
            .write_all(bytes)
            .map_err(|e| format!("write terminal input: {e}"))?;
        writer
            .flush()
            .map_err(|e| format!("flush terminal input: {e}"))
    }

    pub fn exit_code(&self) -> Option<u32> {
        self.exit_code
    }

    pub fn visible_rows(&self) -> Vec<rox_protocol::RowWire> {
        self.screen.viewport_rows(self.scrollback_offset)
    }

    pub fn scroll_scrollback(&mut self, rows: isize) {
        if rows >= 0 {
            self.scrollback_offset = self.scrollback_offset.saturating_add(rows as usize);
        } else {
            self.scrollback_offset = self.scrollback_offset.saturating_sub(rows.unsigned_abs());
        }
        self.scrollback_offset = self.scrollback_offset.min(self.screen.scrollback_len());
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scrollback_offset = 0;
    }
}

impl Drop for RoxtermTerminal {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.writer.take();
        self.master.take();
        if let Some(thread) = self.reader_thread.take() {
            let _ = thread.join();
        }
    }
}

/// Encode common terminal keys as xterm-compatible input bytes.
pub fn encode_key(key: KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let mut bytes = match key.code {
        KeyCode::Char(c) if ctrl => {
            let c = c.to_ascii_lowercase();
            let b = c as u32;
            let control = match c {
                '@' | ' ' => Some(0),
                'a'..='z' => Some((c as u8) - b'a' + 1),
                '[' => Some(0x1b),
                '\\' => Some(0x1c),
                ']' => Some(0x1d),
                '^' => Some(0x1e),
                '_' => Some(0x1f),
                '?' => Some(0x7f),
                _ if b < 0x80 => None,
                _ => None,
            };
            control.map(|b| vec![b]).unwrap_or_default()
        }
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab if shift => b"\x1b[Z".to_vec(),
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => arrow(b'A', modifier),
        KeyCode::Down => arrow(b'B', modifier),
        KeyCode::Right => arrow(b'C', modifier),
        KeyCode::Left => arrow(b'D', modifier),
        KeyCode::Home => csi_tilde_or_final(b'H', modifier),
        KeyCode::End => csi_tilde_or_final(b'F', modifier),
        KeyCode::Insert => csi_tilde(2, modifier),
        KeyCode::Delete => csi_tilde(3, modifier),
        KeyCode::PageUp => csi_tilde(5, modifier),
        KeyCode::PageDown => csi_tilde(6, modifier),
        KeyCode::F(n @ 1..=4) => {
            let final_byte = b'P' + n - 1;
            if modifier == 1 {
                vec![0x1b, b'O', final_byte]
            } else {
                arrow(final_byte, modifier)
            }
        }
        KeyCode::F(n @ 5..=12) => {
            const CODES: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            csi_tilde(CODES[(n - 5) as usize], modifier)
        }
        _ => Vec::new(),
    };
    if alt && !ctrl && !bytes.is_empty() && !bytes.starts_with(b"\x1b") {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn arrow(final_byte: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes()
    }
}

fn csi_tilde(code: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        format!("\x1b[{code}~").into_bytes()
    } else {
        format!("\x1b[{code};{modifier}~").into_bytes()
    }
}

fn csi_tilde_or_final(final_byte: u8, modifier: u8) -> Vec<u8> {
    if modifier == 1 {
        vec![0x1b, b'[', final_byte]
    } else {
        format!("\x1b[1;{modifier}{}", final_byte as char).into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn encodes_basic_terminal_keys() {
        assert_eq!(
            encode_key(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            b"x"
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            &[3]
        );
        assert_eq!(encode_key(key(KeyCode::Enter, KeyModifiers::NONE)), b"\r");
        assert_eq!(encode_key(key(KeyCode::Up, KeyModifiers::NONE)), b"\x1b[A");
        assert_eq!(
            encode_key(key(KeyCode::BackTab, KeyModifiers::NONE)),
            b"\x1b[Z"
        );
    }

    #[test]
    fn encodes_modified_navigation_and_unicode() {
        assert_eq!(
            encode_key(key(KeyCode::Right, KeyModifiers::CONTROL)),
            b"\x1b[1;5C"
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('λ'), KeyModifiers::NONE)),
            "λ".as_bytes()
        );
        assert_eq!(
            encode_key(key(KeyCode::Char('x'), KeyModifiers::ALT)),
            b"\x1bx"
        );
    }

    #[cfg(unix)]
    #[test]
    fn interactive_pty_runs_a_shell_command_and_parses_output() {
        let cwd = std::env::current_dir().expect("current directory");
        let mut terminal = RoxtermTerminal::spawn(80, 24, &cwd).expect("open local PTY shell");
        for ch in "printf '%s\\n' $((1234+3456))".chars() {
            terminal
                .send_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
                .expect("write PTY key");
        }
        terminal
            .send_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .expect("submit shell command");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            terminal.poll();
            if terminal.screen.snapshot_text().contains("4690") {
                return;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!(
            "shell output was not parsed by Roxterm: {}",
            terminal.screen.snapshot_text()
        );
    }
}
