//! Verbatim zstd archive writer: the single direct-write primitive both the
//! sweep and the PreCompact hook call (D-04, no staging).
//!
//! Each capture becomes a write-once generation (ARC-01) under
//! `base_dir/archive/<project>/<session-id>/<sub-path>/`, a numbered +
//! timestamped zstd file plus a rebuildable `meta.json` sidecar. The filesystem
//! is the source of truth: dedup and the next index are derived from the
//! generation files actually on disk, so a crash between the generation write
//! and the meta update, or a concurrent sweep/PreCompact for the same session,
//! never reuses an index or writes a duplicate.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::Config;

/// zstd level. Level 3 balances ratio against capture latency (PreCompact is
/// synchronous and blocks compaction).
const ZSTD_LEVEL: i32 = 3;

const META_FILE: &str = "meta.json";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Which write path produced a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A full-tree sweep captured a new-or-changed transcript.
    Sweep,
    /// The PreCompact hook snapshotted a transcript before compaction.
    Precompact,
}

impl Kind {
    /// Filename tag for this kind.
    pub fn tag(self) -> &'static str {
        match self {
            Kind::Sweep => "sweep",
            Kind::Precompact => "precompact",
        }
    }
}

/// Result of a `write_generation` call. Carries the source's sha256 and size in
/// both cases so the sweep can update its watermark whether or not a new
/// generation was written.
#[derive(Debug)]
pub enum Outcome {
    /// A new generation file was written.
    Written {
        path: PathBuf,
        sha256: String,
        size: u64,
    },
    /// An existing generation already holds these exact bytes; nothing written.
    Deduped { sha256: String, size: u64 },
}

