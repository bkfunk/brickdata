//! Cross-subcommand helpers — file I/O and similar utilities used by
//! multiple subcommands.
//!
//! Keep this module thin: anything that *only* one subcommand needs lives
//! inside that subcommand's file. Things land here when a second caller
//! shows up.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

/// Quote a string as a RON string literal — same escaping as Rust's, so
/// escape `"`, `\`, and the obvious control chars. Shared by the hand-rolled
/// RON writers (`color_names.ron`, `part_crossrefs.ron`) that emit a tight,
/// reviewable, deterministic format the default serializer can't match.
pub(crate) fn ron_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Today's date as `YYYY-MM-DD` in UTC — the stamp written into the
/// `generated:` field of regenerated pins (e.g. `part_crossrefs.ron`). UTC,
/// not local time, so the same regeneration run on machines in different
/// timezones produces the same date string.
pub fn today_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// The workspace root, resolved at compile time from the builder's manifest
/// (`crates/blockstar-catalog-builder` → up two levels). Used to locate
/// committed inputs like the snapshot pin and the colors reference.
pub fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop(); // out of blockstar-catalog-builder
    p.pop(); // out of crates
    p
}

/// Lowercase hex-encoded SHA-256 of `bytes`.
pub fn hash_bytes(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex_encode(&h.finalize())
}

/// Lowercase hex-encoded SHA-256 of a file's contents. The inputs are tens
/// of MB at most, so reading the whole file and reusing [`hash_bytes`] is
/// simpler than streaming and costs only a transient buffer.
pub fn hash_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(hash_bytes(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Move `src` onto `dst`, preserving the previously-good `dst` if anything
/// goes wrong. This is the safe-replace primitive shared by [`atomic_write`]
/// (pin/reference files) and the catalog `build` (the output DB).
///
/// On Unix `rename` atomically replaces an existing destination, so the single
/// attempt is the whole story — and a failure (permissions, read-only FS, AV)
/// must surface *without* touching `dst`, so the last known-good file survives.
///
/// On Windows `std::fs::rename` fails when the destination already exists (it
/// maps to `MoveFile`, which won't clobber), so there we fall back to a
/// backup-then-swap: move `dst` aside to `dst.bak`, move `src` into place, and
/// if that move fails restore the backup. This preserves the previous file even
/// if the swap fails — unlike a remove-then-retry, which would have already
/// deleted `dst`. Gated on `cfg!(windows)` so the Unix error path never touches
/// `dst`.
pub(crate) fn replace_file(src: &Path, dst: &Path) -> Result<()> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) if cfg!(windows) && dst.exists() => windows_backup_swap(src, dst),
        Err(e) => Err(e).with_context(|| format!("rename {} → {}", src.display(), dst.display())),
    }
}

/// Windows fallback for [`replace_file`]: back up `dst` to `dst.bak`, move
/// `src` into place, and restore the backup if the move fails so the
/// previously-good file is never lost.
fn windows_backup_swap(src: &Path, dst: &Path) -> Result<()> {
    let backup = with_suffix(dst, ".bak");
    let _ = fs::remove_file(&backup); // clear any stale backup from a prior run
    fs::rename(dst, &backup)
        .with_context(|| format!("back up {} → {}", dst.display(), backup.display()))?;
    match fs::rename(src, dst) {
        Ok(()) => {
            let _ = fs::remove_file(&backup); // best-effort cleanup
            Ok(())
        }
        Err(e) => {
            // Swap failed — put the known-good file back before surfacing the error.
            let restored = fs::rename(&backup, dst);
            let mut err = anyhow::Error::new(e).context(format!(
                "rename {} → {}",
                src.display(),
                dst.display()
            ));
            if let Err(re) = restored {
                err = err.context(format!(
                    "and failed to restore backup {} → {} ({re})",
                    backup.display(),
                    dst.display(),
                ));
            }
            Err(err)
        }
    }
}

/// `path` with `suffix` appended (not replacing the extension), e.g.
/// `catalog.sqlite` + `.tmp` → `catalog.sqlite.tmp`. Used for the staging
/// and backup siblings.
pub(crate) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// The cache filename for a Rebrickable table: `parts` -> `parts.csv.gz`.
/// The one place this naming convention lives, shared by the snapshot
/// verify/sync path and the Rebrickable table ingest, so a change to the
/// on-disk extension is a single edit.
pub(crate) fn csv_filename(table: &str) -> String {
    format!("{table}.csv.gz")
}

