//! Dependencies and build output inside the user's projects.

use anyhow::Result;
use hm_core::fmt::human_age;
use hm_core::{t, tf, Action, Area, Finding, ScanContext, Scanner, SizeInfo};

use super::full_disk_access_notice;
use crate::sweep::{shared_sweep, SweptArtifact, SweptProject};
use crate::util;

const ID: &str = "dev.projects";

pub(super) struct ArtifactScanner;

impl Scanner for ArtifactScanner {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        t!("Artefactos de proyectos", "Project build artifacts")
    }

    fn description(&self) -> &'static str {
        t!(
            "Dependencias y compilados de tus proyectos. Se preseleccionan solo los de proyectos inactivos.",
            "Dependencies and build output of your projects. Only those of idle projects are preselected."
        )
    }

    fn area(&self) -> Area {
        Area::Developer
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        let sweep = shared_sweep(ctx);
        let mut findings: Vec<Finding> = sweep
            .projects
            .iter()
            .flat_map(|project| project_findings(ctx, project))
            .collect();
        if sweep.skipped_protected {
            findings.push(full_disk_access_notice(
                ID,
                "permisos",
                t!(
                    "Tu carpeta personal es una de las raíces de proyectos, pero Documentos, Escritorio y Descargas no se revisaron porque hacerlo pediría permisos en mitad del escaneo. Concede Acceso total al disco para incluirlas, o añade solo las carpetas que quieras como raíces.",
                    "Your home folder is one of the project roots, but Documents, Desktop and Downloads were not checked, because that would ask for permission in the middle of the scan. Grant Full Disk Access to include them, or add only the folders you want as roots."
                ),
            ));
        }
        Ok(util::finalize(ctx, findings))
    }
}

/// How the project's activity bears on this finding, for its explanation.
fn activity_sentence(ctx: &ScanContext, project: &SweptProject, idle: bool) -> String {
    match (project.last_activity(), project.idle_days(ctx.now)) {
        (_, Some(days)) if idle => {
            tf!(
                "El proyecto lleva {days} días sin actividad.",
                "The project has had no activity for {days} days."
            )
        }
        (Some(last), Some(_)) => tf!(
            "El proyecto tuvo actividad {}, así que no va preseleccionado.",
            "The project was last active {}, so it is not preselected.",
            human_age(last, ctx.now)
        ),
        _ => t!(
            "No se pudo saber cuándo tuvo actividad el proyecto, así que no va preseleccionado.",
            "HealthyMac couldn't tell when the project was last active, so it is not preselected."
        )
        .to_string(),
    }
}

/// Removing it would release under a quarter of what it occupies, because the rest is cloned from
/// or linked to something that stays on disk, like the pnpm store or the uv cache. It would mean
/// reinstalling for almost nothing, so it is listed but never preselected.
fn mostly_shared(info: &SizeInfo) -> bool {
    info.exclusive().saturating_mul(4) < info.bytes
}

fn shared_note(artifact: &SweptArtifact) -> &'static str {
    if mostly_shared(&artifact.info) {
        t!(
            " Casi todo está compartido con otra carpeta, como el almacén global de su gestor de paquetes, y seguiría en disco: limpiarlo apenas liberaría espacio, así que no va preseleccionado.",
            " Almost all of it is shared with another folder, such as its package manager's global store, and would stay on disk: cleaning it would free almost nothing, so it is not preselected."
        )
    } else if artifact.info.shared > 0 {
        t!(
            " Parte de su tamaño está compartido con otra carpeta y seguiría en disco.",
            " Part of its size is shared with another folder and would stay on disk."
        )
    } else {
        ""
    }
}

