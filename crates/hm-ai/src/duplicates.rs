//! The same bytes stored more than once, usually by different runtimes, and the lossless way out:
//! APFS clones, which keep every path valid and share the blocks.
//!
//! Hashing a 40 GB file takes a while, so it's the last resort. In order:
//!
//! 1. Only files of exactly the same size on the same volume can be duplicates.
//! 2. Ollama's blob names and Hugging Face's 64-digit etags already are the SHA-256.
//! 3. Copies already sharing blocks are one copy for this purpose.
//! 4. Files of equal size are often not alike: the shards of one model commonly match to the byte.
//!    A sample of each decides that for the cost of a few reads, before any full hash is started.
//! 5. A full hash is remembered in the hash cache.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::hash::Hasher;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use hm_core::fsutil::{self, clone_id, private_size};
use hm_core::{t, tf, Action, Area, Finding, Recovery, Risk, SafetyPolicy, ScanContext};
use rayon::prelude::*;

use crate::files::{name_of, FileRec};
use crate::hashcache::HashCache;
use crate::inventory::Entry;
use crate::types::{DuplicateGroup, RuntimeKind};

pub(crate) const SCANNER_ID: &str = "ai.duplicates";

/// Below this it's a tokenizer or a config, not weights, and not worth a line in the list.
#[cfg(not(test))]
pub(crate) const MIN_DUPLICATE_BYTES: u64 = 16 * 1024 * 1024;
#[cfg(test)]
pub(crate) const MIN_DUPLICATE_BYTES: u64 = 4096;

const SAMPLE_BYTES: usize = 64 * 1024;

#[derive(Debug, Default)]
pub(crate) struct DuplicateReport {
    pub groups: Vec<DuplicateGroup>,
    pub findings: Vec<Finding>,
    pub hashed_files: u64,
    pub hashed_bytes: u64,
}

#[derive(Clone, Debug)]
struct Copy {
    record: FileRec,
    family: Family,
    holders: BTreeSet<(RuntimeKind, String)>,
}

/// Files in one family share all their blocks, so they count as a single copy. A file not known to
/// be a clone of anything is its own family.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Family {
    Clones(u64),
    Alone(u64),
}

/// A cheap fingerprint from the start, middle and end. Equal ones still need a full hash.
fn sample(path: &Path, len: u64) -> Option<u64> {
    let mut file = File::open(path).ok()?;
    let mut hasher = DefaultHasher::new();
    let mut buffer = vec![0u8; SAMPLE_BYTES];
    let window = SAMPLE_BYTES as u64;
    let offsets = [
        0,
        (len / 2).saturating_sub(window / 2),
        len.saturating_sub(window),
    ];
    for offset in offsets {
        file.seek(SeekFrom::Start(offset)).ok()?;
        let mut filled = 0;
        while filled < buffer.len() {
            match file.read(&mut buffer[filled..]).ok()? {
                0 => break,
                read => filled += read,
            }
        }
        hasher.write(&buffer[..filled]);
    }
    Some(hasher.finish())
}

struct Hashing<'a> {
    ctx: &'a ScanContext,
    cache: Mutex<HashCache>,
    files: AtomicU64,
    bytes: AtomicU64,
}

impl Hashing<'_> {
    fn digest(&self, record: &FileRec) -> Option<String> {
        if let Some(known) = self
            .cache
            .lock()
            .ok()
            .and_then(|mut cache| cache.get(record))
        {
            return Some(known);
        }
        let digest = fsutil::sha256_file(&record.path, &self.ctx.cancel).ok()?;
        self.files.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(record.len, Ordering::Relaxed);
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(record, digest.clone());
        }
        Some(digest)
    }
}

/// Decides which families in a bucket of same-sized files hold the same bytes, reading as little
/// as possible. Returns a digest per family it could establish.
fn resolve_digests(bucket: &[Copy], hashing: &Hashing<'_>) -> HashMap<Family, String> {
    let mut members: BTreeMap<Family, &Copy> = BTreeMap::new();
    let mut digests: HashMap<Family, String> = HashMap::new();
    for copy in bucket {
        members.entry(copy.family).or_insert(copy);
        if let Some(known) = &copy.record.sha256 {
            digests.insert(copy.family, known.clone());
        }
    }
    let any_unknown = members.keys().any(|family| !digests.contains_key(family));
    if !any_unknown || members.len() < 2 {
        return digests;
    }

    let mut by_sample: HashMap<u64, Vec<Family>> = HashMap::new();
    for (family, copy) in &members {
        if let Some(fingerprint) = sample(&copy.record.path, copy.record.len) {
            by_sample.entry(fingerprint).or_default().push(*family);
        }
    }
    let to_hash: Vec<(Family, &FileRec)> = by_sample
        .values()
        .filter(|families| families.len() >= 2)
        .flatten()
        .filter(|family| !digests.contains_key(*family))
        .filter_map(|family| members.get(family).map(|copy| (*family, &copy.record)))
        .collect();
    let hashed: Vec<(Family, String)> = to_hash
        .par_iter()
        .filter_map(|(family, record)| Some((*family, hashing.digest(record)?)))
        .collect();
    digests.extend(hashed);
    digests
}

