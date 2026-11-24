//! Log entries: canonical form, ids, validation. No I/O.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

/// An entry's id: the SHA-256 of its canonical line. Written as 64 lowercase
/// hex characters. Held as the 32 bytes, which sort the same way, so that a
/// map of ids compares and copies them without touching the heap.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Id([u8; 32]);

impl Id {
    fn of(line: &str) -> Id {
        Id(Sha256::digest(line.as_bytes()).into())
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut text = [0; 64];
        hex::encode_to_slice(self.0, &mut text).expect("64 is twice 32");
        f.write_str(std::str::from_utf8(&text).expect("hex is ASCII"))
    }
}

impl fmt::Debug for Id {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl FromStr for Id {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Id> {
        // Stricter than the hex crate, which takes uppercase too.
        let digit = |c: u8| match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        };
        ensure!(s.len() == 64, "malformed id");
        let mut id = [0; 32];
        for (byte, [high, low]) in id.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
            let (high, low) = digit(*high).zip(digit(*low)).context("malformed id")?;
            *byte = high << 4 | low;
        }
        Ok(Id(id))
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Id, D::Error> {
        struct Hex;
        impl serde::de::Visitor<'_> for Hex {
            type Value = Id;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("64 lowercase hex characters")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Id, E> {
                s.parse().map_err(E::custom)
            }
        }
        d.deserialize_str(Hex)
    }
}

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
        Id::of(&self.line())
    }

    /// Parse one untrusted line, giving the entry and its id. Re-encoding
    /// must give the same bytes, which enforces key order, whitespace, number
    /// form, escaping and field set; so the line is the one the id is of.
    pub fn decode(line: &str) -> Result<(Id, Entry)> {
        let e: Entry = serde_json::from_str(line).context("not an entry")?;
        ensure!(e.line() == line, "not in canonical form");
        ensure!(e.hash.as_deref().is_none_or(is_hash), "malformed hash");
        ensure!(e.hash.is_some() || !e.exec, "executable tombstone");
        ensure!(
            e.parents.windows(2).all(|w| w[0] < w[1]),
            "parents are not sorted and distinct"
        );
        ensure!(valid_path(&e.path), "invalid path");
        Ok((Id::of(line), e))
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
        let (id, e) = Entry::decode(line).unwrap();
        assert_eq!(e.line(), line);
        assert_eq!(id, e.id());
        assert_eq!(
            id.to_string(),
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
        assert_eq!(Entry::decode(&e.line()).unwrap(), (e.id(), e));
    }

    #[test]
    fn id_text() {
        let (lo, hi) = ("0a".repeat(32), "f0".repeat(32));
        let (a, b): (Id, Id) = (lo.parse().unwrap(), hi.parse().unwrap());
        assert_eq!(a.to_string(), lo);
        assert!(a < b, "bytes sort as their hex does");
        assert_eq!(serde_json::to_string(&a).unwrap(), format!("\"{lo}\""));
        assert_eq!(serde_json::from_str::<Id>(&format!("\"{hi}\"")).unwrap(), b);
        for bad in ["", &lo[..62], &lo.to_uppercase(), &format!("{lo}0a")] {
            assert!(bad.parse::<Id>().is_err(), "{bad}");
        }
    }

    #[test]
    fn executable_tombstone_rejected() {
        let line = r#"{"exec":true,"hash":null,"mtime":1,"parents":[],"path":"x"}"#;
        assert!(Entry::decode(line).is_err());
    }
}
