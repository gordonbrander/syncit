//! `format`: the one file that says what a vault is (store.md, section 14).
//! It is plaintext on every backend, so it can be read before anything else
//! about the vault is understood, and before its key is known. Nothing else
//! reads or writes it.

use crate::backend::Backend;
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// Where it is on a backend.
pub const FORMAT: &str = "format";

/// Written as canonical JSON: keys in alphabetical order, one line.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Format {
    /// What tells a sealed vault's key from another (`crypt::check`). A
    /// vault that is not sealed has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    /// The layout version (`migrate.rs`).
    pub syncit: u32,
}

impl Format {
    /// The `format` of the vault on `b`; None if it has none.
    pub fn read(b: &impl Backend) -> Result<Option<Format>> {
        let Some(bytes) = b.read(FORMAT)? else {
            return Ok(None);
        };
        let format: Format = serde_json::from_slice(&bytes).context("corrupt format")?;
        ensure!(format.syncit > 0, "corrupt format");
        Ok(Some(format))
    }

    pub fn write(&self, b: &impl Backend) -> Result<()> {
        b.write(FORMAT, (serde_json::to_string(self)? + "\n").as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Fs;

    #[test]
    fn format_is_strict() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(tmp.path().join(".syncit")).unwrap();
        let b = Fs::at(tmp.path());
        assert_eq!(Format::read(&b).unwrap(), None);
        for (format, text) in [
            (None, "{\"syncit\":2}\n"),
            (
                Some("abc".to_string()),
                "{\"check\":\"abc\",\"syncit\":2}\n",
            ),
        ] {
            let format = Format {
                check: format,
                syncit: 2,
            };
            format.write(&b).unwrap();
            assert_eq!(b.read(FORMAT).unwrap().unwrap(), text.as_bytes());
            assert_eq!(Format::read(&b).unwrap(), Some(format));
        }
        for junk in [
            "junk",
            "{}",
            "{\"check\":\"abc\"}",
            "{\"syncit\":0}",
            "{\"syncit\":\"two\"}",
            "{\"syncit\":2,\"more\":1}",
        ] {
            b.write(FORMAT, junk.as_bytes()).unwrap();
            assert!(Format::read(&b).is_err(), "{junk}");
        }
    }
}
