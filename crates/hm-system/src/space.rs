//! Builds the size tree behind the disk map (treemap).

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use hm_core::{fsutil, tf, CancelToken};
use rayon::prelude::*;

use crate::gate;
use crate::types::DirNode;

/// Scans `root` down to `max_depth` levels. Sizes always cover the full subtree, whatever the depth
/// limit. Children smaller than `min_child_bytes` are folded into [`DirNode::other_size`], along
/// with the folder's own metadata, so that a folder's `size` is exactly its children plus
/// `other_size`.
///
/// Never follows symlinks and never crosses onto another volume. A folder that macOS would ask
/// permission for is not entered without Full Disk Access unless `root` is inside it, which means
/// the user chose it: it appears as an empty node marked `partial`. The home directory is worked
/// out from the path, which only recognises `/Users/<name>`: use [`scan_tree_in_home`] for a home
/// anywhere else.
///
/// Errors only if `root` itself does not exist. A cancelled scan returns what it had, marked
/// `partial`.
pub fn scan_tree(
    root: &Path,
    max_depth: usize,
    min_child_bytes: u64,
    cancel: &CancelToken,
) -> Result<DirNode> {
    run(root, Gate::FromPath, max_depth, min_child_bytes, cancel)
}

/// [`scan_tree`], gating against an explicit home directory such as `ctx.home`.
pub fn scan_tree_in_home(
    root: &Path,
    home: &Path,
    max_depth: usize,
    min_child_bytes: u64,
    cancel: &CancelToken,
) -> Result<DirNode> {
    run(
        root,
        Gate::Home(home.to_path_buf()),
        max_depth,
        min_child_bytes,
        cancel,
    )
}

fn run(
    root: &Path,
    gate: Gate,
    max_depth: usize,
    min_child_bytes: u64,
    cancel: &CancelToken,
) -> Result<DirNode> {
    let meta = fs::symlink_metadata(root)
        .with_context(|| tf!("no se pudo leer {}", "could not read {}", root.display()))?;
    let mut devices = vec![meta.dev()];
    // `/` is the sealed system volume, and almost everything a user thinks of as "the disk" is on
    // the data volume, reached through firmlinks. Without this the map of `/` would be nearly empty.
    let whole_disk = root == Path::new("/");
    if whole_disk {
        if let Ok(data) = fs::symlink_metadata("/System/Volumes/Data") {
            devices.push(data.dev());
        }
    }
    let scan = Scan {
        root: root.to_path_buf(),
        devices,
        whole_disk,
        min_child_bytes,
        gate,
        cancel,
        seen: Mutex::new(HashSet::new()),
    };
    let mut node = scan.visit(root, &meta, max_depth);
    node.name = root.display().to_string();
    Ok(node)
}

enum Gate {
    /// Find the home from the path: `/Users/<name>/...`.
    FromPath,
    Home(PathBuf),
}

impl Gate {
    fn home_of(&self, path: &Path) -> Option<PathBuf> {
        match self {
            Gate::Home(home) => path.starts_with(home).then(|| home.clone()),
            Gate::FromPath => {
                let parts: Vec<_> = path.components().collect();
                let users = parts.iter().position(|part| part.as_os_str() == "Users")?;
                let parent: PathBuf = parts[..=users].iter().collect();
                let is_home_parent = parent == Path::new("/Users")
                    || parent == Path::new("/System/Volumes/Data/Users");
                // Needs a component after `Users`, which is the account's name.
                parts.get(users + 1)?;
                is_home_parent.then(|| parts[..=users + 1].iter().collect())
            }
        }
    }

    fn blocks(&self, path: &Path) -> bool {
        self.home_of(path).is_some_and(|home| {
            gate::is_guarded_folder(&home, path) && !fsutil::has_full_disk_access(&home)
        })
    }
}

struct Scan<'a> {
    root: PathBuf,
    devices: Vec<u64>,
    whole_disk: bool,
    min_child_bytes: u64,
    gate: Gate,
    cancel: &'a CancelToken,
    seen: Mutex<HashSet<(u64, u64)>>,
}

