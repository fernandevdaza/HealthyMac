//! Installed apps, their leftovers and the uninstaller.

use std::collections::HashSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use hm_core::{
    exec, t, tf, Action, Area, Finding, Recovery, Risk, SafetyPolicy, SafetyViolation, ScanContext,
};
use rayon::prelude::*;

use crate::gate;
use crate::inventory::{
    find_app_bundles, outermost_app, read_bundle, BundleInfo, RunningProcesses,
};
use crate::scanners::policy;
use crate::types::{AppArch, AppInfo, Leftover};

const MDLS_TIMEOUT: Duration = Duration::from_secs(10);
const MACHO_HEADER_BYTES: usize = 4096;
/// A bare name shorter than this is too likely to match something unrelated.
const MIN_NAME_MATCH_LEN: usize = 4;

/// Apps too generically named for a same-named folder to be assumed theirs.
const GENERIC_APP_NAMES: &[&str] = &[
    "chat",
    "code",
    "data",
    "editor",
    "finder",
    "google",
    "helper",
    "microsoft",
    "music",
    "notes",
    "photos",
    "service",
    "settings",
    "system",
    "terminal",
    "update",
    "updater",
];

/// Apple apps a user installs and can remove, unlike the ones that ship with macOS.
const REMOVABLE_APPLE_PREFIXES: &[&str] = &[
    "com.apple.compressor",
    "com.apple.configurator",
    "com.apple.dt.",
    "com.apple.finalcut",
    "com.apple.garageband",
    "com.apple.imovie",
    "com.apple.iwork.",
    "com.apple.logic",
    "com.apple.mainstage",
    "com.apple.motionapp",
    "com.apple.safari.webapp.",
    "com.apple.testflight",
];

/// What a leftover is, as shown next to it.
#[derive(Clone, Copy, Debug)]
enum LeftoverKind {
    ApplicationSupport,
    Cache,
    Logs,
    Preferences,
    SavedState,
    HttpStorage,
    WebKit,
    Container,
}

impl LeftoverKind {
    fn label(self) -> &'static str {
        match self {
            LeftoverKind::ApplicationSupport => {
                t!("Soporte de aplicación", "Application Support")
            }
            LeftoverKind::Cache => t!("Caché", "Cache"),
            LeftoverKind::Logs => t!("Registros", "Logs"),
            LeftoverKind::Preferences => t!("Preferencias", "Preferences"),
            LeftoverKind::SavedState => t!("Estado guardado", "Saved state"),
            LeftoverKind::HttpStorage => t!("Almacenamiento HTTP", "HTTP storage"),
            LeftoverKind::WebKit => t!("Datos de WebKit", "WebKit data"),
            LeftoverKind::Container => t!("Contenedor", "Container"),
        }
    }
}

struct LeftoverPlace {
    relative: &'static str,
    kind: LeftoverKind,
    /// Items named after the bundle id, given as `{id}{suffix}`.
    id_suffixes: &'static [&'static str],
    /// Some apps name their folder after themselves instead.
    by_app_name: bool,
}

const LEFTOVER_PLACES: &[LeftoverPlace] = &[
    LeftoverPlace {
        relative: "Library/Application Support",
        kind: LeftoverKind::ApplicationSupport,
        id_suffixes: &[""],
        by_app_name: true,
    },
    LeftoverPlace {
        relative: "Library/Caches",
        kind: LeftoverKind::Cache,
        id_suffixes: &[""],
        by_app_name: true,
    },
    LeftoverPlace {
        relative: "Library/Logs",
        kind: LeftoverKind::Logs,
        id_suffixes: &[""],
        by_app_name: true,
    },
    LeftoverPlace {
        relative: "Library/Preferences",
        kind: LeftoverKind::Preferences,
        id_suffixes: &[".plist"],
        by_app_name: false,
    },
    LeftoverPlace {
        relative: "Library/Saved Application State",
        kind: LeftoverKind::SavedState,
        id_suffixes: &[".savedState"],
        by_app_name: false,
    },
    LeftoverPlace {
        relative: "Library/HTTPStorages",
        kind: LeftoverKind::HttpStorage,
        id_suffixes: &["", ".binarycookies"],
        by_app_name: false,
    },
    LeftoverPlace {
        relative: "Library/WebKit",
        kind: LeftoverKind::WebKit,
        id_suffixes: &[""],
        by_app_name: false,
    },
    LeftoverPlace {
        relative: "Library/Containers",
        kind: LeftoverKind::Container,
        id_suffixes: &[""],
        by_app_name: false,
    },
];