/// Prefers a copy that many others already share blocks with, since that's the least work and the
/// most sharing. Then a content-addressed store's copy, which nothing rewrites by design. Then the
/// oldest, because clones take the modification time of the source. The path makes it deterministic.
fn choose_keep<'a>(
    eligible: &[&'a Copy],
    family_sizes: &HashMap<Family, usize>,
) -> Option<&'a Copy> {
    eligible.iter().copied().min_by(|a, b| {
        let size = |copy: &Copy| family_sizes.get(&copy.family).copied().unwrap_or(1);
        size(b)
            .cmp(&size(a))
            .then(b.record.sha256.is_some().cmp(&a.record.sha256.is_some()))
            .then(a.record.mtime_ns.cmp(&b.record.mtime_ns))
            .then(a.record.path.cmp(&b.record.path))
    })
}

struct Plan {
    keep: Option<PathBuf>,
    duplicates: Vec<PathBuf>,
    reclaimable: u64,
}

/// Works out what replacing copies would really release. Replacing a copy frees only blocks nothing
/// else holds: a lone file's private size, and for a family of clones, its blocks once, and only if
/// all of its members can be replaced. Otherwise whichever remains keeps them all allocated.
fn plan(copies: &[Copy], policy: &SafetyPolicy) -> Plan {
    let eligible: Vec<&Copy> = copies
        .iter()
        .filter(|copy| policy.check_removal(&copy.record.path).is_ok())
        .collect();
    let mut family_sizes: HashMap<Family, usize> = HashMap::new();
    for copy in copies {
        *family_sizes.entry(copy.family).or_default() += 1;
    }
    let Some(keep) = choose_keep(&eligible, &family_sizes) else {
        return Plan {
            keep: None,
            duplicates: Vec::new(),
            reclaimable: 0,
        };
    };

    let mut duplicates = Vec::new();
    let mut reclaimable = 0;
    let families: BTreeSet<Family> = copies.iter().map(|copy| copy.family).collect();
    for family in families {
        if family == keep.family {
            continue;
        }
        let in_family: Vec<&Copy> = copies.iter().filter(|copy| copy.family == family).collect();
        let replaceable = in_family.iter().all(|copy| {
            eligible
                .iter()
                .any(|other| other.record.path == copy.record.path)
        });
        let Some(first) = in_family.first().filter(|_| replaceable) else {
            continue;
        };
        let freed = if in_family.len() == 1 {
            private_size(&first.record.path).unwrap_or(first.record.bytes)
        } else {
            first.record.bytes
        };
        if freed > 0 {
            reclaimable += freed;
            duplicates.extend(in_family.iter().map(|copy| copy.record.path.clone()));
        }
    }
    duplicates.sort();
    Plan {
        keep: Some(keep.record.path.clone()),
        duplicates,
        reclaimable,
    }
}

fn build_finding(
    id_key: &str,
    group: &DuplicateGroup,
    holders: &BTreeSet<(RuntimeKind, String)>,
    keep: &Path,
    duplicates: Vec<PathBuf>,
) -> Finding {
    let file_name = name_of(keep);
    // Two runtimes often use one name, and who holds a copy is the whole point.
    let names = holders
        .iter()
        .map(|(runtime, name)| format!("{name} ({})", runtime.label()))
        .collect::<Vec<_>>()
        .join(" · ");
    let copies = group.paths.len();
    let title = tf!(
        "{copies} copias idénticas: {names}",
        "{copies} identical copies: {names}"
    );
    Finding::new(SCANNER_ID, Area::LocalAi, id_key, title)
        .group(t!("Deduplicación", "Deduplication"))
        .subtitle(keep.display().to_string())
        .explanation(tf!(
            "Estas {copies} rutas contienen exactamente los mismos bytes ({file_name}). Al deduplicar \
             se convierten en clones APFS: cada programa sigue encontrando su archivo donde estaba y \
             solo se liberan los bloques repetidos. HealthyMac vuelve a comparar el contenido antes de \
             tocar nada.",
            "These {copies} paths hold exactly the same bytes ({file_name}). Deduplicating turns them \
             into APFS clones: every app still finds its file where it was, and only the repeated \
             blocks are freed. HealthyMac compares the contents again before touching anything."
        ))
        .size(group.reclaimable)
        .items(duplicates.len() as u64)
        .risk(Risk::Safe)
        .recovery(Recovery::NothingLost)
        .selected(true)
        .action(Action::CloneDedupe {
            keep: keep.to_path_buf(),
            duplicates,
        })
        .tag("deduplicación")
}

