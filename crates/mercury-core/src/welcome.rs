//! The welcome screen of the admin shell: a framed header with the planet, the version and what is
//! going on now (people and their tokens, the probes, today's spend against its budget, the alerts)
//! and the keys the prompt understands. In a terminal tall enough it is kept at the top while the
//! commands scroll under it, and it is drawn again after each command, since a command may have
//! changed what it says.
//!
//! The data is read with the functions `status` and `access list` use, which write no audit row, so
//! drawing the header again and again leaves the chain alone. Drawing is pure: a header and a width
//! in, lines out.

use std::path::Path;

use chrono::{DateTime, Utc};
use mercury_schema::Role;

use crate::admin;
use crate::db::{self, Db};
use crate::probes::ProbeStatus;
use crate::status;

/// How a probe stands, as a coloured dot beside its project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Healthy,
    Stale,
    Quarantined,
}

impl From<ProbeStatus> for Mark {
    fn from(status: ProbeStatus) -> Self {
        match status {
            ProbeStatus::Healthy => Mark::Healthy,
            ProbeStatus::Stale => Mark::Stale,
            ProbeStatus::Quarantined => Mark::Quarantined,
        }
    }
}

/// The people the database holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct People {
    pub total: usize,
    pub disabled: usize,
    /// Tokens that work now, of people who are not disabled.
    pub active_tokens: usize,
    /// People who can approve a held answer somewhere.
    pub approvers: usize,
}

/// Today's spend, in dollars, and the day's budget when `models.yaml` can be read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spend {
    pub spent: f64,
    pub budget: Option<f64>,
}

/// Everything the header says.
#[derive(Debug, Clone, Default)]
pub struct Header {
    pub version: String,
    pub database: String,
    pub host: Option<String>,
    /// `None` when the database can't be read.
    pub people: Option<People>,
    pub probes: Vec<(String, Mark)>,
    pub spend: Option<Spend>,
    /// The alerts that hold, each as `status` says it.
    pub alerts: Vec<String>,
    /// Whether the alerts could be read at all.
    pub alerts_known: bool,
    pub judge: Option<String>,
}

/// Reads what the header says, now. What can't be read is left out and drawn as unknown; nothing
/// here fails, since a header that stops the shell opening would be worse than a header that
/// doesn't know.
pub async fn gather(db: &Db, config_dir: &Path, database: &str, now: DateTime<Utc>) -> Header {
    let mut header = Header {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        database: database.to_owned(),
        host: std::env::var("HOSTNAME").ok().filter(|host| !host.is_empty()),
        ..Header::default()
    };
    if let Ok(access) = admin::access(db, None, None).await {
        let disabled = access.people.iter().filter(|p| p.disabled_at.is_some()).count();
        let active_tokens = access
            .tokens
            .iter()
            .filter(|token| {
                token.revoked_at.is_none()
                    && db::parse_time(&token.expires_at).is_some_and(|expires| expires > now)
                    && access
                        .people
                        .iter()
                        .any(|p| p.email == token.email && p.disabled_at.is_none())
            })
            .count();
        let approvers = access
            .people
            .iter()
            .filter(|p| {
                p.disabled_at.is_none()
                    && matches!(p.role, Role::Pm | Role::TechLead)
                    && p.grants.iter().any(|g| g.approver)
            })
            .count();
        header.people = Some(People {
            total: access.people.len(),
            disabled,
            active_tokens,
            approvers,
        });
    }
    if let Ok(read) = status::read(db, config_dir, now, None).await {
        header.probes = read
            .probes
            .iter()
            .map(|probe| (probe.project.clone(), Mark::from(probe.status)))
            .collect();
        header.alerts = read.alerts.iter().map(|alert| alert.line()).collect();
        header.alerts_known = true;
        let budget = read
            .models
            .as_ref()
            .ok()
            .map(|models| models.budgets.global_daily_usd.get());
        header.judge = read
            .models
            .as_ref()
            .ok()
            .map(|models| models.roles.judge.model.as_str().to_owned());
        header.spend = Some(Spend {
            spent: read.everyone,
            budget,
        });
    }
    header
}