/// Lists installed apps with their sizes and leftovers. Slow: call it from a background thread.
pub fn list_apps(ctx: &ScanContext) -> Result<Vec<AppInfo>> {
    let roots = [
        PathBuf::from("/Applications"),
        ctx.home_path("Applications"),
    ];
    Ok(list_apps_in(ctx, &roots, &RunningProcesses::current()))
}

pub(crate) fn list_apps_in(
    ctx: &ScanContext,
    roots: &[PathBuf],
    running: &RunningProcesses,
) -> Vec<AppInfo> {
    let access = gate::has_access(ctx);
    let policy = SafetyPolicy::new(&ctx.home, &ctx.config.excluded_paths);
    let mut seen = HashSet::new();
    let bundles: Vec<PathBuf> = roots
        .iter()
        .flat_map(|root| find_app_bundles(root).paths)
        // The same app linked from two places is still one app.
        .filter(|path| seen.insert(path.canonicalize().unwrap_or_else(|_| path.clone())))
        .collect();
    let mut apps: Vec<AppInfo> = bundles
        .par_iter()
        .filter(|_| !ctx.is_cancelled())
        .map(|path| app_info(ctx, path, running, access, &policy))
        .collect();
    apply_last_used(ctx, &mut apps);
    apps.sort_by_key(|app| std::cmp::Reverse(app.size));
    apps
}

fn apply_last_used(ctx: &ScanContext, apps: &mut [AppInfo]) {
    let resolved: Vec<PathBuf> = apps
        .iter()
        .map(|app| app.path.canonicalize().unwrap_or_else(|_| app.path.clone()))
        .collect();
    let dates = last_used_dates(ctx, &resolved);
    for (app, date) in apps.iter_mut().zip(dates) {
        app.last_used = date;
    }
}

fn app_info(
    ctx: &ScanContext,
    path: &Path,
    running: &RunningProcesses,
    access: bool,
    policy: &SafetyPolicy,
) -> AppInfo {
    let info = read_bundle(path);
    let is_system = is_system_app(&info);
    AppInfo {
        uninstall_blocked: uninstall_blocker(&info, path, is_system, policy),
        name: info.name.clone(),
        bundle_id: info.bundle_id.clone(),
        version: info.version.clone(),
        path: path.to_path_buf(),
        size: ctx.measure(&info.resolved).exclusive(),
        leftovers: if is_system {
            Vec::new()
        } else {
            find_leftovers(ctx, &info, access)
        },
        // Filled in afterwards, in one go.
        last_used: None,
        running: outermost_app(&info.resolved).is_some_and(|app| running.bundles.contains(&app)),
        is_system,
        arch: info
            .executable_path
            .as_deref()
            .map_or(AppArch::Unknown, read_arch),
        icon_path: info.icon_path.clone(),
        executable: info.executable.clone(),
    }
}

/// Asks the same policy the cleaner will consult, so the answer cannot drift from what happens.
fn uninstall_blocker(
    info: &BundleInfo,
    path: &Path,
    is_system: bool,
    policy: &SafetyPolicy,
) -> Option<String> {
    let name = &info.name;
    if is_system {
        return Some(tf!(
            "{name} forma parte de macOS, así que no se puede desinstalar.",
            "{name} is part of macOS, so it cannot be uninstalled."
        ));
    }
    match policy.check_removal(path) {
        Ok(()) => {}
        Err(SafetyViolation::UserExcluded(_)) => {
            return Some(tf!(
                "{name} está en una ubicación que excluiste de la limpieza.",
                "{name} is in a location you excluded from cleaning."
            ));
        }
        Err(_) => {
            return Some(tf!(
                "{name} está en una ubicación que HealthyMac no toca. Usa su desinstalador, o \
                 arrástrala a la Papelera desde el Finder.",
                "{name} is in a location HealthyMac does not touch. Use its uninstaller, or drag \
                 it to the Trash in the Finder."
            ));
        }
    }
    // Moving a folder to another parent needs write access to the folder itself. Finder asks for an
    // administrator password in this case, but HealthyMac never asks for privileges.
    //
    // Only the mode bits are checked. `access(2)` would be more thorough, since it also reflects
    // App Management, but on a signed bundle it reaches TCC and this runs during a scan. A bundle
    // that protection refuses fails at clean time instead, when the user has asked for it.
    if !gate::mode_permits_write(&info.resolved) {
        return Some(tf!(
            "{name} se instaló para todos los usuarios y hace falta una contraseña de administrador \
             para quitarla. Arrástrala a la Papelera desde el Finder, que te la pedirá.",
            "{name} was installed for all users, and removing it takes an administrator password. \
             Drag it to the Trash in the Finder, which will ask you for it."
        ));
    }
    None
}

