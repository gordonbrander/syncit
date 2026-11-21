//! What syncit prints. Code prints a message the moment something happens;
//! nothing is collected. A message has a text form (`Display`) and a JSON
//! form (`Serialize`); the caller picks the stream, `json` picks the form.

use serde::Serialize;
use std::fmt::{self, Display};

/// Settings for the whole process, passed to everything.
pub struct Options {
    /// Print JSON Lines instead of text.
    pub json: bool,
    /// Run in full but write nothing to either vault.
    pub dry_run: bool,
}

/// Something that happened to one file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    pub kind: &'static str,
    pub vault: String,
    pub path: String,
    pub note: String,
    /// Text form only: `->` for our vault and `<-` for theirs in a run with
    /// a peer, empty in a command on one vault.
    #[serde(skip)]
    pub side: &'static str,
}

/// One entry of a file's history: a version, or a deletion.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Version {
    pub path: String,
    /// The content's hash; null for a deletion.
    pub hash: Option<String>,
    pub mtime: i64,
    /// The vaults that recorded it.
    pub nicknames: Vec<String>,
    /// Whether this is the path's current state.
    pub current: bool,
    /// Whether this vault can supply the content, so `restore` would work.
    pub available: bool,
}

/// One line of `ls`: a file, or a folder whose files are all in one state.
/// A state is `held`, `behind`, `missing`, `excluded`, `ignored` or `deleted`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Holding {
    /// A folder's path ends in `/`.
    pub path: String,
    /// How many files the line stands for: 1 for a file.
    pub files: usize,
    pub ours: &'static str,
    /// The other vault's state, when `ls` compares with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theirs: Option<&'static str>,
    /// The path's current version is a deletion; a vault that still has the
    /// file is behind.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

/// Milliseconds since the epoch as `2026-11-20 14:03Z`.
// ponytail: UTC only, by the days-to-civil arithmetic; use a time crate if
// local time is wanted.
fn utc(ms: i64) -> String {
    let (days, secs) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000) / 1000);
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, min) = (secs / 3600, secs % 3600 / 60);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}Z")
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Message {
    Event(Event),
    Version(Version),
    Holding(Holding),
    /// An open conflict. `message` names the conflict copies.
    Conflict {
        vault: String,
        path: String,
        message: String,
        #[serde(skip)]
        side: &'static str,
    },
    Setting {
        name: &'static str,
        value: String,
    },
    /// The vaults that recorded the current version of a file.
    Blame {
        path: String,
        nicknames: Vec<String>,
    },
    Info {
        message: String,
    },
    Warning {
        message: String,
    },
    Error {
        message: String,
    },
    Summary(Summary),
}

