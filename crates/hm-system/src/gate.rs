//! Rule 6: no permission prompts in the middle of a scan.
//!
//! Everything a scanner needs to decide whether it may touch a path lives here, so the gating
//! cannot drift between scanners.

use std::ffi::CString;
use std::fs;
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use hm_core::{fsutil, t, Area, Finding, ScanContext};

/// Home-relative folders that macOS guards with TCC, so reading them can put a dialog on screen.
///
/// Wider than the five that rule 6 names, because the system guards more than that. These were
/// found by watching `tccd` while walking a real home folder: `Music` raised a Media Library
/// request and browser profiles raised "app data" requests. Note the app is ad-hoc signed, with no
/// hardened runtime, so unlike a tool run from Claude Code or a terminal it would really prompt.
const GUARDED_IN_HOME: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Library/CloudStorage",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Mobile Documents",
    "Music",
    "Movies/TV",
    // Personal data with its own permission dialog.
    "Library/Application Support/AddressBook",
    // Browser profiles. Seen for Chrome and Brave. The others are the same kind of thing, so they
    // are assumed to behave alike rather than found out on someone's screen.
    "Library/Application Support/Arc",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Chromium",
    "Library/Application Support/Firefox",
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/Google/Chrome Beta",
    "Library/Application Support/Google/Chrome Canary",
    "Library/Application Support/Microsoft Edge",
    "Library/Application Support/Vivaldi",
    "Library/Application Support/com.operasoftware.Opera",
];

/// Media libraries, guarded wherever they happen to be. Moved libraries keep their protection. Going
/// inside an iMovie library is the likely source of a Media Library dialog seen on a real Mac: only the
/// scanner that measures these got stuck behind a pending dialog, not the one that walks past them.
const GUARDED_EXTENSIONS: &[&str] = &[
    "fcpbundle",
    "imovielibrary",
    "musiclibrary",
    "photoslibrary",
    "tvlibrary",
];

/// The only `~/Library` folders entered without Full Disk Access. `~/Library` holds dozens of guarded
/// areas and macOS adds more with each release. Reading `~/Library/iTunes` put a real dialog on screen
/// and walking this folder raised a Photos one, so a list of what's known to be ordinary app data is
/// the only approach that stays safe on a system nobody has tested yet.
const READABLE_IN_LIBRARY: &[&str] = &[
    "Android",
    "Application Support",
    "Arduino15",
    "Caches",
    "Developer",
    "HTTPStorages",
    "Java",
    "LaunchAgents",
    "Logs",
    "Preferences",
    "Python",
    "Saved Application State",
    "WebKit",
    "pnpm",
];

/// Whether this is a part of `~/Library` to stay out of. Apple's own folders inside the readable ones
/// count too: the system manages them, and several refuse access outright.
fn is_guarded_in_library(relative: &Path) -> bool {
    let parts: Vec<_> = relative.iter().map(|part| part.to_string_lossy()).collect();
    match parts.as_slice() {
        [library, name] if library == "Library" => !READABLE_IN_LIBRARY.contains(&name.as_ref()),
        [library, folder, name]
            if library == "Library" && (folder == "Caches" || folder == "Application Support") =>
        {
            name.starts_with("com.apple.")
        }
        _ => false,
    }
}

/// Whether entering this exact folder could raise a permission prompt.
pub(crate) fn is_guarded_folder(home: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(home) else {
        return false;
    };
    is_guarded_in_library(relative)
        || GUARDED_IN_HOME
            .iter()
            .any(|guarded| relative == Path::new(guarded))
        || path
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
            .is_some_and(|ext| GUARDED_EXTENSIONS.contains(&ext.as_str()))
}

/// Whether touching `path`, even just to stat it, means reaching inside a guarded folder.
pub(crate) fn is_inside_guarded(home: &Path, path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| is_guarded_folder(home, ancestor))
}

