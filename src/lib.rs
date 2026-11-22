//! syncit: two-way sync and partial replication between folders. See SPEC.md.

pub mod blobs;
pub mod commands;
pub mod config;
#[cfg(feature = "remote")]
pub mod crypt;
pub mod dirstate;
pub mod entry;
#[cfg(feature = "remote")]
pub mod keys;
pub mod log;
pub mod materialise;
pub mod output;
#[cfg(feature = "remote")]
pub mod remote;
pub mod scan;
pub mod segment;
pub mod storage;
pub mod vault;