impl Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Whose vault, if there are two; then a letter and the path in the
        // vault, `.` for the vault itself.
        let head = |side: &str, letter: &str, path: &str| {
            let (space, path) = (
                if side.is_empty() { "" } else { " " },
                if path.is_empty() { "." } else { path },
            );
            format!("{side}{space}{letter} {path}")
        };
        match self {
            Message::Event(Event {
                kind,
                path,
                note,
                side,
                ..
            }) => {
                // The letters            } => {
                // The letters of `git status --short`, where git has one.
                let letter = match *kind {
                    "added" => "A",
                    "updated" | "modified" => "M",
                    "deleted" => "D",
                    "copied" => "C",
                    "moved" => "R",
                    "new" => "?",
                    _ => "!",
                };
                write!(f, "{}", head(side, letter, path))?;
                match (letter, note.is_empty()) {
                    ("!", true) => write!(f, " ({kind})"),
                    ("!", false) => write!(f, " ({kind}: {note})"),
                    (_, false) => write!(f, " ({note})"),
                    (_, true) => Ok(()),
                }
            }
            Message::Conflict {
                path,
                message,
                side,
                ..
            } => write!(f, "{} (conflict: {message})", head(side, "U", path)),
            Message::Version(v) => {
                let hash = v.hash.as_deref().map_or("--------", |h| &h[..8]);
                let mut notes = vec![];
                if v.current {
                    notes.push("current");
                }
                match &v.hash {
                    None => notes.push("deleted"),
                    Some(_) if !v.available => notes.push("no content"),
                    Some(_) => {}
                }
                let (time, names) = (utc(v.mtime), v.nicknames.join(","));
                write!(f, "{hash} {time} {names} {}", v.path)?;
                if !notes.is_empty() {
                    write!(f, " ({})", notes.join(", "))?;
                }
                Ok(())
            }
            Message::Holding(h) => {
                let mark = |state: &str| match state {
                    "held" => "✓",
                    "behind" | "missing" => "○",
                    "deleted" => "✗",
                    _ => "-",
                };
                write!(f, "{}", mark(h.ours))?;
                if let Some(theirs) = h.theirs {
                    write!(f, " {}", mark(theirs))?;
                }
                write!(f, " {}", h.path)?;
                if h.path.ends_with('/') {
                    write!(f, " ({} files)", h.files)?;
                }
                // The two states a mark does not tell apart from another.
                for state in ["behind", "ignored"] {
                    let whose = match (h.ours == state, h.theirs.map(|t| t == state)) {
                        (true, None) => "",
                        (true, Some(false)) => "ours ",
                        (false, Some(true)) => "theirs ",
                        (true, Some(true)) => "ours and theirs ",
                        (false, _) => continue,
                    };
                    // No `✗` on the line says what the vault is behind.
                    let gone = h.deleted && h.ours != "deleted" && h.theirs != Some("deleted");
                    let why = if state == "behind" && gone {
                        ": deleted elsewhere"
                    } else {
                        ""
                    };
                    write!(f, " ({whose}{state}{why})")?;
                }
                Ok(())
            }
            Message::Setting { name, value } => write!(f, "{name}: {value}"),
            Message::Blame { path, nicknames } => write!(f, "{} {path}", nicknames.join(",")),
            Message::Info { message } => write!(f, "{message}"),
            Message::Warning { message } => write!(f, "warning: {message}"),
            Message::Error { message } => write!(f, "error: {message}"),
            Message::Summary(summary) => write!(f, "{summary}"),
        }
    }
}

/// What a run did: the events, already printed as they happened, and counts
/// of them. Returned by the commands; `main` prints the counts last and takes
/// the exit code from them.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    /// Entries written by scans.
    pub recorded: usize,
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub conflicts: usize,
    pub pending: usize,
    pub skipped: usize,
    pub incomplete: usize,
    pub rejected: usize,
    #[serde(skip)]
    pub events: Vec<Event>,
}

impl Summary {
    /// 2 if something needs attention, else 0.
    pub fn exit_code(&self) -> i32 {
        let open = self.conflicts + self.pending + self.skipped + self.incomplete + self.rejected;
        if open > 0 { 2 } else { 0 }
    }

    /// Keep an event, and count it if the summary has a count for its kind.
    pub fn record(&mut self, event: Event) {
        match event.kind {
            "added" => self.added += 1,
            "updated" => self.updated += 1,
            "deleted" => self.deleted += 1,
            "pending" => self.pending += 1,
            "skipped" => self.skipped += 1,
            "incomplete" => self.incomplete += 1,
            "rejected" => self.rejected += 1,
            _ => {}
        }
        self.events.push(event);
    }

    pub fn has(&self, kind: &str, path: &str) -> bool {
        self.events.iter().any(|e| e.kind == kind && e.path == path)
    }
}

impl Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "recorded {}, added {}, updated {}, deleted {}, conflicts {}, pending {}, skipped {}",
            self.recorded,
            self.added,
            self.updated,
            self.deleted,
            self.conflicts,
            self.pending,
            self.skipped
        )
    }
}

fn line<T: Display + Serialize>(value: &T, json: bool) -> String {
    if json {
        serde_json::to_string(value).expect("a message is plain data")
    } else {
        value.to_string()
    }
}

