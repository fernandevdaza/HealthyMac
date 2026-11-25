//! System-level features: junk scanners, app inventory and uninstaller, startup items, the disk
//! map, the live monitor and the health score.

pub mod apps;
mod gate;
pub mod health;
mod inventory;
pub mod monitor;
pub mod scanners;
pub mod security;
pub mod space;
pub mod startup;
pub mod types;

#[cfg(test)]
mod test_support;

pub use scanners::scanners;
