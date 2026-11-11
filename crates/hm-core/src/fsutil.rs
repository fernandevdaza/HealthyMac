//! Read-only filesystem helpers: measuring, hashing and APFS clone inspection.

use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{self, File, Metadata};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use anyhow::{bail, Context, Result};
use rayon::prelude::*;
use sha2::{Digest, Sha256};

/// Cooperative cancellation shared between the UI and running scans.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// The result of measuring a file or directory tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizeInfo {
    /// Bytes actually allocated on disk (`st_blocks * 512`). Sparse files count for what they
    /// really use, and hard-linked files are counted once. This is what the tree occupies, not what
    /// removing it would release: see [`SizeInfo::exclusive`].
    pub bytes: u64,
    /// The part of `bytes` also referenced from outside this tree, through APFS clones or hard
    /// links. pnpm, uv and bun build their folders this way, so this is often most of the total.
    pub shared: u64,
    /// Sum of apparent file sizes.
    pub logical: u64,
    pub files: u64,
    pub dirs: u64,
    /// Most recent modification time of any regular file in the tree.
    pub newest_modified: Option<SystemTime>,
    /// Directories that could not be read. Non-zero means the size is a lower bound, usually
    /// because Full Disk Access has not been granted.
    pub unreadable: u64,
}

impl SizeInfo {
    fn merge(mut self, other: SizeInfo) -> SizeInfo {
        self.bytes += other.bytes;
        self.shared += other.shared;
        self.logical += other.logical;
        self.files += other.files;
        self.dirs += other.dirs;
        self.unreadable += other.unreadable;
        self.newest_modified = match (self.newest_modified, other.newest_modified) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self
    }

    pub fn is_empty(&self) -> bool {
        self.files == 0 && self.bytes == 0
    }

    /// Bytes that removing this tree would really release.
    ///
    /// A lower bound: blocks shared between clones are only freed when the last clone goes, so
    /// removing a `.venv` and the uv cache it was cloned from together releases more than the sum
    /// of their two exclusive sizes.
    pub fn exclusive(&self) -> u64 {
        self.bytes.saturating_sub(self.shared)
    }
}

/// A file with several hard links, tracked until the walk ends: only then is it known whether every
/// link was inside the measured tree.
struct HardLink {
    links: u64,
    seen: u64,
    private: u64,
}

type HardLinks = Mutex<HashMap<(u64, u64), HardLink>>;

/// Hard-linked files are first assumed fully shared. Those whose links were all found inside the
/// tree have their private bytes given back here.
fn settle_hard_links(info: &mut SizeInfo, links: HardLinks) {
    let links = links.into_inner().unwrap_or_default();
    for link in links.into_values() {
        if link.seen >= link.links {
            info.shared = info.shared.saturating_sub(link.private);
        }
    }
}

/// Recursively measures `path` in parallel.
///
/// Never follows symlinks and never crosses onto another volume. Returns a default (empty)
/// [`SizeInfo`] when `path` does not exist. Cancelling returns the partial total.
pub fn measure(path: &Path, cancel: &CancelToken) -> SizeInfo {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return SizeInfo::default();
    };
    let links = Mutex::new(HashMap::new());
    let mut info = walk(path, &meta, meta.dev(), &links, cancel);
    settle_hard_links(&mut info, links);
    info
}

/// Measures several paths, counting hard links shared between them only once.
pub fn measure_many(paths: &[PathBuf], cancel: &CancelToken) -> SizeInfo {
    let links = Mutex::new(HashMap::new());
    let mut info = paths
        .par_iter()
        .map(|path| match fs::symlink_metadata(path) {
            Ok(meta) => walk(path, &meta, meta.dev(), &links, cancel),
            Err(_) => SizeInfo::default(),
        })
        .reduce(SizeInfo::default, SizeInfo::merge);
    settle_hard_links(&mut info, links);
    info
}

