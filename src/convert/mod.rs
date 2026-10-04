//! Converting between disc images, Games on Demand (GOD) folders and plain game folders.
//!
//! Every conversion works in a hidden staging folder next to its destination and moves the
//! result into place only when it is complete, so a failed or cancelled run never leaves a
//! half-written game among your files.

pub mod god;
pub mod image;
pub mod plan;
pub mod run;
pub mod verify;

/// Cooperative control for blocking work: stop when asked, and report progress.
pub struct Ctl<'a> {
    pub cancelled: &'a (dyn Fn() -> bool + Sync),
    /// Overall fraction done (0.0 to 1.0) and what is happening.
    pub progress: &'a (dyn Fn(f32, &str) + Sync),
}

impl Ctl<'_> {
    pub fn check(&self) -> Result<(), crate::error::Error> {
        if (self.cancelled)() {
            Err(crate::error::Error::backend("Cancelled"))
        } else {
            Ok(())
        }
    }
}