/// Results.
pub fn print_stdout<T: Display + Serialize>(value: &T, json: bool) {
    println!("{}", line(value, json));
}

/// Warnings and errors.
pub fn print_stderr<T: Display + Serialize>(value: &T, json: bool) {
    eprintln!("{}", line(value, json));
}

pub fn warn(opts: &Options, message: String) {
    print_stderr(&Message::Warning { message }, opts.json);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_forms() {
        let event = |kind, note: &str| Event {
            kind,
            vault: "/v".into(),
            path: "a/f.txt".into(),
            note: note.into(),
            side: "",
        };
        let message = |kind, note| Message::Event(event(kind, note));
        let both = |m: &Message| (line(m, false), line(m, true));
        assert_eq!(
            both(&message("updated", "")),
            (
                "M a/f.txt".into(),
                r#"{"type":"event","kind":"updated","vault":"/v","path":"a/f.txt","note":""}"#
                    .into()
            )
        );
        for (kind, text) in [
            ("added", "A"),
            ("deleted", "D"),
            ("copied", "C"),
            ("moved", "R"),
        ] {
            assert_eq!(line(&message(kind, ""), false), format!("{text} a/f.txt"));
        }
        assert_eq!(line(&message("new", ""), false), "? a/f.txt");
        assert_eq!(line(&message("behind", ""), false), "! a/f.txt (behind)");
        assert_eq!(
            line(&message("skipped", "changed"), false),
            "! a/f.txt (skipped: changed)"
        );
        let conflict = Message::Conflict {
            vault: "/v".into(),
            path: "n.txt".into(),
            message: "n-12345678.txt".into(),
            side: "",
        };
        assert_eq!(
            both(&conflict),
            (
                "U n.txt (conflict: n-12345678.txt)".into(),
                r#"{"type":"conflict","vault":"/v","path":"n.txt","message":"n-12345678.txt"}"#
                    .into()
            )
        );
        // With a peer, each line says whose vault; `.` is the vault itself.
        let sided = |side, kind, path: &str, note: &str| {
            Message::Event(Event {
                side,
                path: path.into(),
                ..event(kind, note)
            })
        };
        assert_eq!(
            both(&sided("->", "updated", "a/f.txt", "")),
            (
                "-> M a/f.txt".into(),
                r#"{"type":"event","kind":"updated","vault":"/v","path":"a/f.txt","note":""}"#
                    .into()
            )
        );
        assert_eq!(
            line(&sided("<-", "rejected", "", "segment x"), false),
            "<- ! . (rejected: segment x)"
        );
        let conflict = Message::Conflict {
            vault: "/v".into(),
            path: "n.txt".into(),
            message: "n-12345678.txt".into(),
            side: "<-",
        };
        assert_eq!(
            line(&conflict, false),
            "<- U n.txt (conflict: n-12345678.txt)"
        );
        let blame = Message::Blame {
            path: "a/f.txt".into(),
            nicknames: vec!["drive".into(), "notes".into()],
        };
        assert_eq!(
            both(&blame),
            (
                "drive,notes a/f.txt".into(),
                r#"{"type":"blame","path":"a/f.txt","nicknames":["drive","notes"]}"#.into()
            )
        );
        let version = |hash: Option<&str>, current, available| {
            Message::Version(Version {
                path: "a/f.txt".into(),
                hash: hash.map(|h| h.repeat(8)),
                mtime: 1_700_000_000_000,
                nicknames: vec!["drive".into()],
                current,
                available,
            })
        };
        assert_eq!(
            both(&version(Some("3f9a1c2e"), true, true)),
            (
                "3f9a1c2e 2023-11-14 22:13Z drive a/f.txt (current)".into(),
                format!(
                    r#"{{"type":"version","path":"a/f.txt","hash":"{}","mtime":1700000000000,"nicknames":["drive"],"current":true,"available":true}}"#,
                    "3f9a1c2e".repeat(8)
                )
            )
        );
        assert_eq!(
            line(&version(Some("3f9a1c2e"), false, false), false),
            "3f9a1c2e 2023-11-14 22:13Z drive a/f.txt (no content)"
        );
        assert_eq!(
            line(&version(None, true, false), false),
            "-------- 2023-11-14 22:13Z drive a/f.txt (current, deleted)"
        );
        let holding = |path: &str, files, ours, theirs| {
            Message::Holding(Holding {
                path: path.into(),
                files,
                ours,
                theirs,
                deleted: false,
            })
        };
        let deleted = |ours, theirs| {
            Message::Holding(Holding {
                path: "f".into(),
                files: 1,
                ours,
                theirs,
                deleted: true,
            })
        };
        assert_eq!(
            both(&deleted("deleted", Some("behind"))),
            (
                "✗ ○ f (theirs behind)".into(),
                r#"{"type":"holding","path":"f","files":1,"ours":"deleted","theirs":"behind","deleted":true}"#
                    .into()
            )
        );
        for (ours, theirs, text) in [
            ("behind", Some("deleted"), "○ ✗ f (ours behind)"),
            ("behind", None, "○ f (behind: deleted elsewhere)"),
            (
                "behind",
                Some("excluded"),
                "○ - f (ours behind: deleted elsewhere)",
            ),
        ] {
            assert_eq!(line(&deleted(ours, theirs), false), text);
        }
        assert_eq!(
            both(&holding("a/f.txt", 1, "behind", None)),
            (
                "○ a/f.txt (behind)".into(),
                r#"{"type":"holding","path":"a/f.txt","files":1,"ours":"behind"}"#.into()
            )
        );
        assert_eq!(
            both(&holding("music/", 1340, "held", Some("excluded"))),
            (
                "✓ - music/ (1340 files)".into(),
                r#"{"type":"holding","path":"music/","files":1340,"ours":"held","theirs":"excluded"}"#
                    .into()
            )
        );
        for (ours, theirs, text) in [
            ("held", None, "✓ f"),
            ("missing", None, "○ f"),
            ("excluded", None, "- f"),
            ("ignored", None, "- f (ignored)"),
            ("behind", Some("held"), "○ ✓ f (ours behind)"),
            ("held", Some("behind"), "✓ ○ f (theirs behind)"),
            ("behind", Some("behind"), "○ ○ f (ours and theirs behind)"),
            ("missing", Some("ignored"), "○ - f (theirs ignored)"),
            (
                "behind",
                Some("ignored"),
                "○ - f (ours behind) (theirs ignored)",
            ),
        ] {
            assert_eq!(line(&holding("f", 1, ours, theirs), false), text);
        }
        assert_eq!(utc(0), "1970-01-01 00:00Z");
        assert_eq!(utc(-1), "1969-12-31 23:59Z");
        assert_eq!(utc(951_782_400_000), "2000-02-29 00:00Z");
        let warning = Message::Warning {
            message: "w".into(),
        };
        assert_eq!(
            both(&warning),
            (
                "warning: w".into(),
                r#"{"type":"warning","message":"w"}"#.into()
            )
        );
        let mut summary = Summary {
            recorded: 2,
            ..Summary::default()
        };
        summary.record(event("added", ""));
        summary.record(event("copied", ""));
        summary.record(event("skipped", "changed"));
        assert!(summary.has("copied", "a/f.txt") && !summary.has("updated", "a/f.txt"));
        assert_eq!(summary.exit_code(), 2);
        let (text, json) = both(&Message::Summary(summary));
        assert_eq!(
            text,
            "recorded 2, added 1, updated 0, deleted 0, conflicts 0, pending 0, skipped 1"
        );
        assert!(
            json.starts_with(r#"{"type":"summary","recorded":2,"added":1,"#),
            "{json}"
        );
        assert_eq!(Summary::default().exit_code(), 0);
    }
}