impl Outcome {
    pub fn sha256(&self) -> &str {
        match self {
            Outcome::Written { sha256, .. } | Outcome::Deduped { sha256, .. } => sha256,
        }
    }
    pub fn size(&self) -> u64 {
        match self {
            Outcome::Written { size, .. } | Outcome::Deduped { size, .. } => *size,
        }
    }
    pub fn was_written(&self) -> bool {
        matches!(self, Outcome::Written { .. })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Meta {
    source_path: String,
    generations: Vec<GenerationMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GenerationMeta {
    filename: String,
    timestamp: String,
    kind: String,
    uncompressed_size: u64,
    sha256: String,
}

/// Write a verbatim generation of `source_path` under its session directory.
///
/// Reads the source's current bytes into memory, computes their sha256, and:
/// - returns `Deduped` without writing if any existing generation already holds
///   the same bytes (write once, ARC-01);
/// - otherwise writes a new generation at one past the highest index present on
///   disk, claiming the final name with an exclusive link so two writers cannot
///   take the same index, and records it in `meta.json`.
pub fn write_generation(
    config: &Config,
    project: &str,
    session_id: &str,
    sub_path: &str,
    source_path: &Path,
    kind: Kind,
) -> Result<Outcome> {
    let bytes = std::fs::read(source_path)
        .with_context(|| format!("reading source transcript {}", source_path.display()))?;
    let sha = sha256_hex(&bytes);
    let size = bytes.len() as u64;

    let dir = resolve_session_dir(config, project, session_id, sub_path)?;
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating session dir {}", dir.display()))?;

    let compressed = zstd::encode_all(&bytes[..], ZSTD_LEVEL).context("zstd compression failed")?;

    // Retry loop: the dedup scan and index derivation both read the on-disk
    // generation files, so a concurrent writer that took our index (link
    // collision) or wrote our exact bytes (dedup) is handled by re-scanning.
    loop {
        let gens = scan_generations(&dir)?;

        // Dedup: decompress each existing generation and compare sha. This does
        // not trust meta.json, so an orphaned generation (meta not yet updated)
        // still dedups.
        for gen in &gens {
            if generation_sha(&gen.path)? == sha {
                return Ok(Outcome::Deduped { sha256: sha, size });
            }
        }

        let next_index = gens.iter().map(|g| g.index).max().map(|m| m + 1).unwrap_or(0);
        let timestamp = utc_timestamp();
        // The exclusivity token is the INDEX ALONE: two concurrent writers that
        // pick the same index build the same final name, so exactly one wins the
        // hard_link and the other retries. Timestamp and kind live in meta.json,
        // not the filename, so they cannot make two writers' names diverge (the
        // reviewed D-04 sweep-vs-PreCompact collision).
        let filename = format!("{:04}.zst", next_index);
        let final_path = dir.join(&filename);

        // Write the fully-compressed bytes to an exclusively-created temp file
        // in the same dir, then claim the final name via an exclusive hard link
        // (fails if the name exists). The temp is created with create_new so it
        // can never truncate a pre-existing file - a stale temp orphan from a
        // PID-reused dead predecessor is reclaimed by unlink, never opened for
        // truncation, so a committed generation sharing its inode is never
        // mutated (ARC-01).
        let temp_path = write_exclusive_temp(&dir, &compressed)?;

        match std::fs::hard_link(&temp_path, &final_path) {
            Ok(()) => {
                let _ = std::fs::remove_file(&temp_path);
                update_meta(&dir, source_path, &filename, &timestamp, kind, size, &sha)?;
                return Ok(Outcome::Written {
                    path: final_path,
                    sha256: sha,
                    size,
                });
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                // Another writer took this index; drop the temp and retry.
                let _ = std::fs::remove_file(&temp_path);
                continue;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&temp_path);
                return Err(e).with_context(|| {
                    format!("claiming generation name {}", final_path.display())
                });
            }
        }
    }
}

/// Read and decompress an archived generation to its verbatim bytes (D-07). The
/// first public read path on this writer-only module: a query hit resolves via
/// `session.archive_refs` (JSON array of labels like `0000.zst` or
/// `subagents/agent-x/0000.zst`) plus `project` + `session_id` to
/// `archive_dir()/<project>/<session-id>/<gen_ref>`, decompressed.
///
/// Every path segment (project, session_id, and each `/`-split segment of
/// `gen_ref`) is validated with the same `check_segment` guard the writer uses,
/// and the resolved path is confirmed to stay under `archive_dir()` (the ARC-02
/// guard `resolve_session_dir` applies), so a crafted `archive_refs` label cannot
/// escape the archive tree.
pub fn read_generation(
    config: &Config,
    project: &str,
    session_id: &str,
    gen_ref: &str,
) -> Result<Vec<u8>> {
    let archive_dir = config.archive_dir();
    let mut path = archive_dir
        .join(check_segment(project)?)
        .join(check_segment(session_id)?);
    for seg in gen_ref.split('/') {
        path = path.join(check_segment(seg)?);
    }
    if !path.starts_with(&archive_dir) {
        bail!(
            "resolved archive path {} escapes archive_dir {} (ARC-02)",
            path.display(),
            archive_dir.display()
        );
    }

    let file = std::fs::File::open(&path)
        .with_context(|| format!("opening generation {}", path.display()))?;
    let bytes = zstd::decode_all(file)
        .with_context(|| format!("decompressing generation {}", path.display()))?;
    Ok(bytes)
}

/// Create a uniquely-named temp file in `dir`, exclusively, and write
/// `compressed` into it. Returns the temp path (the caller hard-links it into
/// place then removes it).
///
/// Exclusivity matters: `create_new` never truncates an existing file, so a
/// stale `.tmp-<mypid>-*` orphan left by a PID-reused dead predecessor cannot
/// be truncated in place - which would corrupt a committed generation that the
/// orphan is hard-linked to. Threads of this process never collide on a name
/// (the atomic counter is monotonic per process), so any pre-existing file at
/// our own pid-prefixed name is provably an orphan and safe to reclaim.
fn write_exclusive_temp(dir: &Path, compressed: &[u8]) -> Result<PathBuf> {
    loop {
        let temp_path = dir.join(format!(
            ".tmp-{}-{}.zst",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        match open_temp_exclusive(&temp_path) {
            Ok(mut f) => {
                f.write_all(compressed).with_context(|| {
                    format!("writing temp generation {}", temp_path.display())
                })?;
                return Ok(temp_path);
            }
            // Even after reclaiming the name still exists (extremely unlikely);
            // pick a fresh name.
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("creating temp generation {}", temp_path.display())
                });
            }
        }
    }
}

/// Open `path` for writing, exclusively (`create_new`). If it already exists it
/// is a stale orphan from a PID-reused dead predecessor: unlink it (safe - a
/// committed generation keeps its own separate name/link, so unlinking this
/// orphan name never drops the committed data) and create a fresh file. Never
/// opens an existing file for truncation, so a shared inode is never mutated.
fn open_temp_exclusive(path: &Path) -> std::io::Result<std::fs::File> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            std::fs::remove_file(path)?;
            OpenOptions::new().write(true).create_new(true).open(path)
        }
        Err(e) => Err(e),
    }
}

