//! Segments: an immutable JSON Lines file of entries, named by its hash. No I/O.

use crate::entry::{Entry, sha256};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

// The first line. Fields are in alphabetical order, as in `Entry`, so that
// serde_json writes the canonical form.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    nickname: String,
    syncit: u32,
}

impl Header {
    fn line(&self) -> String {
        serde_json::to_string(self).expect("a header always serialises")
    }
}

/// A decoded segment: its entries, and the nickname of the vault that wrote it.
#[derive(Debug)]
pub struct Segment {
    pub nickname: String,
    pub entries: Vec<Entry>,
}

/// A nickname is a label, printed as it is: 1 to 64 bytes, with no whitespace
/// and no control characters.
pub fn valid_nickname(s: &str) -> bool {
    (1..=64).contains(&s.len()) && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Returns the segment's name and bytes. Entries are written sorted by path.
pub fn encode(nickname: &str, entries: &[Entry]) -> (String, Vec<u8>) {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let header = Header {
        nickname: nickname.to_string(),
        syncit: 1,
    };
    let mut text = header.line() + "\n";
    for e in sorted {
        text.push_str(&e.line());
        text.push('\n');
    }
    (sha256(text.as_bytes()), text.into_bytes())
}

/// Validate an untrusted segment whole (CRDT.md, "Validation").
pub fn decode(name: &str, bytes: &[u8]) -> Result<Segment> {
    ensure!(sha256(bytes) == name, "content does not match its name");
    let text = std::str::from_utf8(bytes).context("not UTF-8")?;
    let body = text.strip_suffix('\n').context("no final newline")?;
    let mut lines = body.split('\n');
    let first = lines.next().unwrap_or_default();
    let header: Header = serde_json::from_str(first).context("not a segment header")?;
    ensure!(header.line() == first, "header is not in canonical form");
    ensure!(header.syncit == 1, "not a version 1 segment");
    ensure!(valid_nickname(&header.nickname), "invalid nickname");
    Ok(Segment {
        nickname: header.nickname,
        entries: lines.map(Entry::decode).collect::<Result<_>>()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, content: Option<&str>) -> Entry {
        Entry {
            exec: false,
            hash: content.map(|c| sha256(c.as_bytes())),
            mtime: 5,
            parents: vec![],
            path: path.into(),
        }
    }

    #[test]
    fn segment_round_trip() {
        let entries = [
            entry("b", Some("x")),
            entry("a/z", None),
            entry("a", Some("y")),
        ];
        let (name, bytes) = encode("notes", &entries);
        assert_eq!(name, sha256(&bytes));
        let Segment {
            nickname,
            entries: back,
        } = decode(&name, &bytes).unwrap();
        assert_eq!(nickname, "notes");
        let paths: Vec<&str> = back.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["a", "a/z", "b"]);
        for e in &entries {
            assert!(back.contains(e));
        }
    }

    fn line(hash: &str, mtime: &str, parents: &str, path: &str) -> String {
        format!(
            r#"{{"exec":false,"hash":{hash},"mtime":{mtime},"parents":{parents},"path":{path}}}"#
        )
    }

    const HEADER: &str = r#"{"nickname":"notes","syncit":1}"#;

    fn whole(text: &str) -> Result<Segment> {
        decode(&sha256(text.as_bytes()), text.as_bytes())
    }

    fn check(body: &str) -> Result<Segment> {
        whole(&format!("{HEADER}\n{body}\n"))
    }

    #[test]
    fn header_rejects() {
        assert!(whole(&format!("{HEADER}\n")).is_ok(), "no entries");
        let long = format!(r#"{{"nickname":"{}","syncit":1}}"#, "n".repeat(65));
        for (what, header) in [
            ("no nickname", r#"{"syncit":1}"#),
            (
                "extra field",
                r#"{"key":"k","nickname":"notes","syncit":1}"#,
            ),
            ("key order", r#"{"syncit":1,"nickname":"notes"}"#),
            ("whitespace", r#"{"nickname":"notes", "syncit":1}"#),
            ("empty nickname", r#"{"nickname":"","syncit":1}"#),
            ("space in nickname", r#"{"nickname":"my notes","syncit":1}"#),
            ("control character", r#"{"nickname":"a\u001bb","syncit":1}"#),
            ("long nickname", long.as_str()),
            ("nickname is a number", r#"{"nickname":7,"syncit":1}"#),
        ] {
            assert!(
                whole(&format!("{header}\n")).is_err(),
                "{what} was accepted"
            );
        }
        assert!(valid_nickname("gordons-mbp:notes") && valid_nickname("ノート"));
    }

    #[test]
    fn segment_rejects() {
        let a = format!("\"{}\"", "a".repeat(64));
        let b = format!("\"{}\"", "b".repeat(64));
        let good = line(&a, "1", &format!("[{a},{b}]"), "\"x/y\"");
        assert!(check(&good).is_ok());

        let text = format!("{HEADER}\n{good}\n");
        let name = sha256(text.as_bytes());
        assert!(
            decode(&"0".repeat(64), text.as_bytes()).is_err(),
            "wrong name"
        );
        let v2 = text.replace("\"syncit\":1", "\"syncit\":2");
        assert!(
            decode(&sha256(v2.as_bytes()), v2.as_bytes()).is_err(),
            "version 2"
        );
        let cut = text.trim_end();
        assert!(
            decode(&sha256(cut.as_bytes()), cut.as_bytes()).is_err(),
            "no final newline"
        );
        assert!(decode(&name, text.as_bytes()).is_ok());

        let upper = format!("\"{}\"", "A".repeat(64));
        let bad = [
            (
                "extra field",
                r#"{"exec":false,"hash":null,"mtime":1,"parents":[],"path":"x","size":1}"#
                    .to_string(),
            ),
            (
                "missing path",
                r#"{"exec":false,"hash":null,"mtime":1,"parents":[]}"#.to_string(),
            ),
            (
                "missing hash",
                r#"{"exec":false,"mtime":1,"parents":[],"path":"x"}"#.to_string(),
            ),
            (
                "missing exec",
                r#"{"hash":null,"mtime":1,"parents":[],"path":"x"}"#.to_string(),
            ),
            (
                "executable tombstone",
                r#"{"exec":true,"hash":null,"mtime":1,"parents":[],"path":"x"}"#.to_string(),
            ),
            (
                "exec is a number",
                line("null", "1", "[]", "\"x\"").replace("false", "0"),
            ),
            ("not an object", r#"["x"]"#.to_string()),
            ("blank line", String::new()),
            ("mtime is a string", line("null", "\"1\"", "[]", "\"x\"")),
            ("mtime is a float", line("null", "1.0", "[]", "\"x\"")),
            ("hash is a number", line("7", "1", "[]", "\"x\"")),
            (
                "whitespace",
                r#"{"exec":false,"hash":null, "mtime":1,"parents":[],"path":"x"}"#.to_string(),
            ),
            (
                "key order",
                r#"{"hash":null,"exec":false,"mtime":1,"parents":[],"path":"x"}"#.to_string(),
            ),
            ("escaped slash", line("null", "1", "[]", r#""a\/b""#)),
            ("uppercase hex", line(&upper, "1", "[]", "\"x\"")),
            (
                "uppercase parent",
                line("null", "1", &format!("[{upper}]"), "\"x\""),
            ),
            ("short hash", line("\"abc\"", "1", "[]", "\"x\"")),
            ("absolute path", line("null", "1", "[]", "\"/x\"")),
            ("dot dot", line("null", "1", "[]", "\"a/../b\"")),
            ("dot", line("null", "1", "[]", "\"./a\"")),
            ("empty component", line("null", "1", "[]", "\"a//b\"")),
            ("empty path", line("null", "1", "[]", "\"\"")),
            ("under .syncit", line("null", "1", "[]", "\".syncit/x\"")),
            (
                "unsorted parents",
                line("null", "1", &format!("[{b},{a}]"), "\"x\""),
            ),
            (
                "duplicate parents",
                line("null", "1", &format!("[{a},{a}]"), "\"x\""),
            ),
        ];
        for (what, body) in bad {
            assert!(check(&body).is_err(), "{what} was accepted");
        }
    }
}
