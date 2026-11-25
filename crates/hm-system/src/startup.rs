//! Launch agents and daemons. Read-only for now.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use hm_core::{exec, fsutil, ScanContext};
use plist::{Dictionary, Value};

use crate::gate;
use crate::types::{StartupItem, StartupKind};

const LAUNCHCTL_TIMEOUT: Duration = Duration::from_secs(5);

/// Interpreters whose real program is their first argument, so a launch agent that runs
/// `/bin/sh /gone/script.sh` still counts as orphaned.
pub(crate) const INTERPRETERS: &[&str] = &[
    "bash",
    "dash",
    "env",
    "node",
    "open",
    "osascript",
    "perl",
    "python",
    "python2",
    "python3",
    "ruby",
    "sh",
    "zsh",
];

/// launchd's own record of what has been disabled, per domain. It wins over the plist's `Disabled`
/// key, which is only the default.
#[derive(Clone, Debug, Default)]
struct DisabledOverrides {
    user: HashMap<String, bool>,
    system: HashMap<String, bool>,
}

pub fn list_items(ctx: &ScanContext) -> Result<Vec<StartupItem>> {
    let dirs = [
        (
            ctx.home_path("Library/LaunchAgents"),
            StartupKind::UserAgent,
        ),
        (
            PathBuf::from("/Library/LaunchAgents"),
            StartupKind::GlobalAgent,
        ),
        (
            PathBuf::from("/Library/LaunchDaemons"),
            StartupKind::GlobalDaemon,
        ),
    ];
    let overrides = read_overrides(ctx);
    Ok(list_from(ctx, &dirs, &overrides))
}

fn list_from(
    ctx: &ScanContext,
    dirs: &[(PathBuf, StartupKind)],
    overrides: &DisabledOverrides,
) -> Vec<StartupItem> {
    let access = gate::has_access(ctx);
    let mut items: Vec<StartupItem> = dirs
        .iter()
        .flat_map(|(dir, kind)| {
            fsutil::children(dir)
                .into_iter()
                .filter(|path| path.extension().is_some_and(|ext| ext == "plist"))
                .map(|path| parse_item(ctx, &path, *kind, overrides, access))
                .collect::<Vec<_>>()
        })
        .collect();
    items.sort_by_key(|item| item.label.to_lowercase());
    items
}

fn parse_item(
    ctx: &ScanContext,
    path: &Path,
    kind: StartupKind,
    overrides: &DisabledOverrides,
    access: bool,
) -> StartupItem {
    let file_label = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut item = StartupItem {
        label: file_label,
        kind,
        plist_path: path.to_path_buf(),
        program: None,
        arguments: Vec::new(),
        run_at_load: false,
        keep_alive: false,
        disabled: false,
        orphaned: false,
        malformed: true,
    };
    let Some(dict) = Value::from_file(path).ok().and_then(Value::into_dictionary) else {
        return item;
    };
    item.malformed = false;
    if let Some(label) = dict.get("Label").and_then(Value::as_string) {
        item.label = label.to_string();
    }
    let arguments = program_arguments(&dict);
    item.program = dict
        .get("Program")
        .and_then(Value::as_string)
        .map(str::to_string)
        .or_else(|| arguments.first().cloned())
        // Apps registered through SMAppService point inside their own bundle, relative to it, so
        // there is no absolute path to check.
        .or_else(|| {
            dict.get("BundleProgram")
                .and_then(Value::as_string)
                .map(str::to_string)
        });
    item.arguments = arguments.clone();
    item.run_at_load = dict
        .get("RunAtLoad")
        .and_then(Value::as_boolean)
        .unwrap_or(false);
    item.keep_alive = match dict.get("KeepAlive") {
        Some(Value::Boolean(keep)) => *keep,
        // A dictionary means "keep alive under these conditions", and any condition means it can
        // come back on its own.
        Some(Value::Dictionary(conditions)) => !conditions.is_empty(),
        _ => false,
    };
    let default_disabled = dict
        .get("Disabled")
        .and_then(Value::as_boolean)
        .unwrap_or(false);
    let domain = match kind {
        StartupKind::GlobalDaemon => &overrides.system,
        StartupKind::UserAgent | StartupKind::GlobalAgent => &overrides.user,
    };
    item.disabled = domain.get(&item.label).copied().unwrap_or(default_disabled);
    item.orphaned = is_orphaned(ctx, item.program.as_deref(), &arguments, access);
    item
}

fn program_arguments(dict: &Dictionary) -> Vec<String> {
    dict.get("ProgramArguments")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_string)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the thing this job runs is definitely gone. Anything uncertain reads as not orphaned,
/// since the UI will suggest removing the item.
fn is_orphaned(
    ctx: &ScanContext,
    program: Option<&str>,
    arguments: &[String],
    access: bool,
) -> bool {
    let Some(program) = program else {
        return false;
    };
    if is_missing(ctx, program, access) {
        return true;
    }
    let is_interpreter = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| INTERPRETERS.contains(&name));
    if !is_interpreter {
        return false;
    }
    interpreted_target(arguments).is_some_and(|target| is_missing(ctx, target, access))
}

