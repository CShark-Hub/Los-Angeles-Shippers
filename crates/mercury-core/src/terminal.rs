//! The terminal under the admin shell's editor. On unix, while a line is read the tty is put in raw
//! mode (no echo, no line buffering, no signals from the keys) and the bytes that come are fed to
//! [`crate::line_editor`]; the header is kept on the top rows by giving the commands a scrolling
//! region under it. Anywhere else, when the input or the output isn't a terminal, and when the
//! terminal says it is dumb, lines are read as plain lines.
//!
//! The terminal is back in its own mode whenever a command runs, so a command's output is drawn as
//! any other program's; Ctrl-C then stops the command and the prompt stays. When the session ends,
//! or fails, the scrolling region is given up.
//!
//! The terminal's size is asked of the tty (`TIOCGWINSZ`), not of the terminal with an escape
//! sequence: an answer that must be read back would sit in the input among the keys typed ahead.

use crate::shell::{Lines, PlainLines, Style};
use crate::welcome;

/// The lines of a session: read with the editor at a terminal, as plain lines from the standard
/// input otherwise. The input is locked only in the second case: the editor reads it a byte at a time
/// and would wait for ever on a lock held for the whole session.
pub fn lines(style: &Style) -> Box<dyn Lines> {
    #[cfg(unix)]
    {
        use std::io::IsTerminal;
        let dumb = std::env::var("TERM").is_ok_and(|term| term == "dumb");
        if style.interactive && !dumb && std::io::stdout().is_terminal() {
            return Box::new(raw::RawLines::new(style));
        }
    }
    Box::new(PlainLines::new(std::io::stdin().lock(), *style))
}

/// How the header stands on the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// Not drawn yet.
    Nothing,
    /// Drawn once among the lines, scrolling away with them: the terminal was too small to keep it
    /// on top, or became so. It stays that way for the session, so it never covers a command's output.
    Inline,
    /// Kept on the top rows of a terminal of this size, the commands scrolling under it.
    Pinned { rows: usize, cols: usize },
}

/// A terminal needs this many rows beyond the header's to keep it on top: under it the commands
/// would have no room, and the header is drawn once and scrolls away like anything else.
pub const ROOM_BELOW: usize = 8;

/// The first row under the header, where the commands scroll.
const TOP: usize = welcome::HEIGHT + 1;

/// What to write to show the header, `lines` as rendered for `cols`, in a terminal of `rows` by
/// `cols`, given how it stands now; and how it stands after. Only the first time it is pinned is
/// the screen cleared: later, after a command or a resize, the header is drawn over its own rows
/// and the cursor is put back, so nothing a command printed is wiped.
pub fn show(shown: Shown, lines: &[String], rows: usize, cols: usize) -> (String, Shown) {
    let fits = lines.len() == welcome::HEIGHT && rows >= welcome::HEIGHT + ROOM_BELOW;
    let pinned = Shown::Pinned { rows, cols };
    match (shown, fits) {
        (Shown::Inline, _) => (String::new(), Shown::Inline),
        (Shown::Nothing, true) => {
            let mut seq = format!("\x1b[H\x1b[2J\x1b[{TOP};{rows}r");
            rows_of(&mut seq, lines);
            seq.push_str(&format!("\x1b[{TOP};1H"));
            (seq, pinned)
        }
        (Shown::Pinned { .. }, true) => {
            // the region set again only when the size changed; setting it moves the cursor home
            let mut seq = "\x1b7".to_owned();
            if shown != pinned {
                seq.push_str(&format!("\x1b[{TOP};{rows}r"));
            }
            rows_of(&mut seq, lines);
            seq.push_str("\x1b8");
            (seq, pinned)
        }
        // too small now: the region given back, the header left where it is to scroll away
        (Shown::Pinned { .. }, false) => ("\x1b7\x1b[r\x1b8".to_owned(), Shown::Inline),
        (Shown::Nothing, false) => {
            let mut seq = String::new();
            for line in lines {
                seq.push_str(line);
                seq.push('\n');
            }
            (seq, Shown::Inline)
        }
    }
}