/// Resolve and ARC-02-guard the session directory under `archive_dir()`.
fn resolve_session_dir(
    config: &Config,
    project: &str,
    session_id: &str,
    sub_path: &str,
) -> Result<PathBuf> {
    let archive_dir = config.archive_dir();
    let mut dir = archive_dir.join(check_segment(project)?).join(check_segment(session_id)?);
    if !sub_path.is_empty() {
        for seg in sub_path.split('/') {
            dir = dir.join(check_segment(seg)?);
        }
    }
    if !dir.starts_with(&archive_dir) {
        bail!(
            "resolved archive path {} escapes archive_dir {} (ARC-02)",
            dir.display(),
            archive_dir.display()
        );
    }
    Ok(dir)
}

/// Reject a path segment that could escape the archive tree.
fn check_segment(seg: &str) -> Result<&str> {
    if seg.is_empty() || seg == "." || seg == ".." || seg.contains('/') || seg.contains('\\') {
        bail!("unsafe archive path segment {:?}", seg);
    }
    Ok(seg)
}

struct OnDiskGeneration {
    index: u64,
    path: PathBuf,
}

/// List generation files (`NNNN-...zst`) in a session dir, ignoring temp files
/// and the meta sidecar.
fn scan_generations(dir: &Path) -> Result<Vec<OnDiskGeneration>> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
    };
    for entry in rd {
        let entry = entry?;
        let name = entry.file_name();
        let name = match name.to_str() {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') || !name.ends_with(".zst") {
            continue;
        }
        // Generation files are named by index alone: `NNNN.zst`.
        let stem = &name[..name.len() - ".zst".len()];
        let index = match stem.parse::<u64>() {
            Ok(i) => i,
            Err(_) => continue,
        };
        out.push(OnDiskGeneration {
            index,
            path: entry.path(),
        });
    }
    Ok(out)
}

/// Decompress a generation file and return the sha256 of its uncompressed bytes.
fn generation_sha(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("opening generation {}", path.display()))?;
    let bytes = zstd::decode_all(file)
        .with_context(|| format!("decompressing generation {}", path.display()))?;
    Ok(sha256_hex(&bytes))
}

/// Append a generation entry to `meta.json`, writing via temp-file-plus-rename.
fn update_meta(
    dir: &Path,
    source_path: &Path,
    filename: &str,
    timestamp: &str,
    kind: Kind,
    uncompressed_size: u64,
    sha256: &str,
) -> Result<()> {
    let meta_path = dir.join(META_FILE);
    let mut meta: Meta = match std::fs::read(&meta_path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing {}", meta_path.display()))?,
        Err(e) if e.kind() == ErrorKind::NotFound => Meta::default(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", meta_path.display())),
    };
    meta.source_path = source_path.to_string_lossy().into_owned();
    if !meta.generations.iter().any(|g| g.filename == filename) {
        meta.generations.push(GenerationMeta {
            filename: filename.to_string(),
            timestamp: timestamp.to_string(),
            kind: kind.tag().to_string(),
            uncompressed_size,
            sha256: sha256.to_string(),
        });
    }
    meta.generations.sort_by(|a, b| a.filename.cmp(&b.filename));

    let json = serde_json::to_vec_pretty(&meta).context("serializing meta.json")?;
    let temp = dir.join(format!(
        ".meta-tmp-{}-{}.json",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&temp, &json)
        .with_context(|| format!("writing temp meta {}", temp.display()))?;
    std::fs::rename(&temp, &meta_path)
        .with_context(|| format!("renaming meta into place {}", meta_path.display()))?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// UTC timestamp `YYYYMMDDTHHMMSSZ` for the current wall clock.
fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    utc_timestamp_from_unix(secs)
}

fn utc_timestamp_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        y, m, d, hour, min, sec
    )
}