/// The script or app an interpreter was given, if it is given as a path. Inline code (`sh -c`,
/// `osascript -e`) has no file to check.
pub(crate) fn interpreted_target(arguments: &[String]) -> Option<&str> {
    for argument in arguments.iter().skip(1) {
        match argument.as_str() {
            "-c" | "-e" => return None,
            flag if flag.starts_with('-') => continue,
            assignment if assignment.contains('=') && !assignment.starts_with('/') => continue,
            target => return Some(target),
        }
    }
    None
}

fn is_missing(ctx: &ScanContext, path: &str, access: bool) -> bool {
    let path = Path::new(path);
    // A volume can simply be unmounted, so its absence says nothing about the program.
    if !path.is_absolute() || path.starts_with("/Volumes") {
        return false;
    }
    if !access && gate::is_inside_guarded(&ctx.home, path) {
        return false;
    }
    matches!(std::fs::metadata(path), Err(err) if err.kind() == ErrorKind::NotFound)
}

fn read_overrides(ctx: &ScanContext) -> DisabledOverrides {
    let Some(launchctl) = ctx.which("launchctl") else {
        return DisabledOverrides::default();
    };
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    // `print-disabled` only reads launchd's state.
    let read = |domain: &str| {
        exec::run(&launchctl, &["print-disabled", domain], LAUNCHCTL_TIMEOUT)
            .ok()
            .filter(|output| output.success())
            .map(|output| parse_print_disabled(&output.stdout))
            .unwrap_or_default()
    };
    DisabledOverrides {
        user: read(&format!("gui/{uid}")),
        system: read("system"),
    }
}