pub(crate) fn is_system_app(info: &BundleInfo) -> bool {
    if info.resolved.starts_with("/System") {
        return true;
    }
    let Some(id) = &info.bundle_id else {
        return false;
    };
    let id = id.to_ascii_lowercase();
    id.starts_with("com.apple.")
        && !REMOVABLE_APPLE_PREFIXES
            .iter()
            .any(|prefix| id.starts_with(prefix))
}

fn find_leftovers(ctx: &ScanContext, info: &BundleInfo, access: bool) -> Vec<Leftover> {
    let Some(id) = &info.bundle_id else {
        return Vec::new();
    };
    // Password managers and sync clients keep vaults and pending uploads here. Uninstalling the app
    // should not offer those up.
    if policy::is_sensitive(id) || policy::is_sensitive(&info.name) {
        return Vec::new();
    }
    let name_matches = info.name.chars().count() >= MIN_NAME_MATCH_LEN
        && !GENERIC_APP_NAMES.contains(&info.name.to_lowercase().as_str());

    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for place in LEFTOVER_PLACES {
        let dir = ctx.home_path(place.relative);
        let mut candidates: Vec<PathBuf> = place
            .id_suffixes
            .iter()
            .map(|suffix| dir.join(format!("{id}{suffix}")))
            .collect();
        if place.by_app_name && name_matches {
            candidates.push(dir.join(&info.name));
        }
        for path in candidates {
            // Containers are guarded. So is whatever an app named "Microsoft Edge" finds under its
            // own name in Application Support: its browser profile.
            if (!access && gate::is_inside_guarded(&ctx.home, &path))
                || std::fs::symlink_metadata(&path).is_err()
                || gate::is_symlink(&path)
                || gate::is_excluded(ctx, &path)
                // `is_excluded` covers the user's list. On a case-insensitive volume the id and the
                // name can be the same folder, so count it once.
                || !seen.insert(path.to_string_lossy().to_lowercase())
            {
                continue;
            }
            found.push(Leftover {
                size: ctx.measure(&path).exclusive(),
                path,
                kind: place.kind.label().to_string(),
            });
        }
    }
    found
}

/// When each app was last opened, from Spotlight, so without reading the apps themselves. `None`
/// for an app that was never opened or that Spotlight has no record of.
///
/// One `mdls` for all of them: each process makes its own round of permission checks at launch, so
/// one per app would add up to hundreds of pointless queries to TCC on every listing.
fn last_used_dates(ctx: &ScanContext, paths: &[PathBuf]) -> Vec<Option<SystemTime>> {
    let unknown = || vec![None; paths.len()];
    let Some(mdls) = ctx.which("mdls") else {
        return unknown();
    };
    let Some(path_args) = paths
        .iter()
        .map(|path| path.to_str())
        .collect::<Option<Vec<_>>>()
    else {
        return unknown();
    };
    if path_args.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["-name", "kMDItemLastUsedDate", "-raw"];
    args.extend(path_args);
    let Ok(output) = exec::run(&mdls, &args, MDLS_TIMEOUT) else {
        return unknown();
    };
    parse_mdls_batch(&output.stdout, paths.len()).unwrap_or_else(unknown)
}

/// With `-raw`, `mdls` separates values with NUL. If the count is off there is no telling which date
/// belongs to which app, and a wrong "last used" is worse than none.
fn parse_mdls_batch(stdout: &str, expected: usize) -> Option<Vec<Option<SystemTime>>> {
    let values: Vec<&str> = stdout.split('\0').collect();
    // Tolerates one trailing separator.
    let values = match values.split_last() {
        Some((last, rest)) if last.trim().is_empty() && rest.len() == expected => rest,
        _ => &values[..],
    };
    (values.len() == expected).then(|| {
        values
            .iter()
            .map(|value| parse_mdls_date(value.trim()))
            .collect()
    })
}