/// Days since 1970-01-01 to a (year, month, day) in the proleptic Gregorian
/// calendar (Howard Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn test_config(base: &Path) -> Config {
        Config::from_toml_str(&format!(
            "base_dir = {:?}\nidle_timeout_secs = 5\n",
            base
        ))
        .unwrap()
    }

    #[test]
    fn timestamp_matches_known_epoch() {
        // 2026-07-20T22:15:00Z == 1784printfree... verify via a known value.
        // 1721513700 = 2024-07-20T22:15:00Z (checked against date -u).
        assert_eq!(utc_timestamp_from_unix(1_721_513_700), "20240720T221500Z");
        assert_eq!(utc_timestamp_from_unix(0), "19700101T000000Z");
    }

    #[test]
    fn writes_generation_that_decompresses_byte_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        let src = tmp.path().join("source.jsonl");
        let content = b"{\"one\":1}\n{\"two\":2}\n";
        std::fs::write(&src, content).unwrap();

        let outcome =
            write_generation(&cfg, "proj", "sess", "", &src, Kind::Sweep).unwrap();
        let path = match &outcome {
            Outcome::Written { path, .. } => path.clone(),
            _ => panic!("expected Written"),
        };
        assert!(path.starts_with(cfg.archive_dir()), "path under archive_dir");

        // Decompress and compare to source bytes.
        let file = std::fs::File::open(&path).unwrap();
        let round = zstd::decode_all(file).unwrap();
        assert_eq!(round, content);

        // meta.json sha256 matches the source sha.
        let meta: Meta =
            serde_json::from_slice(&std::fs::read(path.parent().unwrap().join(META_FILE)).unwrap())
                .unwrap();
        assert_eq!(meta.generations.len(), 1);
        assert_eq!(meta.generations[0].sha256, sha256_hex(content));
        assert_eq!(meta.generations[0].uncompressed_size, content.len() as u64);
    }

    #[test]
    fn second_write_of_unchanged_bytes_dedups() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        let src = tmp.path().join("source.jsonl");
        std::fs::write(&src, b"same-bytes").unwrap();

        let first = write_generation(&cfg, "p", "s", "", &src, Kind::Sweep).unwrap();
        assert!(first.was_written());
        let second = write_generation(&cfg, "p", "s", "", &src, Kind::Sweep).unwrap();
        assert!(!second.was_written(), "second write should dedup");

        let dir = cfg.archive_dir().join("p").join("s");
        let count = scan_generations(&dir).unwrap().len();
        assert_eq!(count, 1, "only one generation file on disk");
    }

    #[test]
    fn orphaned_generation_absent_from_meta_neither_reused_nor_duplicated() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        let src = tmp.path().join("source.jsonl");
        std::fs::write(&src, b"orphan-bytes").unwrap();

        let first = write_generation(&cfg, "p", "s", "", &src, Kind::Sweep).unwrap();
        let first_path = match first {
            Outcome::Written { path, .. } => path,
            _ => panic!(),
        };
        let dir = first_path.parent().unwrap().to_path_buf();

        // Simulate a crash between the generation write and the meta update:
        // delete meta.json, leaving the generation file orphaned on disk.
        std::fs::remove_file(dir.join(META_FILE)).unwrap();

        // Re-run over unchanged bytes: must dedup via decompression (not reuse
        // index 0000, not write 0001).
        let again = write_generation(&cfg, "p", "s", "", &src, Kind::Sweep).unwrap();
        assert!(!again.was_written(), "orphaned generation must dedup");

        let gens = scan_generations(&dir).unwrap();
        assert_eq!(gens.len(), 1, "no duplicate generation written");
        assert_eq!(gens[0].index, 0, "existing index preserved");
    }

    #[test]
    fn nested_sub_path_files_under_session() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        let src = tmp.path().join("agent.jsonl");
        std::fs::write(&src, b"agent-bytes").unwrap();

        let outcome =
            write_generation(&cfg, "proj", "sess", "subagents/agent-x", &src, Kind::Sweep)
                .unwrap();
        let path = match outcome {
            Outcome::Written { path, .. } => path,
            _ => panic!(),
        };
        let expected_dir = cfg
            .archive_dir()
            .join("proj")
            .join("sess")
            .join("subagents")
            .join("agent-x");
        assert_eq!(path.parent().unwrap(), expected_dir);
    }

    #[test]
    fn concurrent_writers_never_share_an_index() {
        use std::sync::{Arc, Barrier};

        // Repeat to give the barrier-synchronized threads a real chance to both
        // compute the same next_index and race the exclusive claim. With the
        // index-only final name, one wins the hard_link and the other retries;
        // the buggy ts/kind-in-name variant would let both take index 0.
        for _ in 0..40 {
            let tmp = tempfile::tempdir().unwrap();
            let cfg = Arc::new(test_config(tmp.path()));
            let src_a = tmp.path().join("a.jsonl");
            let src_b = tmp.path().join("b.jsonl");
            // Distinct bytes so neither dedups: both must be written.
            std::fs::write(&src_a, b"writer-a-distinct-bytes").unwrap();
            std::fs::write(&src_b, b"writer-b-distinct-bytes").unwrap();
            let barrier = Arc::new(Barrier::new(2));

            let handles: Vec<_> = [(src_a, Kind::Sweep), (src_b, Kind::Precompact)]
                .into_iter()
                .map(|(src, kind)| {
                    let cfg = Arc::clone(&cfg);
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        write_generation(&cfg, "p", "s", "", &src, kind).unwrap()
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }

            let dir = cfg.archive_dir().join("p").join("s");
            let mut indices: Vec<u64> =
                scan_generations(&dir).unwrap().iter().map(|g| g.index).collect();
            indices.sort_unstable();
            assert_eq!(
                indices,
                vec![0, 1],
                "two distinct sequential indices, none shared"
            );
        }
    }

    #[test]
    fn concurrent_writers_same_bytes_dedup_to_one() {
        use std::sync::{Arc, Barrier};

        // Same bytes from two writers racing the same empty session: at most one
        // generation exists (one wins, the other dedups on rescan).
        for _ in 0..40 {
            let tmp = tempfile::tempdir().unwrap();
            let cfg = Arc::new(test_config(tmp.path()));
            let src = tmp.path().join("shared.jsonl");
            std::fs::write(&src, b"identical-bytes-from-both").unwrap();
            let barrier = Arc::new(Barrier::new(2));

            let handles: Vec<_> = [Kind::Sweep, Kind::Precompact]
                .into_iter()
                .map(|kind| {
                    let cfg = Arc::clone(&cfg);
                    let barrier = Arc::clone(&barrier);
                    let src = src.clone();
                    std::thread::spawn(move || {
                        barrier.wait();
                        write_generation(&cfg, "p", "s", "", &src, kind).unwrap()
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap();
            }

            let dir = cfg.archive_dir().join("p").join("s");
            let count = scan_generations(&dir).unwrap().len();
            assert_eq!(count, 1, "identical bytes must not produce two generations");
        }
    }

    #[test]
    fn stale_temp_orphan_does_not_corrupt_committed_generation() {
        use std::io::Write as _;

        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        let src = tmp.path().join("s.jsonl");
        let content = b"committed-write-once-ground-truth-bytes";
        std::fs::write(&src, content).unwrap();

        let committed = match write_generation(&cfg, "p", "s", "", &src, Kind::Sweep).unwrap() {
            Outcome::Written { path, .. } => path,
            _ => panic!("expected Written"),
        };
        let dir = committed.parent().unwrap().to_path_buf();

        // Simulate a PID-reused predecessor's orphan: a temp name hard-linked to
        // the committed generation's inode. A create+truncate open of this name
        // would mutate the committed generation through the shared inode
        // (an ARC-01 break). create_new must refuse to truncate it.
        let orphan = dir.join(format!(".tmp-{}-4242.zst", std::process::id()));
        std::fs::hard_link(&committed, &orphan).unwrap();

        let mut f = open_temp_exclusive(&orphan).unwrap();
        f.write_all(b"unrelated-fresh-temp-payload").unwrap();
        drop(f);

        // The committed generation is untouched: it still decompresses
        // byte-identical and the dedup scan (which would error on a truncated
        // .zst) still yields the original sha.
        let round = zstd::decode_all(std::fs::File::open(&committed).unwrap()).unwrap();
        assert_eq!(round, content, "committed generation must be uncorrupted");
        assert_eq!(generation_sha(&committed).unwrap(), sha256_hex(content));
    }

    #[test]
    fn read_generation_round_trips_written_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        let src = tmp.path().join("s.jsonl");
        let content = b"{\"type\":\"user\",\"uuid\":\"u1\"}\n{\"type\":\"assistant\",\"uuid\":\"u2\"}\n";
        std::fs::write(&src, content).unwrap();

        // Top-level generation: read it back by its `0000.zst` ref label.
        write_generation(&cfg, "proj", "sess", "", &src, Kind::Sweep).unwrap();
        let bytes = read_generation(&cfg, "proj", "sess", "0000.zst").unwrap();
        assert_eq!(bytes, content, "read_generation returns the verbatim source bytes");

        // Nested subagent generation: the label carries its sub-path.
        write_generation(&cfg, "proj", "sess", "subagents/agent-x", &src, Kind::Sweep).unwrap();
        let nested = read_generation(&cfg, "proj", "sess", "subagents/agent-x/0000.zst").unwrap();
        assert_eq!(nested, content, "a nested subagent ref label resolves and decompresses");
    }

    #[test]
    fn read_generation_rejects_escape_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        // A `..` segment in the ref label is rejected by check_segment.
        assert!(read_generation(&cfg, "p", "s", "../escape").is_err());
        assert!(read_generation(&cfg, "p", "s", "subagents/../../etc/passwd").is_err());
    }

    #[test]
    fn rejects_dotdot_sub_path_segment() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = test_config(tmp.path());
        let src = tmp.path().join("s.jsonl");
        std::fs::write(&src, b"x").unwrap();
        assert!(write_generation(&cfg, "p", "s", "../escape", &src, Kind::Sweep).is_err());
    }
}