/// The header's rows, each cleared and drawn in its place.
fn rows_of(seq: &mut String, lines: &[String]) {
    for (n, line) in lines.iter().enumerate() {
        seq.push_str(&format!("\x1b[{};1H\x1b[2K{line}", n + 1));
    }
}

/// What Ctrl-L writes: what is under a header kept on top, or the whole screen.
pub fn clear(shown: Shown) -> String {
    match shown {
        Shown::Pinned { .. } => format!("\x1b[{TOP};1H\x1b[J"),
        _ => "\x1b[H\x1b[2J".to_owned(),
    }
}

/// What gives the screen back when the session ends: the region given up, and the cursor on the
/// last row, so what comes next is written under what the session left, not over the header.
pub fn finish(shown: Shown) -> String {
    match shown {
        Shown::Pinned { rows, .. } => format!("\x1b[r\x1b[{rows};1H\r\n"),
        _ => String::new(),
    }
}

#[cfg(unix)]
mod raw {
    use std::future::Future;
    use std::io::{self, Read, Write};
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    use rustix::termios::{self, InputModes, LocalModes, OptionalActions, SpecialCodeIndex, Termios};
    use tokio::signal::unix::{Signal, SignalKind, signal};

    use super::Shown;
    use crate::line_editor::{self, History, Prompt, Read as Typed, Settings};
    use crate::shell::{Lines, Style, command_tree};
    use crate::welcome::{self, Header, Look};

    /// The terminal in raw mode, until this is dropped.
    struct RawMode {
        original: Termios,
    }

    impl RawMode {
        fn enter() -> io::Result<RawMode> {
            let stdin = io::stdin();
            let original = termios::tcgetattr(&stdin)?;
            let mut raw = original.clone();
            // no echo and no line buffering; Ctrl-C, Ctrl-Z and Ctrl-S reach the editor as keys
            raw.local_modes
                .remove(LocalModes::ICANON | LocalModes::ECHO | LocalModes::ISIG | LocalModes::IEXTEN);
            // Enter arrives as a carriage return, and Ctrl-S and Ctrl-Q as keys
            raw.input_modes
                .remove(InputModes::ICRNL | InputModes::INLCR | InputModes::IGNCR | InputModes::IXON);
            // what is written still gets its newlines turned into carriage returns and newlines
            raw.special_codes[SpecialCodeIndex::VMIN] = 1;
            raw.special_codes[SpecialCodeIndex::VTIME] = 0;
            // after what was written, and keeping what was typed ahead
            termios::tcsetattr(&stdin, OptionalActions::Drain, &raw)?;
            Ok(RawMode { original })
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            let _ = termios::tcsetattr(io::stdin(), OptionalActions::Drain, &self.original);
        }
    }

