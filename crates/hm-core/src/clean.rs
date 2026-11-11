//! Executes approved findings. The only code in the project that removes anything.
//!
//! Every target is re-validated against [`SafetyPolicy`] immediately before acting, because the
//! world may have changed since the scan and because scanners are not trusted to be correct.

use std::ffi::CString;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use trash::macos::{DeleteMethod, TrashContextExtMacos};
use trash::TrashContext;

use crate::audit::{AuditEntry, AuditLog};
use crate::config::RemovalMode;
use crate::exec;
use crate::fsutil::{self, measure, CancelToken};
use crate::model::{Action, Finding, Recovery, Risk};
use crate::safety::{SafetyPolicy, SafetyViolation};

/// Tools like `docker system prune` can legitimately take a long time.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Clone, Debug)]
pub struct CleanOptions {
    pub mode: RemovalMode,
    /// Validate and report everything without changing anything.
    pub dry_run: bool,
}

impl Default for CleanOptions {
    fn default() -> Self {
        Self {
            mode: RemovalMode::Trash,
            dry_run: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanStatus {
    /// Everything requested was done.
    Done,
    /// Dry run: this is what would have happened.
    WouldClean,
    /// Part of it was done. See `errors`.
    Partial,
    /// Nothing was done, and that is fine: it is report-only, already gone or an app is open.
    Skipped,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CleanOutcome {
    pub finding_id: String,
    pub status: CleanStatus,
    /// Bytes released from disk right now.
    pub freed_bytes: u64,
    /// Bytes moved to the Trash. They only become free space when the Trash is emptied.
    pub trashed_bytes: u64,
    /// `freed_bytes` is the scan's estimate, not a measurement. A tool's own cleanup command rarely
    /// reports what it released, and may release less: `pnpm store prune` keeps what projects still
    /// use, and Docker's disk image doesn't shrink right away.
    #[serde(default)]
    pub freed_is_estimate: bool,
    pub message: Option<String>,
    pub errors: Vec<String>,
}

impl CleanOutcome {
    fn empty(finding: &Finding, status: CleanStatus) -> Self {
        Self {
            finding_id: finding.id.clone(),
            status,
            freed_bytes: 0,
            trashed_bytes: 0,
            freed_is_estimate: false,
            message: None,
            errors: Vec::new(),
        }
    }

    fn skipped(finding: &Finding, message: impl Into<String>) -> Self {
        let mut outcome = Self::empty(finding, CleanStatus::Skipped);
        outcome.message = Some(message.into());
        outcome
    }

    fn failed(finding: &Finding, error: impl Into<String>) -> Self {
        let mut outcome = Self::empty(finding, CleanStatus::Failed);
        outcome.errors.push(error.into());
        outcome
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CleanReport {
    pub outcomes: Vec<CleanOutcome>,
}

impl CleanReport {
    pub fn freed_bytes(&self) -> u64 {
        self.outcomes
            .iter()
            .map(|outcome| outcome.freed_bytes)
            .sum()
    }

    pub fn trashed_bytes(&self) -> u64 {
        self.outcomes
            .iter()
            .map(|outcome| outcome.trashed_bytes)
            .sum()
    }

    /// Whether any part of `freed_bytes` is an estimate.
    pub fn has_estimates(&self) -> bool {
        self.outcomes
            .iter()
            .any(|outcome| outcome.freed_is_estimate && outcome.freed_bytes > 0)
    }

    pub fn failures(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| matches!(outcome.status, CleanStatus::Failed | CleanStatus::Partial))
            .count()
    }
}

#[derive(Clone, Debug)]
pub enum CleanEvent {
    Started {
        finding_id: String,
        index: usize,
        total: usize,
    },
    Finished(CleanOutcome),
}

pub struct Cleaner {
    policy: SafetyPolicy,
    home: PathBuf,
    options: CleanOptions,
    audit: Option<AuditLog>,
}

impl Cleaner {
    pub fn new(home: impl Into<PathBuf>, excluded: &[PathBuf], options: CleanOptions) -> Self {
        let home = home.into();
        Self {
            policy: SafetyPolicy::new(&home, excluded),
            home,
            options,
            audit: None,
        }
    }

    pub fn with_audit(mut self, audit: AuditLog) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Cleans findings one at a time. A failure never stops the rest.
    pub fn clean(
        &self,
        findings: &[Finding],
        cancel: &CancelToken,
        on_event: &(dyn Fn(CleanEvent) + Send + Sync),
    ) -> CleanReport {
        let mut report = CleanReport::default();
        for (index, finding) in findings.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            on_event(CleanEvent::Started {
                finding_id: finding.id.clone(),
                index,
                total: findings.len(),
            });
            let outcome = self.clean_one(finding, cancel);
            if !self.options.dry_run {
                if let Some(audit) = &self.audit {
                    let entry = AuditEntry {
                        at: SystemTime::now(),
                        title: finding.title.clone(),
                        outcome: outcome.clone(),
                    };
                    // Losing a history line must not fail a clean that already happened.
                    let _ = audit.append(&entry);
                }
            }
            on_event(CleanEvent::Finished(outcome.clone()));
            report.outcomes.push(outcome);
        }
        report
    }

    fn clean_one(&self, finding: &Finding, cancel: &CancelToken) -> CleanOutcome {
        for process in &finding.blocked_by {
            if exec::is_process_running(process) {
                return CleanOutcome::skipped(
                    finding,
                    crate::tf!("{process} está abierto", "{process} is open"),
                );
            }
        }
        match &finding.action {
            Action::ReportOnly => CleanOutcome::skipped(
                finding,
                crate::t!("solo informativo", "for information only"),
            ),
            Action::Trash { paths } => self.remove_paths(finding, paths, cancel),
            Action::TrashContents { dir } => match fsutil::try_children(dir) {
                Ok(children) if children.is_empty() => {
                    CleanOutcome::skipped(finding, crate::t!("ya está vacío", "already empty"))
                }
                Ok(children) => self.remove_paths(finding, &children, cancel),
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    CleanOutcome::skipped(finding, crate::t!("ya no existe", "no longer exists"))
                }
                Err(err) => CleanOutcome::failed(finding, unreadable_folder(dir, &err)),
            },
            Action::Command { program, args } => self.run_command(finding, program, args),
            Action::CloneDedupe { keep, duplicates } => {
                self.clone_dedupe(finding, keep, duplicates, cancel)
            }
            Action::EmptyTrash => self.empty_trash(finding, cancel),
        }
    }

    fn empty_trash(&self, finding: &Finding, cancel: &CancelToken) -> CleanOutcome {
        let trash_dir = self.home.join(".Trash");
        let children = match fsutil::try_children(&trash_dir) {
            Ok(children) => children,
            Err(err) if err.kind() == ErrorKind::NotFound => Vec::new(),
            Err(err) => return CleanOutcome::failed(finding, unreadable_folder(&trash_dir, &err)),
        };
        if children.is_empty() {
            return CleanOutcome::skipped(
                finding,
                crate::t!("la Papelera ya está vacía", "the Trash is already empty"),
            );
        }
        let mut outcome = CleanOutcome::empty(finding, CleanStatus::Done);
        let mut acted = 0usize;
        for path in &children {
            if cancel.is_cancelled() {
                outcome.errors.push("cancelado".to_string());
                break;
            }
            if let Err(violation) = self.policy.check_removal(path) {
                outcome.errors.push(violation.to_string());
                continue;
            }
            let size = measure(path, cancel).exclusive();
            let result = if self.options.dry_run {
                Ok(())
            } else {
                delete_path(path)
            };
            match result {
                Ok(()) => {
                    acted += 1;
                    outcome.freed_bytes += size;
                }
                Err(err) => outcome.errors.push(format!("{}: {err:#}", path.display())),
            }
        }
        outcome.status = match (acted, outcome.errors.is_empty()) {
            (0, _) => CleanStatus::Failed,
            (_, true) if self.options.dry_run => CleanStatus::WouldClean,
            (_, true) => CleanStatus::Done,
            (_, false) => CleanStatus::Partial,
        };
        outcome
    }

    /// Permanent deletion is only ever honoured for things that regenerate by themselves.
    fn may_delete_permanently(&self, finding: &Finding) -> bool {
        self.options.mode == RemovalMode::DeleteRegenerable
            && finding.risk == Risk::Safe
            && matches!(finding.recovery, Recovery::Automatic | Recovery::Rebuild)
    }

    fn remove_paths(
        &self,
        finding: &Finding,
        paths: &[PathBuf],
        cancel: &CancelToken,
    ) -> CleanOutcome {
        let delete_permanently = self.may_delete_permanently(finding);
        let mut outcome = CleanOutcome::empty(finding, CleanStatus::Done);
        let mut acted = 0usize;
        let mut already_gone = 0usize;

        let mut trash = TrashContext::new();
        // Finder's method needs an Automation permission and plays a sound for every item.
        trash.set_delete_method(DeleteMethod::NsFileManager);

        for path in paths {
            if cancel.is_cancelled() {
                outcome.errors.push("cancelado".to_string());
                break;
            }
            match self.policy.check_removal(path) {
                Ok(()) => {}
                Err(SafetyViolation::Missing(_)) => {
                    already_gone += 1;
                    continue;
                }
                Err(violation) => {
                    outcome.errors.push(violation.to_string());
                    continue;
                }
            }
            let size = measure(path, cancel).exclusive();
            if self.options.dry_run {
                acted += 1;
                if delete_permanently {
                    outcome.freed_bytes += size;
                } else {
                    outcome.trashed_bytes += size;
                }
                continue;
            }
            let result = if delete_permanently {
                delete_path(path)
            } else {
                trash.delete(path).map_err(|err| anyhow!(err.to_string()))
            };
            match result {
                Ok(()) => {
                    acted += 1;
                    if delete_permanently {
                        outcome.freed_bytes += size;
                    } else {
                        outcome.trashed_bytes += size;
                    }
                }
                Err(err) => outcome.errors.push(format!("{}: {err:#}", path.display())),
            }
        }

        outcome.status = match (acted, outcome.errors.is_empty()) {
            (0, true) if already_gone > 0 => {
                outcome.message = Some(crate::t!("ya no existe", "no longer exists").to_string());
                CleanStatus::Skipped
            }
            (0, _) => CleanStatus::Failed,
            (_, true) if self.options.dry_run => CleanStatus::WouldClean,
            (_, true) => CleanStatus::Done,
            (_, false) => CleanStatus::Partial,
        };
        outcome
    }

    fn run_command(&self, finding: &Finding, program: &str, args: &[String]) -> CleanOutcome {
        if let Err(violation) = self.policy.check_command(program) {
            return CleanOutcome::failed(finding, violation.to_string());
        }
        let Some(resolved) = exec::which(program, &self.home) else {
            return CleanOutcome::failed(
                finding,
                crate::tf!("{program} no está instalado", "{program} is not installed"),
            );
        };
        if self.options.dry_run {
            let mut outcome = CleanOutcome::empty(finding, CleanStatus::WouldClean);
            outcome.freed_bytes = finding.size;
            outcome.freed_is_estimate = true;
            outcome.message = Some(crate::tf!(
                "ejecutaría: {program} {}",
                "would run: {program} {}",
                args.join(" ")
            ));
            return outcome;
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        match exec::run(&resolved, &args, COMMAND_TIMEOUT) {
            Ok(output) if output.success() => {
                let mut outcome = CleanOutcome::empty(finding, CleanStatus::Done);
                outcome.freed_bytes = finding.size;
                outcome.freed_is_estimate = true;
                outcome
            }
            Ok(output) if output.timed_out => CleanOutcome::failed(
                finding,
                crate::tf!(
                    "{program} no terminó a tiempo",
                    "{program} did not finish in time"
                ),
            ),
            Ok(output) => CleanOutcome::failed(
                finding,
                crate::tf!(
                    "{program} terminó con error ({}): {}",
                    "{program} exited with an error ({}): {}",
                    output.code.unwrap_or(-1),
                    output.stderr.trim()
                ),
            ),
            Err(err) => CleanOutcome::failed(finding, format!("{err:#}")),
        }
    }

    fn clone_dedupe(
        &self,
        finding: &Finding,
        keep: &Path,
        duplicates: &[PathBuf],
        cancel: &CancelToken,
    ) -> CleanOutcome {
        let mut outcome = CleanOutcome::empty(finding, CleanStatus::Done);
        let mut acted = 0usize;
        let mut already_done = 0usize;

        for duplicate in duplicates {
            if cancel.is_cancelled() {
                outcome.errors.push("cancelado".to_string());
                break;
            }
            match self.dedupe_one(keep, duplicate, cancel) {
                Ok(0) => already_done += 1,
                Ok(freed) => {
                    acted += 1;
                    outcome.freed_bytes += freed;
                }
                Err(err) => outcome
                    .errors
                    .push(format!("{}: {err:#}", duplicate.display())),
            }
        }

        outcome.status = match (acted, outcome.errors.is_empty()) {
            (0, true) if already_done > 0 => {
                outcome.message =
                    Some(crate::t!("ya estaba deduplicado", "already deduplicated").to_string());
                CleanStatus::Skipped
            }
            (0, _) => CleanStatus::Failed,
            (_, true) if self.options.dry_run => CleanStatus::WouldClean,
            (_, true) => CleanStatus::Done,
            (_, false) => CleanStatus::Partial,
        };
        outcome
    }

    /// Returns the bytes released, or 0 if the two files already share their blocks.
    fn dedupe_one(&self, keep: &Path, duplicate: &Path, cancel: &CancelToken) -> Result<u64> {
        // The duplicate's name is replaced, so it needs the same clearance as a removal.
        self.policy.check_removal(duplicate)?;
        self.policy.check_removal(keep)?;

        let keep_meta = fs::symlink_metadata(keep)?;
        let duplicate_meta = fs::symlink_metadata(duplicate)?;
        if !keep_meta.is_file() || !duplicate_meta.is_file() {
            bail!(crate::t!("no es un archivo regular", "not a regular file"));
        }
        if keep_meta.dev() != duplicate_meta.dev() {
            bail!(crate::t!(
                "están en volúmenes distintos",
                "they are on different volumes"
            ));
        }
        if keep_meta.ino() == duplicate_meta.ino() {
            return Ok(0);
        }
        if let (Some(a), Some(b)) = (fsutil::clone_id(keep), fsutil::clone_id(duplicate)) {
            if a == b {
                return Ok(0);
            }
        }
        if keep_meta.len() != duplicate_meta.len() {
            bail!(crate::t!(
                "los tamaños no coinciden",
                "the sizes do not match"
            ));
        }
        // Never trust the scan's verdict: a file swapped by a clone must be proven identical.
        let keep_hash = fsutil::sha256_file(keep, cancel)?;
        let duplicate_hash = fsutil::sha256_file(duplicate, cancel)?;
        if keep_hash != duplicate_hash {
            bail!(crate::t!(
                "el contenido no coincide",
                "the contents do not match"
            ));
        }

        // Clones of something else release nothing, so ask APFS rather than counting blocks.
        let allocated = duplicate_meta.blocks() * 512;
        let freed = fsutil::exclusive_size(duplicate, allocated);
        if self.options.dry_run {
            return Ok(freed);
        }

        let parent = duplicate
            .parent()
            .context(crate::t!("sin carpeta padre", "no parent folder"))?;
        let name = duplicate
            .file_name()
            .context(crate::t!("sin nombre", "no name"))?
            .to_string_lossy();
        let temporary = parent.join(format!(".{name}.hm-clone-{}", std::process::id()));
        clone_file(keep, &temporary)?;
        // The clone takes the mode of `keep`. The duplicate's name should keep its own.
        let swap = fs::set_permissions(&temporary, duplicate_meta.permissions())
            .context(crate::t!(
                "no se pudieron copiar los permisos",
                "could not copy the permissions"
            ))
            .and_then(|()| {
                fs::rename(&temporary, duplicate).context(crate::t!(
                    "no se pudo reemplazar el archivo",
                    "could not replace the file"
                ))
            });
        if let Err(err) = swap {
            let _ = fs::remove_file(&temporary);
            return Err(err);
        }
        Ok(freed)
    }
}

fn unreadable_folder(dir: &Path, err: &std::io::Error) -> String {
    if err.kind() == ErrorKind::PermissionDenied {
        crate::tf!(
            "macOS no deja leer {}. Concede a HealthyMac el Acceso total al disco en Ajustes del Sistema.",
            "macOS won't let HealthyMac read {}. Give it Full Disk Access in System Settings.",
            dir.display()
        )
    } else {
        crate::tf!(
            "no se pudo leer {}: {err}",
            "could not read {}: {err}",
            dir.display()
        )
    }
}

fn delete_path(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// From <sys/clonefile.h>. `libc` does not export it.
const CLONE_NOFOLLOW: u32 = 0x0001;

fn clone_file(source: &Path, destination: &Path) -> Result<()> {
    let source_c = CString::new(source.as_os_str().as_bytes())?;
    let destination_c = CString::new(destination.as_os_str().as_bytes())?;
    // SAFETY: both pointers come from `CString`s that outlive the call.
    let result =
        unsafe { libc::clonefile(source_c.as_ptr(), destination_c.as_ptr(), CLONE_NOFOLLOW) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context(crate::t!(
            "clonefile falló (¿el volumen no es APFS?)",
            "clonefile failed (is the volume not APFS?)"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Area;

    struct Fixture {
        dir: tempfile::TempDir,
        home: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let home = dir.path().canonicalize().unwrap();
            Self { dir, home }
        }

        fn file(&self, relative: &str, content: &[u8]) -> PathBuf {
            let path = self.home.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }

        fn cleaner(&self, options: CleanOptions) -> Cleaner {
            Cleaner::new(&self.home, &[], options)
        }
    }

    fn safe_finding(paths: Vec<PathBuf>) -> Finding {
        Finding::new("t", Area::System, "k", "t")
            .risk(Risk::Safe)
            .recovery(Recovery::Automatic)
            .action(Action::Trash { paths })
    }

    fn clean(cleaner: &Cleaner, finding: Finding) -> CleanOutcome {
        cleaner
            .clean(&[finding], &CancelToken::new(), &|_| {})
            .outcomes
            .remove(0)
    }

    fn delete_mode() -> CleanOptions {
        CleanOptions {
            mode: RemovalMode::DeleteRegenerable,
            dry_run: false,
        }
    }

    #[test]
    fn dry_run_changes_nothing() {
        let fx = Fixture::new();
        let path = fx.file("Library/Caches/app/data.bin", &[1u8; 8192]);
        let cleaner = fx.cleaner(CleanOptions {
            mode: RemovalMode::DeleteRegenerable,
            dry_run: true,
        });
        let outcome = clean(&cleaner, safe_finding(vec![path.clone()]));
        assert_eq!(outcome.status, CleanStatus::WouldClean);
        assert!(outcome.freed_bytes > 0);
        assert!(path.exists());
    }

    #[test]
    fn delete_mode_removes_safe_regenerable_findings() {
        let fx = Fixture::new();
        let path = fx.file("Library/Caches/app/data.bin", &[1u8; 8192]);
        let outcome = clean(&fx.cleaner(delete_mode()), safe_finding(vec![path.clone()]));
        assert_eq!(outcome.status, CleanStatus::Done);
        assert!(outcome.freed_bytes >= 8192);
        assert_eq!(outcome.trashed_bytes, 0);
        assert!(!path.exists());
    }

    #[test]
    fn safety_policy_blocks_cleaning_even_if_a_scanner_asks() {
        let fx = Fixture::new();
        let secret = fx.file(".ssh/id_ed25519", b"secret");
        let outcome = clean(
            &fx.cleaner(delete_mode()),
            safe_finding(vec![secret.clone()]),
        );
        assert_eq!(outcome.status, CleanStatus::Failed);
        assert!(secret.exists());
    }

    #[test]
    fn a_blocked_path_does_not_stop_the_others() {
        let fx = Fixture::new();
        let secret = fx.file(".ssh/id_ed25519", b"secret");
        let cache = fx.file("Library/Caches/app/x", b"x");
        let outcome = clean(
            &fx.cleaner(delete_mode()),
            safe_finding(vec![secret.clone(), cache.clone()]),
        );
        assert_eq!(outcome.status, CleanStatus::Partial);
        assert!(secret.exists());
        assert!(!cache.exists());
    }

    #[test]
    fn delete_mode_is_ignored_for_anything_not_regenerable() {
        let fx = Fixture::new();
        let cleaner = fx.cleaner(delete_mode());
        let review = Finding::new("t", Area::Files, "k", "t")
            .risk(Risk::Review)
            .recovery(Recovery::None);
        assert!(!cleaner.may_delete_permanently(&review));
        let redownload = Finding::new("t", Area::LocalAi, "k", "t")
            .risk(Risk::Moderate)
            .recovery(Recovery::Redownload);
        assert!(!cleaner.may_delete_permanently(&redownload));
        let safe_but_unrecoverable = Finding::new("t", Area::System, "k", "t")
            .risk(Risk::Safe)
            .recovery(Recovery::None);
        assert!(!cleaner.may_delete_permanently(&safe_but_unrecoverable));
    }

    #[test]
    fn trash_mode_never_deletes_permanently() {
        let fx = Fixture::new();
        let cleaner = fx.cleaner(CleanOptions::default());
        let finding = safe_finding(vec![]);
        assert!(!cleaner.may_delete_permanently(&finding));
    }

    #[test]
    fn missing_target_is_skipped_not_failed() {
        let fx = Fixture::new();
        let gone = fx.home.join("Library/Caches/gone");
        let outcome = clean(&fx.cleaner(delete_mode()), safe_finding(vec![gone]));
        assert_eq!(outcome.status, CleanStatus::Skipped);
    }

    #[test]
    fn contents_mode_keeps_the_folder() {
        let fx = Fixture::new();
        let a = fx.file("Library/Caches/app/a", b"a");
        let b = fx.file("Library/Caches/app/sub/b", b"b");
        let dir = a.parent().unwrap().to_path_buf();
        let finding = safe_finding(vec![]).action(Action::TrashContents { dir: dir.clone() });
        let outcome = clean(&fx.cleaner(delete_mode()), finding);
        assert_eq!(outcome.status, CleanStatus::Done);
        assert!(dir.is_dir());
        assert!(!a.exists());
        assert!(!b.exists());
    }

    #[test]
    fn empty_trash_deletes_only_the_trash_of_this_home() {
        let fx = Fixture::new();
        let trashed = fx.file(".Trash/viejo/archivo.bin", &[1u8; 4096]);
        let kept = fx.file("Coding/proyecto/main.rs", b"fn main() {}");
        let finding = Finding::new("t", Area::System, "k", "t").action(Action::EmptyTrash);

        let dry = fx.cleaner(CleanOptions {
            dry_run: true,
            ..CleanOptions::default()
        });
        assert_eq!(clean(&dry, finding.clone()).status, CleanStatus::WouldClean);
        assert!(trashed.exists());

        let outcome = clean(&fx.cleaner(CleanOptions::default()), finding.clone());
        assert_eq!(outcome.status, CleanStatus::Done, "{:?}", outcome.errors);
        assert!(outcome.freed_bytes >= 4096);
        assert!(!trashed.exists());
        assert!(fx.home.join(".Trash").is_dir());
        assert!(kept.exists());

        let again = clean(&fx.cleaner(CleanOptions::default()), finding);
        assert_eq!(again.status, CleanStatus::Skipped);
    }

    #[test]
    fn a_folder_it_cannot_read_is_a_failure_not_an_empty_folder() {
        use std::os::unix::fs::PermissionsExt;
        let fx = Fixture::new();
        fx.file(".Trash/viejo.bin", b"x");
        let trash = fx.home.join(".Trash");
        fs::set_permissions(&trash, fs::Permissions::from_mode(0o000)).unwrap();

        let finding = Finding::new("t", Area::System, "k", "t").action(Action::EmptyTrash);
        let outcome = clean(&fx.cleaner(CleanOptions::default()), finding);
        let contents = safe_finding(vec![]).action(Action::TrashContents { dir: trash.clone() });
        let contents_outcome = clean(&fx.cleaner(CleanOptions::default()), contents);

        // Restored first, so a failed assertion can't leave a folder the temp dir can't remove.
        fs::set_permissions(&trash, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(outcome.status, CleanStatus::Failed);
        assert!(
            outcome.errors[0].contains("Acceso total al disco"),
            "{:?}",
            outcome.errors
        );
        assert_eq!(contents_outcome.status, CleanStatus::Failed);
    }

    #[test]
    fn the_size_freed_by_a_tool_is_marked_as_an_estimate() {
        let fx = Fixture::new();
        let finding = Finding::new("t", Area::Developer, "k", "t")
            .size(1000)
            .action(Action::Command {
                program: "git".to_string(),
                args: vec!["--version".to_string()],
            });
        let cleaner = fx.cleaner(CleanOptions {
            dry_run: true,
            ..CleanOptions::default()
        });
        let report = cleaner.clean(&[finding], &CancelToken::new(), &|_| {});
        assert!(report.outcomes[0].freed_is_estimate);
        assert!(report.has_estimates());
    }

    #[test]
    fn report_only_never_touches_anything() {
        let fx = Fixture::new();
        let finding = Finding::new("t", Area::Files, "k", "t");
        let outcome = clean(&fx.cleaner(delete_mode()), finding);
        assert_eq!(outcome.status, CleanStatus::Skipped);
    }

    #[test]
    fn commands_outside_the_allow_list_are_refused() {
        let fx = Fixture::new();
        let victim = fx.file("Library/Caches/app/x", b"x");
        let finding = Finding::new("t", Area::System, "k", "t").action(Action::Command {
            program: "rm".to_string(),
            args: vec![victim.display().to_string()],
        });
        let outcome = clean(&fx.cleaner(CleanOptions::default()), finding);
        assert_eq!(outcome.status, CleanStatus::Failed);
        assert!(victim.exists());
    }

    #[test]
    fn a_running_process_blocks_the_finding() {
        let fx = Fixture::new();
        let path = fx.file("Library/Caches/app/x", b"x");
        // launchd is always running.
        let finding = safe_finding(vec![path.clone()]).blocked_by(&["launchd"]);
        let outcome = clean(&fx.cleaner(delete_mode()), finding);
        assert_eq!(outcome.status, CleanStatus::Skipped);
        assert!(path.exists());
    }

    #[test]
    fn clone_dedupe_keeps_both_paths_valid_and_independent() {
        let fx = Fixture::new();
        let content = vec![9u8; 256 * 1024];
        let keep = fx.file(".lmstudio/models/org/m/model.gguf", &content);
        let duplicate = fx.file(".cache/huggingface/hub/m/blobs/abc", &content);
        let finding = Finding::new("t", Area::LocalAi, "k", "t").action(Action::CloneDedupe {
            keep: keep.clone(),
            duplicates: vec![duplicate.clone()],
        });

        let outcome = clean(&fx.cleaner(CleanOptions::default()), finding.clone());
        assert_eq!(outcome.status, CleanStatus::Done, "{:?}", outcome.errors);
        assert!(outcome.freed_bytes >= 256 * 1024);
        assert_eq!(fs::read(&keep).unwrap(), content);
        assert_eq!(fs::read(&duplicate).unwrap(), content);
        assert_eq!(fsutil::clone_id(&keep), fsutil::clone_id(&duplicate));

        // Running it again finds nothing left to do.
        let again = clean(&fx.cleaner(CleanOptions::default()), finding);
        assert_eq!(again.status, CleanStatus::Skipped);

        // Clones are copy-on-write: writing to one must not change the other.
        fs::write(&duplicate, b"changed").unwrap();
        assert_eq!(fs::read(&keep).unwrap(), content);
    }

    #[test]
    fn clone_dedupe_refuses_files_with_different_content() {
        let fx = Fixture::new();
        let keep = fx.file("a/model.gguf", &[1u8; 4096]);
        let other = fx.file("b/model.gguf", &[2u8; 4096]);
        let finding = Finding::new("t", Area::LocalAi, "k", "t").action(Action::CloneDedupe {
            keep,
            duplicates: vec![other.clone()],
        });
        let outcome = clean(&fx.cleaner(CleanOptions::default()), finding);
        assert_eq!(outcome.status, CleanStatus::Failed);
        assert_eq!(fs::read(&other).unwrap(), vec![2u8; 4096]);
    }

    #[test]
    fn dry_run_clone_dedupe_changes_nothing() {
        let fx = Fixture::new();
        let keep = fx.file("a/model.gguf", &[1u8; 4096]);
        let duplicate = fx.file("b/model.gguf", &[1u8; 4096]);
        let ino_before = fs::metadata(&duplicate).unwrap().ino();
        let finding = Finding::new("t", Area::LocalAi, "k", "t").action(Action::CloneDedupe {
            keep,
            duplicates: vec![duplicate.clone()],
        });
        let cleaner = fx.cleaner(CleanOptions {
            dry_run: true,
            ..CleanOptions::default()
        });
        let outcome = clean(&cleaner, finding);
        assert_eq!(outcome.status, CleanStatus::WouldClean);
        assert_eq!(fs::metadata(&duplicate).unwrap().ino(), ino_before);
    }

    #[test]
    fn audit_log_records_real_cleans_but_not_dry_runs() {
        let fx = Fixture::new();
        let log_path = fx.home.join("history.jsonl");
        let a = fx.file("Library/Caches/app/a", b"a");

        let dry = fx
            .cleaner(CleanOptions {
                dry_run: true,
                ..delete_mode()
            })
            .with_audit(AuditLog::new(&log_path));
        clean(&dry, safe_finding(vec![a.clone()]));
        assert!(AuditLog::new(&log_path).read_all().unwrap().is_empty());

        let real = fx
            .cleaner(delete_mode())
            .with_audit(AuditLog::new(&log_path));
        clean(&real, safe_finding(vec![a]));
        let entries = AuditLog::new(&log_path).read_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].outcome.status, CleanStatus::Done);
    }

    #[test]
    fn cancelled_clean_stops_before_starting() {
        let fx = Fixture::new();
        let path = fx.file("Library/Caches/app/a", b"a");
        let cancel = CancelToken::new();
        cancel.cancel();
        let report =
            fx.cleaner(delete_mode())
                .clean(&[safe_finding(vec![path.clone()])], &cancel, &|_| {});
        assert!(report.outcomes.is_empty());
        assert!(path.exists());
        drop(fx.dir);
    }
}