/// Lets the UI collapse the per-scanner "grant access" findings into one call to action.
pub(crate) const NEEDS_ACCESS_TAG: &str = hm_core::TAG_NEEDS_FULL_DISK_ACCESS;

pub(crate) fn has_access(ctx: &ScanContext) -> bool {
    fsutil::has_full_disk_access(&ctx.home)
}

/// The single report-only finding a scanner emits when it had to skip something. `gain` says what
/// the user would get by granting access.
pub(crate) fn missing_access_finding(scanner_id: &str, area: Area, gain: &str) -> Finding {
    Finding::new(
        scanner_id,
        area,
        "sin_acceso",
        t!(
            "Falta el Acceso total al disco",
            "Full Disk Access is missing"
        ),
    )
    .explanation(gain)
    .tag(NEEDS_ACCESS_TAG)
}

/// Roots the user configured inside `dir`. Rule 6 allows touching a gated folder without Full Disk
/// Access when the user pointed us at it themselves.
pub(crate) fn configured_roots_inside(ctx: &ScanContext, dir: &Path) -> Vec<PathBuf> {
    ctx.config
        .project_roots
        .iter()
        .filter(|root| root.starts_with(dir))
        .cloned()
        .collect()
}

/// A directory listing that keeps "not there" apart from "not allowed". Reporting an unreadable
/// folder as empty is the one thing a cleaner must not do.
pub(crate) enum DirListing {
    Missing,
    Unreadable,
    Entries(Vec<PathBuf>),
}

pub(crate) fn list_dir(path: &Path) -> DirListing {
    match fs::read_dir(path) {
        Ok(entries) => DirListing::Entries(entries.flatten().map(|entry| entry.path()).collect()),
        Err(err) if err.kind() == ErrorKind::NotFound => DirListing::Missing,
        Err(_) => DirListing::Unreadable,
    }
}

/// The user asked us never to touch this path, or something inside it.
pub(crate) fn is_excluded(ctx: &ScanContext, path: &Path) -> bool {
    ctx.config
        .excluded_paths
        .iter()
        .any(|excluded| path.starts_with(excluded) || excluded.starts_with(path))
}

