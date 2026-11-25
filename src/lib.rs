//! syncit: two-way sync and partial replication between folders. See SPEC.md.

pub mod backend;
pub mod blobs;
pub mod commands;
pub mod config;
#[cfg(feature = "remote")]
pub mod crypt;
pub mod dirstate;
pub mod entry;
pub mod format;
#[cfg(feature = "remote")]
pub mod keys;
pub mod log;
pub mod materialise;
pub mod migrate;
pub mod output;
#[cfg(feature = "remote")]
pub mod remote;
pub mod scan;
#[cfg(feature = "remote")]
pub mod sealed;
pub mod segment;
pub mod storage;
pub mod vault;