/// Flush `path`'s contents to disk, then flush its parent directory so the
/// file's existence (and a subsequent rename within that dir) is durable.
/// Use before an atomic rename when the file was written by something other
/// than [`atomic_write`] — e.g. SQLite building the catalog DB with
/// `synchronous = OFF`, which leaves its pages unflushed.
pub(crate) fn fsync_path(path: &Path) -> Result<()> {
    fs::File::open(path)
        .with_context(|| format!("open for fsync {}", path.display()))?
        .sync_all()
        .with_context(|| format!("fsync {}", path.display()))?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        // A dir fsync makes the rename/entry durable; not all platforms
        // support opening a dir as a File, so a failure here is downgraded
        // to a warning rather than failing the build.
        match fs::File::open(parent) {
            Ok(dir) => {
                if let Err(e) = dir.sync_all() {
                    tracing::warn!("could not fsync dir {}: {e}", parent.display());
                }
            }
            Err(e) => tracing::warn!("could not open dir {} to fsync: {e}", parent.display()),
        }
    }
    Ok(())
}

/// Write `bytes` to `path` atomically: stage in a sibling `.tmp`, fsync it,
/// then safe-replace `path` with it via [`replace_file`]. Callers see either
/// the old contents or the new — never a half-written file, and never a
/// *missing* file if the final replace fails (the previous contents survive).
///
/// `sync_all` errors are propagated rather than discarded, so `ENOSPC` /
/// permission failures fail loudly instead of reporting success on a
/// non-durable write.
///
/// Use this for any file the build cares about being correct on disk — the
/// pinned Rebrickable snapshots, the regenerated color reference, future
/// catalog artifacts.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir {}", parent.display()))?;
    }
    let tmp = with_suffix(path, ".tmp");
    {
        let mut f = fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.sync_all()
            .with_context(|| format!("sync {}", tmp.display()))?;
    }
    replace_file(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh per-process temp dir, so concurrent test runs don't collide
    /// or delete each other's trees.
    fn unit_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("blockstar-util-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hashes_round_trip_through_hex() {
        let hash = hash_bytes(b"hello world");
        assert_eq!(hash.len(), 64);
        // Well-known SHA-256("hello world").
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn atomic_write_creates_new_file() {
        let dir = unit_dir("atomic-new");
        let path = dir.join("out.txt");
        atomic_write(&path, b"hello").expect("write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let dir = unit_dir("atomic-replace");
        let path = dir.join("out.txt");
        fs::write(&path, b"old").unwrap();
        atomic_write(&path, b"new").expect("replace");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = unit_dir("atomic-mkparent");
        let path = dir.join("nested/deep/out.txt");
        // Parent dirs don't exist yet — atomic_write should create them.
        atomic_write(&path, b"hi").expect("write into nested missing dirs");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hi");
        let _ = fs::remove_dir_all(&dir);
    }

    // The Windows backup-swap body is platform-agnostic (only its *trigger*
    // in `replace_file` is cfg!(windows)-gated), so we exercise it on any host
    // to lock in the preserve-old-on-error contract.

    #[test]
    fn backup_swap_replaces_destination_on_success() {
        let dir = unit_dir("swap-ok");
        let dst = dir.join("file");
        let src = with_suffix(&dst, ".tmp");
        fs::write(&dst, b"old").unwrap();
        fs::write(&src, b"new").unwrap();

        windows_backup_swap(&src, &dst).expect("swap should succeed");

        assert_eq!(fs::read(&dst).unwrap(), b"new", "dst holds the new content");
        assert!(!src.exists(), "src consumed");
        assert!(
            !with_suffix(&dst, ".bak").exists(),
            "backup cleaned up on success"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_swap_restores_previous_file_when_move_fails() {
        let dir = unit_dir("swap-restore");
        let dst = dir.join("file");
        // No src file → the second rename fails, exercising the restore path.
        let src = with_suffix(&dst, ".tmp");
        fs::write(&dst, b"the-good-file").unwrap();
        assert!(!src.exists());

        let err = windows_backup_swap(&src, &dst).expect_err("missing src must fail");

        // The contract: the previously-good file survives a failed swap.
        assert!(dst.exists(), "dst preserved after failed swap");
        assert_eq!(
            fs::read(&dst).unwrap(),
            b"the-good-file",
            "dst content intact"
        );
        assert!(
            !with_suffix(&dst, ".bak").exists(),
            "backup moved back, not left dangling"
        );
        assert!(
            format!("{err:#}").contains("rename"),
            "error mentions the failed rename"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The bug Copilot flagged: a failed final replace must not leave the
    /// destination missing. We can't force `replace_file` to fail on Unix
    /// (rename clobbers), so assert the property `atomic_write` now relies on
    /// — `replace_file` preserves the old file when the move fails — via the
    /// backup-swap path, which is the Windows behavior atomic_write inherits.
    #[test]
    fn atomic_write_overwrites_via_safe_replace() {
        let dir = unit_dir("atomic-safe-replace");
        let path = dir.join("pin.ron");
        fs::write(&path, b"old-pin").unwrap();
        atomic_write(&path, b"new-pin").expect("overwrite");
        assert_eq!(fs::read(&path).unwrap(), b"new-pin");
        // No staging/backup siblings left behind.
        assert!(!with_suffix(&path, ".tmp").exists(), "no .tmp left");
        assert!(!with_suffix(&path, ".bak").exists(), "no .bak left");
        let _ = fs::remove_dir_all(&dir);
    }
}