/// Whether this user may write to `path`. Used to avoid offering a clean that can only fail, since
/// nothing in HealthyMac runs with elevated privileges.
///
/// Never use this on an app bundle: `access(2)` there goes through TCC's App Management check. Use
/// [`mode_permits_write`] instead.
pub(crate) fn is_writable(path: &Path) -> bool {
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is NUL-terminated and outlives the call.
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

/// Whether the owner and mode bits let this user write to `path`. A plain `stat`, so unlike
/// [`is_writable`] it never consults TCC, which also means it cannot see App Management.
pub(crate) fn mode_permits_write(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    let mode = meta.mode();
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    if meta.uid() == unsafe { libc::geteuid() } {
        return mode & 0o200 != 0;
    }
    if in_group(meta.gid()) {
        return mode & 0o020 != 0;
    }
    mode & 0o002 != 0
}

fn in_group(gid: u32) -> bool {
    // SAFETY: `getegid` has no preconditions and cannot fail.
    if gid == unsafe { libc::getegid() } {
        return true;
    }
    let mut groups = [0 as libc::gid_t; 64];
    // SAFETY: `groups` is writable for the number of entries passed.
    let count = unsafe { libc::getgroups(groups.len() as libc::c_int, groups.as_mut_ptr()) };
    usize::try_from(count).is_ok_and(|count| groups.iter().take(count).any(|group| *group == gid))
}

pub(crate) fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(crate) fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.file_type().is_symlink())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, LockedDir};

    #[test]
    fn media_and_personal_data_are_guarded_but_ordinary_caches_are_not() {
        let home = Path::new("/Users/ana");
        for guarded in [
            "Library/iTunes",
            "Library/iTunes/iPhone Software Updates",
            "Library/Photos/Libraries",
            "Library/Calendars",
            "Library/Safari",
            "Library/SomethingNewInTheNextMacOS",
            "Library/Caches/com.apple.Music",
            "Library/Application Support/com.apple.sharedfilelist",
            "Library/Application Support/AddressBook",
            "Movies/TV",
            "Movies/Mis vídeos/Viaje.imovielibrary",
        ] {
            assert!(
                is_inside_guarded(home, &home.join(guarded).join("nested/file")),
                "{guarded}"
            );
        }
        for free in [
            "Library/Caches",
            "Library",
            "Library/Caches",
            "Library/Caches/com.spotify.client",
            "Library/Application Support/discord",
            "Library/Developer/Xcode/DerivedData",
            "Library/pnpm/store",
            "Movies",
            "Movies/CapCut",
        ] {
            assert!(!is_guarded_folder(home, &home.join(free)), "{free}");
        }
    }

    #[test]
    fn listing_tells_missing_from_unreadable() {
        let home = Fixture::new();
        let dir = home.mkdir("locked");
        let _guard = LockedDir::new(&dir);
        assert!(matches!(list_dir(&dir), DirListing::Unreadable));
        assert!(matches!(list_dir(&home.path("nope")), DirListing::Missing));
    }

    #[test]
    fn mode_bits_decide_write_access_without_asking_the_system() {
        use std::os::unix::fs::PermissionsExt;
        let home = Fixture::new();
        let dir = home.mkdir("folder");
        assert!(mode_permits_write(&dir));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).expect("chmod");
        assert!(!mode_permits_write(&dir));
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod");
        assert!(!mode_permits_write(&home.path("missing")));
        // Owned by root and only writable by it, as an app installed from a package is.
        assert!(!mode_permits_write(Path::new("/usr/bin")));
    }

    #[test]
    fn guarded_folders_cover_what_macos_really_guards() {
        let home = Path::new("/Users/ana");
        for path in [
            "/Users/ana/Documents",
            "/Users/ana/Music",
            "/Users/ana/Library/Containers",
            "/Users/ana/Library/Application Support/Google/Chrome",
            "/Users/ana/Pictures/Photos Library.photoslibrary",
        ] {
            assert!(is_guarded_folder(home, Path::new(path)), "{path}");
        }
        for path in [
            "/Users/ana",
            "/Users/ana/Pictures",
            "/Users/ana/Library",
            "/Users/ana/Library/Application Support",
            "/Users/ana/Library/Application Support/Google",
            "/opt/Documents",
        ] {
            assert!(!is_guarded_folder(home, Path::new(path)), "{path}");
        }
    }

    #[test]
    fn inside_guarded_looks_at_every_ancestor() {
        let home = Path::new("/Users/ana");
        assert!(is_inside_guarded(
            home,
            Path::new("/Users/ana/Documents/a/b.sh")
        ));
        assert!(is_inside_guarded(home, Path::new("/Users/ana/Documents")));
        assert!(!is_inside_guarded(home, Path::new("/Users/ana/bin/b.sh")));
        assert!(!is_inside_guarded(home, Path::new("/usr/local/bin/b.sh")));
    }

    #[test]
    fn exclusion_applies_in_both_directions() {
        let mut home = Fixture::new();
        home.ctx.config.excluded_paths = vec![home.path("Library/Caches/keep")];
        assert!(is_excluded(
            &home.ctx,
            &home.path("Library/Caches/keep/inner")
        ));
        assert!(is_excluded(&home.ctx, &home.path("Library/Caches")));
        assert!(!is_excluded(&home.ctx, &home.path("Library/Caches/other")));
    }

    #[test]
    fn configured_root_inside_gated_folder_is_found() {
        let mut home = Fixture::new();
        let documents = home.path("Documents");
        home.ctx.config.project_roots = vec![documents.join("code"), home.path("Coding")];
        assert_eq!(
            configured_roots_inside(&home.ctx, &documents),
            vec![documents.join("code")]
        );
    }
}
