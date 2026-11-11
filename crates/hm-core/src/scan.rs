//! The scanner contract and the parallel runner.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use rayon::prelude::*;

use crate::config::Config;
use crate::fsutil::{measure, CancelToken, SizeInfo};
use crate::model::{Area, Finding};

/// A source of findings.
///
/// **Scanners are read-only.** They must not create, modify or remove anything, and any command
/// they run must be side-effect free. All mutation goes through [`crate::clean::Cleaner`].
pub trait Scanner: Send + Sync {
    /// Stable id, namespaced by area: `"dev.xcode.derived_data"`.
    fn id(&self) -> &'static str;
    /// Display name, in Spanish.
    fn name(&self) -> &'static str;
    /// One sentence, in Spanish.
    fn description(&self) -> &'static str;
    fn area(&self) -> Area;
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>>;
}

/// Everything a scanner may depend on from the environment.
///
/// Scanners must resolve paths through [`ScanContext::home`], never through `$HOME` or `dirs`,
/// so they can be tested against a fixture home.
#[derive(Clone, Debug)]
pub struct ScanContext {
    pub home: PathBuf,
    pub config: Config,
    pub now: SystemTime,
    pub cancel: CancelToken,
}

impl ScanContext {
    /// For the current user, loading their config from its default location.
    pub fn for_current_user() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .context(crate::t!("HOME no está definido", "HOME is not set"))?;
        let config = Config::load(&Config::default_path(&home))?;
        Ok(Self::with_home(home, config))
    }

    /// For an explicit home directory. This is what tests use.
    pub fn with_home(home: impl Into<PathBuf>, config: Config) -> Self {
        Self {
            home: home.into(),
            config,
            now: SystemTime::now(),
            cancel: CancelToken::new(),
        }
    }

    /// `home` joined with a relative path.
    pub fn home_path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.home.join(relative)
    }

    pub fn measure(&self, path: &Path) -> SizeInfo {
        measure(path, &self.cancel)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Finds an external tool. See [`crate::exec::which`].
    pub fn which(&self, program: &str) -> Option<PathBuf> {
        crate::exec::which(program, &self.home)
    }
}

/// Progress events from [`run_scan`]. Delivered on worker threads.
#[derive(Clone, Debug)]
pub enum ScanEvent {
    Started {
        scanner_id: &'static str,
    },
    Finished {
        scanner_id: &'static str,
        findings: Vec<Finding>,
        elapsed: Duration,
    },
    /// The scanner returned an error or panicked. The other scanners keep running.
    Failed {
        scanner_id: &'static str,
        error: String,
    },
}

/// Runs scanners in parallel and returns all findings, deduplicated.
///
/// A failing scanner never takes down the scan: failures, including panics, surface as
/// [`ScanEvent::Failed`]. Each scanner's raw findings are in its `Finished` event, while the
/// returned list has already been through [`crate::model::dedupe_findings`].
pub fn run_scan(
    scanners: &[Box<dyn Scanner>],
    ctx: &ScanContext,
    on_event: &(dyn Fn(ScanEvent) + Send + Sync),
) -> Vec<Finding> {
    let all: Vec<Finding> = scanners
        .par_iter()
        .flat_map_iter(|scanner| {
            if ctx.is_cancelled() {
                return Vec::new();
            }
            let scanner_id = scanner.id();
            on_event(ScanEvent::Started { scanner_id });
            let started = Instant::now();
            let result = catch_unwind(AssertUnwindSafe(|| scanner.scan(ctx)));
            match result {
                Ok(Ok(findings)) => {
                    on_event(ScanEvent::Finished {
                        scanner_id,
                        findings: findings.clone(),
                        elapsed: started.elapsed(),
                    });
                    findings
                }
                Ok(Err(error)) => {
                    on_event(ScanEvent::Failed {
                        scanner_id,
                        error: format!("{error:#}"),
                    });
                    Vec::new()
                }
                Err(payload) => {
                    let error = payload
                        .downcast_ref::<&str>()
                        .map(|text| text.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| {
                            crate::t!("pánico sin mensaje", "panic without a message").to_string()
                        });
                    on_event(ScanEvent::Failed { scanner_id, error });
                    Vec::new()
                }
            }
        })
        .collect();
    crate::model::dedupe_findings(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fixed(&'static str);
    impl Scanner for Fixed {
        fn id(&self) -> &'static str {
            self.0
        }
        fn name(&self) -> &'static str {
            "fijo"
        }
        fn description(&self) -> &'static str {
            ""
        }
        fn area(&self) -> Area {
            Area::System
        }
        fn scan(&self, _: &ScanContext) -> Result<Vec<Finding>> {
            Ok(vec![Finding::new(self.0, Area::System, "k", "t")])
        }
    }

    struct Fails;
    impl Scanner for Fails {
        fn id(&self) -> &'static str {
            "fails"
        }
        fn name(&self) -> &'static str {
            ""
        }
        fn description(&self) -> &'static str {
            ""
        }
        fn area(&self) -> Area {
            Area::System
        }
        fn scan(&self, _: &ScanContext) -> Result<Vec<Finding>> {
            anyhow::bail!("boom")
        }
    }

    struct Panics;
    impl Scanner for Panics {
        fn id(&self) -> &'static str {
            "panics"
        }
        fn name(&self) -> &'static str {
            ""
        }
        fn description(&self) -> &'static str {
            ""
        }
        fn area(&self) -> Area {
            Area::System
        }
        fn scan(&self, _: &ScanContext) -> Result<Vec<Finding>> {
            panic!("kaboom")
        }
    }

    #[test]
    fn failures_do_not_stop_other_scanners() {
        let scanners: Vec<Box<dyn Scanner>> =
            vec![Box::new(Fixed("a")), Box::new(Fails), Box::new(Panics)];
        let ctx = ScanContext::with_home("/tmp", Config::default());
        let errors = Mutex::new(Vec::new());
        let findings = run_scan(&scanners, &ctx, &|event| {
            if let ScanEvent::Failed { scanner_id, error } = event {
                errors.lock().unwrap().push((scanner_id, error));
            }
        });
        assert_eq!(findings.len(), 1);
        let mut errors = errors.into_inner().unwrap();
        errors.sort();
        assert_eq!(errors[0], ("fails", "boom".to_string()));
        assert_eq!(errors[1], ("panics", "kaboom".to_string()));
    }

    #[test]
    fn cancelled_scan_runs_nothing() {
        let scanners: Vec<Box<dyn Scanner>> = vec![Box::new(Fixed("a"))];
        let ctx = ScanContext::with_home("/tmp", Config::default());
        ctx.cancel.cancel();
        assert!(run_scan(&scanners, &ctx, &|_| {}).is_empty());
    }
}