// ---------------------------------------------------------------------------------------------
// Drawing

/// How the header is drawn.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub colour: bool,
    /// Box-drawing characters and block glyphs; plain ASCII when false.
    pub unicode: bool,
}

/// The narrowest terminal the framed header is drawn in; under it, two plain lines.
pub const FRAME_MIN: usize = 44;
/// From this width the planet stands in the frame, beside the text; under it the text has the frame
/// to itself.
pub const LOGO_MIN: usize = 62;
/// The widest the frame grows.
pub const MAX_WIDTH: usize = 100;

/// How many lines the framed header takes.
pub const HEIGHT: usize = 9;

const LOGO_UNICODE: [&str; 5] = [
    "    ▄▄▄▄▄▄    ",
    "  ▄████████▄  ",
    "━━██████████━━",
    "  ▀████████▀  ",
    "    ▀▀▀▀▀▀    ",
];
const LOGO_ASCII: [&str; 5] = [
    "    ______    ",
    "  .'      '.  ",
    "==(        )==",
    "  '.______.'  ",
    "              ",
];
/// The colours of the planet's rows, light to dark, and of its ring.
const BODY: [&str; 5] = ["38;5;105", "38;5;99", "38;5;63", "38;5;62", "38;5;61"];
const RING: &str = "38;5;43";
const LOGO_WIDTH: usize = 14;

struct Glyphs {
    top_left: char,
    top_right: char,
    bottom_left: char,
    bottom_right: char,
    horizontal: char,
    vertical: char,
    tee_left: char,
    tee_right: char,
    dot: char,
    ok: char,
    warn: char,
    bar_full: char,
    bar_empty: char,
    arrows: &'static str,
    sep: char,
}

const UNICODE: Glyphs = Glyphs {
    top_left: '╭',
    top_right: '╮',
    bottom_left: '╰',
    bottom_right: '╯',
    horizontal: '─',
    vertical: '│',
    tee_left: '├',
    tee_right: '┤',
    dot: '●',
    ok: '✓',
    warn: '▲',
    bar_full: '█',
    bar_empty: '░',
    arrows: "↑↓",
    sep: '·',
};

const ASCII: Glyphs = Glyphs {
    top_left: '+',
    top_right: '+',
    bottom_left: '+',
    bottom_right: '+',
    horizontal: '-',
    vertical: '|',
    tee_left: '+',
    tee_right: '+',
    dot: '*',
    ok: 'v',
    warn: '!',
    bar_full: '#',
    bar_empty: '-',
    arrows: "Up/Down",
    sep: '|',
};

const RESET: &str = "\x1b[0m";

/// A line being built to a width: text in styles, and how many cells it takes without them.
struct Cells {
    text: String,
    used: usize,
    max: usize,
    colour: bool,
}

impl Cells {
    fn new(max: usize, colour: bool) -> Self {
        Cells {
            text: String::new(),
            used: 0,
            max,
            colour,
        }
    }

    fn room(&self) -> usize {
        self.max - self.used
    }

    /// Adds `text` in `style` (an SGR code, or none), cut with an ellipsis if it doesn't fit.
    fn put(&mut self, text: &str, style: &str) {
        let room = self.room();
        let count = text.chars().count();
        let shown: String = if count <= room {
            text.to_owned()
        } else if room == 0 {
            return;
        } else {
            text.chars().take(room - 1).chain(['…']).collect()
        };
        self.used += shown.chars().count();
        if self.colour && !style.is_empty() {
            self.text.push_str(&format!("\x1b[{style}m{shown}{RESET}"));
        } else {
            self.text.push_str(&shown);
        }
    }

    /// Whether `width` more cells fit.
    fn fits(&self, width: usize) -> bool {
        width <= self.room()
    }

    /// The line, padded with spaces to its width.
    fn finish(mut self) -> String {
        let pad = self.room();
        self.text.extend(std::iter::repeat_n(' ', pad));
        self.text
    }
}

/// `one` for 1 and `many` for any other number.
fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

fn dollars(amount: f64) -> String {
    if amount >= 1.0 {
        format!("${amount:.2}")
    } else {
        format!("${amount:.4}")
    }
}