fn project_findings(ctx: &ScanContext, project: &SweptProject) -> Vec<Finding> {
    let idle = project
        .idle_days(ctx.now)
        .is_some_and(|days| days >= u64::from(ctx.config.project_idle_days));
    let activity = activity_sentence(ctx, project, idle);

    let mut findings = Vec::new();
    let mut aggregated: Vec<&SweptArtifact> = Vec::new();
    for artifact in &project.artifacts {
        if artifact.rule.aggregate {
            aggregated.push(artifact);
            continue;
        }
        let rule = artifact.rule;
        let recover = match artifact.hint {
            Some(command) => tf!(
                "Se recupera con «{command}».",
                "It comes back with “{command}”."
            ),
            None => rule.recover.get().to_string(),
        };
        findings.push(
            Finding::new(
                ID,
                Area::Developer,
                artifact.path.display().to_string(),
                artifact.label.clone(),
            )
            .group(project.display.clone())
            .explanation(format!(
                "{} {recover}{} {activity}",
                rule.what.get(),
                shared_note(artifact)
            ))
            .measured(&artifact.info)
            .last_used(project.last_activity())
            .risk(rule.class.risk())
            .recovery(rule.class.recovery())
            .selected(idle && rule.preselect && !mostly_shared(&artifact.info))
            .trash(&artifact.path)
            .tag(if idle { "inactivo" } else { "activo" }),
        );
    }

    // `__pycache__` appears beside every module. One line per project is all anyone wants to see.
    if let Some(first) = aggregated.first() {
        let rule = first.rule;
        let paths: Vec<_> = aggregated
            .iter()
            .map(|artifact| artifact.path.clone())
            .collect();
        let bytes = aggregated
            .iter()
            .map(|artifact| artifact.info.exclusive())
            .sum();
        let shared = aggregated.iter().map(|artifact| artifact.info.shared).sum();
        let files = aggregated.iter().map(|artifact| artifact.info.files).sum();
        let title = if paths.len() == 1 {
            first.label.clone()
        } else {
            format!(
                "{} ({})",
                first.label,
                util::count(
                    paths.len() as u64,
                    t!("carpeta", "folder"),
                    t!("carpetas", "folders")
                )
            )
        };
        findings.push(
            Finding::new(
                ID,
                Area::Developer,
                format!("{}/{}", project.path.display(), first.label),
                title,
            )
            .group(project.display.clone())
            .subtitle(project.path.display().to_string())
            .explanation(format!(
                "{} {} {activity}",
                rule.what.get(),
                rule.recover.get()
            ))
            .size(bytes)
            .shared_bytes(shared)
            .items(files)
            .last_used(project.last_activity())
            .risk(rule.class.risk())
            .recovery(rule.class.recovery())
            .selected(idle && rule.preselect)
            .action(Action::Trash { paths })
            .tag(if idle { "inactivo" } else { "activo" }),
        );
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;
    use hm_core::{Recovery, Risk};
    use std::time::{Duration, SystemTime};

    fn scan(ctx: &ScanContext) -> Vec<Finding> {
        ArtifactScanner.scan(ctx).unwrap()
    }

    #[test]
    fn something_that_would_free_almost_nothing_is_not_worth_preselecting() {
        let occupying = |bytes, shared| SizeInfo {
            bytes,
            shared,
            ..SizeInfo::default()
        };
        // A uv `.venv` measured on a real machine: 961 MB occupied, 45 MB exclusive.
        assert!(mostly_shared(&occupying(961, 916)));
        assert!(!mostly_shared(&occupying(100, 75)));
        assert!(!mostly_shared(&occupying(100, 0)));
    }

    fn by_title<'a>(findings: &'a [Finding], title: &str) -> &'a Finding {
        findings
            .iter()
            .find(|finding| finding.title == title)
            .unwrap_or_else(|| panic!("no finding called {title}: {findings:?}"))
    }

    const DAY: Duration = Duration::from_secs(86_400);

    #[test]
    fn idle_projects_are_preselected_and_active_ones_only_listed() {
        let fx = Fixture::new();
        fx.file("Coding/old/package.json", b"{}");
        fx.file("Coding/old/node_modules/x.js", b"x");
        fx.set_age("Coding/old/package.json", 60);
        fx.file("Coding/new/package.json", b"{}");
        fx.file("Coding/new/node_modules/x.js", b"x");

        let found = scan(&fx.ctx());
        assert_eq!(found.len(), 2);
        let old = found
            .iter()
            .find(|f| f.group.as_deref() == Some("old"))
            .unwrap();
        let new = found
            .iter()
            .find(|f| f.group.as_deref() == Some("new"))
            .unwrap();
        assert!(old.selected_by_default);
        assert!(!new.selected_by_default);
        assert_eq!(old.recovery, Recovery::Redownload);
        assert_eq!(old.area, Area::Developer);
        assert!(
            old.explanation.contains("sin actividad"),
            "{}",
            old.explanation
        );
        assert!(
            new.explanation.contains("no va preseleccionado"),
            "{}",
            new.explanation
        );
    }

    #[test]
    fn a_freshly_rebuilt_artifact_does_not_make_a_project_active() {
        let fx = Fixture::new();
        fx.file("Coding/rs/Cargo.toml", b"");
        fx.file("Coding/rs/src/main.rs", b"fn main() {}");
        fx.set_age("Coding/rs/Cargo.toml", 60);
        fx.set_age("Coding/rs/src/main.rs", 60);
        // Written just now, which is what any build does.
        fx.file("Coding/rs/target/debug/app", b"x");
        let found = scan(&fx.ctx());
        let target = by_title(&found, "target");
        assert!(target.selected_by_default);
        assert_eq!(target.recovery, Recovery::Rebuild);
    }

    #[test]
    fn a_recent_commit_keeps_a_project_active_even_with_old_files() {
        let fx = Fixture::new();
        fx.file("Coding/rs/Cargo.toml", b"");
        fx.file("Coding/rs/target/debug/app", b"x");
        fx.file("Coding/rs/.gitignore", b"target\n");
        fx.git("Coding/rs", &["init", "-q"]);
        fx.git("Coding/rs", &["add", "Cargo.toml", ".gitignore"]);
        fx.git("Coding/rs", &["commit", "-q", "-m", "x"]);
        fx.set_age("Coding/rs/Cargo.toml", 60);
        fx.set_age("Coding/rs/.gitignore", 60);
        let found = scan(&fx.ctx());
        assert!(!by_title(&found, "target").selected_by_default);
    }

    #[test]
    fn idle_threshold_follows_the_config() {
        let fx = Fixture::new();
        fx.file("Coding/old/package.json", b"{}");
        fx.file("Coding/old/node_modules/x.js", b"x");
        fx.set_age("Coding/old/package.json", 10);

        let mut ctx = fx.ctx();
        ctx.config.project_idle_days = 30;
        assert!(!scan(&ctx)[0].selected_by_default);

        let mut ctx = fx.ctx();
        ctx.config.project_idle_days = 5;
        assert!(scan(&ctx)[0].selected_by_default);
    }

    #[test]
    fn a_project_with_a_modification_time_in_the_future_is_not_idle() {
        let fx = Fixture::new();
        fx.file("Coding/odd/package.json", b"{}");
        fx.file("Coding/odd/node_modules/x.js", b"x");
        fx.set_mtime("Coding/odd/package.json", SystemTime::now() + 400 * DAY);
        assert!(!scan(&fx.ctx())[0].selected_by_default);
    }

    #[test]
    fn ambiguous_outputs_that_might_not_regenerate_are_never_preselected() {
        let fx = Fixture::new();
        fx.file("Coding/app/package.json", b"{}");
        fx.file("Coding/app/.gitignore", b"dist/\nout/\n");
        fx.file("Coding/app/dist/x.js", b"x");
        fx.file("Coding/app/out/results.csv", b"x");
        fx.git("Coding/app", &["init", "-q"]);
        for rel in ["Coding/app/package.json", "Coding/app/.gitignore"] {
            fx.set_age(rel, 60);
        }
        let found = scan(&fx.ctx());
        assert!(by_title(&found, "dist").selected_by_default);
        let out = by_title(&found, "out");
        assert!(
            !out.selected_by_default,
            "idle or not, out/ can hold results"
        );
        assert_eq!(out.risk, Risk::Moderate);
    }

    #[test]
    fn pycache_is_one_line_per_project() {
        let fx = Fixture::new();
        fx.file("Coding/py/pyproject.toml", b"");
        fx.file("Coding/py/a/__pycache__/m.pyc", b"x");
        fx.file("Coding/py/b/__pycache__/n.pyc", b"x");
        let found = scan(&fx.ctx());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].title, "__pycache__ (2 carpetas)");
        assert_eq!(found[0].target_paths().len(), 2);
        assert_eq!(found[0].risk, Risk::Safe);
    }

    #[test]
    fn the_lockfile_says_how_to_get_dependencies_back() {
        let fx = Fixture::new();
        fx.file("Coding/web/package.json", b"{}");
        fx.file("Coding/web/pnpm-lock.yaml", b"");
        fx.file("Coding/web/node_modules/x.js", b"x");
        let found = scan(&fx.ctx());
        assert!(
            found[0].explanation.contains("pnpm install"),
            "{}",
            found[0].explanation
        );
        // This file isn't shared with anything, so nothing should claim it is.
        assert!(!found[0].explanation.contains("compartido"));
    }

    #[test]
    fn dependencies_cloned_from_a_global_store_report_what_is_really_released() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let fx = Fixture::new();
        let store = fx.file(".pnpm-store/pkg.bin", &vec![7u8; 1024 * 1024]);
        fx.file("Coding/web/package.json", b"{}");
        fx.file("Coding/web/pnpm-lock.yaml", b"");
        let cloned = fx.file("Coding/web/node_modules/placeholder", b"x");
        std::fs::remove_file(&cloned).unwrap();
        let source = CString::new(store.as_os_str().as_bytes()).unwrap();
        let destination = CString::new(cloned.as_os_str().as_bytes()).unwrap();
        // SAFETY: both pointers come from `CString`s that outlive the call.
        assert_eq!(
            unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) },
            0
        );

        // A little of its own, so there is something to list: a finding that would release nothing
        // at all is dropped.
        fx.file("Coding/web/node_modules/own.js", &vec![1u8; 16 * 1024]);

        let found = scan(&fx.ctx());
        assert!(
            found[0].size < 64 * 1024,
            "the clone itself releases nothing"
        );
        assert!(found[0].shared_bytes >= 1024 * 1024);
        assert!(!found[0].selected_by_default);
        assert!(
            found[0].explanation.contains("almacén global"),
            "{}",
            found[0].explanation
        );
    }

    #[test]
    fn a_project_outside_the_home_folder_is_reported_not_offered() {
        let fx = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        let project = outside.path().join("work/app");
        std::fs::create_dir_all(project.join("node_modules")).unwrap();
        std::fs::write(project.join("package.json"), b"{}").unwrap();
        std::fs::write(project.join("node_modules/x.js"), b"x").unwrap();

        let mut ctx = fx.ctx();
        ctx.config.project_roots = vec![outside.path().join("work")];
        let found = scan(&ctx);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].action, Action::ReportOnly);
        assert!(!found[0].selected_by_default);
    }

    #[test]
    fn home_as_a_root_explains_what_was_skipped_exactly_once() {
        let fx = Fixture::new();
        fx.file("Documents/a/package.json", b"{}");
        fx.file("Downloads/b/package.json", b"{}");
        fx.file("Desktop/c/package.json", b"{}");
        let mut ctx = fx.ctx();
        ctx.config.project_roots = vec![fx.home.clone()];
        let found = scan(&ctx);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].action, Action::ReportOnly);
    }

    #[test]
    fn every_finding_can_pass_the_cleaner_safety_policy() {
        let fx = Fixture::new();
        fx.file("Coding/web/package.json", b"{}");
        fx.file("Coding/web/node_modules/x.js", b"x");
        let ctx = fx.ctx();
        let policy = hm_core::SafetyPolicy::new(&ctx.home, &[]);
        for finding in scan(&ctx) {
            for path in finding.target_paths() {
                assert_eq!(policy.check_removal(path), Ok(()));
            }
        }
    }
}