fn walk(
    path: &Path,
    meta: &Metadata,
    root_dev: u64,
    links: &HardLinks,
    cancel: &CancelToken,
) -> SizeInfo {
    let mut info = account(path, meta, links);
    if !meta.is_dir() || cancel.is_cancelled() {
        return info;
    }
    let entries: Vec<_> = match fs::read_dir(path) {
        Ok(entries) => entries.flatten().collect(),
        Err(_) => {
            info.unreadable += 1;
            return info;
        }
    };
    let children = entries
        .par_iter()
        .map(|entry| match entry.path().symlink_metadata() {
            Ok(child) if child.dev() == root_dev => {
                walk(&entry.path(), &child, root_dev, links, cancel)
            }
            _ => SizeInfo::default(),
        })
        .reduce(SizeInfo::default, SizeInfo::merge);
    info.merge(children)
}

fn account(path: &Path, meta: &Metadata, links: &HardLinks) -> SizeInfo {
    let mut info = SizeInfo::default();
    let allocated = meta.blocks() * 512;
    if meta.is_dir() {
        info.dirs = 1;
        info.bytes = allocated;
        return info;
    }
    info.files = 1;
    // Only regular files can be cloned, and a symlink's own metadata must not be followed.
    let private = if meta.is_file() {
        exclusive_size(path, allocated)
    } else {
        allocated
    };
    if meta.nlink() > 1 {
        let mut links = match links.lock() {
            Ok(links) => links,
            Err(poisoned) => poisoned.into_inner(),
        };
        let link = links.entry((meta.dev(), meta.ino())).or_insert(HardLink {
            links: meta.nlink(),
            seen: 0,
            private,
        });
        link.seen += 1;
        if link.seen > 1 {
            return info;
        }
        // Assume another link lives elsewhere. `settle_hard_links` corrects this at the end.
        info.shared = allocated;
    } else {
        info.shared = allocated - private;
    }
    info.bytes = allocated;
    info.logical = meta.len();
    info.newest_modified = meta.modified().ok();
    info
}

/// Immediate children of `dir`, or an empty list if it cannot be read.
pub fn children(dir: &Path) -> Vec<PathBuf> {
    match fs::read_dir(dir) {
        Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
        Err(_) => Vec::new(),
    }
}

/// Immediate children of `dir`, telling "could not read it" apart from "it is empty". Without Full
/// Disk Access, an unreadable folder must not be reported as an empty one.
pub fn try_children(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    Ok(fs::read_dir(dir)?
        .flatten()
        .map(|entry| entry.path())
        .collect())
}

/// `true` if `path` is a real directory. Symlinks do not count.
pub fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|meta| meta.is_dir())
        .unwrap_or(false)
}

/// Whether this process has Full Disk Access.
///
/// Probes folders that macOS refuses silently without it, so this never raises a permission prompt.
/// Use it to gate anything that *would* prompt: `~/Documents`, `~/Desktop`, `~/Downloads`,
/// `~/Library/Containers/*` and `~/Library/Group Containers/*`.
pub fn has_full_disk_access(home: &Path) -> bool {
    let mut probed_any = false;
    for relative in ["Library/Safari", "Library/Mail", "Library/Messages"] {
        let path = home.join(relative);
        if !path.exists() {
            continue;
        }
        probed_any = true;
        if fs::read_dir(&path).is_err() {
            return false;
        }
    }
    probed_any
}