    /// The next byte typed, or `None` when the input is closed.
    fn read_byte() -> io::Result<Option<u8>> {
        let mut byte = [0_u8; 1];
        loop {
            match io::stdin().lock().read(&mut byte) {
                Ok(0) => return Ok(None),
                Ok(_) => return Ok(Some(byte[0])),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }

    /// How many rows and columns the terminal has, as the tty knows it. Falls back to `LINES` and
    /// `COLUMNS`, then to 24 by 80, when it doesn't say.
    fn size() -> (usize, usize) {
        let env = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<usize>().ok());
        termios::tcgetwinsize(io::stdout())
            .ok()
            .map(|size| (usize::from(size.ws_row), usize::from(size.ws_col)))
            .filter(|(rows, cols)| *rows >= 2 && *cols >= 10)
            .unwrap_or_else(|| (env("LINES").unwrap_or(24), env("COLUMNS").unwrap_or(80)))
    }

    /// The editor on a unix terminal.
    pub struct RawLines {
        history: History,
        look: Look,
        shown: Shown,
        /// The last header drawn, to draw it again when the terminal is resized.
        last: Option<Header>,
        /// Ctrl-C while a command runs, when the tty is in its own mode and sends a signal: listened
        /// to from the start, so it never ends the process with the region still set.
        interrupt: Option<Signal>,
    }

    impl RawLines {
        pub fn new(style: &Style) -> Self {
            RawLines {
                history: History::default(),
                look: style.look(),
                shown: Shown::Nothing,
                last: None,
                interrupt: signal(SignalKind::interrupt()).ok(),
            }
        }

        fn place(
            &mut self,
            header: &Header,
            rows: usize,
            cols: usize,
            out: &mut dyn Write,
        ) -> io::Result<()> {
            let lines = welcome::render(header, cols, self.look);
            let (seq, shown) = super::show(self.shown, &lines, rows, cols);
            self.shown = shown;
            if !seq.is_empty() {
                write!(out, "{seq}")?;
                out.flush()?;
            }
            Ok(())
        }
    }

    impl Lines for RawLines {
        fn wants_header(&self) -> bool {
            true
        }

        fn read(&mut self, prompt: &Prompt, out: &mut dyn Write) -> io::Result<Option<String>> {
            let (rows, cols) = size();
            // a terminal resized since the header was drawn: the header and its region are drawn again
            let resized =
                matches!(self.shown, Shown::Pinned { .. }) && self.shown != (Shown::Pinned { rows, cols });
            if resized && let Some(header) = self.last.clone() {
                self.place(&header, rows, cols, out)?;
            }
            let clear = super::clear(self.shown);
            let raw = RawMode::enter()?;
            let settings = Settings {
                prompt,
                tree: command_tree(),
                colour: self.look.colour,
                cols,
                clear: &clear,
            };
            let typed = line_editor::read_line(&mut read_byte, out, &mut self.history, &settings);
            // back in the terminal's own mode before a command runs, whatever happened
            drop(raw);
            Ok(match typed? {
                Typed::Line(line) => Some(line),
                Typed::Eof => None,
            })
        }

        fn header(&mut self, header: &Header, out: &mut dyn Write) -> io::Result<()> {
            let (rows, cols) = size();
            self.last = Some(header.clone());
            self.place(header, rows, cols, out)
        }

        fn interrupted(&mut self) -> Pin<Box<dyn Future<Output = ()> + '_>> {
            let Some(signal) = self.interrupt.as_mut() else {
                return Box::pin(std::future::pending());
            };
            // a Ctrl-C pressed before this command began was meant for nothing that runs now
            let mut cx = Context::from_waker(Waker::noop());
            while let Poll::Ready(Some(())) = signal.poll_recv(&mut cx) {}
            Box::pin(async move {
                if signal.recv().await.is_none() {
                    std::future::pending::<()>().await;
                }
            })
        }

        fn finish(&mut self, out: &mut dyn Write) -> io::Result<()> {
            let seq = super::finish(self.shown);
            self.shown = Shown::Nothing;
            if !seq.is_empty() {
                write!(out, "{seq}")?;
                out.flush()?;
            }
            Ok(())
        }
    }