/// `2026-03-01 19:22:11 +0000` to a time. Spotlight always answers in UTC. Anything else, including
/// `(null)`, is `None`.
fn parse_mdls_date(text: &str) -> Option<SystemTime> {
    let (date, rest) = text.split_once(' ')?;
    let (time, zone) = rest.split_once(' ')?;
    if zone != "+0000" {
        return None;
    }
    let mut date = date.split('-').map(str::parse::<i64>);
    let (year, month, day) = (date.next()?.ok()?, date.next()?.ok()?, date.next()?.ok()?);
    let mut time = time.split(':').map(str::parse::<i64>);
    let (hour, minute, second) = (time.next()?.ok()?, time.next()?.ok()?, time.next()?.ok()?);
    let valid = (1970..=9999).contains(&year)
        && (1..=12).contains(&month)
        && (1..=31).contains(&day)
        && (0..24).contains(&hour)
        && (0..60).contains(&minute)
        && (0..=60).contains(&second);
    if !valid {
        return None;
    }
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(UNIX_EPOCH + Duration::from_secs(u64::try_from(seconds).ok()?))
}

/// Days since 1970-01-01 in the proleptic Gregorian calendar (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn read_arch(executable: &Path) -> AppArch {
    let mut header = [0u8; MACHO_HEADER_BYTES];
    let Ok(mut file) = File::open(executable) else {
        return AppArch::Unknown;
    };
    let Ok(read) = file.read(&mut header) else {
        return AppArch::Unknown;
    };
    parse_macho_arch(&header[..read])
}

const CPU_TYPE_X86: u32 = 0x0000_0007;
const CPU_TYPE_X86_64: u32 = 0x0100_0007;
const CPU_TYPE_ARM64: u32 = 0x0100_000C;
/// Java class files share the fat magic number, but their next field is a version around 45-70
/// in the low half and would claim far more slices than any real binary has.
const MAX_FAT_SLICES: u32 = 32;

fn parse_macho_arch(bytes: &[u8]) -> AppArch {
    let read_u32 = |offset: usize, big_endian: bool| -> Option<u32> {
        let chunk: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
        Some(if big_endian {
            u32::from_be_bytes(chunk)
        } else {
            u32::from_le_bytes(chunk)
        })
    };
    let Some(magic) = read_u32(0, true) else {
        return AppArch::Unknown;
    };
    match magic {
        // Fat: FAT_MAGIC and FAT_MAGIC_64. Headers are always big-endian.
        0xCAFE_BABE | 0xCAFE_BABF => {
            let entry_size = if magic == 0xCAFE_BABE { 20 } else { 32 };
            let Some(count) = read_u32(4, true).filter(|n| (1..=MAX_FAT_SLICES).contains(n)) else {
                return AppArch::Unknown;
            };
            let mut arm = false;
            let mut intel = false;
            for slice in 0..count as usize {
                match read_u32(8 + slice * entry_size, true) {
                    Some(CPU_TYPE_ARM64) => arm = true,
                    Some(CPU_TYPE_X86_64 | CPU_TYPE_X86) => intel = true,
                    Some(_) => {}
                    None => break,
                }
            }
            arch_from(arm, intel)
        }
        // Thin: MH_MAGIC_64 and MH_MAGIC as they read when the file is little-endian.
        0xCFFA_EDFE | 0xCEFA_EDFE => match read_u32(4, false) {
            Some(CPU_TYPE_ARM64) => AppArch::AppleSilicon,
            Some(CPU_TYPE_X86_64 | CPU_TYPE_X86) => AppArch::Intel,
            _ => AppArch::Unknown,
        },
        _ => AppArch::Unknown,
    }
}

fn arch_from(arm: bool, intel: bool) -> AppArch {
    match (arm, intel) {
        (true, true) => AppArch::Universal,
        (true, false) => AppArch::AppleSilicon,
        (false, true) => AppArch::Intel,
        (false, false) => AppArch::Unknown,
    }
}

