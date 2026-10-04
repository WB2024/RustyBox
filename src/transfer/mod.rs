//! Moving folders and files between places: a folder on this server, or a folder on a drive
//! attached to another computer (through its agent). Copying works across any pair of them.
//! Moving, hard links and symlinks only make sense on one filesystem here, and the planner
//! says so before anything happens.

pub mod engine;
pub mod loc;
pub mod plan;
pub mod run;

pub use loc::{Loc, TreeEntry};