/// The header as lines, `width` cells wide each: the framed one when the terminal is wide enough,
/// two plain lines when it isn't.
pub fn render(header: &Header, width: usize, look: Look) -> Vec<String> {
    if width < FRAME_MIN {
        return compact(header, width, look);
    }
    let width = width.min(MAX_WIDTH);
    let g = if look.unicode { &UNICODE } else { &ASCII };
    let edge = |text: &str| -> String {
        if look.colour {
            format!("\x1b[90m{text}{RESET}")
        } else {
            text.to_owned()
        }
    };
    let inner = width - 2;
    let mut lines = Vec::with_capacity(HEIGHT);

    // the top edge carries the name on the left and the version on the right
    let mut top = Cells::new(inner, look.colour);
    top.put(&g.horizontal.to_string(), "90");
    top.put(" Mercury admin ", "1;38;5;105");
    let version = format!(" v{} ", header.version);
    let fill = top.room().saturating_sub(version.chars().count() + 1);
    top.put(&g.horizontal.to_string().repeat(fill), "90");
    top.put(&version, "2");
    top.put(&g.horizontal.to_string(), "90");
    lines.push(format!(
        "{}{}{}",
        edge(&g.top_left.to_string()),
        top.finish(),
        edge(&g.top_right.to_string())
    ));

    let logo = if look.unicode { &LOGO_UNICODE } else { &LOGO_ASCII };
    let with_logo = width >= LOGO_MIN;
    let text_width = if with_logo {
        inner - 1 - LOGO_WIDTH - 2 - 1
    } else {
        inner - 2
    };
    let rows = [
        greeting(header, text_width, look),
        people_row(header, text_width, look),
        probes_row(header, text_width, look, g),
        spend_row(header, text_width, look, g),
        alerts_row(header, text_width, look, g),
    ];
    for (n, text) in rows.into_iter().enumerate() {
        let mut row = Cells::new(inner, look.colour);
        row.put(" ", "");
        if with_logo {
            paint_logo(&mut row, logo[n], n);
            row.put("  ", "");
        }
        lines.push(format!(
            "{}{}{} {}",
            edge(&g.vertical.to_string()),
            row.text,
            text.finish(),
            edge(&g.vertical.to_string())
        ));
    }

    lines.push(format!(
        "{}{}{}",
        edge(&g.tee_left.to_string()),
        edge(&g.horizontal.to_string().repeat(inner)),
        edge(&g.tee_right.to_string())
    ));
    let mut tips = Cells::new(inner, look.colour);
    tips.put(" ", "");
    for (key, what) in [
        ("Tab", "completes"),
        (g.arrows, "history"),
        ("exit", "leaves"),
        ("help", "lists the commands"),
        ("Ctrl-L", "clears"),
    ] {
        if !tips.fits(key.chars().count() + what.chars().count() + 4) {
            break;
        }
        tips.put(key, "1;36");
        tips.put(&format!(" {what}  "), "2");
    }
    lines.push(format!(
        "{}{}{}",
        edge(&g.vertical.to_string()),
        tips.finish(),
        edge(&g.vertical.to_string())
    ));
    lines.push(format!(
        "{}{}{}",
        edge(&g.bottom_left.to_string()),
        edge(&g.horizontal.to_string().repeat(inner)),
        edge(&g.bottom_right.to_string())
    ));
    lines
}

fn paint_logo(row: &mut Cells, art: &str, n: usize) {
    for c in art.chars() {
        let style = match c {
            '━' | '=' => RING,
            ' ' => "",
            _ => BODY[n],
        };
        // the ring and the body are painted a cell at a time
        row.put(&c.to_string(), style);
    }
}

fn greeting(header: &Header, width: usize, look: Look) -> Cells {
    let mut row = Cells::new(width, look.colour);
    row.put("Mercury admin shell", "1");
    if let Some(judge) = &header.judge {
        row.put(&format!("   judge {judge}"), "2");
    }
    row
}