/// Builds the finding that uninstalls `app` when handed to the cleaner.
///
/// Never preselected. When [`AppInfo::uninstall_blocked`] says it cannot be done, the finding is
/// report-only and carries that reason, so even a UI that forgot to check cannot trigger a removal
/// that would fail, or remove a system app.
pub fn uninstall_finding(app: &AppInfo, include_leftovers: bool) -> Finding {
    let key = app.bundle_id.as_deref().unwrap_or(&app.name);
    let base = Finding::new(
        "apps.uninstall",
        Area::Apps,
        key,
        tf!("Desinstalar {}", "Uninstall {}", app.name),
    )
    .subtitle(app.path.display().to_string())
    .risk(Risk::Review)
    .recovery(Recovery::None)
    .selected(false);

    if let Some(reason) = &app.uninstall_blocked {
        return base.explanation(reason.clone());
    }

    let mut paths = vec![app.path.clone()];
    let mut size = app.size;
    let mut explanation = tf!(
        "Mueve {} a la Papelera. Podrás recuperarla desde ahí hasta que la vacíes. Si macOS lo \
         impide, concede a HealthyMac el permiso «Gestión de apps» en Ajustes del Sistema, dentro \
         de Privacidad y seguridad.",
        "Moves {} to the Trash. You can get it back from there until you empty it. If macOS \
         blocks it, give HealthyMac the App Management permission in System Settings, under \
         Privacy & Security.",
        app.name
    );
    if include_leftovers && !app.leftovers.is_empty() {
        paths.extend(app.leftovers.iter().map(|leftover| leftover.path.clone()));
        size += app
            .leftovers
            .iter()
            .map(|leftover| leftover.size)
            .sum::<u64>();
        explanation.push_str(t!(
            " También se llevan sus datos: ajustes, cachés y lo que haya guardado. Si piensas \
             volver a instalarla, quizá quieras conservarlos.",
            " Its data goes too: settings, caches and anything it saved. If you plan to install \
             it again, you may want to keep them."
        ));
    } else if !app.leftovers.is_empty() {
        explanation.push_str(t!(
            " Sus datos se quedan, por si vuelves a instalarla.",
            " Its data stays, in case you install it again."
        ));
    }

    let mut finding = base
        .size(size)
        .items(paths.len() as u64)
        .last_used(app.last_used)
        .explanation(explanation)
        .action(Action::Trash { paths });
    if let Some(process) = &app.executable {
        finding = finding.blocked_by(&[process.as_str()]);
    }
    finding
}