/// SHA-256 of a file as lowercase hex. Returns an error if cancelled.
pub fn sha256_file(path: &Path, cancel: &CancelToken) -> Result<String> {
    let mut file = File::open(path)
        .with_context(|| crate::tf!("no se pudo abrir {}", "could not open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if cancel.is_cancelled() {
            bail!("cancelado");
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

// Values from <sys/attr.h>. `libc` does not export the extended common attributes.
const FSOPT_ATTR_CMN_EXTENDED: u32 = 0x0000_0020;
const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x0000_0008;
const ATTR_CMNEXT_CLONEID: u32 = 0x0000_0100;
const ATTR_CMNEXT_CLONE_REFCNT: u32 = 0x0000_1000;

/// Asks for extended common attributes, filling `reply`, whose first field must be the `u32` length
/// the kernel writes. Returns how many bytes it reports, or `None` if the call failed.
fn read_extended_attrs<R>(path: &Path, attrs: u32, reply: &mut R) -> Option<usize> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut list: libc::attrlist = unsafe { std::mem::zeroed() };
    list.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    // With FSOPT_ATTR_CMN_EXTENDED the `forkattr` field carries the ATTR_CMNEXT_* bits.
    list.forkattr = attrs;
    let reply_ptr: *mut R = reply;
    // SAFETY: `c_path` is NUL-terminated, and `list` and `reply` are live for the whole call with
    // the size passed alongside them.
    let result = unsafe {
        libc::getattrlist(
            c_path.as_ptr(),
            (&raw mut list).cast(),
            reply_ptr.cast(),
            std::mem::size_of::<R>(),
            libc::FSOPT_NOFOLLOW | FSOPT_ATTR_CMN_EXTENDED,
        )
    };
    if result != 0 {
        return None;
    }
    // SAFETY: `R` starts with the length, as documented above, and the kernel has just written it.
    Some(unsafe { reply_ptr.cast::<u32>().read_unaligned() } as usize)
}

fn extended_attr_u64(path: &Path, attr: u32) -> Option<u64> {
    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        value: u64,
    }
    let mut reply = Reply {
        length: 0,
        value: 0,
    };
    // On a volume without this attribute the call can succeed and simply omit it, which must not
    // read as a value of 0.
    let length = read_extended_attrs(path, attr, &mut reply)?;
    (length >= std::mem::size_of::<Reply>()).then_some(reply.value)
}

/// What removing this regular file would really release, given the blocks it occupies.
pub fn exclusive_size(path: &Path, allocated: u64) -> u64 {
    // The kernel returns attributes in ascending bit order.
    #[repr(C, packed(4))]
    struct Reply {
        length: u32,
        private: u64,
        clones: u32,
    }
    let mut reply = Reply {
        length: 0,
        private: 0,
        clones: 0,
    };
    let length = read_extended_attrs(
        path,
        ATTR_CMNEXT_PRIVATESIZE | ATTR_CMNEXT_CLONE_REFCNT,
        &mut reply,
    );
    if length != Some(std::mem::size_of::<Reply>()) {
        return private_size(path).map_or(allocated, |private| private.min(allocated));
    }
    // Private bytes lag behind on a file just written, whose blocks aren't assigned yet, and would
    // read as 0. With no clones there is nothing to share, so it doesn't need them.
    if reply.clones <= 1 {
        allocated
    } else {
        let private = reply.private;
        private.min(allocated)
    }
}

/// APFS clone id. Two files with the same non-zero id share their blocks, so they are already
/// deduplicated and "removing" one would free nothing.
pub fn clone_id(path: &Path) -> Option<u64> {
    extended_attr_u64(path, ATTR_CMNEXT_CLONEID).filter(|id| *id != 0)
}

/// Bytes that belong to this file alone: what deleting it would really free once APFS clones are
/// taken into account.
pub fn private_size(path: &Path) -> Option<u64> {
    extended_attr_u64(path, ATTR_CMNEXT_PRIVATESIZE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write(path: &Path, bytes: usize) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; bytes]).unwrap();
    }

    #[test]
    fn measures_tree() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/one.bin"), 10_000);
        write(&dir.path().join("a/b/two.bin"), 20_000);
        let info = measure(dir.path(), &CancelToken::new());
        assert_eq!(info.files, 2);
        assert_eq!(info.logical, 30_000);
        assert!(info.bytes >= 30_000);
        assert!(info.newest_modified.is_some());
    }

    #[test]
    fn missing_path_is_empty() {
        let info = measure(Path::new("/definitely/not/here"), &CancelToken::new());
        assert!(info.is_empty());
    }

    #[test]
    fn does_not_follow_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("big.bin"), 1_000_000);
        symlink(outside.path(), dir.path().join("link")).unwrap();
        let info = measure(dir.path(), &CancelToken::new());
        assert_eq!(
            info.logical,
            fs::symlink_metadata(dir.path().join("link")).unwrap().len()
        );
    }

    #[test]
    fn hard_links_count_once() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.bin"), 100_000);
        fs::hard_link(dir.path().join("a.bin"), dir.path().join("b.bin")).unwrap();
        let info = measure(dir.path(), &CancelToken::new());
        assert_eq!(info.logical, 100_000);
        assert_eq!(info.files, 2);
    }

    /// Writes a file big enough that clone behaviour is unambiguous.
    fn big_file(path: &Path) {
        write(path, 1024 * 1024);
    }

    fn clone(source: &Path, destination: &Path) {
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let source = CString::new(source.as_os_str().as_bytes()).unwrap();
        let destination = CString::new(destination.as_os_str().as_bytes()).unwrap();
        // SAFETY: both pointers come from `CString`s that outlive the call.
        assert_eq!(
            unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) },
            0
        );
    }

    #[test]
    fn an_ordinary_file_is_entirely_exclusive() {
        let dir = tempfile::tempdir().unwrap();
        big_file(&dir.path().join("tree/a.bin"));
        let info = measure(&dir.path().join("tree"), &CancelToken::new());
        assert!(info.bytes >= 1024 * 1024);
        assert_eq!(info.exclusive(), info.bytes);
    }

    #[test]
    fn a_file_just_written_counts_in_full_even_before_its_blocks_settle() {
        // Private bytes read 0 for a moment after writing, so this failed intermittently.
        let dir = tempfile::tempdir().unwrap();
        for index in 0..200 {
            let path = dir.path().join(format!("fresh-{index}.bin"));
            fs::write(&path, vec![1u8; 8192]).unwrap();
            let allocated = fs::symlink_metadata(&path).unwrap().blocks() * 512;
            assert_eq!(exclusive_size(&path, allocated), allocated, "file {index}");
        }
    }

    #[test]
    fn deleting_the_last_other_clone_makes_a_file_exclusive_again() {
        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("a.bin");
        let copy = dir.path().join("b.bin");
        big_file(&original);
        clone(&original, &copy);
        let allocated = fs::symlink_metadata(&copy).unwrap().blocks() * 512;
        assert_eq!(exclusive_size(&copy, allocated), 0);
        fs::remove_file(&original).unwrap();
        assert_eq!(exclusive_size(&copy, allocated), allocated);
    }

    #[test]
    fn a_clone_of_something_outside_the_tree_is_not_reclaimable() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store/pkg.bin");
        big_file(&store);
        clone(&store, &dir.path().join("project/node_modules/pkg.bin"));
        let info = measure(&dir.path().join("project"), &CancelToken::new());
        assert!(info.bytes >= 1024 * 1024, "it still occupies its full size");
        assert_eq!(info.exclusive(), 0, "but removing it would free nothing");
    }

    #[test]
    fn a_hard_link_to_something_outside_the_tree_is_not_reclaimable() {
        let dir = tempfile::tempdir().unwrap();
        let store = dir.path().join("store/pkg.bin");
        big_file(&store);
        fs::create_dir_all(dir.path().join("project")).unwrap();
        fs::hard_link(&store, dir.path().join("project/pkg.bin")).unwrap();
        let info = measure(&dir.path().join("project"), &CancelToken::new());
        assert!(info.bytes >= 1024 * 1024);
        assert_eq!(info.exclusive(), 0);
    }

    #[test]
    fn hard_links_all_inside_the_tree_are_reclaimable_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("tree/a.bin");
        big_file(&first);
        fs::hard_link(&first, dir.path().join("tree/b.bin")).unwrap();
        let info = measure(&dir.path().join("tree"), &CancelToken::new());
        assert_eq!(info.exclusive(), info.bytes);
        assert!(info.exclusive() >= 1024 * 1024);
        assert!(info.exclusive() < 2 * 1024 * 1024);
    }

    #[test]
    fn measure_many_sees_links_across_its_paths() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("one/a.bin");
        big_file(&first);
        fs::create_dir_all(dir.path().join("two")).unwrap();
        fs::hard_link(&first, dir.path().join("two/b.bin")).unwrap();
        let paths = [dir.path().join("one"), dir.path().join("two")];
        let info = measure_many(&paths, &CancelToken::new());
        assert!(info.exclusive() >= 1024 * 1024);
    }

    #[test]
    fn hash_matches_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path, &CancelToken::new()).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hash_honours_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        fs::write(&path, b"abc").unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(sha256_file(&path, &cancel).is_err());
    }
}
