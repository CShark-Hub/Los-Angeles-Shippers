//! The line editor of the admin shell: what a key does to a line, the history the arrows walk, Tab
//! completion and colouring of the command as it is typed, and the redrawing of a line that wraps.
//!
//! Nothing here touches a terminal. Bytes come in through a function and escape sequences go out to
//! a writer, so the whole of it is tested without one; the unix terminal (`terminal.rs`) only puts
//! the tty in raw mode and feeds this its bytes. The history lives in memory for the session and is
//! never written to disk.

use std::collections::HashSet;
use std::io::{self, Write};

// ---------------------------------------------------------------------------------------------
// Keys

/// What a press of a key means to the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
    Up,
    Down,
    Tab,
    /// Ctrl-C: drop the line and start another.
    Interrupt,
    /// Ctrl-D: leave on an empty line, delete the character under the cursor otherwise.
    Eof,
    KillToStart,
    KillToEnd,
    KillWord,
    /// Ctrl-L.
    Clear,
    /// A key the editor has no use for; it changes nothing.
    Unknown,
}

/// Turns the bytes a terminal sends into keys, one byte at a time: an arrow is three bytes, a letter
/// with an accent two.
#[derive(Debug, Default)]
pub struct KeyDecoder {
    pending: Vec<u8>,
}

impl KeyDecoder {
    /// The key the bytes so far make up, or `None` while a sequence is still to be finished.
    pub fn push(&mut self, byte: u8) -> Option<Key> {
        if self.pending.is_empty() {
            return self.first(byte);
        }
        self.pending.push(byte);
        if self.pending[0] == 0x1b {
            self.escape()
        } else {
            self.utf8()
        }
    }

    fn first(&mut self, byte: u8) -> Option<Key> {
        Some(match byte {
            0x1b => {
                self.pending.push(byte);
                return None;
            }
            b'\r' | b'\n' => Key::Enter,
            0x7f | 0x08 => Key::Backspace,
            0x03 => Key::Interrupt,
            0x04 => Key::Eof,
            0x01 => Key::Home,
            0x05 => Key::End,
            0x02 => Key::Left,
            0x06 => Key::Right,
            0x0b => Key::KillToEnd,
            0x0c => Key::Clear,
            0x15 => Key::KillToStart,
            0x17 => Key::KillWord,
            0x09 => Key::Tab,
            0x10 => Key::Up,
            0x0e => Key::Down,
            0x20..=0x7e => Key::Char(char::from(byte)),
            0x00..=0x1f => Key::Unknown,
            0x80..=0xff => {
                self.pending.push(byte);
                return self.utf8();
            }
        })
    }

    /// A character of more than one byte: complete when the lead byte's length is reached. A byte
    /// that can't continue it (a lead byte of another encoding, a key typed after a broken one) drops
    /// what came before and is read as a key of its own, so no key is lost to a broken character.
    fn utf8(&mut self) -> Option<Key> {
        let wanted = match self.pending[0] {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => return self.done(Key::Unknown),
        };
        let last = self.pending[self.pending.len() - 1];
        if self.pending.len() > 1 && !(0x80..=0xbf).contains(&last) {
            self.pending.clear();
            return self.first(last);
        }
        if self.pending.len() < wanted {
            return None;
        }
        let key = match std::str::from_utf8(&self.pending)
            .ok()
            .and_then(|s| s.chars().next())
        {
            Some(c) => Key::Char(c),
            None => Key::Unknown,
        };
        self.done(key)
    }

    fn done(&mut self, key: Key) -> Option<Key> {
        self.pending.clear();
        Some(key)
    }

    /// `ESC` and what follows: `ESC [ …` and `ESC O …` for the arrows and the like, `ESC b` and
    /// `ESC f` (Alt with a letter) for a word, Alt-Backspace for the word before the cursor. A lone
    /// `ESC` before a control key (Enter, Tab, Ctrl-C, another `ESC`) gives way to that key.
    fn escape(&mut self) -> Option<Key> {
        match self.pending.len() {
            0 | 1 => None,
            2 => match self.pending[1] {
                b'[' | b'O' => None,
                b'b' => self.done(Key::WordLeft),
                b'f' => self.done(Key::WordRight),
                0x7f | 0x08 => self.done(Key::KillWord),
                control @ (0x00..=0x1f) => {
                    self.pending.clear();
                    self.first(control)
                }
                _ => self.done(Key::Unknown),
            },
            len => {
                let last = self.pending[len - 1];
                // a byte no sequence has (a control key, a character) ends it unfinished, and is
                // read as a key of its own
                if !(0x20..=0x7e).contains(&last) {
                    self.pending.clear();
                    return self.first(last);
                }
                if self.pending[1] == b'O' {
                    let key = Self::final_key(last, "");
                    return self.done(key);
                }
                match last {
                    // a final byte ends the sequence; the bytes before it are its parameters
                    0x40..=0x7e => {
                        let params = String::from_utf8_lossy(&self.pending[2..len - 1]).into_owned();
                        let key = Self::final_key(last, &params);
                        self.done(key)
                    }
                    _ if len > 16 => self.done(Key::Unknown),
                    _ => None,
                }
            }
        }
    }