pub(crate) fn find(ctx: &ScanContext, entries: &[Entry]) -> DuplicateReport {
    let mut unique: HashMap<(u64, u64), Copy> = HashMap::new();
    for (model, files) in entries {
        for file in files.iter().filter(|file| file.len >= MIN_DUPLICATE_BYTES) {
            let copy = unique.entry(file.key()).or_insert_with(|| Copy {
                record: file.clone(),
                family: Family::Alone(file.ino),
                holders: BTreeSet::new(),
            });
            copy.holders.insert((model.runtime, model.name.clone()));
            if copy.record.sha256.is_none() {
                copy.record.sha256.clone_from(&file.sha256);
            }
        }
    }

    let mut buckets: BTreeMap<(u64, u64), Vec<Copy>> = BTreeMap::new();
    for copy in unique.into_values() {
        buckets
            .entry((copy.record.dev, copy.record.len))
            .or_default()
            .push(copy);
    }

    let hashing = Hashing {
        ctx,
        cache: Mutex::new(HashCache::load(&ctx.home)),
        files: AtomicU64::new(0),
        bytes: AtomicU64::new(0),
    };
    let policy = SafetyPolicy::new(&ctx.home, &ctx.config.excluded_paths);
    let mut report = DuplicateReport::default();
    let mut used_keys: HashSet<String> = HashSet::new();

    for (_, mut bucket) in buckets {
        if bucket.len() < 2 || ctx.is_cancelled() {
            continue;
        }
        bucket.sort_by(|a, b| a.record.path.cmp(&b.record.path));
        for copy in &mut bucket {
            if let Some(id) = clone_id(&copy.record.path) {
                copy.family = Family::Clones(id);
            }
        }
        let digests = resolve_digests(&bucket, &hashing);

        // Group families by digest. Families with no digest are alone unless they have members.
        let mut by_digest: BTreeMap<String, Vec<Family>> = BTreeMap::new();
        for (family, digest) in &digests {
            by_digest.entry(digest.clone()).or_default().push(*family);
        }
        let mut sets: Vec<(String, Vec<Copy>)> = Vec::new();
        let mut placed: BTreeSet<Family> = BTreeSet::new();
        for (digest, families) in by_digest {
            placed.extend(families.iter().copied());
            let copies: Vec<Copy> = bucket
                .iter()
                .filter(|copy| families.contains(&copy.family))
                .cloned()
                .collect();
            sets.push((digest, copies));
        }
        // Clones of each other with no digest known are still the same bytes.
        let remaining: BTreeSet<Family> = bucket
            .iter()
            .map(|copy| copy.family)
            .filter(|family| !placed.contains(family))
            .collect();
        for family in remaining {
            let copies: Vec<Copy> = bucket
                .iter()
                .filter(|copy| copy.family == family)
                .cloned()
                .collect();
            sets.push((String::new(), copies));
        }

        for (digest, copies) in sets {
            if copies.len() < 2 {
                continue;
            }
            let plan = plan(&copies, &policy);
            let holders: BTreeSet<(RuntimeKind, String)> = copies
                .iter()
                .flat_map(|copy| copy.holders.iter().cloned())
                .collect();
            let names: BTreeSet<String> = holders.iter().map(|(_, name)| name.clone()).collect();
            let runtimes: BTreeSet<RuntimeKind> =
                holders.iter().map(|(runtime, _)| *runtime).collect();
            let mut paths: Vec<PathBuf> =
                copies.iter().map(|copy| copy.record.path.clone()).collect();
            paths.sort_by_key(|path| Some(path) != plan.keep.as_ref());

            let base_key = if digest.is_empty() {
                format!("clones:{}", paths[0].display())
            } else {
                digest.clone()
            };
            let mut key = base_key.clone();
            let mut suffix = 2;
            while !used_keys.insert(key.clone()) {
                key = format!("{base_key}:{suffix}");
                suffix += 1;
            }

            let group = DuplicateGroup {
                sha256: digest,
                file_size: copies[0].record.len,
                reclaimable: plan.reclaimable,
                paths,
                model_names: names.into_iter().collect(),
                runtimes: runtimes.into_iter().collect(),
                finding_id: format!("{SCANNER_ID}:{key}"),
                keep: plan.keep.clone(),
            };
            if let Some(keep) = plan.keep.as_deref().filter(|_| plan.reclaimable > 0) {
                report
                    .findings
                    .push(build_finding(&key, &group, &holders, keep, plan.duplicates));
            }
            report.groups.push(group);
        }
    }

    report.groups.sort_by(|a, b| {
        b.reclaimable
            .cmp(&a.reclaimable)
            .then(a.finding_id.cmp(&b.finding_id))
    });
    report
        .findings
        .sort_by(|a, b| b.size.cmp(&a.size).then(a.id.cmp(&b.id)));
    report.hashed_files = hashing.files.load(Ordering::Relaxed);
    report.hashed_bytes = hashing.bytes.load(Ordering::Relaxed);
    if let Ok(mut cache) = hashing.cache.into_inner() {
        cache.save(!ctx.is_cancelled());
    }
    report
}