fn people_row(header: &Header, width: usize, look: Look) -> Cells {
    let mut row = Cells::new(width, look.colour);
    match &header.people {
        None => row.put("people: the database can't be read", "31"),
        Some(people) => {
            // the full words when they fit, shorter ones in a narrow frame
            let full = people_parts(people, false);
            let parts = if parts_width(&full) <= width {
                full
            } else {
                people_parts(people, true)
            };
            for (text, style) in parts {
                row.put(&text, style);
            }
        }
    }
    row
}

/// The people row as parts and their styles; `short` drops the words that can go.
fn people_parts(people: &People, short: bool) -> Vec<(String, &'static str)> {
    let mut parts = vec![
        (people.total.to_string(), "1"),
        (plural(people.total, " person", " people").to_owned(), ""),
    ];
    if people.disabled > 0 && !short {
        parts.push((format!(" ({} disabled)", people.disabled), "2"));
    }
    parts.push(("  ".to_owned(), ""));
    parts.push((people.active_tokens.to_string(), "1"));
    let tokens = if short {
        plural(people.active_tokens, " token  ", " tokens  ")
    } else {
        plural(people.active_tokens, " active token  ", " active tokens  ")
    };
    parts.push((tokens.to_owned(), ""));
    parts.push((people.approvers.to_string(), "1"));
    parts.push((plural(people.approvers, " approver", " approvers").to_owned(), ""));
    parts
}

