//! Exact duplicates among the user's files.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::Result;
use hm_core::fmt::human_size;
use hm_core::{fsutil, t, tf, Action, Area, Finding, Recovery, Risk, ScanContext, Scanner};
use rayon::prelude::*;

use super::walk::{self, UserFile};
use crate::gate;

const SCANNER_ID: &str = "files.duplicates";
const MIN_BYTES: u64 = 10_000_000;
/// Hashing whole files is the expensive part, so same-sized files are first compared by their ends.
const SAMPLE_BYTES: usize = 64 * 1024;
const LISTED_PATHS: usize = 5;

pub(crate) struct Duplicates;

impl Scanner for Duplicates {
    fn id(&self) -> &'static str {
        SCANNER_ID
    }
    fn name(&self) -> &'static str {
        t!("Archivos duplicados", "Duplicate files")
    }
    fn description(&self) -> &'static str {
        t!(
            "Copias idénticas del mismo archivo que ocupan espacio de verdad.",
            "Identical copies of the same file that really take up space."
        )
    }
    fn area(&self) -> Area {
        Area::Files
    }
    fn scan(&self, ctx: &ScanContext) -> Result<Vec<Finding>> {
        Ok(scan_duplicates(ctx, MIN_BYTES))
    }
}

pub(crate) fn scan_duplicates(ctx: &ScanContext, min_bytes: u64) -> Vec<Finding> {
    let outcome = walk::user_files(ctx, min_bytes, walk::Purpose::Duplicates);
    let mut findings: Vec<Finding> = identical_groups(ctx, outcome.files)
        .into_iter()
        .filter_map(|(hash, files)| group_finding(ctx, &hash, files))
        .collect();
    findings.sort_by_key(|finding| std::cmp::Reverse(finding.size));
    if outcome.skipped_gated {
        findings.push(gate::missing_access_finding(
            SCANNER_ID,
            Area::Files,
            t!(
                "Sin el Acceso total al disco no se buscan duplicados en Documentos, Escritorio, \
                 Descargas ni Música, que es donde más suelen acumularse. Con el permiso, HealthyMac \
                 también comparará esos archivos.",
                "Without Full Disk Access, duplicates are not searched for in Documents, Desktop, \
                 Downloads or Music, where they pile up the most. With the permission, HealthyMac \
                 will compare those files too."
            ),
        ));
    }
    findings
}