    impl Drop for RawLines {
        fn drop(&mut self) {
            // a session that ended in an error still gives the region back, with the cursor under it
            let seq = super::finish(self.shown);
            if !seq.is_empty() {
                let mut out = io::stdout();
                let _ = write!(out, "{seq}");
                let _ = out.flush();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Vec<String> {
        (1..=welcome::HEIGHT).map(|n| format!("row {n}")).collect()
    }

    #[test]
    fn the_first_time_it_fits_the_screen_is_cleared_and_the_header_pinned() {
        let (seq, shown) = show(Shown::Nothing, &header(), 30, 100);
        assert_eq!(shown, Shown::Pinned { rows: 30, cols: 100 });
        assert!(seq.starts_with("\x1b[H\x1b[2J\x1b[10;30r"), "{seq:?}");
        assert!(
            seq.contains("\x1b[1;1H\x1b[2Krow 1") && seq.contains("\x1b[9;1H\x1b[2Krow 9"),
            "{seq:?}"
        );
        // the cursor where the commands begin
        assert!(seq.ends_with("\x1b[10;1H"), "{seq:?}");
    }

    #[test]
    fn after_a_command_the_header_is_drawn_again_without_wiping_anything() {
        let pinned = Shown::Pinned { rows: 30, cols: 100 };
        let (seq, shown) = show(pinned, &header(), 30, 100);
        assert_eq!(shown, pinned);
        assert!(!seq.contains("\x1b[2J") && !seq.contains("\x1b[J"), "{seq:?}");
        assert!(
            !seq.contains("\x1b[10;30r"),
            "the region is left as it is: {seq:?}"
        );
        assert!(seq.contains("\x1b[1;1H\x1b[2Krow 1"), "{seq:?}");
        assert!(seq.starts_with("\x1b7") && seq.ends_with("\x1b8"), "{seq:?}");
    }

    #[test]
    fn a_resized_terminal_gets_its_region_again_and_nothing_is_cleared() {
        let (seq, shown) = show(Shown::Pinned { rows: 30, cols: 100 }, &header(), 40, 120);
        assert_eq!(shown, Shown::Pinned { rows: 40, cols: 120 });
        assert!(seq.contains("\x1b[10;40r"), "{seq:?}");
        assert!(!seq.contains("\x1b[2J") && !seq.contains("\x1b[J"), "{seq:?}");
        // setting the region sends the cursor home: it is put back where the command left it
        assert!(seq.starts_with("\x1b7") && seq.ends_with("\x1b8"), "{seq:?}");
    }

    #[test]
    fn a_terminal_made_too_small_gives_the_region_back_and_stays_unpinned() {
        let (seq, shown) = show(Shown::Pinned { rows: 30, cols: 100 }, &header(), 12, 100);
        assert_eq!(shown, Shown::Inline);
        assert_eq!(seq, "\x1b7\x1b[r\x1b8");
        // made big again, the header isn't drawn over what is on the screen now
        assert_eq!(show(shown, &header(), 30, 100), (String::new(), Shown::Inline));
    }

    #[test]
    fn too_small_from_the_start_the_header_is_shown_once_among_the_lines() {
        let (seq, shown) = show(Shown::Nothing, &header(), welcome::HEIGHT + ROOM_BELOW - 1, 100);
        assert_eq!(shown, Shown::Inline);
        assert!(seq.starts_with("row 1\n") && seq.ends_with("row 9\n"), "{seq:?}");
        assert!(!seq.contains('\x1b'), "no region, no cursor moves: {seq:?}");
        assert_eq!(show(shown, &header(), 50, 100).0, "");
    }

    #[test]
    fn the_short_header_of_a_narrow_terminal_is_never_pinned() {
        let short = vec!["one".to_owned(), "two".to_owned()];
        let (seq, shown) = show(Shown::Nothing, &short, 50, 30);
        assert_eq!(shown, Shown::Inline);
        assert_eq!(seq, "one\ntwo\n");
    }

    #[test]
    fn the_end_gives_the_region_back_with_the_cursor_on_the_last_row() {
        assert_eq!(
            finish(Shown::Pinned { rows: 30, cols: 100 }),
            "\x1b[r\x1b[30;1H\r\n"
        );
        assert_eq!(finish(Shown::Inline), "");
        assert_eq!(finish(Shown::Nothing), "");
    }

    #[test]
    fn ctrl_l_clears_under_the_header_when_it_is_pinned() {
        assert_eq!(clear(Shown::Pinned { rows: 30, cols: 100 }), "\x1b[10;1H\x1b[J");
        assert_eq!(clear(Shown::Inline), "\x1b[H\x1b[2J");
    }
}
