//! Junk scanners for the Developer area.

mod artifacts;
mod containers;
mod git_repos;
mod packages;
mod toolchains;
mod tools;
mod xcode;

use hm_core::{fsutil, t, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner};
use rayon::prelude::*;

use crate::util;

/// Every scanner this crate provides.
pub fn scanners() -> Vec<Box<dyn Scanner>> {
    vec![
        Box::new(xcode::XcodeScanner),
        Box::new(artifacts::ArtifactScanner),
        Box::new(packages::PackageCacheScanner),
        Box::new(tools::ToolCacheScanner),
        Box::new(toolchains::ToolchainScanner),
        Box::new(containers::DockerScanner),
        Box::new(git_repos::LargeGitScanner),
    ]
}

/// What to do when a command-based cleanup's tool is not installed.
#[derive(Clone, Copy, Debug)]
enum Fallback {
    /// Move the folder's contents to the Trash instead.
    TrashContents,
    /// Do not offer a clean at all, because going around the tool would not work.
    ReportOnly(&'static str),
}

/// How a cache in the table is removed.
#[derive(Clone, Copy, Debug)]
enum Removal {
    /// Everything inside each listed folder, keeping the folder. One finding per folder.
    Contents,
    /// The listed folders themselves, together, as one finding.
    Folders,
    /// The tool's own command, measured on the first listed folder with anything in it.
    Command {
        program: &'static str,
        args: &'static [&'static str],
        fallback: Fallback,
    },
}

/// One well-known cache. Paths are relative to the home folder.
#[derive(Clone, Copy, Debug)]
struct CacheSpec {
    key: &'static str,
    group: &'static str,
    title: &'static str,
    paths: &'static [&'static str],
    removal: Removal,
    risk: Risk,
    recovery: Recovery,
    selected: bool,
    blocked_by: &'static [&'static str],
    explanation: &'static str,
}

/// Whether a tool can be run. Injected so tests can exercise a machine without it.
type HasTool<'a> = &'a (dyn Fn(&str) -> bool + Sync);

fn base_finding(scanner_id: &str, key: &str, spec: &CacheSpec) -> Finding {
    Finding::new(scanner_id, Area::Developer, key, spec.title)
        .group(spec.group)
        .explanation(spec.explanation)
        .risk(spec.risk)
        .recovery(spec.recovery)
        .selected(spec.selected)
        .blocked_by(spec.blocked_by)
}

fn spec_findings(
    ctx: &ScanContext,
    scanner_id: &str,
    spec: &CacheSpec,
    has_tool: HasTool,
) -> Vec<Finding> {
    if ctx.is_cancelled() {
        return Vec::new();
    }
    // Symlinks do not count: never measure or offer something that lives somewhere else.
    let folders: Vec<(&str, std::path::PathBuf)> = spec
        .paths
        .iter()
        .map(|relative| (*relative, ctx.home_path(relative)))
        .filter(|(_, path)| fsutil::is_real_dir(path))
        .collect();

    match spec.removal {
        Removal::Contents => folders
            .iter()
            .filter_map(|(relative, path)| {
                let info = ctx.measure(path);
                if info.files == 0 {
                    return None;
                }
                let key = if spec.paths.len() > 1 {
                    format!("{}:{relative}", spec.key)
                } else {
                    spec.key.to_string()
                };
                Some(
                    base_finding(scanner_id, &key, spec)
                        .subtitle(path.display().to_string())
                        .measured(&info)
                        .action(Action::TrashContents { dir: path.clone() }),
                )
            })
            .collect(),
        Removal::Folders => {
            let paths: Vec<_> = folders.into_iter().map(|(_, path)| path).collect();
            let info = fsutil::measure_many(&paths, &ctx.cancel);
            if info.files == 0 {
                return Vec::new();
            }
            vec![base_finding(scanner_id, spec.key, spec)
                .subtitle(paths[0].display().to_string())
                .measured(&info)
                .action(Action::Trash { paths })]
        }
        Removal::Command {
            program,
            args,
            fallback,
        } => {
            let measured = folders.iter().find_map(|(_, path)| {
                let info = ctx.measure(path);
                (info.files > 0).then_some((path, info))
            });
            let Some((path, info)) = measured else {
                return Vec::new();
            };
            // The target is hidden inside a command, so the final vetting cannot see it.
            if util::is_user_excluded(ctx, path) {
                return Vec::new();
            }
            let finding = base_finding(scanner_id, spec.key, spec)
                .subtitle(path.display().to_string())
                .measured(&info);
            let finding = if has_tool(program) {
                finding.action(Action::Command {
                    program: program.to_string(),
                    args: args.iter().map(|arg| arg.to_string()).collect(),
                })
            } else {
                match fallback {
                    Fallback::TrashContents => {
                        finding.action(Action::TrashContents { dir: path.clone() })
                    }
                    Fallback::ReportOnly(why) => {
                        let mut report = finding
                            .action(Action::ReportOnly)
                            .selected(false)
                            .size(0)
                            .tag("sin_herramienta");
                        report.explanation = format!("{} {why}", report.explanation);
                        report
                    }
                }
            };
            vec![finding]
        }
    }
}

