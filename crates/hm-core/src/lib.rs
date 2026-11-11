//! HealthyMac core.
//!
//! This crate owns the contract every other crate codes against:
//!
//! - [`model`]: what a scan produces ([`model::Finding`]) and how it can be acted on.
//! - [`scan`]: the [`scan::Scanner`] trait, [`scan::ScanContext`] and the parallel runner.
//! - [`safety`]: [`safety::SafetyPolicy`], which decides what may ever be removed.
//! - [`clean`]: [`clean::Cleaner`], the **only** code in the project allowed to mutate the
//!   filesystem or run a destructive command. Scanners are read-only by contract.
//! - [`fsutil`], [`exec`], [`fmt`]: shared helpers.

pub mod audit;
pub mod clean;
pub mod config;
pub mod exec;
pub mod fmt;
pub mod fsutil;
pub mod gpu;
pub mod i18n;
pub mod model;
pub mod safety;
pub mod scan;
pub mod threads;

pub use clean::{CleanEvent, CleanOptions, CleanOutcome, CleanReport, CleanStatus, Cleaner};
pub use config::{Appearance, Config, LanguageChoice, RemovalMode};
pub use fsutil::{measure, CancelToken, SizeInfo};
pub use model::{Action, Area, Finding, Recovery, Risk, TAG_NEEDS_FULL_DISK_ACCESS};
pub use safety::{SafetyPolicy, SafetyViolation};
pub use scan::{run_scan, ScanContext, ScanEvent, Scanner};