/// Lines look like `"com.foo.bar" => disabled`. Older systems print `true` and `false` instead.
fn parse_print_disabled(output: &str) -> HashMap<String, bool> {
    output
        .lines()
        .filter_map(|line| {
            let (label, state) = line.trim().split_once(" => ")?;
            let label = label.trim().strip_prefix('"')?.strip_suffix('"')?;
            let disabled = match state.trim() {
                "disabled" | "true" => true,
                "enabled" | "false" => false,
                _ => return None,
            };
            Some((label.to_string(), disabled))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, LockedDir};

    fn agent(label: &str, body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Label</key><string>{label}</string>{body}</dict></plist>"#
        )
    }

    fn program(path: &str) -> String {
        format!("<key>Program</key><string>{path}</string>")
    }

    fn arguments(args: &[&str]) -> String {
        let items: String = args
            .iter()
            .map(|a| format!("<string>{a}</string>"))
            .collect();
        format!("<key>ProgramArguments</key><array>{items}</array>")
    }

    fn list(home: &Fixture, overrides: &DisabledOverrides) -> Vec<StartupItem> {
        list_from(
            &home.ctx,
            &[(home.path("Library/LaunchAgents"), StartupKind::UserAgent)],
            overrides,
        )
    }

    fn only(home: &Fixture, file: &str, content: &str) -> StartupItem {
        home.write_text(&format!("Library/LaunchAgents/{file}.plist"), content);
        let items = list(home, &DisabledOverrides::default());
        assert_eq!(items.len(), 1);
        items.into_iter().next().expect("one item")
    }

    #[test]
    fn missing_and_unreadable_folders_are_empty() {
        let home = Fixture::new();
        assert!(list(&home, &DisabledOverrides::default()).is_empty());
        let dir = home.mkdir("Library/LaunchAgents");
        let _guard = LockedDir::new(&dir);
        assert!(list(&home, &DisabledOverrides::default()).is_empty());
    }

    #[test]
    fn parses_the_common_fields() {
        let home = Fixture::new();
        let item = only(
            &home,
            "com.example.a",
            &agent(
                "com.example.a",
                &format!(
                    "{}<key>RunAtLoad</key><true/><key>KeepAlive</key><true/>",
                    arguments(&["/bin/ls", "-la"])
                ),
            ),
        );
        assert_eq!(item.label, "com.example.a");
        assert_eq!(item.program.as_deref(), Some("/bin/ls"));
        assert!(item.run_at_load && item.keep_alive);
        assert!(!item.orphaned && !item.disabled && !item.malformed);
    }

    #[test]
    fn program_key_wins_over_arguments() {
        let home = Fixture::new();
        let item = only(
            &home,
            "a",
            &agent(
                "a",
                &format!("{}{}", program("/bin/ls"), arguments(&["ignored"])),
            ),
        );
        assert_eq!(item.program.as_deref(), Some("/bin/ls"));
    }

    #[test]
    fn conditional_keep_alive_counts_as_keep_alive() {
        let home = Fixture::new();
        let item = only(
            &home,
            "a",
            &agent(
                "a",
                "<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>",
            ),
        );
        assert!(item.keep_alive);
    }

    #[test]
    fn missing_program_is_orphaned() {
        let home = Fixture::new();
        let item = only(&home, "a", &agent("a", &program("/no/such/program")));
        assert!(item.orphaned);
    }

    #[test]
    fn interpreter_with_a_missing_script_is_orphaned() {
        let home = Fixture::new();
        let item = only(
            &home,
            "a",
            &agent("a", &arguments(&["/bin/sh", "/no/such/script.sh"])),
        );
        assert!(item.orphaned);
    }

    #[test]
    fn inline_code_is_not_orphaned() {
        let home = Fixture::new();
        let item = only(
            &home,
            "a",
            &agent("a", &arguments(&["/bin/sh", "-c", "/no/such/thing"])),
        );
        assert!(!item.orphaned);
    }

    #[test]
    fn unknowable_programs_are_not_orphaned() {
        let home = Fixture::new();
        for (index, path) in ["node", "/Volumes/External/tool"].iter().enumerate() {
            let file = format!("agent{index}");
            let content = agent(&file, &program(path));
            home.write_text(&format!("Library/LaunchAgents/{file}.plist"), &content);
        }
        home.write_text(
            "Library/LaunchAgents/bundle.plist",
            &agent(
                "bundle",
                "<key>BundleProgram</key><string>Contents/MacOS/x</string>",
            ),
        );
        let items = list(&home, &DisabledOverrides::default());
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|item| !item.orphaned), "{items:?}");
    }

    #[test]
    fn a_program_in_a_gated_folder_is_not_checked_without_access() {
        let home = Fixture::new();
        let gone = home.path("Documents/gone.sh").display().to_string();
        let item = only(&home, "a", &agent("a", &program(&gone)));
        assert!(!item.orphaned, "must not touch Documents to find out");

        home.grant_full_disk_access();
        let items = list(&home, &DisabledOverrides::default());
        assert!(items[0].orphaned);
    }

    #[test]
    fn malformed_plist_is_listed_and_flagged() {
        let home = Fixture::new();
        let item = only(&home, "com.broken.agent", "<plist><dict>nope");
        assert!(item.malformed);
        assert_eq!(item.label, "com.broken.agent");
        assert!(item.program.is_none());
        assert!(!item.orphaned, "unknown, not orphaned");
    }

    #[test]
    fn non_plist_files_are_ignored() {
        let home = Fixture::new();
        home.write_text("Library/LaunchAgents/notes.txt", "hola");
        assert!(list(&home, &DisabledOverrides::default()).is_empty());
    }

    #[test]
    fn launchd_override_beats_the_plist_in_both_directions() {
        let home = Fixture::new();
        home.write_text(
            "Library/LaunchAgents/on.plist",
            &agent("on", "<key>Disabled</key><true/>"),
        );
        home.write_text("Library/LaunchAgents/off.plist", &agent("off", ""));
        home.write_text(
            "Library/LaunchAgents/default.plist",
            &agent("default", "<key>Disabled</key><true/>"),
        );
        let overrides = DisabledOverrides {
            user: HashMap::from([("on".to_string(), false), ("off".to_string(), true)]),
            system: HashMap::new(),
        };
        let items = list(&home, &overrides);
        let disabled = |label: &str| {
            items
                .iter()
                .find(|item| item.label == label)
                .map(|item| item.disabled)
        };
        assert_eq!(disabled("on"), Some(false));
        assert_eq!(disabled("off"), Some(true));
        assert_eq!(disabled("default"), Some(true));
    }

    #[test]
    fn daemons_read_the_system_domain() {
        let home = Fixture::new();
        home.write_text("daemons/d.plist", &agent("d", ""));
        let overrides = DisabledOverrides {
            user: HashMap::new(),
            system: HashMap::from([("d".to_string(), true)]),
        };
        let items = list_from(
            &home.ctx,
            &[(home.path("daemons"), StartupKind::GlobalDaemon)],
            &overrides,
        );
        assert!(items[0].disabled);
        assert_eq!(items[0].kind, StartupKind::GlobalDaemon);
    }

    #[test]
    fn parses_launchctl_output() {
        let output = "\n\tdisabled services = {\n\
            \t\t\"com.docker.helper\" => enabled\n\
            \t\t\"com.apple.Siri.agent\" => disabled\n\
            \t\t\"com.old.style\" => true\n\
            \t\t\"com.bad\" => maybe\n\
            \t}\n\tlogin item associations = {\n\t}\n";
        let parsed = parse_print_disabled(output);
        assert_eq!(parsed.get("com.docker.helper"), Some(&false));
        assert_eq!(parsed.get("com.apple.Siri.agent"), Some(&true));
        assert_eq!(parsed.get("com.old.style"), Some(&true));
        assert_eq!(parsed.len(), 3);
    }
}