/// Measures every cache in a table, in parallel.
fn table_findings(
    ctx: &ScanContext,
    scanner_id: &str,
    specs: &[CacheSpec],
    has_tool: HasTool,
) -> Vec<Finding> {
    specs
        .par_iter()
        .flat_map_iter(|spec| spec_findings(ctx, scanner_id, spec, has_tool))
        .collect()
}

/// The single report-only finding rule 6 asks for when something could not be looked at.
fn full_disk_access_notice(scanner_id: &str, key: &str, what_it_unlocks: &str) -> Finding {
    Finding::new(
        scanner_id,
        Area::Developer,
        key,
        t!(
            "Hace falta Acceso total al disco",
            "Full Disk Access is needed"
        ),
    )
    .group(t!("Permisos", "Permissions"))
    .explanation(what_it_unlocks)
    .tag("permisos")
    .tag(hm_core::TAG_NEEDS_FULL_DISK_ACCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    const WITH_COMMAND: CacheSpec = CacheSpec {
        key: "x",
        group: "g",
        title: "t",
        paths: &["Library/Caches/x"],
        removal: Removal::Command {
            program: "xtool",
            args: &["clean"],
            fallback: Fallback::TrashContents,
        },
        risk: Risk::Safe,
        recovery: Recovery::Automatic,
        selected: true,
        blocked_by: &[],
        explanation: "e",
    };

    #[test]
    fn the_tool_is_preferred_and_the_trash_is_the_fallback() {
        let fx = Fixture::new();
        fx.file("Library/Caches/x/data", b"x");
        let ctx = fx.ctx();

        let with = spec_findings(&ctx, "s", &WITH_COMMAND, &|_| true);
        assert!(matches!(with[0].action, Action::Command { .. }));

        let without = spec_findings(&ctx, "s", &WITH_COMMAND, &|_| false);
        assert!(matches!(without[0].action, Action::TrashContents { .. }));
    }

    #[test]
    fn a_tool_that_cannot_be_replaced_reports_instead_of_offering() {
        let fx = Fixture::new();
        fx.file("Library/Caches/x/data", b"x");
        let spec = CacheSpec {
            removal: Removal::Command {
                program: "xtool",
                args: &["clean"],
                fallback: Fallback::ReportOnly("Hace falta xtool."),
            },
            ..WITH_COMMAND
        };
        let out = spec_findings(&fx.ctx(), "s", &spec, &|_| false);
        assert_eq!(out[0].action, Action::ReportOnly);
        assert!(!out[0].selected_by_default);
        assert!(out[0].explanation.contains("Hace falta xtool."));
    }

    #[test]
    fn empty_missing_or_symlinked_caches_produce_nothing() {
        let fx = Fixture::new();
        let ctx = fx.ctx();
        assert!(spec_findings(&ctx, "s", &WITH_COMMAND, &|_| true).is_empty());

        fx.dir("Library/Caches/x");
        assert!(spec_findings(&ctx, "s", &WITH_COMMAND, &|_| true).is_empty());

        let fx = Fixture::new();
        fx.file("elsewhere/data", b"x");
        fx.dir("Library/Caches");
        fx.symlink("elsewhere", "Library/Caches/x");
        assert!(spec_findings(&fx.ctx(), "s", &WITH_COMMAND, &|_| true).is_empty());
    }

    #[test]
    fn user_exclusions_also_cover_command_based_cleanups() {
        let fx = Fixture::new();
        fx.file("Library/Caches/x/data", b"x");
        let mut ctx = fx.ctx();
        ctx.config.excluded_paths = vec![fx.home.join("Library/Caches/x")];
        assert!(spec_findings(&ctx, "s", &WITH_COMMAND, &|_| true).is_empty());
    }

    #[test]
    fn scanners_are_namespaced_and_in_spanish_with_real_descriptions() {
        for scanner in scanners() {
            assert!(scanner.id().starts_with("dev."), "{}", scanner.id());
            assert!(!scanner.name().is_empty());
            assert!(!scanner.description().is_empty());
            assert_eq!(scanner.area(), Area::Developer);
        }
    }

    #[test]
    fn no_command_uses_a_program_off_the_allow_list() {
        // Every program named in a table must be one the cleaner will actually run.
        let policy = hm_core::SafetyPolicy::new("/nonexistent", &[]);
        let specs = [packages::specs(), tools::specs(), xcode::specs()].concat();
        for spec in &specs {
            if let Removal::Command { program, .. } = spec.removal {
                assert!(policy.check_command(program).is_ok(), "{program}");
            }
        }
    }

    #[test]
    fn review_findings_are_never_preselected_in_any_table() {
        let specs = [packages::specs(), tools::specs(), xcode::specs()].concat();
        for spec in &specs {
            if spec.risk == Risk::Review || spec.recovery == Recovery::None {
                assert!(!spec.selected, "{} should not be selected", spec.key);
            }
            assert!(
                !spec.explanation.is_empty(),
                "{} needs an explanation",
                spec.key
            );
        }
    }
}
