//! Segments: an immutable JSON Lines file of entries, named by its hash. No I/O.

use crate::entry::{Entry, sha256};
use anyhow::{Context, Result, ensure};

pub const HEADER: &str = r#"{"syncit":1}"#;

/// Returns the segment's name and bytes. Entries are written sorted by path.
pub fn encode(entries: &[Entry]) -> (String, Vec<u8>) {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut text = format!("{HEADER}\n");
    for e in sorted {
        text.push_str(&e.line());
        text.push('\n');
    }
    (sha256(text.as_bytes()), text.into_bytes())
}

/// Validate an untrusted segment whole (CRDT.md, "Validation").
pub fn decode(name: &str, bytes: &[u8]) -> Result<Vec<Entry>> {
    ensure!(sha256(bytes) == name, "content does not match its name");
    let text = std::str::from_utf8(bytes).context("not UTF-8")?;
    let body = text.strip_suffix('\n').context("no final newline")?;
    let mut lines = body.split('\n');
    ensure!(lines.next() == Some(HEADER), "not a version 1 segment");
    lines.map(Entry::decode).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, content: Option<&str>) -> Entry {
        Entry {
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
        let (name, bytes) = encode(&entries);
        assert_eq!(name, sha256(&bytes));
        let back = decode(&name, &bytes).unwrap();
        let paths: Vec<&str> = back.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["a", "a/z", "b"]);
        for e in &entries {
            assert!(back.contains(e));
        }
    }

    fn line(hash: &str, mtime: &str, parents: &str, path: &str) -> String {
        format!(r#"{{"hash":{hash},"mtime":{mtime},"parents":{parents},"path":{path}}}"#)
    }

    fn check(body: &str) -> Result<Vec<Entry>> {
        let text = format!("{HEADER}\n{body}\n");
        decode(&sha256(text.as_bytes()), text.as_bytes())
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
                r#"{"hash":null,"mtime":1,"parents":[],"path":"x","size":1}"#.to_string(),
            ),
            (
                "missing path",
                r#"{"hash":null,"mtime":1,"parents":[]}"#.to_string(),
            ),
            (
                "missing hash",
                r#"{"mtime":1,"parents":[],"path":"x"}"#.to_string(),
            ),
            ("not an object", r#"["x"]"#.to_string()),
            ("blank line", String::new()),
            ("mtime is a string", line("null", "\"1\"", "[]", "\"x\"")),
            ("mtime is a float", line("null", "1.0", "[]", "\"x\"")),
            ("hash is a number", line("7", "1", "[]", "\"x\"")),
            (
                "whitespace",
                r#"{"hash":null, "mtime":1,"parents":[],"path":"x"}"#.to_string(),
            ),
            (
                "key order",
                r#"{"mtime":1,"hash":null,"parents":[],"path":"x"}"#.to_string(),
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
