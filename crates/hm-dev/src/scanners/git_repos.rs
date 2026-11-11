//! Git repositories whose `.git` folder has grown large.

use anyhow::Result;
use hm_core::fmt::human_size;
use hm_core::{t, tf, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner};
use rayon::prelude::*;

use crate::sweep::{shared_sweep, SweptProject};
use crate::util;

const ID: &str = "dev.git";
const LARGE_GIT_DIR: u64 = 500_000_000;
/// Below this much loose data, with little fragmentation, `git gc` would achieve next to nothing.
const WORTH_COMPACTING: u64 = 50_000_000;
const FRAGMENTED_PACKS: u64 = 4;

pub(super) struct LargeGitScanner;

impl Scanner for LargeGitScanner {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        t!("Repositorios Git grandes", "Large Git repositories")
    }

    fn description(&self) -> &'static str {
        t!(
            "Repositorios cuya carpeta .git ha crecido y puede compactarse con git gc.",
            "Repositories whose .git folder has grown and can be compacted with git gc."
        )
    }

    fn area(&self) -> Area {
        Area::Developer
    }

    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        let sweep = shared_sweep(ctx);
        let git = util::tool(ctx, "git");
        let findings = sweep
            .projects
            .par_iter()
            .filter(|project| project.git_dir_bytes >= LARGE_GIT_DIR)
            .filter_map(|project| {
                let objects = git.as_deref().and_then(|git| {
                    let output = util::run_git(git, &project.path, &["count-objects", "-v"])?;
                    output
                        .success()
                        .then(|| parse_count_objects(&output.stdout))
                });
                finding_for(project, objects)
            })
            .collect();
        Ok(util::finalize(ctx, findings))
    }
}

/// The figures from `git count-objects -v`, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ObjectCounts {
    loose_bytes: u64,
    packs: u64,
    pack_bytes: u64,
    garbage_bytes: u64,
}

pub(crate) fn parse_count_objects(output: &str) -> ObjectCounts {
    let mut counts = ObjectCounts::default();
    for line in output.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let Ok(value) = value.trim().parse::<u64>() else {
            continue;
        };
        match key.trim() {
            "size" => counts.loose_bytes = value.saturating_mul(1024),
            "packs" => counts.packs = value,
            "size-pack" => counts.pack_bytes = value.saturating_mul(1024),
            "size-garbage" => counts.garbage_bytes = value.saturating_mul(1024),
            _ => {}
        }
    }
    counts
}