/// The icon file of the app bundle at `path`, read from its `Info.plist`. `None` when the bundle
/// names no icon file, as apps that keep theirs in an asset catalog do.
pub fn icon_path_of(path: &Path) -> Option<PathBuf> {
    crate::inventory::read_bundle(path).icon_path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Fixture;

    fn policy(home: &Fixture) -> SafetyPolicy {
        SafetyPolicy::new(&home.ctx.home, &home.ctx.config.excluded_paths)
    }

    fn app_info_for(home: &Fixture, relative: &str, id: &str, exe: &str) -> AppInfo {
        let path = home.app(relative, id, exe);
        app_info(
            &home.ctx,
            &path,
            &RunningProcesses::default(),
            false,
            &policy(home),
        )
    }

    fn thin(magic: [u8; 4], cpu: u32) -> Vec<u8> {
        let mut bytes = magic.to_vec();
        bytes.extend_from_slice(&cpu.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 24]);
        bytes
    }

    fn fat(cpus: &[u32]) -> Vec<u8> {
        let mut bytes = 0xCAFE_BABEu32.to_be_bytes().to_vec();
        bytes.extend_from_slice(&(cpus.len() as u32).to_be_bytes());
        for cpu in cpus {
            bytes.extend_from_slice(&cpu.to_be_bytes());
            bytes.extend_from_slice(&[0u8; 16]);
        }
        bytes
    }

    #[test]
    fn reads_architectures_from_mach_o_headers() {
        let magic64 = [0xCF, 0xFA, 0xED, 0xFE];
        assert_eq!(
            parse_macho_arch(&thin(magic64, CPU_TYPE_ARM64)),
            AppArch::AppleSilicon
        );
        assert_eq!(
            parse_macho_arch(&thin(magic64, CPU_TYPE_X86_64)),
            AppArch::Intel
        );
        assert_eq!(
            parse_macho_arch(&fat(&[CPU_TYPE_X86_64, CPU_TYPE_ARM64])),
            AppArch::Universal
        );
        assert_eq!(
            parse_macho_arch(&fat(&[CPU_TYPE_ARM64])),
            AppArch::AppleSilicon
        );
        assert_eq!(parse_macho_arch(&fat(&[CPU_TYPE_X86_64])), AppArch::Intel);
    }

    #[test]
    fn odd_binaries_are_unknown_rather_than_a_panic() {
        assert_eq!(parse_macho_arch(b""), AppArch::Unknown);
        assert_eq!(parse_macho_arch(b"#!/bin/sh\n"), AppArch::Unknown);
        assert_eq!(parse_macho_arch(&[0xCA, 0xFE]), AppArch::Unknown);
        // A Java class file: same magic, then minor and major version.
        assert_eq!(
            parse_macho_arch(&[0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x00, 0x00, 0x41]),
            AppArch::Unknown
        );
        // A fat header claiming more slices than the bytes hold.
        assert_eq!(
            parse_macho_arch(&[0xCA, 0xFE, 0xBA, 0xBE, 0x00, 0x00, 0x00, 0x02]),
            AppArch::Unknown
        );
    }

    #[test]
    fn a_real_system_binary_has_a_known_arch() {
        assert_ne!(read_arch(Path::new("/bin/ls")), AppArch::Unknown);
    }

    #[test]
    fn parses_spotlight_dates() {
        let expected = UNIX_EPOCH + Duration::from_secs(1_772_392_931);
        assert_eq!(parse_mdls_date("2026-03-01 19:22:11 +0000"), Some(expected));
        assert_eq!(parse_mdls_date("(null)"), None);
        assert_eq!(parse_mdls_date(""), None);
        assert_eq!(parse_mdls_date("2026-13-01 19:22:11 +0000"), None);
        assert_eq!(
            parse_mdls_date("99999999999999999-01-01 19:22:11 +0000"),
            None
        );
        assert_eq!(parse_mdls_date("2026-03-01 99:22:11 +0000"), None);
    }

    #[test]
    fn parses_a_batch_of_spotlight_dates_in_order() {
        let first = UNIX_EPOCH + Duration::from_secs(1_772_392_931);
        let parsed = parse_mdls_batch("2026-03-01 19:22:11 +0000\0(null)", 2).expect("two values");
        assert_eq!(parsed, [Some(first), None]);
        let trailing = parse_mdls_batch("(null)\0(null)\0", 2).expect("two values");
        assert_eq!(trailing, [None, None]);
    }

    #[test]
    fn a_batch_of_the_wrong_length_is_not_trusted() {
        assert_eq!(parse_mdls_batch("2026-03-01 19:22:11 +0000", 2), None);
        assert_eq!(parse_mdls_batch("", 3), None);
    }

    #[test]
    fn lists_apps_with_their_details_and_leftovers() {
        let home = Fixture::new();
        home.app("Applications/Foozle.app", "com.example.foo", "foo");
        home.write("Library/Caches/com.example.foo/data", 5_000);
        home.write("Library/Preferences/com.example.foo.plist", 500);
        home.write("Library/Application Support/Foozle/data", 5_000);
        let apps = list_apps_in(
            &home.ctx,
            &[home.path("Applications")],
            &RunningProcesses::default(),
        );
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        assert_eq!(app.name, "Foozle");
        assert_eq!(app.executable.as_deref(), Some("foo"));
        assert!(!app.is_system && !app.running);
        assert!(app.size > 0);
        let kinds: Vec<&str> = app.leftovers.iter().map(|l| l.kind.as_str()).collect();
        assert_eq!(app.leftovers.len(), 3, "{kinds:?}");
        assert!(kinds.contains(&"Caché"));
        assert!(kinds.contains(&"Preferencias"));
        assert!(kinds.contains(&"Soporte de aplicación"));
    }

    #[test]
    fn missing_roots_list_nothing() {
        let home = Fixture::new();
        let apps = list_apps_in(
            &home.ctx,
            &[home.path("nope")],
            &RunningProcesses::default(),
        );
        assert!(apps.is_empty());
    }

    #[test]
    fn app_with_a_malformed_plist_is_still_listed() {
        let home = Fixture::new();
        home.write_text("Applications/Bad.app/Contents/Info.plist", "<plist>nope");
        let apps = list_apps_in(
            &home.ctx,
            &[home.path("Applications")],
            &RunningProcesses::default(),
        );
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Bad");
        assert!(apps[0].bundle_id.is_none());
        assert_eq!(apps[0].arch, AppArch::Unknown);
        assert!(apps[0].leftovers.is_empty());
    }

    #[test]
    fn running_is_detected_from_the_bundle() {
        let home = Fixture::new();
        let path = home.app("Applications/Foo.app", "com.example.foo", "foo");
        let mut running = RunningProcesses::default();
        running
            .bundles
            .insert(path.canonicalize().expect("canonical"));
        let apps = list_apps_in(&home.ctx, &[home.path("Applications")], &running);
        assert!(apps[0].running);
    }

    #[test]
    fn symlinked_duplicate_is_listed_once() {
        let home = Fixture::new();
        let path = home.app("Applications/Foo.app", "com.example.foo", "foo");
        home.mkdir("Other");
        std::os::unix::fs::symlink(&path, home.path("Other/Foo.app")).expect("symlink");
        let apps = list_apps_in(
            &home.ctx,
            &[home.path("Applications"), home.path("Other")],
            &RunningProcesses::default(),
        );
        assert_eq!(apps.len(), 1);
    }

    #[test]
    fn apple_apps_are_system_unless_user_installable() {
        let home = Fixture::new();
        let safari = app_info_for(
            &home,
            "Applications/Safari.app",
            "com.apple.Safari",
            "Safari",
        );
        assert!(safari.is_system);
        assert!(safari.leftovers.is_empty());
        let xcode = app_info_for(
            &home,
            "Applications/Xcode.app",
            "com.apple.dt.Xcode",
            "Xcode",
        );
        assert!(!xcode.is_system);
        let pages = app_info_for(
            &home,
            "Applications/Pages.app",
            "com.apple.iWork.Pages",
            "Pages",
        );
        assert!(!pages.is_system);
    }

    #[test]
    fn anything_resolving_under_system_is_system() {
        let info = BundleInfo {
            resolved: PathBuf::from("/System/Applications/Whatever.app"),
            bundle_id: Some("com.example.notapple".to_string()),
            ..BundleInfo::default()
        };
        assert!(is_system_app(&info));
    }

    #[test]
    fn real_safari_is_system_through_its_symlink() {
        // Safari is a symlink into a cryptex on every supported macOS. Read-only check.
        let path = Path::new("/Applications/Safari.app");
        if path.exists() {
            assert!(is_system_app(&read_bundle(path)));
        }
    }

    #[test]
    fn containers_need_full_disk_access() {
        let home = Fixture::new();
        let app = home.app("Applications/Foo.app", "com.example.foo", "foo");
        home.write("Library/Containers/com.example.foo/Data/x", 1_000);

        let policy = policy(&home);
        let without = app_info(
            &home.ctx,
            &app,
            &RunningProcesses::default(),
            false,
            &policy,
        );
        assert!(without.leftovers.iter().all(|l| l.kind != "Contenedor"));

        let with = app_info(&home.ctx, &app, &RunningProcesses::default(), true, &policy);
        assert!(with.leftovers.iter().any(|l| l.kind == "Contenedor"));
    }

    #[test]
    fn a_browser_profile_is_not_measured_without_access() {
        let home = Fixture::new();
        let app = home.app(
            "Applications/Microsoft Edge.app",
            "com.microsoft.edgemac",
            "Edge",
        );
        home.write(
            "Library/Application Support/Microsoft Edge/Default/Cookies",
            1_000,
        );
        let policy = policy(&home);
        let without = app_info(
            &home.ctx,
            &app,
            &RunningProcesses::default(),
            false,
            &policy,
        );
        assert!(without.leftovers.is_empty());
        let with = app_info(&home.ctx, &app, &RunningProcesses::default(), true, &policy);
        assert_eq!(with.leftovers.len(), 1);
    }

    #[test]
    fn sensitive_apps_offer_no_leftovers() {
        let home = Fixture::new();
        home.write(
            "Library/Application Support/com.1password.1password/vault",
            1_000,
        );
        let app = app_info_for(
            &home,
            "Applications/1Password.app",
            "com.1password.1password",
            "1Password",
        );
        assert!(app.leftovers.is_empty());
    }

    #[test]
    fn generically_named_apps_do_not_claim_same_named_folders() {
        let home = Fixture::new();
        home.write("Library/Application Support/Code/data", 1_000);
        home.write("Library/Application Support/Zed/data", 1_000);
        let code = app_info_for(&home, "Applications/Code.app", "com.example.code", "Code");
        assert!(code.leftovers.is_empty());
        let zed = app_info_for(&home, "Applications/Zed.app", "dev.zed.Zed", "zed");
        assert!(zed.leftovers.is_empty(), "name too short to match safely");
    }

    #[test]
    fn uninstall_is_review_never_selected_and_blocked_by_the_process() {
        let home = Fixture::new();
        home.write("Library/Caches/com.example.foo/data", 5_000);
        let app = app_info_for(&home, "Applications/Foo.app", "com.example.foo", "foo");

        let bundle_only = uninstall_finding(&app, false);
        assert_eq!(bundle_only.risk, Risk::Review);
        assert!(!bundle_only.selected_by_default);
        assert_eq!(bundle_only.blocked_by, ["foo"]);
        assert_eq!(bundle_only.target_paths(), [app.path.as_path()]);
        assert_eq!(bundle_only.size, app.size);

        let with_data = uninstall_finding(&app, true);
        assert_eq!(with_data.target_paths().len(), 2);
        assert!(with_data.size > app.size);
        assert_eq!(bundle_only.id, with_data.id);
    }

    #[test]
    fn a_system_app_can_never_be_uninstalled() {
        let home = Fixture::new();
        let app = app_info_for(
            &home,
            "Applications/Safari.app",
            "com.apple.Safari",
            "Safari",
        );
        assert!(app.uninstall_blocked.is_some());
        let finding = uninstall_finding(&app, true);
        assert_eq!(finding.action, Action::ReportOnly);
        assert!(finding.target_paths().is_empty());
        assert_eq!(finding.size, 0);
    }

    #[test]
    fn an_ordinary_app_in_the_users_folder_can_be_uninstalled() {
        let home = Fixture::new();
        let app = app_info_for(&home, "Applications/Foozle.app", "com.example.foo", "foo");
        assert_eq!(app.uninstall_blocked, None);
    }

    #[test]
    fn the_policy_refusing_blocks_the_uninstall_up_front() {
        // Outside the home folder and outside /Applications.
        let home = Fixture::new();
        let elsewhere = tempfile::tempdir().expect("tempdir");
        let path = elsewhere.path().join("Vendor/Thing.app");
        std::fs::create_dir_all(&path).expect("mkdir");
        let app = app_info(
            &home.ctx,
            &path,
            &RunningProcesses::default(),
            false,
            &policy(&home),
        );
        let reason = app.uninstall_blocked.clone().expect("blocked");
        assert!(reason.contains("no toca"), "{reason}");
        let finding = uninstall_finding(&app, true);
        assert_eq!(finding.action, Action::ReportOnly);
        assert_eq!(finding.explanation, reason);
    }

    #[test]
    fn an_excluded_app_says_why() {
        let mut home = Fixture::new();
        let path = home.app("Applications/Foozle.app", "com.example.foo", "foo");
        home.ctx.config.excluded_paths = vec![path.clone()];
        let app = app_info(
            &home.ctx,
            &path,
            &RunningProcesses::default(),
            false,
            &policy(&home),
        );
        assert!(app
            .uninstall_blocked
            .is_some_and(|reason| reason.contains("excluiste")));
    }

    #[test]
    fn a_bundle_we_cannot_write_to_says_it_needs_an_administrator() {
        use std::os::unix::fs::PermissionsExt;
        let home = Fixture::new();
        let path = home.app("Applications/Foozle.app", "com.example.foo", "foo");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o555)).expect("chmod");
        let app = app_info(
            &home.ctx,
            &path,
            &RunningProcesses::default(),
            false,
            &policy(&home),
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(app
            .uninstall_blocked
            .is_some_and(|reason| reason.contains("administrador")));
    }
}