/// Same size, then same ends, then same SHA-256. Returns each set of identical files by its hash.
fn identical_groups(ctx: &ScanContext, files: Vec<UserFile>) -> Vec<(String, Vec<UserFile>)> {
    // Hard links are one file on disk, whatever the number of names.
    let mut seen = HashSet::new();
    let mut by_size: HashMap<u64, Vec<UserFile>> = HashMap::new();
    for file in files {
        if !file.is_bundle && seen.insert((file.dev, file.ino)) {
            by_size.entry(file.logical).or_default().push(file);
        }
    }
    by_size
        .into_values()
        .filter(|group| group.len() > 1)
        .collect::<Vec<_>>()
        .into_par_iter()
        .flat_map_iter(|group| {
            let sampled = split_by(group, |file| sample_hash(&file.path).map(|h| h.to_string()));
            sampled
                .into_iter()
                .flat_map(|(_, group)| {
                    split_by(group, |file| {
                        fsutil::sha256_file(&file.path, &ctx.cancel).ok()
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Groups by `key`, keeping groups of two or more. Files `key` cannot read are dropped: a file we
/// could not compare can never be called a duplicate.
fn split_by(
    files: Vec<UserFile>,
    key: impl Fn(&UserFile) -> Option<String> + Sync,
) -> Vec<(String, Vec<UserFile>)> {
    let keyed: Vec<(String, UserFile)> = files
        .into_par_iter()
        .filter_map(|file| key(&file).map(|key| (key, file)))
        .collect();
    let mut groups: HashMap<String, Vec<UserFile>> = HashMap::new();
    for (key, file) in keyed {
        groups.entry(key).or_default().push(file);
    }
    groups
        .into_iter()
        .filter(|(_, group)| group.len() > 1)
        .collect()
}

fn sample_hash(path: &Path) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut buffer = vec![0u8; SAMPLE_BYTES];
    let mut hasher = DefaultHasher::new();
    let read = file.read(&mut buffer).ok()?;
    buffer[..read].hash(&mut hasher);
    if len > SAMPLE_BYTES as u64 {
        file.seek(SeekFrom::End(-(SAMPLE_BYTES as i64))).ok()?;
        let read = file.read(&mut buffer).ok()?;
        buffer[..read].hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// The copy to keep. People tend to file things away and leave the extra copy in Downloads, so
/// anything else is preferred, then the oldest, then the shortest path.
fn keep_rank(file: &UserFile, home: &Path) -> impl Ord {
    let transient = file.path.starts_with(home.join("Downloads"))
        || file.path.starts_with(home.join("Desktop"));
    (
        transient,
        file.modified,
        file.path.as_os_str().len(),
        file.path.clone(),
    )
}

fn group_finding(ctx: &ScanContext, hash: &str, mut files: Vec<UserFile>) -> Option<Finding> {
    files.sort_by_cached_key(|file| keep_rank(file, &ctx.home));
    let keep = files.first()?.clone();

    // APFS clones share their blocks, which changes what removing one means. A clone of the kept
    // file costs nothing, so it stays. In any other family of clones the blocks are only released
    // once every member is gone, so it is all of them or nothing.
    let keep_family = fsutil::clone_id(&keep.path);
    let mut families: HashMap<u64, Vec<&UserFile>> = HashMap::new();
    let mut extra: Vec<PathBuf> = Vec::new();
    let mut reclaimed = 0u64;
    for file in files.iter().skip(1) {
        match fsutil::clone_id(&file.path) {
            Some(id) if Some(id) == keep_family => {}
            Some(id) => families.entry(id).or_default().push(file),
            // Not on APFS: every copy has its own blocks.
            None => {
                reclaimed += file.on_disk;
                extra.push(file.path.clone());
            }
        }
    }
    for members in families.values() {
        reclaimed += match members.as_slice() {
            // On its own, so the private size is exact even if it shares part of its blocks.
            [only] => fsutil::private_size(&only.path).unwrap_or(only.on_disk),
            // Clones report a private size of 0 each. Together they free one copy's worth.
            [first, ..] => first.on_disk,
            [] => 0,
        };
        extra.extend(members.iter().map(|member| member.path.clone()));
    }
    extra.sort();
    if extra.is_empty() || reclaimed == 0 {
        return None;
    }

    let name = gate::name_of(&keep.path);
    let mut listed: Vec<String> = extra
        .iter()
        .take(LISTED_PATHS)
        .map(|path| path.display().to_string())
        .collect();
    if extra.len() > LISTED_PATHS {
        listed.push(tf!("y {} más", "and {} more", extra.len() - LISTED_PATHS));
    }
    let count = extra.len() as u64;
    Some(
        Finding::new(
            SCANNER_ID,
            Area::Files,
            // The content hash, so a selection survives the files being moved around.
            &hash[..hash.len().min(16)],
            tf!("{name}: {} copias", "{name}: {} copies", extra.len() + 1),
        )
        .subtitle(tf!("Se conserva {}", "Keeping {}", keep.path.display()))
        .explanation(tf!(
            "Archivos idénticos byte a byte, de {} cada uno. Se conserva una copia y el resto va a \
             la Papelera: {}. Comprueba que ninguna app o proyecto dependa de esa ubicación.",
            "Files identical byte for byte, {} each. One copy is kept and the rest go to the \
             Trash: {}. Check that no app or project depends on that location.",
            human_size(keep.logical),
            listed.join(", ")
        ))
        .size(reclaimed)
        .items(count)
        .last_used(keep.modified)
        .risk(Risk::Review)
        .recovery(Recovery::None)
        .selected(false)
        .action(Action::Trash { paths: extra }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{find, targets, Fixture, LockedDir};
    use std::fs;

    const MIN: u64 = 50_000;

    fn write_unique(home: &Fixture, relative: &str, seed: u8, bytes: usize) -> PathBuf {
        let path = home.path(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut content = vec![seed; bytes];
        // Differ only in the middle, past what the ends can tell apart.
        content[bytes / 2] = seed.wrapping_add(1);
        fs::write(&path, content).expect("write");
        path
    }

    fn scan(home: &Fixture) -> Vec<Finding> {
        scan_duplicates(&home.ctx, MIN)
    }

    #[test]
    fn nothing_to_compare_is_silent() {
        let home = Fixture::new();
        home.write("Projects/only.bin", 100_000);
        assert!(scan(&home).is_empty());
    }

    #[test]
    fn identical_files_keep_one_and_trash_the_rest() {
        let home = Fixture::new();
        let a = home.write("Projects/a.bin", 100_000);
        let b = home.write("Movies/b.bin", 100_000);
        let c = home.write("Pictures/c.bin", 100_000);
        let findings = scan(&home);
        assert_eq!(findings.len(), 1);
        let finding = &findings[0];
        assert_eq!(finding.risk, Risk::Review);
        assert!(!finding.selected_by_default);
        let removed = targets(finding);
        assert_eq!(removed.len(), 2);
        let all = [a, b, c];
        assert!(removed.iter().all(|path| all.contains(path)));
        assert!(finding.size >= 200_000, "{}", finding.size);
        assert!(finding
            .subtitle
            .as_deref()
            .is_some_and(|s| s.starts_with("Se conserva")));
    }

    #[test]
    fn same_size_and_same_ends_but_different_content_is_not_a_duplicate() {
        let home = Fixture::new();
        write_unique(&home, "Projects/a.bin", 1, 200_000);
        let other = write_unique(&home, "Projects/b.bin", 1, 200_000);
        // Same ends, different middle.
        let mut content = fs::read(&other).expect("read");
        content[100_000] = 99;
        fs::write(&other, content).expect("write");
        assert!(scan(&home).is_empty());
    }

    #[test]
    fn below_the_minimum_is_ignored() {
        let home = Fixture::new();
        home.write("Projects/a.bin", 1_000);
        home.write("Projects/b.bin", 1_000);
        assert!(scan(&home).is_empty());
    }

    #[test]
    fn hard_links_are_one_file() {
        let home = Fixture::new();
        let a = home.write("Projects/a.bin", 100_000);
        fs::hard_link(&a, home.path("Projects/b.bin")).expect("hard link");
        assert!(scan(&home).is_empty());
    }

    /// `fs::copy` clones on APFS, so this is the case the scanner has to see through. On a volume
    /// without clones the two are real copies and it correctly reports them.
    #[test]
    fn apfs_clones_are_not_duplicates() {
        let home = Fixture::new();
        let a = home.write("Projects/a.bin", 100_000);
        let b = home.path("Projects/b.bin");
        fs::copy(&a, &b).expect("copy");
        if fsutil::clone_id(&a).is_some() && fsutil::clone_id(&a) == fsutil::clone_id(&b) {
            assert!(scan(&home).is_empty());
        }
    }

    #[test]
    fn a_real_copy_alongside_a_clone_is_still_reported_once() {
        let home = Fixture::new();
        let a = home.write("Projects/a.bin", 100_000);
        let b = home.path("Projects/b.bin");
        fs::copy(&a, &b).expect("copy");
        let c = home.write("Projects/c.bin", 100_000);
        if fsutil::clone_id(&a).is_none() || fsutil::clone_id(&a) != fsutil::clone_id(&b) {
            return;
        }
        let findings = scan(&home);
        assert_eq!(findings.len(), 1);
        let mut removed = targets(&findings[0]);
        removed.sort();
        // Either the real copy goes, or the whole clone family does. Removing one clone of a pair
        // would free nothing.
        let mut family = vec![a, b];
        family.sort();
        assert!(removed == [c] || removed == family, "{removed:?}");
        assert!(findings[0].size >= 100_000, "{}", findings[0].size);
    }

    /// The case that used to vanish: the copy worth keeping is the odd one out, and each clone
    /// reports a private size of 0.
    #[test]
    fn a_clone_family_is_removed_whole_and_counted_once() {
        let home = Fixture::new();
        home.grant_full_disk_access();
        let a = home.write("Downloads/a.bin", 100_000);
        let b = home.path("Downloads/b.bin");
        fs::copy(&a, &b).expect("copy");
        let keep = home.write("Movies/keep.bin", 100_000);
        if fsutil::clone_id(&a).is_none() || fsutil::clone_id(&a) != fsutil::clone_id(&b) {
            return;
        }
        let findings = scan(&home);
        assert_eq!(findings.len(), 1, "must not vanish");
        let mut removed = targets(&findings[0]);
        removed.sort();
        let mut family = vec![a, b];
        family.sort();
        assert_eq!(removed, family);
        assert!(findings[0]
            .subtitle
            .as_deref()
            .is_some_and(|s| s.contains(&keep.display().to_string())));
        let size = findings[0].size;
        assert!(
            (100_000..150_000).contains(&size),
            "shared blocks count once: {size}"
        );
    }

    #[test]
    fn a_clone_of_the_kept_file_stays() {
        let home = Fixture::new();
        home.grant_full_disk_access();
        let keep = home.write("Movies/keep.bin", 100_000);
        let clone = home.path("Pictures/clone.bin");
        fs::create_dir_all(clone.parent().expect("parent")).expect("mkdir");
        fs::copy(&keep, &clone).expect("copy");
        let real = home.write("Downloads/real.bin", 100_000);
        if fsutil::clone_id(&keep).is_none() || fsutil::clone_id(&keep) != fsutil::clone_id(&clone)
        {
            return;
        }
        let findings = scan(&home);
        assert_eq!(targets(&findings[0]), [real]);
    }

    #[test]
    fn prefers_keeping_the_copy_outside_downloads() {
        let home = Fixture::new();
        home.grant_full_disk_access();
        let downloaded = home.write("Downloads/video.mov", 100_000);
        let filed = home.write("Movies/video.mov", 100_000);
        let findings = scan(&home);
        assert_eq!(targets(&findings[0]), [downloaded]);
        assert!(findings[0]
            .subtitle
            .as_deref()
            .is_some_and(|subtitle| subtitle.contains(&filed.display().to_string())));
    }

    #[test]
    fn gated_folders_are_reported_once_without_access() {
        let home = Fixture::new();
        home.write("Downloads/a.bin", 100_000);
        home.write("Downloads/b.bin", 100_000);
        let findings = scan(&home);
        assert_eq!(findings.len(), 1);
        assert!(find(&findings, "sin_acceso").is_some());
    }

    #[test]
    fn a_file_that_cannot_be_read_is_never_called_a_duplicate() {
        use std::os::unix::fs::PermissionsExt;
        let home = Fixture::new();
        home.write("Projects/a.bin", 100_000);
        let b = home.write("Projects/b.bin", 100_000);
        fs::set_permissions(&b, fs::Permissions::from_mode(0o000)).expect("chmod");
        assert!(scan(&home).is_empty());
        fs::set_permissions(&b, fs::Permissions::from_mode(0o644)).expect("chmod");
    }

    #[test]
    fn unreadable_folder_is_not_fatal() {
        let home = Fixture::new();
        home.write("Projects/a.bin", 100_000);
        home.write("Projects/b.bin", 100_000);
        let locked = home.mkdir("Projects/locked");
        let _guard = LockedDir::new(&locked);
        assert_eq!(scan(&home).len(), 1);
    }

    #[test]
    fn cancelling_while_hashing_finds_nothing() {
        let home = Fixture::new();
        home.write("Projects/a.bin", 100_000);
        home.write("Projects/b.bin", 100_000);
        home.ctx.cancel.cancel();
        assert!(scan(&home).is_empty());
    }
}