impl Scan<'_> {
    fn leaf(&self, path: &Path, meta: &Metadata) -> DirNode {
        let mut size = meta.blocks() * 512;
        if !meta.is_dir() && meta.nlink() > 1 {
            // A hard link's blocks are counted at its first name only.
            let first = self
                .seen
                .lock()
                .map(|mut seen| seen.insert((meta.dev(), meta.ino())))
                .unwrap_or(true);
            if !first {
                size = 0;
            }
        }
        DirNode {
            name: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            path: path.to_path_buf(),
            size,
            is_dir: meta.is_dir(),
            file_count: u64::from(!meta.is_dir()),
            last_modified: if meta.is_dir() {
                None
            } else {
                meta.modified().ok()
            },
            children: Vec::new(),
            other_size: 0,
            partial: false,
        }
    }

    fn visit(&self, path: &Path, meta: &Metadata, depth_left: usize) -> DirNode {
        let mut node = self.leaf(path, meta);
        if !meta.is_dir() {
            return node;
        }
        // The node stands for the whole folder from here on, including its own metadata.
        node.other_size = node.size;

        if path != self.root && self.gate.blocks(path) {
            node.partial = true;
            node.size = 0;
            node.other_size = 0;
            return node;
        }
        if self.cancel.is_cancelled() {
            node.partial = true;
            return node;
        }
        let entries: Vec<PathBuf> = match fs::read_dir(path) {
            Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
            Err(_) => {
                node.partial = true;
                return node;
            }
        };
        let children: Vec<DirNode> = entries
            .par_iter()
            .filter_map(|child| {
                let child_meta = fs::symlink_metadata(child).ok()?;
                if !self.devices.contains(&child_meta.dev()) {
                    return None;
                }
                // The data volume's mount point mirrors what the firmlinks already reach.
                if self.whole_disk && child == Path::new("/System/Volumes") {
                    return None;
                }
                Some(self.visit(child, &child_meta, depth_left.saturating_sub(1)))
            })
            .collect();

        let keep_children = depth_left > 0;
        for child in children {
            node.size += child.size;
            node.file_count += child.file_count;
            node.partial |= child.partial;
            node.last_modified = node.last_modified.max(child.last_modified);
            if keep_children && child.size >= self.min_child_bytes && child.size > 0 {
                node.children.push(child);
            } else if keep_children && child.partial && child.size == 0 {
                // A gated or unreadable folder must stay visible, or the map would imply it holds
                // nothing when in fact we just could not look.
                node.children.push(child);
            } else {
                node.other_size += child.size;
            }
        }
        node.children
            .sort_by_key(|child| std::cmp::Reverse(child.size));
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, LockedDir};

    fn scan(root: &Path, home: &Path, depth: usize, min: u64) -> DirNode {
        scan_tree_in_home(root, home, depth, min, &CancelToken::new()).expect("scan")
    }

    /// Folders only. A file has no children to add up, and its size is simply its size.
    fn assert_adds_up(node: &DirNode) {
        if !node.is_dir {
            return;
        }
        let children: u64 = node.children.iter().map(|child| child.size).sum();
        assert_eq!(
            node.size,
            children + node.other_size,
            "{}",
            node.path.display()
        );
        node.children.iter().for_each(assert_adds_up);
    }

    #[test]
    fn missing_root_is_an_error() {
        let home = Fixture::new();
        let result = scan_tree(&home.path("nope"), 2, 0, &CancelToken::new());
        assert!(result.is_err());
    }

    #[test]
    fn a_file_as_root_works() {
        let home = Fixture::new();
        let file = home.write("a.bin", 10_000);
        let node = scan(&file, home.dir.path(), 2, 0);
        assert!(!node.is_dir);
        assert_eq!(node.file_count, 1);
        assert!(node.size >= 10_000);
    }

    #[test]
    fn sizes_cover_the_whole_subtree_whatever_the_depth() {
        let home = Fixture::new();
        home.write("top/a/b/c/deep.bin", 100_000);
        let shallow = scan(&home.path("top"), home.dir.path(), 1, 0);
        let deep = scan(&home.path("top"), home.dir.path(), 10, 0);
        assert_eq!(shallow.size, deep.size);
        assert!(shallow.size >= 100_000);
        assert_eq!(shallow.file_count, 1);
        assert_eq!(shallow.children.len(), 1);
        assert!(
            shallow.children[0].children.is_empty(),
            "depth limit keeps it shallow"
        );
        assert_adds_up(&shallow);
        assert_adds_up(&deep);
    }

    #[test]
    fn small_children_fold_into_other_and_large_come_first() {
        let home = Fixture::new();
        home.write("top/big/a.bin", 500_000);
        home.write("top/medium/a.bin", 200_000);
        home.write("top/tiny.bin", 1_000);
        let node = scan(&home.path("top"), home.dir.path(), 2, 100_000);
        let names: Vec<&str> = node
            .children
            .iter()
            .map(|child| child.name.as_str())
            .collect();
        assert_eq!(names, ["big", "medium"]);
        assert!(node.other_size >= 1_000);
        assert_adds_up(&node);
    }

    #[test]
    fn does_not_follow_symlinks() {
        let home = Fixture::new();
        home.write("outside/big.bin", 1_000_000);
        home.mkdir("top");
        std::os::unix::fs::symlink(home.path("outside"), home.path("top/link")).expect("symlink");
        let node = scan(&home.path("top"), home.dir.path(), 3, 0);
        assert!(node.size < 1_000_000, "{}", node.size);
    }

    #[test]
    fn hard_links_count_once() {
        let home = Fixture::new();
        let a = home.write("top/a.bin", 100_000);
        std::fs::hard_link(&a, home.path("top/b.bin")).expect("hard link");
        let node = scan(&home.path("top"), home.dir.path(), 2, 0);
        assert!(node.size < 200_000, "{}", node.size);
        assert_eq!(node.file_count, 2);
    }

    #[test]
    fn unreadable_subtree_marks_the_path_up_to_the_root_as_partial() {
        let home = Fixture::new();
        home.write("top/ok/a.bin", 10_000);
        let locked = home.mkdir("top/ok/locked");
        let _guard = LockedDir::new(&locked);
        let node = scan(&home.path("top"), home.dir.path(), 3, 0);
        assert!(node.partial);
        assert!(node.children[0].partial);
        assert!(node.size >= 10_000);
    }

    /// Every path used is absolute, so `PATH_MAX` is what bounds the recursion. A stack overflow
    /// would abort the process, so this must hold at the deepest tree that can exist.
    #[test]
    fn the_deepest_possible_tree_does_not_crash_and_stays_exact() {
        crate::test_support::with_big_stack(|| {
            let home = Fixture::new();
            let room = 1000usize.saturating_sub(home.path("top").as_os_str().len());
            let deep = "d/".repeat(room / 2);
            home.write(&format!("top/{deep}a.bin"), 1_000);
            let node = scan(&home.path("top"), home.dir.path(), 2, 0);
            assert_eq!(node.file_count, 1);
            assert!(!node.partial);
            assert!(node.size >= 1_000);
            assert_adds_up(&node);
        });
    }

    #[test]
    fn cancelling_returns_a_partial_tree() {
        let home = Fixture::new();
        home.write("top/a.bin", 10_000);
        let cancel = CancelToken::new();
        cancel.cancel();
        let node =
            scan_tree_in_home(&home.path("top"), home.dir.path(), 2, 0, &cancel).expect("scan");
        assert!(node.partial);
        assert_eq!(node.file_count, 0);
    }

    #[test]
    fn gated_folders_are_not_entered_without_access() {
        let home = Fixture::new();
        home.write("Documents/secret.bin", 1_000_000);
        home.write("Library/Containers/com.x/data.bin", 1_000_000);
        home.write("Coding/a.bin", 10_000);
        let node = scan(home.dir.path(), home.dir.path(), 3, 0);
        assert!(node.partial);
        assert!(node.size < 1_000_000, "{}", node.size);
        let documents = node
            .children
            .iter()
            .find(|child| child.name == "Documents")
            .expect("still visible");
        assert!(documents.partial);
        assert_eq!(documents.size, 0);
        assert_adds_up(&node);
    }

    /// Every one of these raised a real TCC request when a home folder was walked.
    #[test]
    fn media_libraries_and_browser_profiles_are_not_entered_without_access() {
        let home = Fixture::new();
        for guarded in [
            "Music/Music/song.bin",
            "Pictures/Photos Library.photoslibrary/originals/a.bin",
            "Library/Application Support/Google/Chrome/Default/Cookies",
            "Library/Application Support/BraveSoftware/Brave-Browser/Default/Cookies",
        ] {
            home.write(guarded, 1_000_000);
        }
        home.write("Pictures/holiday.jpg", 10_000);
        home.write("Library/Application Support/Google/AndroidStudio/x", 10_000);
        let node = scan(home.dir.path(), home.dir.path(), 5, 0);
        assert!(node.partial);
        assert!(node.size < 1_000_000, "{}", node.size);
        assert_eq!(node.file_count, 2, "only the two unguarded files were seen");
    }

    #[test]
    fn gated_folders_are_entered_with_access() {
        let home = Fixture::new();
        home.grant_full_disk_access();
        home.write("Documents/a.bin", 1_000_000);
        let node = scan(home.dir.path(), home.dir.path(), 2, 0);
        assert!(!node.partial);
        assert!(node.size >= 1_000_000);
    }

    #[test]
    fn choosing_a_gated_folder_as_root_is_allowed() {
        let home = Fixture::new();
        home.write("Documents/sub/a.bin", 10_000);
        let node = scan(&home.path("Documents"), home.dir.path(), 2, 0);
        assert!(!node.partial);
        assert!(node.size >= 10_000);
    }

    #[test]
    fn home_is_worked_out_from_the_path() {
        let gate = Gate::FromPath;
        let home = gate.home_of(Path::new("/Users/ana/Documents"));
        assert_eq!(home, Some(PathBuf::from("/Users/ana")));
        let home = gate.home_of(Path::new(
            "/System/Volumes/Data/Users/ana/Library/Containers",
        ));
        assert_eq!(home, Some(PathBuf::from("/System/Volumes/Data/Users/ana")));
        assert_eq!(gate.home_of(Path::new("/Users")), None);
        assert_eq!(gate.home_of(Path::new("/opt/Documents")), None);
    }

    #[test]
    fn a_gated_name_outside_a_home_is_just_a_folder() {
        let home = Fixture::new();
        home.write("project/Documents/a.bin", 10_000);
        let node = scan(&home.path("project"), home.dir.path(), 2, 0);
        assert!(!node.partial);
    }
}
