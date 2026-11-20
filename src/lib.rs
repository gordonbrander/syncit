//! syncit: two-way sync and partial replication between folders. See SPEC.md.

pub mod blobs;
pub mod cache;
pub mod commands;
pub mod config;
pub mod entry;
pub mod log;
pub mod materialise;
pub mod scan;
pub mod segment;
pub mod vault;
