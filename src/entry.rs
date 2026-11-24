//! Log entries: canonical form, ids, validation. No I/O.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type Id = String;

// Fields are in alphabetical order: serde_json writes them in declaration
// order, which gives the canonical key order.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub exec: bool,           // the owner may execute the file; false for a tombstone
    pub hash: Option<String>, // 64 lowercase hex, or None for a tombstone
    pub mtime: i64,           // milliseconds since the Unix epoch
    pub parents: Vec<Id>,     // entry ids, sorted, no duplicates
    pub path: String,
}

pub fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn is_hash(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Relative, no empty, `.` or `..` component, not under `.syncit`.
pub fn valid_path(path: &str) -> bool {
    path.split('/')
        .all(|c| !c.is_empty() && c != "." && c != "..")
        && path.split('/').next() != Some(".syncit")
}

impl Entry {
    /// The canonical line, without the newline.
    pub fn line(&self) -> String {
        serde_json::to_string(self).expect("an entry always serialises")
    }

    pub fn id(&self) -> Id {
        sha256(self.line().as_bytes())
    }

    /// Parse one untrusted line. Re-encoding must give the same bytes, which
    /// enforces key order, whitespace, number form, escaping and field set.
    pub fn decode(line: &str) -> Result<Entry> {
        let e: Entry = serde_json::from_str(line).context("not an entry")?;
        ensure!(e.line() == line, "not in canonical form");
        ensure!(e.hash.as_deref().is_none_or(is_hash), "malformed hash");
        ensure!(e.hash.is_some() || !e.exec, "executable tombstone");
        ensure!(e.parents.iter().all(|p| is_hash(p)), "malformed parent id");
        ensure!(
            e.parents.windows(2).all(|w| w[0] < w[1]),
            "parents are not sorted and distinct"
        );
        ensure!(valid_path(&e.path), "invalid path");
        Ok(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment;

    #[test]
    fn entry_id_vector() {
        let line =
            r#"{"exec":false,"hash":null,"mtime":1763600005000,"parents":[],"path":"old.txt"}"#;
        let e = Entry::decode(line).unwrap();
        assert_eq!(e.line(), line);
        assert_eq!(
            e.id(),
            "7143ad25f65a2079629dadd17cde1155bc1152bb41be99705c0f807933e841c9"
        );
        let (name, bytes) = segment::encode("notes", &[e]);
        let header = r#"{"nickname":"notes","syncit":1}"#;
        assert_eq!(bytes, format!("{header}\n{line}\n").into_bytes());
        assert_eq!(
            name,
            "84f57e1040620ebe774999c3e166a89617ec808413902b99f36de3e21b0caec2"
        );
    }

    #[test]
    fn canonical_escaping() {
        let e = Entry {
            exec: false,
            hash: None,
            mtime: 0,
            parents: vec![],
            path: "a\"b\\c\u{1}d\ne\u{e9}/f\u{7f}".into(),
        };
        assert_eq!(
            e.line(),
            "{\"exec\":false,\"hash\":null,\"mtime\":0,\"parents\":[],\"path\":\"a\\\"b\\\\c\\u0001d\\ne\u{e9}/f\u{7f}\"}"
        );
        assert_eq!(Entry::decode(&e.line()).unwrap(), e);
    }

    #[test]
    fn executable_tombstone_rejected() {
        let line = r#"{"exec":true,"hash":null,"mtime":1,"parents":[],"path":"x"}"#;
        assert!(Entry::decode(line).is_err());
    }
}