fn finding_for(project: &SweptProject, objects: Option<ObjectCounts>) -> Option<Finding> {
    // Only a repository with its own `.git` folder: a worktree or submodule's `git gc` would really
    // be working on another repository's data.
    if project.git_dir.as_deref() != Some(project.path.join(".git").as_path()) {
        return None;
    }
    let total = project.git_dir_bytes;
    let base = Finding::new(
        ID,
        Area::Developer,
        project.path.display().to_string(),
        tf!(
            "Repositorio Git de {}",
            "{} Git repository",
            human_size(total)
        ),
    )
    .group(project.display.clone())
    .subtitle(project.path.display().to_string())
    .risk(Risk::Safe)
    .recovery(Recovery::Automatic);

    let Some(objects) = objects else {
        return Some(base.explanation(t!(
            "La carpeta .git es grande, pero Git no pudo informar de cuánto se podría recuperar, así que no se ofrece compactarla.",
            "The .git folder is large, but Git couldn't report how much could be reclaimed, so compacting it is not offered."
        )));
    };
    let reclaimable = objects.loose_bytes.saturating_add(objects.garbage_bytes);
    if reclaimable < WORTH_COMPACTING && objects.packs < FRAGMENTED_PACKS {
        return Some(base.explanation(tf!(
            "Ya está compactado: {} de sus datos están en {}, así que git gc liberaría muy poco. Su tamaño se debe a su historial.",
            "It is already compacted: {} of its data is in {}, so git gc would free very little. Its size comes from its history.",
            human_size(objects.pack_bytes),
            util::count(objects.packs, t!("paquete", "pack"), t!("paquetes", "packs")),
        )));
    }
    let Some(path) = project.path.to_str() else {
        return Some(base.explanation(t!(
            "Se podría compactar con git gc, pero la ruta tiene caracteres que HealthyMac no puede pasarle a Git.",
            "It could be compacted with git gc, but the path has characters HealthyMac can't pass to Git."
        )));
    };
    Some(
        base.size(reclaimable)
            .explanation(tf!(
                "Tiene {} sueltos o sobrantes y {} de Git, que git gc puede compactar. Lo indicado es una estimación: puede liberar algo menos, o más si hay mucha fragmentación. Tus commits y ramas no cambian; solo desaparecen objetos inalcanzables antiguos, los que ya no aparecen ni en el reflog. Puede tardar varios minutos en un repositorio grande.",
                "It has {} of loose or leftover objects and {} of Git data, which git gc can compact. The figure shown is an estimate: it may free a little less, or more if there is a lot of fragmentation. Your commits and branches don't change; only old unreachable objects go, the ones no longer even in the reflog. It can take several minutes on a large repository.",
                human_size(reclaimable),
                util::count(objects.packs, t!("paquete", "pack"), t!("paquetes", "packs")),
            ))
            .action(Action::Command {
                program: "git".to_string(),
                args: vec!["-C".to_string(), path.to_string(), "gc".to_string()],
            })
            .tag("estimacion"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const CAPTURED: &str = "count: 8120\nsize: 91234\nin-pack: 942113\npacks: 6\nsize-pack: 1843210\nprune-packable: 12\ngarbage: 1\nsize-garbage: 2048\n";

    fn project(path: &str, bytes: u64, git_dir: Option<PathBuf>) -> SweptProject {
        SweptProject {
            path: PathBuf::from(path),
            name: "repo".to_string(),
            display: "repo".to_string(),
            kinds: Vec::new(),
            artifacts: Vec::new(),
            newest_source: None,
            last_commit: None,
            repo: Some(PathBuf::from(path)),
            git_dir,
            git_dir_bytes: bytes,
        }
    }

    #[test]
    fn parses_realistic_count_objects() {
        let counts = parse_count_objects(CAPTURED);
        assert_eq!(counts.loose_bytes, 91_234 * 1024);
        assert_eq!(counts.packs, 6);
        assert_eq!(counts.pack_bytes, 1_843_210 * 1024);
        assert_eq!(counts.garbage_bytes, 2_048 * 1024);
        assert_eq!(
            parse_count_objects("nonsense\nsize: lots\n"),
            ObjectCounts::default()
        );
    }

    #[test]
    fn offers_gc_with_an_estimate_not_the_whole_folder() {
        let repo = project(
            "/h/repo",
            2_000_000_000,
            Some(PathBuf::from("/h/repo/.git")),
        );
        let finding = finding_for(&repo, Some(parse_count_objects(CAPTURED))).unwrap();
        assert_eq!(finding.risk, Risk::Safe);
        assert!(!finding.selected_by_default);
        assert_eq!(finding.size, (91_234 + 2_048) * 1024);
        assert_eq!(
            finding.action,
            Action::Command {
                program: "git".to_string(),
                args: vec!["-C".to_string(), "/h/repo".to_string(), "gc".to_string()],
            }
        );
    }

    #[test]
    fn a_repository_already_compact_is_only_reported() {
        let repo = project(
            "/h/repo",
            2_000_000_000,
            Some(PathBuf::from("/h/repo/.git")),
        );
        let compact = "size: 12\npacks: 1\nsize-pack: 1900000\nsize-garbage: 0\n";
        let finding = finding_for(&repo, Some(parse_count_objects(compact))).unwrap();
        assert_eq!(finding.action, Action::ReportOnly);
        assert_eq!(finding.size, 0);
    }

    #[test]
    fn git_failing_means_nothing_is_offered() {
        let repo = project(
            "/h/repo",
            2_000_000_000,
            Some(PathBuf::from("/h/repo/.git")),
        );
        assert_eq!(finding_for(&repo, None).unwrap().action, Action::ReportOnly);
    }

    #[test]
    fn worktrees_and_submodules_are_skipped() {
        let worktree = project(
            "/h/wt",
            2_000_000_000,
            Some(PathBuf::from("/h/repo/.git/worktrees/wt")),
        );
        assert!(finding_for(&worktree, Some(parse_count_objects(CAPTURED))).is_none());
    }
}