fn parts_width(parts: &[(String, &'static str)]) -> usize {
    parts.iter().map(|(text, _)| text.chars().count()).sum()
}

fn probes_row(header: &Header, width: usize, look: Look, g: &Glyphs) -> Cells {
    let mut row = Cells::new(width, look.colour);
    if header.probes.is_empty() {
        row.put("no probe has registered", "2");
        return row;
    }
    for (shown, (project, mark)) in header.probes.iter().enumerate() {
        let left = header.probes.len() - shown;
        // room for this probe, and for a "+N" if more would follow it
        let needed = 2 + project.chars().count() + 2 + if left > 1 { 4 } else { 0 };
        if !row.fits(needed) {
            row.put(&format!("+{left}"), "2");
            break;
        }
        let colour = match mark {
            Mark::Healthy => "32",
            Mark::Stale => "33",
            Mark::Quarantined => "31",
        };
        row.put(&g.dot.to_string(), colour);
        row.put(&format!(" {project}  "), "");
    }
    row
}

fn spend_row(header: &Header, width: usize, look: Look, g: &Glyphs) -> Cells {
    let mut row = Cells::new(width, look.colour);
    let Some(spend) = header.spend else {
        row.put("spend: not known", "2");
        return row;
    };
    row.put("today ", "2");
    row.put(&dollars(spend.spent), "1");
    let Some(budget) = spend.budget.filter(|budget| *budget > 0.0) else {
        return row;
    };
    row.put(&format!(" of {}  ", dollars(budget)), "");
    let share = spend.spent / budget;
    let cells = 10_usize;
    let full = ((share * cells as f64).floor().max(0.0) as usize).min(cells);
    let colour = if share >= 1.0 {
        "31"
    } else if share >= 0.8 {
        "33"
    } else {
        "32"
    };
    if row.fits(cells + 6) {
        row.put(&g.bar_full.to_string().repeat(full), colour);
        row.put(&g.bar_empty.to_string().repeat(cells - full), "90");
        row.put(&format!(" {}%", (share * 100.0).floor()), "");
    }
    row
}

fn alerts_row(header: &Header, width: usize, look: Look, g: &Glyphs) -> Cells {
    let mut row = Cells::new(width, look.colour);
    if !header.alerts_known {
        row.put("alerts: not known", "2");
    } else if let Some(first) = header.alerts.first() {
        row.put(&format!("{} ", g.warn), "1;31");
        let more = header.alerts.len() - 1;
        let tail = if more > 0 {
            format!("  +{more} more")
        } else {
            String::new()
        };
        let room = row.room().saturating_sub(tail.chars().count());
        let shown: String = if first.chars().count() > room {
            first.chars().take(room.saturating_sub(1)).chain(['…']).collect()
        } else {
            first.clone()
        };
        row.put(&shown, "31");
        row.put(&tail, "2");
    } else {
        row.put(&format!("{} ", g.ok), "1;32");
        row.put("no alerts", "32");
    }
    row
}

/// Two plain lines, for a terminal too narrow for the frame.
fn compact(header: &Header, width: usize, look: Look) -> Vec<String> {
    let g = if look.unicode { &UNICODE } else { &ASCII };
    let mut first = Cells::new(width, look.colour);
    first.put("Mercury admin", "1;38;5;105");
    first.put(&format!(" v{}", header.version), "2");
    if let Some(people) = &header.people {
        first.put(
            &format!(
                "  {} {} {}",
                g.sep,
                people.total,
                plural(people.total, "person", "people")
            ),
            "",
        );
    }
    let healthy = header
        .probes
        .iter()
        .filter(|(_, mark)| *mark == Mark::Healthy)
        .count();
    if !header.probes.is_empty() {
        first.put(
            &format!(" {} {healthy}/{} probes", g.sep, header.probes.len()),
            "",
        );
    }
    if !header.alerts.is_empty() {
        first.put(&format!(" {} {} alert(s)", g.sep, header.alerts.len()), "31");
    }
    let mut second = Cells::new(width, look.colour);
    second.put("Tab completes, help lists, exit leaves", "2");
    vec![first.finish(), second.finish()]
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn sample() -> Header {
        Header {
            version: "0.0.0".into(),
            database: "/data/mercury.db".into(),
            host: Some("abc123".into()),
            people: Some(People {
                total: 8,
                disabled: 1,
                active_tokens: 5,
                approvers: 2,
            }),
            probes: vec![
                ("gamma".into(), Mark::Healthy),
                ("poison".into(), Mark::Healthy),
                ("delta".into(), Mark::Stale),
            ],
            spend: Some(Spend {
                spent: 0.0005,
                budget: Some(2.0),
            }),
            alerts: Vec::new(),
            alerts_known: true,
            judge: Some("typesafe/jev-1.13".into()),
        }
    }

    const COLOUR: Look = Look {
        colour: true,
        unicode: true,
    };
    const PLAIN: Look = Look {
        colour: false,
        unicode: true,
    };
    const ASCII_LOOK: Look = Look {
        colour: false,
        unicode: false,
    };

    #[test]
    fn the_frame_is_as_wide_as_asked_in_every_line_with_or_without_colour() {
        for look in [COLOUR, PLAIN, ASCII_LOOK] {
            for width in [44, 50, 61, 62, 80, 100] {
                let lines = render(&sample(), width, look);
                assert_eq!(lines.len(), HEIGHT);
                for line in &lines {
                    assert_eq!(strip(line).chars().count(), width, "{width} {:?}", strip(line));
                }
            }
        }
    }

    #[test]
    fn a_narrow_frame_leaves_the_planet_out_and_shortens_the_words() {
        let wide = render(&sample(), 80, PLAIN).join("\n");
        let narrow = render(&sample(), 50, PLAIN);
        assert_eq!(narrow.len(), HEIGHT, "still the frame, and still kept on top");
        let text = narrow.join("\n");
        assert!(wide.contains("██████"), "the planet stands in a wide frame");
        assert!(!text.contains("██████"), "{text}");
        assert!(text.contains("8 people"), "{text}");
        assert!(text.contains("5 tokens"), "{text}");
        assert!(text.contains("2 approvers"), "{text}");
        assert!(text.contains("exit leaves"), "the way out is always said: {text}");
    }

    #[test]
    fn it_never_grows_past_its_widest() {
        let lines = render(&sample(), 200, PLAIN);
        assert!(lines.iter().all(|l| l.chars().count() == MAX_WIDTH));
    }

    #[test]
    fn it_says_what_is_going_on() {
        let text = render(&sample(), 90, PLAIN).join("\n");
        for wanted in [
            "Mercury admin",
            "v0.0.0",
            "8 people (1 disabled)",
            "5 active tokens",
            "2 approvers",
            "gamma",
            "poison",
            "delta",
            "today $0.0005 of $2.00",
            "0%",
            "no alerts",
            "Tab completes",
            "exit leaves",
            "judge typesafe/jev-1.13",
        ] {
            assert!(text.contains(wanted), "{wanted:?} in\n{text}");
        }
    }

    #[test]
    fn an_alert_is_shown_in_red_with_how_many_more() {
        let mut header = sample();
        header.alerts = vec![
            "probe delta: silent for 4 days, last seen 2026-10-06 16:21 UTC".into(),
            "judge degraded".into(),
        ];
        let coloured = render(&header, 90, COLOUR).join("\n");
        assert!(coloured.contains("\x1b[1;31m▲ "), "{coloured:?}");
        let plain = strip(&coloured);
        assert!(plain.contains("probe delta: silent"), "{plain}");
        assert!(plain.contains("+1 more"), "{plain}");
        assert!(!plain.contains("no alerts"));
    }

    #[test]
    fn the_spend_bar_turns_amber_and_red_with_the_budget() {
        let mut header = sample();
        header.spend = Some(Spend {
            spent: 1.7,
            budget: Some(2.0),
        });
        let amber = render(&header, 90, COLOUR).join("\n");
        assert!(amber.contains("\x1b[33m████████"), "{amber:?}");
        header.spend = Some(Spend {
            spent: 2.5,
            budget: Some(2.0),
        });
        let red = render(&header, 90, COLOUR).join("\n");
        assert!(red.contains("\x1b[31m██████████"), "{red:?}");
        assert!(strip(&red).contains("125%"));
    }

    #[test]
    fn probes_are_dots_in_the_colour_of_their_state() {
        let mut header = sample();
        header.probes.push(("legacy".into(), Mark::Quarantined));
        let shown = render(&header, 100, COLOUR).join("\n");
        assert!(shown.contains("\x1b[32m●"));
        assert!(shown.contains("\x1b[33m●"));
        assert!(shown.contains("\x1b[31m●"));
    }

    #[test]
    fn many_probes_are_cut_with_a_count_instead_of_overflowing() {
        let mut header = sample();
        header.probes = (0..30).map(|n| (format!("project-{n}"), Mark::Healthy)).collect();
        for line in render(&header, 70, PLAIN) {
            assert_eq!(line.chars().count(), 70);
        }
        assert!(render(&header, 70, PLAIN).join("\n").contains('+'));
    }

    #[test]
    fn what_cannot_be_read_is_said_not_hidden() {
        let header = Header {
            version: "0.0.0".into(),
            ..Header::default()
        };
        let text = render(&header, 90, PLAIN).join("\n");
        assert!(text.contains("the database can't be read"), "{text}");
        assert!(text.contains("no probe has registered"), "{text}");
        assert!(text.contains("spend: not known"), "{text}");
        assert!(text.contains("alerts: not known"), "{text}");
    }

    #[test]
    fn the_ascii_look_has_nothing_but_ascii() {
        let text = render(&sample(), 90, ASCII_LOOK).join("\n");
        assert!(text.is_ascii(), "{text}");
    }

    #[test]
    fn a_narrow_terminal_gets_two_plain_lines() {
        let lines = render(&sample(), 40, PLAIN);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.chars().count() == 40));
        assert!(lines[0].contains("Mercury admin"), "{lines:?}");
    }

    #[test]
    fn colour_off_means_no_escape_sequence_at_all() {
        assert!(!render(&sample(), 90, PLAIN).join("").contains('\x1b'));
    }

    #[test]
    fn one_is_a_person_a_token_and_an_approver() {
        let mut header = sample();
        header.people = Some(People {
            total: 1,
            disabled: 0,
            active_tokens: 1,
            approvers: 1,
        });
        let text = render(&header, 90, PLAIN).join(
            "
",
        );
        assert!(text.contains("1 person"), "{text}");
        assert!(text.contains("1 active token "), "{text}");
        assert!(text.contains("1 approver "), "{text}");
        assert!(!text.contains("1 people"), "{text}");
    }

    #[test]
    fn dollars_are_shown_to_the_precision_that_matters() {
        assert_eq!(dollars(0.0005), "$0.0005");
        assert_eq!(dollars(2.0), "$2.00");
        assert_eq!(dollars(1.234), "$1.23");
    }
}