    fn final_key(last: u8, params: &str) -> Key {
        // a modifier after the first parameter ("1;5C" is Ctrl-Right, "1;3C" Alt-Right) moves by a word
        let by_word =
            params.rsplit(';').next().is_some_and(|m| matches!(m, "3" | "5")) && params.contains(';');
        match last {
            b'A' => Key::Up,
            b'B' => Key::Down,
            b'C' if by_word => Key::WordRight,
            b'C' => Key::Right,
            b'D' if by_word => Key::WordLeft,
            b'D' => Key::Left,
            b'H' => Key::Home,
            b'F' => Key::End,
            b'~' => match params.split(';').next() {
                Some("1" | "7") => Key::Home,
                Some("4" | "8") => Key::End,
                Some("3") => Key::Delete,
                _ => Key::Unknown,
            },
            _ => Key::Unknown,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The line

/// The longest line the editor takes, in characters: far more than any command, and short enough
/// that a paste gone wrong can't make each key redraw a screenful.
pub const MAX_LINE: usize = 4096;

/// The line being typed, and where the cursor stands in it. Positions count characters, not bytes.
#[derive(Debug, Default, Clone)]
pub struct Editor {
    chars: Vec<char>,
    cursor: usize,
}

impl Editor {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn before_cursor(&self) -> String {
        self.chars[..self.cursor].iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Replaces the line, with the cursor at its end.
    pub fn set(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// Types `c` at the cursor; nothing, once the line is [`MAX_LINE`] long.
    pub fn insert(&mut self, c: char) {
        if self.chars.len() >= MAX_LINE {
            return;
        }
        self.chars.insert(self.cursor, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.chars.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.chars.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Where the word before the cursor begins: the spaces left of the cursor, then the word.
    fn word_start(&self) -> usize {
        let mut at = self.cursor;
        while at > 0 && self.chars[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !self.chars[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }

    pub fn word_left(&mut self) {
        self.cursor = self.word_start();
    }

    pub fn word_right(&mut self) {
        let len = self.chars.len();
        while self.cursor < len && self.chars[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
        while self.cursor < len && !self.chars[self.cursor].is_whitespace() {
            self.cursor += 1;
        }
    }

    pub fn kill_to_start(&mut self) {
        self.chars.drain(..self.cursor);
        self.cursor = 0;
    }

    pub fn kill_to_end(&mut self) {
        self.chars.truncate(self.cursor);
    }

    pub fn kill_word(&mut self) {
        let start = self.word_start();
        self.chars.drain(start..self.cursor);
        self.cursor = start;
    }

    /// Puts `text` in place of what stands between `start` and the cursor, the cursor after it.
    pub fn replace_from(&mut self, start: usize, text: &str) {
        let start = start.min(self.cursor);
        self.chars.splice(start..self.cursor, text.chars());
        self.cursor = start + text.chars().count();
    }

    fn apply(&mut self, key: Key) {
        match key {
            Key::Char(c) => self.insert(c),
            Key::Backspace => self.backspace(),
            Key::Delete | Key::Eof => self.delete(),
            Key::Left => self.left(),
            Key::Right => self.right(),
            Key::WordLeft => self.word_left(),
            Key::WordRight => self.word_right(),
            Key::Home => self.home(),
            Key::End => self.end(),
            Key::KillToStart => self.kill_to_start(),
            Key::KillToEnd => self.kill_to_end(),
            Key::KillWord => self.kill_word(),
            _ => {}
        }
    }
}

/// The lines typed before, for the up and down arrows to walk.
#[derive(Debug, Default)]
pub struct History {
    entries: Vec<String>,
    /// Which entry is on screen, while walking.
    at: Option<usize>,
    /// What was being typed when the walk began, which down brings back at its end.
    draft: String,
}

/// How many lines the history keeps.
const HISTORY_LIMIT: usize = 500;

impl History {
    /// Keeps a line, unless it is empty or the one before it.
    pub fn remember(&mut self, line: &str) {
        self.at = None;
        if line.trim().is_empty() || self.entries.last().is_some_and(|last| last == line) {
            return;
        }
        self.entries.push(line.to_owned());
        if self.entries.len() > HISTORY_LIMIT {
            self.entries.remove(0);
        }
    }

    pub fn reset(&mut self) {
        self.at = None;
    }

    /// One line further back; `current` is kept for the way down. `None` at the oldest.
    pub fn older(&mut self, current: &str) -> Option<&str> {
        let next = match self.at {
            None if self.entries.is_empty() => return None,
            None => {
                self.draft = current.to_owned();
                self.entries.len() - 1
            }
            Some(0) => return None,
            Some(at) => at - 1,
        };
        self.at = Some(next);
        Some(&self.entries[next])
    }

    /// One line forward, and at the end the line that was being typed. `None` when not walking.
    pub fn newer(&mut self) -> Option<String> {
        let at = self.at?;
        if at + 1 < self.entries.len() {
            self.at = Some(at + 1);
            Some(self.entries[at + 1].clone())
        } else {
            self.at = None;
            Some(std::mem::take(&mut self.draft))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// What the commands are: completion and colours

/// A command of the admin shell, with the flags it takes and the commands under it. Built from the
/// command line's own definition, so completion and colouring know what the parser knows.
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub name: String,
    pub hidden: bool,
    /// Long flags, with their dashes.
    pub flags: Vec<String>,
    pub subs: Vec<Node>,
}

impl Node {
    pub fn from_clap(command: &clap::Command) -> Node {
        Node {
            name: command.get_name().to_owned(),
            hidden: command.is_hide_set(),
            flags: command
                .get_arguments()
                .filter_map(|arg| arg.get_long())
                .map(|long| format!("--{long}"))
                .collect(),
            subs: command.get_subcommands().map(Node::from_clap).collect(),
        }
    }

    fn sub(&self, name: &str) -> Option<&Node> {
        self.subs.iter().find(|sub| sub.name == name)
    }
}

/// What Tab can put in place of the word the cursor is in.
#[derive(Debug, PartialEq, Eq)]
pub struct Completion {
    /// Where that word begins, in characters.
    pub start: usize,
    pub candidates: Vec<String>,
}

/// The words Tab offers after `before`, the line up to the cursor. None inside an open quote, where
/// what is typed is a value, and where nothing fits.
pub fn complete(tree: &Node, before: &str) -> Option<Completion> {
    if before.chars().filter(|c| matches!(c, '\'' | '"')).count() % 2 == 1 {
        return None;
    }
    let chars: Vec<char> = before.chars().collect();
    let partial_start = if chars.last().is_none_or(|c| c.is_whitespace()) {
        chars.len()
    } else {
        chars
            .iter()
            .rposition(|c| c.is_whitespace())
            .map_or(0, |at| at + 1)
    };
    let partial: String = chars[partial_start..].iter().collect();
    let earlier: String = chars[..partial_start].iter().collect();
    let words: Vec<&str> = earlier.split_whitespace().collect();

    let mut node = tree;
    let mut used = HashSet::new();
    for word in &words {
        if word.starts_with('-') {
            used.insert(word.split('=').next().unwrap_or(word).to_owned());
        } else if !node.subs.is_empty() {
            // a word that names no command here: there is nothing sensible to offer after it
            node = node.sub(word)?;
        }
        // under a command with no commands of its own the words are values
    }

    let mut candidates: Vec<String> = if partial.starts_with('-') {
        if partial.contains('=') {
            return None;
        }
        node.flags
            .iter()
            .filter(|flag| flag.starts_with(&partial) && !used.contains(*flag))
            .cloned()
            .collect()
    } else if !node.subs.is_empty() {
        node.subs
            .iter()
            .filter(|sub| !sub.hidden && sub.name.starts_with(&partial))
            .map(|sub| sub.name.clone())
            .collect()
    } else if partial.is_empty() && words.last().is_some_and(|last| !last.starts_with('-')) {
        // a command with its words in, and nothing typed yet: what it takes
        node.flags
            .iter()
            .filter(|flag| !used.contains(*flag) && flag.as_str() != "--help" && flag.as_str() != "--version")
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    candidates.sort();
    candidates.dedup();
    (!candidates.is_empty()).then_some(Completion {
        start: partial_start,
        candidates,
    })
}

/// What a Tab did to the line.
#[derive(Debug, PartialEq, Eq)]
pub enum Tabbed {
    Nothing,
    /// The word was completed, or as far as the candidates agree.
    Completed,
    /// The candidates disagree from here on: they are to be listed.
    List(Vec<String>),
}

/// Carries out a completion on the line.
pub fn apply_completion(editor: &mut Editor, completion: Option<Completion>) -> Tabbed {
    let Some(Completion { start, candidates }) = completion else {
        return Tabbed::Nothing;
    };
    if let [only] = candidates.as_slice() {
        editor.replace_from(start, &format!("{only} "));
        return Tabbed::Completed;
    }
    let common = common_prefix(&candidates);
    if common.chars().count() > editor.cursor() - start {
        editor.replace_from(start, &common);
        Tabbed::Completed
    } else {
        Tabbed::List(candidates)
    }
}

fn common_prefix(words: &[String]) -> String {
    let mut prefix: Vec<char> = words.first().map(|w| w.chars().collect()).unwrap_or_default();
    for word in words.iter().skip(1) {
        let shared = prefix
            .iter()
            .zip(word.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        prefix.truncate(shared);
    }
    prefix.into_iter().collect()
}

/// The escape sequences the editor draws with.
mod paint {
    pub const RESET: &str = "\x1b[0m";
    pub const COMMAND: &str = "\x1b[1;32m";
    pub const UNKNOWN: &str = "\x1b[31m";
    pub const FLAG: &str = "\x1b[36m";
    pub const QUOTED: &str = "\x1b[33m";
}

/// The line with colours: the words that name a command bold green, a word that names none red, the
/// flags a command takes cyan (red when it takes no such flag), what is in quotes yellow. Taking the
/// escape sequences out gives the line back exactly.
pub fn highlight(tree: &Node, text: &str) -> String {
    let mut out = String::new();
    let mut node = tree;
    let mut on_path = true;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // one word, up to a space outside quotes
        let start = i;
        let mut quote: Option<char> = None;
        while i < chars.len() && (quote.is_some() || !chars[i].is_whitespace()) {
            match (quote, chars[i]) {
                (None, c @ ('\'' | '"')) => quote = Some(c),
                (Some(open), c) if c == open => quote = None,
                _ => {}
            }
            i += 1;
        }
        let word: String = chars[start..i].iter().collect();
        if word.starts_with('-') {
            let (flag, rest) = word
                .split_once('=')
                .map_or((word.as_str(), ""), |(f, _)| (f, &word[f.len()..]));
            let colour = if node.flags.iter().any(|known| known == flag) {
                paint::FLAG
            } else {
                paint::UNKNOWN
            };
            out.push_str(&format!("{colour}{flag}{}{rest}", paint::RESET));
        } else if word.starts_with(['\'', '"']) {
            out.push_str(&format!("{}{word}{}", paint::QUOTED, paint::RESET));
        } else if on_path && !node.subs.is_empty() {
            match node.sub(&word) {
                Some(sub) => {
                    out.push_str(&format!("{}{word}{}", paint::COMMAND, paint::RESET));
                    node = sub;
                }
                None => {
                    out.push_str(&format!("{}{word}{}", paint::UNKNOWN, paint::RESET));
                    on_path = false;
                }
            }
        } else {
            on_path = false;
            out.push_str(&word);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The screen

/// How a prompt is shown, and how wide it is without its colours.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub styled: String,
    pub width: usize,
}

impl Prompt {
    pub fn plain(text: &str) -> Prompt {
        Prompt {
            styled: text.to_owned(),
            width: text.chars().count(),
        }
    }
}

/// Where the cursor stands among the rows the line takes, so the next drawing can start from the top
/// of them.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    cursor_row: usize,
}

/// Draws the prompt and the line again: up to the first row of the line, clear to the end of the
/// screen, the prompt and the line, then the cursor back to where it stands in the line. A line wider
/// than the terminal wraps, and every row it takes is redrawn.
pub fn refresh(
    out: &mut dyn Write,
    screen: &mut Screen,
    prompt: &Prompt,
    styled_line: &str,
    line_len: usize,
    cursor: usize,
    cols: usize,
) -> io::Result<()> {
    let cols = cols.max(2);
    if screen.cursor_row > 0 {
        write!(out, "\x1b[{}A", screen.cursor_row)?;
    }
    write!(out, "\r\x1b[J{}{styled_line}", prompt.styled)?;
    let end = prompt.width + line_len;
    // a line that ends exactly at the edge leaves the cursor waiting there: a newline moves it down
    if end > 0 && end.is_multiple_of(cols) {
        write!(out, "\r\n")?;
    }
    let end_row = end / cols;
    let at = prompt.width + cursor;
    let (row, col) = (at / cols, at % cols);
    if end_row > row {
        write!(out, "\x1b[{}A", end_row - row)?;
    }
    write!(out, "\r")?;
    if col > 0 {
        write!(out, "\x1b[{col}C")?;
    }
    screen.cursor_row = row;
    out.flush()
}

/// The words laid out in rows no wider than the terminal, for the list a Tab shows.
fn in_rows(words: &[String], cols: usize) -> String {
    let mut out = String::new();
    let mut width = 0;
    for word in words {
        let w = word.chars().count();
        if width > 0 && width + 2 + w > cols {
            out.push_str("\r\n");
            width = 0;
        }
        if width > 0 {
            out.push_str("  ");
            width += 2;
        }
        out.push_str(word);
        width += w;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Reading a line

/// What the line is read with.
pub struct Settings<'a> {
    pub prompt: &'a Prompt,
    pub tree: &'a Node,
    /// Colour the line as it is typed.
    pub colour: bool,
    pub cols: usize,
    /// What Ctrl-L writes to clear the screen: all of it, or what is under a header kept on top.
    pub clear: &'a str,
}

/// How reading a line ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Read {
    Line(String),
    /// Ctrl-D on an empty line, or the input closed.
    Eof,
}

/// Reads one line with the editor: draws the prompt, takes keys from `next_byte` until Enter, and
/// draws the line as it changes. `next_byte` gives `None` when the input is closed.
pub fn read_line(
    next_byte: &mut dyn FnMut() -> io::Result<Option<u8>>,
    out: &mut dyn Write,
    history: &mut History,
    settings: &Settings<'_>,
) -> io::Result<Read> {
    let mut editor = Editor::default();
    let mut decoder = KeyDecoder::default();
    let mut screen = Screen::default();
    history.reset();
    let draw = |out: &mut dyn Write, screen: &mut Screen, editor: &Editor| -> io::Result<()> {
        let text = editor.text();
        let styled = if settings.colour {
            highlight(settings.tree, &text)
        } else {
            text
        };
        refresh(
            out,
            screen,
            settings.prompt,
            &styled,
            editor.len(),
            editor.cursor(),
            settings.cols,
        )
    };
    draw(out, &mut screen, &editor)?;
    // a Ctrl-C just pressed, with no other key since: the next one leaves
    let mut armed = false;
    loop {
        let Some(byte) = next_byte()? else {
            return Ok(Read::Eof);
        };
        let Some(key) = decoder.push(byte) else {
            continue;
        };
        if key != Key::Interrupt {
            armed = false;
        }
        match key {
            Key::Enter => {
                // below the last row of the line, so what comes next starts on a clean one
                let at = settings.prompt.width + editor.cursor();
                let end = settings.prompt.width + editor.len();
                let down = (end / settings.cols.max(2)).saturating_sub(at / settings.cols.max(2));
                if down > 0 {
                    write!(out, "\x1b[{down}B")?;
                }
                write!(out, "\r\n")?;
                out.flush()?;
                let line = editor.text();
                history.remember(&line);
                return Ok(Read::Line(line));
            }
            Key::Eof if editor.is_empty() => {
                write!(out, "\r\n")?;
                out.flush()?;
                return Ok(Read::Eof);
            }
            // the second Ctrl-C in a row leaves, as Ctrl-D on an empty line does
            Key::Interrupt if armed => {
                write!(out, "^C\r\n")?;
                out.flush()?;
                return Ok(Read::Eof);
            }
            Key::Interrupt => {
                let hint = "(Ctrl-C again leaves the shell)";
                if settings.colour {
                    write!(out, "^C  \x1b[2m{hint}\x1b[0m\r\n")?;
                } else {
                    write!(out, "^C  {hint}\r\n")?;
                }
                editor.clear();
                history.reset();
                screen = Screen::default();
                armed = true;
                draw(out, &mut screen, &editor)?;
                continue;
            }
            Key::Up => {
                if let Some(line) = history.older(&editor.text()).map(str::to_owned) {
                    editor.set(&line);
                }
            }
            Key::Down => {
                if let Some(line) = history.newer() {
                    editor.set(&line);
                }
            }
            Key::Tab => {
                let completion = complete(settings.tree, &editor.before_cursor());
                if let Tabbed::List(candidates) = apply_completion(&mut editor, completion) {
                    // the candidates under the line, then the prompt again below them
                    let down = ((settings.prompt.width + editor.len()) / settings.cols.max(2))
                        .saturating_sub((settings.prompt.width + editor.cursor()) / settings.cols.max(2));
                    if down > 0 {
                        write!(out, "\x1b[{down}B")?;
                    }
                    write!(out, "\r\n{}\r\n", in_rows(&candidates, settings.cols))?;
                    screen = Screen::default();
                }
            }
            Key::Clear => {
                write!(out, "{}", settings.clear)?;
                screen = Screen::default();
            }
            Key::Unknown => continue,
            other => editor.apply(other),
        }
        draw(out, &mut screen, &editor)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<Key> {
        let mut decoder = KeyDecoder::default();
        bytes.iter().filter_map(|b| decoder.push(*b)).collect()
    }

    #[test]
    fn letters_enter_and_the_control_keys() {
        assert_eq!(decode(b"ab\r"), [Key::Char('a'), Key::Char('b'), Key::Enter]);
        assert_eq!(decode(&[0x7f, 0x08]), [Key::Backspace, Key::Backspace]);
        assert_eq!(
            decode(&[0x03, 0x04, 0x01, 0x05, 0x15, 0x17, 0x0b, 0x0c, 0x09]),
            [
                Key::Interrupt,
                Key::Eof,
                Key::Home,
                Key::End,
                Key::KillToStart,
                Key::KillWord,
                Key::KillToEnd,
                Key::Clear,
                Key::Tab
            ]
        );
    }

    #[test]
    fn the_arrows_in_both_forms_a_terminal_sends() {
        assert_eq!(
            decode(b"\x1b[A\x1b[B\x1b[C\x1b[D"),
            [Key::Up, Key::Down, Key::Right, Key::Left]
        );
        assert_eq!(
            decode(b"\x1bOA\x1bOB\x1bOC\x1bOD"),
            [Key::Up, Key::Down, Key::Right, Key::Left]
        );
    }

    #[test]
    fn home_end_delete_and_the_word_moves() {
        assert_eq!(
            decode(b"\x1b[H\x1b[F\x1b[1~\x1b[4~\x1b[3~"),
            [Key::Home, Key::End, Key::Home, Key::End, Key::Delete]
        );
        assert_eq!(
            decode(b"\x1b[1;5C\x1b[1;5D\x1bf\x1bb"),
            [Key::WordRight, Key::WordLeft, Key::WordRight, Key::WordLeft]
        );
    }

    #[test]
    fn a_character_of_several_bytes_is_one_key() {
        assert_eq!(decode("naïveté".as_bytes()).len(), 7);
        assert_eq!(decode("é".as_bytes()), [Key::Char('é')]);
        assert_eq!(decode("€".as_bytes()), [Key::Char('€')]);
        assert_eq!(decode("😀".as_bytes()), [Key::Char('😀')]);
        // a stray continuation byte is nothing, and the next key is read afresh
        assert_eq!(decode(&[0x80, b'a']), [Key::Unknown, Key::Char('a')]);
    }

    #[test]
    fn an_unknown_sequence_is_one_key_that_does_nothing() {
        assert_eq!(decode(b"\x1b[x"), [Key::Unknown]);
        assert_eq!(decode(b"\x1bz"), [Key::Unknown]);
        // after an unknown sequence the decoder is clean
        assert_eq!(decode(b"\x1b[99Zq"), [Key::Unknown, Key::Char('q')]);
        assert_eq!(decode(b"\x1b[24;80Rq"), [Key::Unknown, Key::Char('q')]);
    }

    #[test]
    fn a_lone_escape_gives_way_to_the_control_key_after_it() {
        assert_eq!(decode(b"\x1b\r"), [Key::Enter]);
        assert_eq!(decode(b"\x1b\x03"), [Key::Interrupt]);
        assert_eq!(decode(b"\x1b\t"), [Key::Tab]);
        // a second escape starts a sequence of its own
        assert_eq!(decode(b"\x1b\x1b[A"), [Key::Up]);
        // Alt-Backspace takes the word before the cursor
        assert_eq!(decode(b"\x1b\x7f"), [Key::KillWord]);
        // a sequence cut short by a control key or a character: that key is read
        assert_eq!(decode(b"\x1b[1\r"), [Key::Enter]);
        assert_eq!(decode(b"\x1b[\x03"), [Key::Interrupt]);
        assert_eq!(decode("\x1b[é".as_bytes()), [Key::Char('é')]);
        assert_eq!(decode(b"\x1bO\r"), [Key::Enter]);
    }

    #[test]
    fn a_broken_character_loses_no_key_typed_after_it() {
        // a lead byte with an ASCII key after it: the key is read
        assert_eq!(decode(&[0xc3, b'a']), [Key::Char('a')]);
        // a Latin-1 "ñ" (one byte that reads as the lead of four) takes none of the next keys
        assert_eq!(decode(&[0xf1, b'a', b'b']), [Key::Char('a'), Key::Char('b')]);
        assert_eq!(decode(&[0xe2, 0x82, b'\r']), [Key::Enter]);
        // a lead byte after a broken one starts a character of its own
        assert_eq!(decode(&[0xc3, 0xc3, 0xb1]), [Key::Char('ñ')]);
        assert_eq!(decode(&[0xc3, 0x1b, b'[', b'A']), [Key::Up]);
    }

    #[test]
    fn a_line_stops_growing_at_its_limit() {
        let mut e = Editor::default();
        for _ in 0..MAX_LINE + 10 {
            e.insert('x');
        }
        assert_eq!(e.len(), MAX_LINE);
        // what is in it can still be edited
        e.backspace();
        e.insert('y');
        assert_eq!(e.len(), MAX_LINE);
        assert!(e.text().ends_with('y'));
    }

    #[test]
    fn the_line_edits_where_the_cursor_is() {
        let mut e = Editor::default();
        for c in "user add".chars() {
            e.insert(c);
        }
        e.home();
        e.right();
        e.insert('X');
        assert_eq!(e.text(), "uXser add");
        e.backspace();
        e.delete();
        assert_eq!(e.text(), "uer add");
        e.end();
        e.left();
        e.left();
        e.kill_to_end();
        assert_eq!(e.text(), "uer a");
        e.kill_to_start();
        assert!(e.is_empty() && e.cursor() == 0);
        e.set("crème brûlée");
        e.left();
        e.backspace();
        assert_eq!(e.text(), "crème brûle");
    }

    #[test]
    fn words_are_moved_over_and_killed() {
        let mut e = Editor::default();
        e.set("token create  --email a@example.com");
        e.word_left();
        assert_eq!(e.cursor(), 22, "the start of the address");
        e.word_left();
        assert_eq!(e.before_cursor(), "token create  ");
        e.word_right();
        assert_eq!(e.before_cursor(), "token create  --email");
        e.kill_word();
        assert_eq!(e.text(), "token create   a@example.com");
        e.home();
        e.word_right();
        assert_eq!(e.before_cursor(), "token");
    }

    #[test]
    fn the_history_walks_back_and_returns_the_draft() {
        let mut h = History::default();
        h.remember("access list");
        h.remember("status");
        h.remember("status");
        h.remember("   ");
        assert_eq!(h.older("tok"), Some("status"));
        assert_eq!(h.older("tok"), Some("access list"));
        assert_eq!(h.older("tok"), None, "the oldest stays");
        assert_eq!(h.newer().as_deref(), Some("status"));
        assert_eq!(h.newer().as_deref(), Some("tok"), "the end brings the draft back");
        assert_eq!(h.newer(), None, "not walking any more");
    }

    #[test]
    fn an_empty_history_has_nothing_to_walk() {
        let mut h = History::default();
        assert_eq!(h.older("x"), None);
        assert_eq!(h.newer(), None);
    }

    #[test]
    fn the_history_is_capped() {
        let mut h = History::default();
        for n in 0..(HISTORY_LIMIT + 20) {
            h.remember(&format!("status {n}"));
        }
        assert_eq!(h.entries.len(), HISTORY_LIMIT);
        assert_eq!(h.entries[0], "status 20");
    }

    fn tree() -> Node {
        let leaf = |name: &str, flags: &[&str]| Node {
            name: name.to_owned(),
            hidden: false,
            flags: flags.iter().map(|f| (*f).to_owned()).collect(),
            subs: Vec::new(),
        };
        let group = |name: &str, subs: Vec<Node>| Node {
            name: name.to_owned(),
            hidden: false,
            flags: vec!["--help".to_owned()],
            subs,
        };
        Node {
            name: "mercury".to_owned(),
            hidden: false,
            flags: vec!["--help".to_owned()],
            subs: vec![
                group(
                    "user",
                    vec![
                        leaf("add", &["--email", "--name", "--role", "--help"]),
                        leaf("grant", &["--email", "--projects", "--approver", "--help"]),
                        leaf("disable", &["--email", "--help"]),
                    ],
                ),
                group("token", vec![leaf("create", &["--email", "--label", "--help"])]),
                leaf("status", &["--check", "--since", "--help"]),
                Node {
                    hidden: true,
                    ..leaf("shell", &["--help"])
                },
            ],
        }
    }

    fn words(c: Option<Completion>) -> Vec<String> {
        c.map(|c| c.candidates).unwrap_or_default()
    }

    #[test]
    fn tab_offers_the_commands_flags_and_nothing_hidden() {
        let t = tree();
        assert_eq!(
            words(complete(&t, "")),
            ["status", "token", "user"],
            "`shell` is hidden"
        );
        assert_eq!(words(complete(&t, "us")), ["user"]);
        assert_eq!(words(complete(&t, "user ")), ["add", "disable", "grant"]);
        assert_eq!(words(complete(&t, "user d")), ["disable"]);
        assert_eq!(words(complete(&t, "user add --e")), ["--email"]);
        assert_eq!(
            words(complete(&t, "user add --")),
            ["--email", "--help", "--name", "--role"]
        );
    }

    #[test]
    fn a_flag_already_in_the_line_is_not_offered_again() {
        let t = tree();
        assert_eq!(
            words(complete(&t, "user add --email a@example.com --")),
            ["--help", "--name", "--role"]
        );
        assert_eq!(
            words(complete(&t, "user add --email a@example.com ")),
            ["--name", "--role"]
        );
    }

    #[test]
    fn tab_after_a_flag_that_wants_a_value_offers_nothing() {
        let t = tree();
        assert_eq!(complete(&t, "user add --email "), None);
        assert_eq!(complete(&t, "user add --email=a"), None);
        assert_eq!(complete(&t, "user add --name \"An"), None, "inside a quote");
        assert_eq!(complete(&t, "nonsense "), None);
    }

    #[test]
    fn a_completion_says_where_the_word_begins() {
        let t = tree();
        let c = complete(&t, "user add --e").unwrap();
        assert_eq!(c.start, 9);
        assert_eq!(complete(&t, "us").unwrap().start, 0);
        assert_eq!(complete(&t, "user ").unwrap().start, 5);
    }

    #[test]
    fn one_candidate_completes_with_a_space_and_several_as_far_as_they_agree() {
        let t = tree();
        let mut e = Editor::default();
        e.set("us");
        let before = e.before_cursor();
        assert_eq!(apply_completion(&mut e, complete(&t, &before)), Tabbed::Completed);
        assert_eq!(e.text(), "user ");

        e.set("user ");
        let before = e.text();
        assert_eq!(
            apply_completion(&mut e, complete(&t, &before)),
            Tabbed::List(vec!["add".into(), "disable".into(), "grant".into()])
        );
        assert_eq!(e.text(), "user ", "nothing to add: they are listed");

        e.set("user add --");
        let before = e.text();
        assert!(matches!(
            apply_completion(&mut e, complete(&t, &before)),
            Tabbed::List(_)
        ));

        let mut e = Editor::default();
        e.set("t");
        // `token` is the only one that begins with t
        assert_eq!(apply_completion(&mut e, complete(&t, "t")), Tabbed::Completed);
        assert_eq!(e.text(), "token ");
        e.clear();
        assert_eq!(apply_completion(&mut e, None), Tabbed::Nothing);
    }

    #[test]
    fn candidates_that_share_a_start_complete_to_it() {
        let candidates = vec!["--email".to_owned(), "--emails".to_owned()];
        let mut e = Editor::default();
        e.set("add --e");
        assert_eq!(
            apply_completion(&mut e, Some(Completion { start: 4, candidates })),
            Tabbed::Completed
        );
        assert_eq!(e.text(), "add --email");
    }

    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for skip in chars.by_ref() {
                    if skip == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn colouring_takes_nothing_away_and_adds_nothing() {
        let t = tree();
        for line in [
            "",
            "user add --email a@example.com --name \"Ann Lee\" --role pm",
            "  user   grant --approver ",
            "nonsense --email x",
            "user add --name 'unclosed",
            "status --since=2026-10-01",
            "token create --nope",
        ] {
            assert_eq!(strip(&highlight(&t, line)), line, "{line:?}");
        }
    }

    #[test]
    fn commands_flags_and_quotes_get_their_colours_and_mistakes_red() {
        let t = tree();
        let shown = highlight(&t, "user add --email a@example.com --name \"Ann Lee\"");
        assert!(shown.contains("\x1b[1;32muser\x1b[0m"), "{shown:?}");
        assert!(shown.contains("\x1b[1;32madd\x1b[0m"), "{shown:?}");
        assert!(shown.contains("\x1b[36m--email\x1b[0m"), "{shown:?}");
        assert!(shown.contains("\x1b[33m\"Ann Lee\"\x1b[0m"), "{shown:?}");
        // a value is left as it is
        assert!(shown.contains(" a@example.com "), "{shown:?}");
        assert!(highlight(&t, "usr").contains("\x1b[31musr\x1b[0m"));
        assert!(highlight(&t, "user add --bogus").contains("\x1b[31m--bogus\x1b[0m"));
        assert!(highlight(&t, "status --since=2026-10-01").contains("\x1b[36m--since\x1b[0m=2026-10-01"));
    }

    fn drawn(line: &str, cursor: usize, cols: usize, row: usize, prompt: &Prompt) -> (String, Screen) {
        let mut out = Vec::new();
        let mut screen = Screen { cursor_row: row };
        refresh(
            &mut out,
            &mut screen,
            prompt,
            line,
            line.chars().count(),
            cursor,
            cols,
        )
        .unwrap();
        (String::from_utf8(out).unwrap(), screen)
    }

    #[test]
    fn a_line_that_fits_is_drawn_from_the_start_with_the_cursor_where_it_stands() {
        let p = Prompt::plain("m> ");
        let (out, screen) = drawn("status", 6, 80, 0, &p);
        assert_eq!(out, "\r\x1b[Jm> status\r\x1b[9C");
        assert_eq!(screen.cursor_row, 0);
        // the cursor in the middle of the line
        let (out, _) = drawn("status", 2, 80, 0, &p);
        assert!(out.ends_with("\r\x1b[5C"), "{out:?}");
        // at the very start, nothing to move over but the prompt
        let (out, _) = drawn("", 0, 80, 0, &p);
        assert_eq!(out, "\r\x1b[Jm> \r\x1b[3C");
    }

    #[test]
    fn a_line_that_wraps_is_redrawn_from_its_first_row() {
        let p = Prompt::plain("> ");
        // 10 columns: the prompt and 14 characters take two rows
        let (out, screen) = drawn("abcdefghijklmn", 14, 10, 0, &p);
        assert_eq!(screen.cursor_row, 1);
        assert!(out.ends_with("\r\x1b[6C"), "{out:?}");
        // drawn again from the second row: up one first
        let (out, _) = drawn("abcdefghijklmno", 15, 10, 1, &p);
        assert!(out.starts_with("\x1b[1A\r\x1b[J"), "{out:?}");
    }

    #[test]
    fn a_line_ending_exactly_at_the_edge_moves_the_cursor_down() {
        let p = Prompt::plain("> ");
        let (out, screen) = drawn("abcdefgh", 8, 10, 0, &p);
        assert!(out.contains("abcdefgh\r\n"), "{out:?}");
        assert_eq!(screen.cursor_row, 1);
    }

    #[test]
    fn the_cursor_back_in_an_earlier_row_of_a_wrapped_line() {
        let p = Prompt::plain("> ");
        let (out, screen) = drawn("abcdefghijklmn", 2, 10, 0, &p);
        // the line ends on row 1 and the cursor is on row 0: up one, then over
        assert!(out.contains("\x1b[1A\r\x1b[4C"), "{out:?}");
        assert_eq!(screen.cursor_row, 0);
    }

    /// Types the keys in `script` at a prompt, and says what was read and what was drawn.
    fn type_in(script: &[u8], history: &mut History) -> (Read, String) {
        let t = tree();
        let prompt = Prompt::plain("m> ");
        let settings = Settings {
            prompt: &prompt,
            tree: &t,
            colour: false,
            cols: 80,
            clear: "\x1b[H\x1b[2J",
        };
        let mut bytes = script.iter().copied();
        let mut out = Vec::new();
        let read = read_line(&mut || Ok(bytes.next()), &mut out, history, &settings).unwrap();
        (read, String::from_utf8(out).unwrap())
    }

    #[test]
    fn a_typed_line_is_returned_on_enter_and_remembered() {
        let mut h = History::default();
        let (read, out) = type_in(b"status\r", &mut h);
        assert_eq!(read, Read::Line("status".into()));
        assert!(out.ends_with("\r\n"), "{out:?}");
        assert_eq!(h.older(""), Some("status"));
    }

    #[test]
    fn up_brings_back_an_earlier_line_and_it_can_be_edited() {
        let mut h = History::default();
        h.remember("access list");
        h.remember("status --check");
        let (read, _) = type_in(b"\x1b[A\x1b[A\x1b[B\x7f\x7f\r", &mut h);
        assert_eq!(read, Read::Line("status --che".into()));
    }

    #[test]
    fn down_past_the_newest_brings_back_what_was_being_typed() {
        let mut h = History::default();
        h.remember("status");
        let (read, _) = type_in(b"tok\x1b[A\x1b[B\r", &mut h);
        assert_eq!(read, Read::Line("tok".into()));
    }

    #[test]
    fn tab_completes_and_the_line_is_read_whole() {
        let mut h = History::default();
        let (read, _) = type_in(b"us\tad\t--em\ta@example.com\r", &mut h);
        assert_eq!(read, Read::Line("user add --email a@example.com".into()));
    }

    #[test]
    fn tab_with_several_candidates_lists_them_below_the_line() {
        let mut h = History::default();
        let (read, out) = type_in(b"user \t\r", &mut h);
        assert_eq!(read, Read::Line("user ".into()));
        assert!(out.contains("add  disable  grant"), "{out:?}");
    }

    #[test]
    fn ctrl_d_leaves_on_an_empty_line_and_deletes_otherwise() {
        let mut h = History::default();
        assert_eq!(type_in(&[0x04], &mut h).0, Read::Eof);
        // in the middle of a line it deletes the character under the cursor
        let (read, _) = type_in(b"stxatus\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\x04\r", &mut h);
        assert_eq!(read, Read::Line("status".into()));
    }

    #[test]
    fn ctrl_c_drops_the_line_and_a_new_one_is_read() {
        let mut h = History::default();
        let (read, out) = type_in(b"user add\x03status\r", &mut h);
        assert_eq!(read, Read::Line("status".into()));
        assert!(out.contains("^C"), "{out:?}");
        assert!(
            out.contains("Ctrl-C again leaves"),
            "the way out is said: {out:?}"
        );
    }

    #[test]
    fn two_ctrl_c_in_a_row_leave() {
        let mut h = History::default();
        assert_eq!(type_in(b"\x03\x03", &mut h).0, Read::Eof);
        // with a line being typed: the first drops it, the second leaves
        assert_eq!(type_in(b"user add\x03\x03", &mut h).0, Read::Eof);
    }

    #[test]
    fn a_key_between_two_ctrl_c_keeps_the_shell_open() {
        let mut h = History::default();
        let (read, _) = type_in(b"\x03s\x03status\r", &mut h);
        assert_eq!(read, Read::Line("status".into()));
        // an arrow counts as a key too
        let (read, _) = type_in(b"\x03\x1b[D\x03st\r", &mut h);
        assert_eq!(read, Read::Line("st".into()));
    }

    #[test]
    fn a_closed_input_is_the_end() {
        let mut h = History::default();
        assert_eq!(type_in(b"sta", &mut h).0, Read::Eof);
    }

    #[test]
    fn unknown_keys_change_nothing_and_ctrl_l_clears_what_it_is_told_to() {
        let mut h = History::default();
        let (read, out) = type_in(b"sta\x1b[99Z\x0ctus\r", &mut h);
        assert_eq!(read, Read::Line("status".into()));
        assert!(out.contains("\x1b[H\x1b[2J"), "{out:?}");
    }

    #[test]
    fn a_line_with_accents_is_one_character_per_key() {
        let mut h = History::default();
        let (read, _) = type_in("user add --name Renée\r".as_bytes(), &mut h);
        assert_eq!(read, Read::Line("user add --name Renée".into()));
    }

    #[test]
    fn the_candidates_are_laid_out_in_rows_no_wider_than_the_terminal() {
        let words: Vec<String> = ["aaaa", "bbbb", "cccc"].iter().map(|w| (*w).to_owned()).collect();
        assert_eq!(in_rows(&words, 80), "aaaa  bbbb  cccc");
        assert_eq!(in_rows(&words, 10), "aaaa  bbbb\r\ncccc");
    }
}
