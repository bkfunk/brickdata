//! Vendored from blockstar-core `src/parts/catalog.rs` for the builder
//! migration (bkfunk/brickdata#3); kept item-for-item compatible.
//!
//! The part catalog: a lightweight metadata index over an LDraw
//! library's `parts/` directory.
//!
//! The catalog is built by scanning each `.dat` file's leading header
//! lines — never its geometry — so indexing the full ~22K-part library
//! stays cheap. Geometry baking happens later, in the cache pipeline.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::categories::{self, Subcategory};

/// Errors from scanning an LDraw library tree.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("LDraw library not found at {0}")]
    LDrawLibraryNotFound(String),
    #[error("parse error: {0}")]
    ParseError(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// One part's catalog metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartEntry {
    /// LDraw design id — the `.dat` filename stem, e.g. `"3001"`.
    pub design_id: String,
    /// Part description / display name, e.g. `"Brick 2 x 4"`.
    pub name: String,
    /// The classified leaf subcategory. Its parent
    /// [`Category`](crate::core::Category) is reachable via
    /// [`Subcategory::category`].
    pub subcategory: Subcategory,
    /// Stud dimensions parsed from the name (e.g. `[2, 4]`); empty when
    /// the name has no `N x N` run. Used for within-category sorting.
    pub dimensions: Vec<u32>,
    pub is_decorated: bool,
    /// LDraw flexion-position variants of this design, as their `-fN`
    /// position numbers (sorted, e.g. `[1, 2]` for `<id>-f1`/`<id>-f2`).
    /// Empty for an ordinary part with no flexion files. These all share
    /// this entry's canonical `design_id`; the UI uses them to let the user
    /// swap between positions of the same part (e.g. a linear actuator
    /// contracted vs. extended). See `docs/lego-reference/ldraw-part-numbering.md`.
    pub flexion_variants: Vec<u32>,
    /// Whether a plain `<design_id>.dat` (no `-fN` suffix) was catalogued for
    /// this design. With flexion variants present, two shapes are otherwise
    /// indistinguishable: a design that ships a base file *and* positions
    /// (`ABCD.dat` + `ABCD-f1.dat`) vs. one that is flexion-only
    /// (`EFGH-f1.dat` + `EFGH-f2.dat`, no `EFGH.dat`). This bit records which,
    /// so [`source_dat_ids`](PartEntry::source_dat_ids) knows whether to
    /// include the bare `design_id` among the files backing this part.
    pub has_base_file: bool,
}

impl PartEntry {
    /// The actual `.dat` filename stems that back this catalogued design — the
    /// files a geometry baker must read. This is the bare `design_id` (when a
    /// base file was catalogued, [`has_base_file`](Self::has_base_file)) plus
    /// `<design_id>-fN` for each flexion position. Pure: it reflects what the
    /// scan recorded, no filesystem access.
    ///
    /// For a flexion-only design (no base file) this yields only the `-fN`
    /// stems — never the bare `design_id`, which has no `.dat` on disk. For an
    /// ordinary part it yields just `[design_id]`.
    ///
    /// Note the cache keyed by these stems is *per source file*: a flexion
    /// design is baked under each `<id>-fN` key, not under the canonical
    /// `design_id`. Choosing which position a canonical id resolves to is a
    /// reader-side policy (see the cache-reader work, issue #92), not the
    /// build's job — so the build deliberately bakes every position.
    pub fn source_dat_ids(&self) -> Vec<String> {
        let mut ids =
            Vec::with_capacity(self.flexion_variants.len() + usize::from(self.has_base_file));
        if self.has_base_file {
            ids.push(self.design_id.clone());
        }
        for &pos in &self.flexion_variants {
            ids.push(format!("{}-f{pos}", self.design_id));
        }
        ids
    }
}

/// A metadata index over every buildable part in an LDraw library.
#[derive(Debug, Clone, Default)]
pub struct PartCatalog {
    entries: BTreeMap<String, PartEntry>,
    /// `~Moved to` tombstones seen during the scan: the retired design id →
    /// the id LDraw renamed it to. One hop per tombstone, with both ids
    /// canonicalized (`-fN` flexion suffixes collapsed) like every id the
    /// catalog keys on; a target that was itself renamed later appears as
    /// its own key, so multi-rename histories form chains. Tombstoned files
    /// are excluded from [`entries`](Self::entries), so without this map a
    /// retired id (which external data like Rebrickable may still use) would
    /// be unresolvable.
    moved_to: BTreeMap<String, String>,
    /// Hard aliases seen during the scan: the alias design id → the id whose
    /// geometry it duplicates (the alias file's single type-1 reference).
    /// Alias files (`!LDRAW_ORG … Alias`) are excluded from
    /// [`entries`](Self::entries) as duplicate geometry, but the alias id is
    /// a *current, valid* part number that external data may use — unlike a
    /// [`moved_to`](Self::moved_to) id, which is retired.
    aliases: BTreeMap<String, String>,
}

impl PartCatalog {
    /// Build the catalog by scanning `<library_root>/parts/*.dat` headers.
    ///
    /// Subparts (`parts/s/`) and primitives (`p/`, including `8/` and
    /// `48/`) are skipped — only top-level `.dat` files are buildable
    /// parts. Parts excluded by [`categories::is_excluded`] are left out.
    ///
    /// Individual `.dat` files that can't be read or parsed are logged
    /// at `warn` level and skipped, so a single bad file in a user's
    /// library doesn't abort the whole scan. The build only fails on a
    /// missing `parts/` directory or a read-dir failure on it.
    pub fn build(library_root: impl AsRef<Path>) -> Result<PartCatalog, CoreError> {
        let parts_dir = library_root.as_ref().join("parts");
        if !parts_dir.is_dir() {
            return Err(CoreError::LDrawLibraryNotFound(
                parts_dir.display().to_string(),
            ));
        }

        // One entry per canonical design. LDraw flexion-position files
        // (`<id>-f1`, `<id>-f2`, …) all live in `parts/` but are one
        // pickable design; they collapse onto `<id>`, keeping the
        // best-ranked source (the base file, else the default `-f1`).
        let mut best: BTreeMap<String, (u32, PartEntry)> = BTreeMap::new();
        // Flexion-position numbers seen per canonical design, so the
        // grouping survives the collapse (the UI swaps between them). A
        // `BTreeSet` keeps them sorted and deduped; the plain base file
        // (rank 0) contributes no position, so a part with only a base and
        // no `-fN` files ends up with an empty set.
        let mut flexion_positions: BTreeMap<String, std::collections::BTreeSet<u32>> =
            BTreeMap::new();
        // `~Moved to` tombstones, keyed by the retired canonical id.
        let mut moved_to: BTreeMap<String, String> = BTreeMap::new();
        // Hard aliases, keyed by the alias canonical id.
        let mut aliases: BTreeMap<String, String> = BTreeMap::new();
        for dirent in fs::read_dir(&parts_dir)? {
            let path = match dirent {
                Ok(d) => d.path(),
                Err(e) => {
                    tracing::warn!("skipping unreadable directory entry: {e}");
                    continue;
                }
            };
            // Case-insensitive `.dat` match — some library distributions
            // ship `.DAT` on case-preserving filesystems.
            let is_dat = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("dat"));
            if !is_dat {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let (canonical, rank) = canonical_design_id(stem);

            let header = match read_header(&path) {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("skipping {}: {e}", path.display());
                    continue;
                }
            };
            // Record a `~Moved to <target>` tombstone before the exclusion
            // check drops it — the retired id → new id hop is the only place
            // this rename is recorded, and external part numbering (e.g.
            // Rebrickable's) may still reference the retired id.
            if let Some(target) = moved_to_target(&header.description) {
                moved_to.insert(canonical.to_string(), target.to_string());
            }
            // Likewise a hard alias (`!LDRAW_ORG … Alias`): its id is a
            // valid current part number for the target's geometry. The
            // target isn't in the header — it's the alias file's single
            // type-1 reference — so only alias-classified files pay the
            // body read.
            if is_alias_type(header.ldraw_org.as_deref()) {
                match alias_target(&path) {
                    Ok(Some(target)) => {
                        aliases.insert(canonical.to_string(), target);
                    }
                    Ok(None) => {
                        tracing::warn!(
                            "alias {} has no type-1 reference; skipping",
                            path.display()
                        );
                    }
                    Err(e) => tracing::warn!("skipping alias {}: {e}", path.display()),
                }
            }
            // The `0 !LDRAW_ORG` type/qualifier is the authoritative pickability
            // classifier (docs/lego-reference/ldraw-part-numbering.md): it keeps
            // alias / flexible-section / non-Part files that live directly in
            // `parts/` out of the catalog. The description/name heuristics still
            // run as a fallback (and for header-less non-standard libraries).
            if !categories::is_pickable_type(header.ldraw_org.as_deref())
                || categories::is_excluded(&header.description, canonical)
            {
                continue;
            }
            // Record every `-fN` position before any best-rank skip, so the
            // grouping survives the collapse, not just the winner. (The plain
            // base file, rank 0, needs no separate tracking: rank 0 is the
            // minimum, so if a base was catalogued it always wins the entry,
            // and `has_base_file` is derived from the winning rank below.)
            if rank > 0 {
                flexion_positions
                    .entry(canonical.to_string())
                    .or_default()
                    .insert(rank);
            }
            // A better-ranked file already covers this canonical design.
            if best.get(canonical).is_some_and(|(seen, _)| *seen <= rank) {
                continue;
            }

            best.insert(
                canonical.to_string(),
                (
                    rank,
                    PartEntry {
                        design_id: canonical.to_string(),
                        subcategory: categories::classify(
                            &header.description,
                            header.category.as_deref(),
                        ),
                        dimensions: parse_dimensions(&header.description),
                        is_decorated: categories::is_decorated(&header.description),
                        name: collapse_whitespace(&header.description),
                        // Both filled in after the scan, once every source
                        // file for this canonical design has been seen.
                        flexion_variants: Vec::new(),
                        has_base_file: false,
                    },
                ),
            );
        }
        let entries = best
            .into_iter()
            .map(|(id, (rank, mut entry))| {
                if let Some(positions) = flexion_positions.remove(&id) {
                    entry.flexion_variants = positions.into_iter().collect();
                }
                // The winning file is rank 0 exactly when a plain `<id>.dat`
                // was catalogued (rank 0 is the minimum, so a base always wins
                // over its `-fN` siblings). An excluded/unreadable base never
                // becomes the winner, so this correctly reports false for it.
                entry.has_base_file = rank == 0;
                (id, entry)
            })
            .collect();
        Ok(PartCatalog {
            entries,
            moved_to,
            aliases,
        })
    }

    /// The `~Moved to` tombstones seen during the scan: retired design id →
    /// the id it was renamed to (single hops; see the field doc).
    pub fn moved_to(&self) -> &BTreeMap<String, String> {
        &self.moved_to
    }

    /// The hard aliases seen during the scan: alias design id → the id whose
    /// geometry it duplicates (single hops; see the field doc).
    pub fn aliases(&self) -> &BTreeMap<String, String> {
        &self.aliases
    }

    /// Look up a part by its design id.
    pub fn get(&self, design_id: &str) -> Option<&PartEntry> {
        self.entries.get(design_id)
    }

    /// All entries, in **lexicographic** order by design id (e.g.
    /// `"10197"` before `"3001"` before `"973"`). Callers that want
    /// numeric or by-dimension ordering should resort the iterator.
    pub fn entries(&self) -> impl Iterator<Item = &PartEntry> {
        self.entries.values()
    }

    /// Number of parts in the catalog.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The header fields scanned from a `.dat` file.
struct Header {
    description: String,
    category: Option<String>,
    /// The raw `0 !LDRAW_ORG` value (type plus any qualifiers and release tag,
    /// e.g. `"Part"`, `"Shortcut UPDATE 2023-01"`, `"Part Alias"`), or `None`
    /// when the file carries no such line.
    ldraw_org: Option<String>,
}

/// Read the leading meta lines of a `.dat` file, stopping at the first
/// geometry line. Tolerates a UTF-8 BOM and blank leading lines — the
/// description is taken from the first non-blank `0 ...` line, not the
/// raw first line.
fn read_header(path: &Path) -> Result<Header, CoreError> {
    let file = fs::File::open(path)?;
    let mut description = String::new();
    let mut category = None;
    let mut ldraw_org = None;
    let mut have_description = false;

    for line in BufReader::new(file).lines() {
        let line = line?;
        // Strip a leading UTF-8 BOM if present (only meaningful on the
        // first line; a no-op anywhere else).
        let trimmed = line.trim_start_matches('\u{FEFF}').trim();
        if trimmed.is_empty() {
            continue;
        }

        // Line type 0: comment or meta. Anything else (1..=5) is
        // geometry — the header (where !CATEGORY lives) is done.
        let Some(meta) = trimmed
            .strip_prefix("0 ")
            .or_else(|| trimmed.strip_prefix("0\t"))
        else {
            if trimmed == "0" {
                // Bare `0` is an unusual empty-meta line; skip it.
                continue;
            }
            break;
        };
        let meta = meta.trim();

        // First non-blank meta line is the part description.
        if !have_description {
            description = meta.to_string();
            have_description = true;
            continue;
        }

        if let Some(value) = meta.strip_prefix("!CATEGORY ") {
            category = Some(value.trim().to_string());
        } else if let Some(value) = meta.strip_prefix("!LDRAW_ORG ") {
            ldraw_org = Some(value.trim().to_string());
        }
    }
    if !have_description {
        // A file with no `0 …` meta line before geometry (or an empty
        // file) has no part description to index by — treat as a parse
        // failure so the catalog's per-file warn+skip drops it instead
        // of inserting an empty-named entry that misclassifies.
        return Err(CoreError::ParseError(format!(
            "{}: no part description found in header",
            path.display(),
        )));
    }
    Ok(Header {
        description,
        category,
        ldraw_org,
    })
}

/// Collapse whitespace runs to single spaces. LDraw descriptions pad
/// dimensions for column alignment (`"Brick  2 x  4"`); entry names are
/// stored collapsed so display and search (which relies on this
/// invariant — see `search`) never see the
/// artifact.
pub(crate) fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parse a part's stud dimensions from its description — the first
/// `N x N [x N ...]` run, e.g. `"Brick 2 x 4"` → `[2, 4]`. The slope
/// angle in `"Slope Brick 45 2 x 1"` is skipped (no `x` follows it).
fn parse_dimensions(description: &str) -> Vec<u32> {
    let tokens: Vec<&str> = description.split_whitespace().collect();
    for start in 0..tokens.len() {
        let Ok(first) = tokens[start].parse::<u32>() else {
            continue;
        };
        let mut dims = vec![first];
        let mut k = start + 1;
        while k + 1 < tokens.len() && tokens[k].eq_ignore_ascii_case("x") {
            match tokens[k + 1].parse::<u32>() {
                Ok(n) => {
                    dims.push(n);
                    k += 2;
                }
                Err(_) => break,
            }
        }
        if dims.len() >= 2 {
            return dims;
        }
    }
    Vec::new()
}

/// Strip a trailing LDraw flexion-position suffix (`-f<N>`) from a design
/// id, returning the canonical id and a *rank* used to pick a single entry
/// when several files collapse onto it. A flexible part ships the base
/// assembly (`92693c01`) plus one file per position (`92693c01-f1`
/// contracted, `92693c01-f2` extended), all in `parts/` — but it is one
/// pickable design. The base file ranks best (0); `-f1` (the default
/// position) beats `-f2`, and so on. Non-flexion ids return unchanged at
/// rank 0. See `docs/lego-reference/ldraw-part-numbering.md`.
///
/// Public so build-time tooling (the catalog builder's `refresh-part-mappings`)
/// derives the canonical design id by the *same* rule the catalog keys on —
/// a parallel re-implementation would drift on the `-f0` / overflow edges.
pub fn canonical_design_id(stem: &str) -> (&str, u32) {
    if let Some((base, pos)) = stem.rsplit_once("-f") {
        // `-f1` → rank 1, `-f2` → rank 2, …. Rank 0 is reserved for the plain
        // base file, so a parsed position of 0 is NOT treated as a flexion
        // suffix — a `<id>-f0` stem stays its own canonical id, otherwise it
        // would masquerade as the base (corrupting `has_base_file`) and drop
        // itself from `flexion_variants`. A run that doesn't fit in u32 can't
        // be a real position either, so it's left as non-flexion rather than
        // collapsed to a phantom `<base>-f<u32::MAX>` that no `.dat` backs.
        if !pos.is_empty() && pos.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(rank) = pos.parse::<u32>() {
                if rank >= 1 {
                    return (base, rank);
                }
            }
        }
    }
    (stem, 0)
}

/// Whether an `!LDRAW_ORG` value classifies the file as a hard alias —
/// `Part Alias`, `Shortcut Alias`, etc. (the `Alias` qualifier is what
/// matters, wherever it sits among the type words and release tag).
fn is_alias_type(ldraw_org: Option<&str>) -> bool {
    ldraw_org.is_some_and(|value| {
        value
            .split_whitespace()
            .any(|word| word.eq_ignore_ascii_case("alias"))
    })
}

/// The design id an alias file duplicates: the referenced stem of its first
/// (and, per the alias convention, only) type-1 line, canonicalized like
/// every other id. `Ok(None)` for a file with no type-1 line.
fn alias_target(path: &Path) -> Result<Option<String>, CoreError> {
    let file = fs::File::open(path)?;
    for line in BufReader::new(file).lines() {
        let line = line?;
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first() != Some(&"1") || tokens.len() < 15 {
            continue;
        }
        // The filename is everything after the 14 transform fields; strip
        // any subdirectory prefix (`s\`) and the `.dat` extension.
        let reference = tokens[14..].join(" ");
        let basename = reference
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or(reference.as_str());
        let stem = basename
            .strip_suffix(".dat")
            .or_else(|| basename.strip_suffix(".DAT"))
            .unwrap_or(basename);
        if stem.is_empty() {
            return Ok(None);
        }
        return Ok(Some(canonical_design_id(stem).0.to_string()));
    }
    Ok(None)
}

/// The rename target of a `~Moved to <id>` tombstone description, or `None`
/// for any other description (including other `~`-prefixed exclusion markers
/// like `~Obsolete`). Matched case-insensitively; the target is the canonical
/// id (a `-fN` flexion target collapses to its base, like every other id the
/// catalog keys on).
fn moved_to_target(description: &str) -> Option<&str> {
    const PREFIX: &str = "~moved to ";
    let head = description.get(..PREFIX.len())?;
    if !head.eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let target = description[PREFIX.len()..].trim();
    if target.is_empty() {
        return None;
    }
    Some(canonical_design_id(target).0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::categories::Category;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixtures_path() -> PathBuf {
        let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        p.push("tests/fixtures/ldraw");
        p
    }

    /// Build a one-off `parts/` directory under a fresh temp dir and
    /// return the *library root* (the dir containing `parts/`). The
    /// path is suffixed with PID + monotonic counter + nanos so
    /// concurrent `cargo test` runs (and re-runs within a process)
    /// don't collide. The caller writes files into `<root>/parts/`.
    fn temp_library(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let pid = std::process::id();
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "blockstar-catalog-test-{tag}-{pid}-{counter}-{nanos}"
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("parts")).expect("mkdir parts");
        root
    }

    fn write_dat(root: &Path, name: &str, contents: &[u8]) {
        let path = root.join("parts").join(name);
        let mut f = fs::File::create(&path).expect("create dat");
        f.write_all(contents).expect("write dat");
    }

    #[test]
    fn parses_stud_dimensions() {
        assert_eq!(parse_dimensions("Brick 2 x 4"), vec![2, 4]);
        assert_eq!(parse_dimensions("Plate 1 x 1"), vec![1, 1]);
        assert_eq!(parse_dimensions("Brick 1 x 2 x 3"), vec![1, 2, 3]);
        // The 45 is a slope angle, not a dimension — no `x` follows it.
        assert_eq!(parse_dimensions("Slope Brick 45 2 x 1"), vec![2, 1]);
        assert!(parse_dimensions("Technic Axle 2").is_empty());
    }

    #[test]
    fn canonical_design_id_strips_flexion_suffix() {
        assert_eq!(canonical_design_id("3001"), ("3001", 0));
        assert_eq!(canonical_design_id("92693c01"), ("92693c01", 0));
        assert_eq!(canonical_design_id("92693c01-f1"), ("92693c01", 1));
        assert_eq!(canonical_design_id("92693c01-f2"), ("92693c01", 2));
        // "-f" not followed by digits is part of the id, not a flexion tag.
        assert_eq!(canonical_design_id("abc-fold"), ("abc-fold", 0));
        // `-f0` is NOT a flexion position (rank 0 is reserved for the base
        // file); the stem stays intact as its own canonical id.
        assert_eq!(canonical_design_id("92693c01-f0"), ("92693c01-f0", 0));
        // A run that overflows u32 isn't a real position: treat it as
        // non-flexion (stem intact) instead of collapsing to a phantom
        // `<base>-f<u32::MAX>` that no `.dat` backs.
        assert_eq!(
            canonical_design_id("foo-f999999999999"),
            ("foo-f999999999999", 0)
        );
    }

    #[test]
    fn f0_stem_is_its_own_part_not_a_base_alias() {
        // A `<id>-f0.dat` must not masquerade as the base of `<id>`: it should
        // be its own canonical entry, and an unrelated `<id>` (or `<id>-f1`)
        // must keep its own correct has_base_file.
        let root = temp_library("f0-edge");
        write_dat(&root, "77777-f0.dat", b"0 Oddly Named Part 1 x 1\n");
        let catalog = PartCatalog::build(&root).expect("build catalog");
        let entry = catalog.get("77777-f0").expect("-f0 indexed as its own id");
        assert!(
            entry.flexion_variants.is_empty(),
            "-f0 is not a flexion pos"
        );
        assert!(
            entry.has_base_file,
            "-f0 is itself a base .dat for '77777-f0'"
        );
        assert_eq!(entry.source_dat_ids(), vec!["77777-f0"]);
        assert!(
            catalog.get("77777").is_none(),
            "-f0 must not synthesize a '77777' canonical entry",
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ldraw_org_header_excludes_non_pickable_files_in_parts() {
        // Alias / flexible-section / subpart files can sit directly in `parts/`;
        // the `0 !LDRAW_ORG` header is what keeps them out of the catalog, even
        // when their description wouldn't trip the heuristics in `is_excluded`.
        let root = temp_library("ldraw-org");
        write_dat(
            &root,
            "3001.dat",
            b"0 Brick 2 x 4\n0 !LDRAW_ORG Part UPDATE 2002-01\n",
        );
        write_dat(
            &root,
            "3001a.dat",
            b"0 Brick 2 x 4 without Cross Supports\n0 !LDRAW_ORG Part Alias\n",
        );
        write_dat(&root, "s1.dat", b"0 Some Subpart\n0 !LDRAW_ORG Subpart\n");
        let catalog = PartCatalog::build(&root).expect("build catalog");
        assert!(catalog.get("3001").is_some(), "a real Part is catalogued");
        assert!(
            catalog.get("3001a").is_none(),
            "a Part Alias in parts/ must be excluded by its !LDRAW_ORG header",
        );
        assert!(
            catalog.get("s1").is_none(),
            "a Subpart in parts/ must be excluded by its !LDRAW_ORG header",
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn moved_to_target_parses_tombstones_only() {
        assert_eq!(moved_to_target("~Moved to 6141"), Some("6141"));
        // Case-insensitive, and a flexion target collapses to its base.
        assert_eq!(moved_to_target("~MOVED TO 75c09-f1"), Some("75c09"));
        assert_eq!(moved_to_target("~Moved to "), None, "empty target");
        assert_eq!(moved_to_target("~Obsolete part"), None);
        assert_eq!(moved_to_target("Brick 2 x 4"), None);
    }

    #[test]
    fn is_alias_type_matches_the_alias_qualifier() {
        assert!(is_alias_type(Some("Part Alias")));
        assert!(is_alias_type(Some("Part Alias UPDATE 2013-02")));
        assert!(is_alias_type(Some("Shortcut Alias")));
        assert!(!is_alias_type(Some("Part")));
        assert!(!is_alias_type(Some("Shortcut UPDATE 2023-01")));
        assert!(!is_alias_type(None));
    }

    #[test]
    fn scan_records_hard_aliases() {
        let root = temp_library("aliases");
        write_dat(
            &root,
            "64289.dat",
            b"0 =Technic Beam  9\n\
              0 !LDRAW_ORG Part Alias UPDATE 2013-02\n\
              1 16 0 0 0 1 0 0 0 1 0 0 0 1 40490.dat\n",
        );
        write_dat(
            &root,
            "40490.dat",
            b"0 Technic Beam  9\n0 !LDRAW_ORG Part UPDATE 2013-02\n",
        );
        let catalog = PartCatalog::build(&root).expect("build catalog");
        assert!(catalog.get("64289").is_none(), "alias stays excluded");
        assert_eq!(
            catalog.aliases().get("64289").map(String::as_str),
            Some("40490"),
            "the alias hop is recorded"
        );
        assert!(catalog.get("40490").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_records_moved_to_tombstones() {
        let root = temp_library("moved-to");
        write_dat(&root, "4073.dat", b"0 ~Moved to 6141\n0 !LDRAW_ORG Moved\n");
        write_dat(
            &root,
            "6141.dat",
            b"0 Plate Round 1 x 1\n0 !LDRAW_ORG Part UPDATE 2023-05\n",
        );
        let catalog = PartCatalog::build(&root).expect("build catalog");
        assert!(catalog.get("4073").is_none(), "tombstone stays excluded");
        assert_eq!(
            catalog.moved_to().get("4073").map(String::as_str),
            Some("6141"),
            "the rename hop is recorded"
        );
        assert!(catalog.get("6141").is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn excluded_base_leaves_has_base_file_false() {
        // A base `<id>.dat` that is excluded (e.g. a '~Moved to' stub) while a
        // surviving `-f1` forms the entry must NOT report has_base_file=true —
        // source_dat_ids() would otherwise list a base the baker can't use.
        let root = temp_library("excluded-base");
        // `~`-prefixed descriptions are excluded (Moved/obsolete markers).
        write_dat(&root, "88888.dat", b"0 ~Moved to 99999\n");
        write_dat(
            &root,
            "88888-f1.dat",
            b"0 Real Geometry 1 x 1 (Contracted)\n",
        );
        let catalog = PartCatalog::build(&root).expect("build catalog");
        let entry = catalog.get("88888").expect("entry from the surviving -f1");
        assert_eq!(entry.flexion_variants, vec![1]);
        assert!(
            !entry.has_base_file,
            "an excluded base must not be reported as a usable base file",
        );
        assert_eq!(
            entry.source_dat_ids(),
            vec!["88888-f1"],
            "must not list the excluded base among source files",
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn collapses_flexion_position_variants() {
        let root = temp_library("flexion");
        // Base assembly + two flexion positions, all in parts/.
        write_dat(
            &root,
            "92693c01.dat",
            b"0 Technic Linear Actuator 4 x 1 x 1 Body Assembly\n0 !LDRAW_ORG Shortcut\n",
        );
        write_dat(
            &root,
            "92693c01-f1.dat",
            b"0 Technic Linear Actuator 4 x 1 x 1 (Contracted)\n0 !LDRAW_ORG Shortcut\n",
        );
        write_dat(
            &root,
            "92693c01-f2.dat",
            b"0 Technic Linear Actuator 4 x 1 x 1 (Extended)\n0 !LDRAW_ORG Shortcut\n",
        );

        let catalog = PartCatalog::build(&root).expect("build catalog");
        // One pickable design, not three.
        assert!(catalog.get("92693c01").is_some(), "canonical id missing");
        assert!(
            catalog.get("92693c01-f1").is_none(),
            "flexion variant leaked"
        );
        assert!(
            catalog.get("92693c01-f2").is_none(),
            "flexion variant leaked"
        );
        // The base file wins, so the name is the assembly's, not a position.
        let entry = catalog.get("92693c01").unwrap();
        assert_eq!(
            entry.name,
            "Technic Linear Actuator 4 x 1 x 1 Body Assembly"
        );
        // The grouping survives the collapse: both flexion positions are
        // recorded so the UI can swap between them.
        assert_eq!(entry.flexion_variants, vec![1, 2]);
        // A base file is present, so all three .dat files back this design.
        assert!(entry.has_base_file, "base file should be recorded");
        assert_eq!(
            entry.source_dat_ids(),
            vec!["92693c01", "92693c01-f1", "92693c01-f2"],
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn flexion_variants_without_base_keep_default_position() {
        let root = temp_library("flexion-no-base");
        // No base file — only positions. The default (-f1) provides the
        // single entry under the canonical id.
        write_dat(&root, "12345c01-f1.dat", b"0 Widget 1 x 1 (Contracted)\n");
        write_dat(&root, "12345c01-f2.dat", b"0 Widget 1 x 1 (Extended)\n");
        let catalog = PartCatalog::build(&root).expect("build catalog");
        assert_eq!(catalog.len(), 1);
        let entry = catalog.get("12345c01").expect("canonical entry from -f1");
        assert_eq!(entry.name, "Widget 1 x 1 (Contracted)");
        assert_eq!(entry.flexion_variants, vec![1, 2]);
        // No base file: the bare design_id has no .dat, so source_dat_ids
        // must NOT include it — only the position files exist on disk.
        assert!(!entry.has_base_file, "no base file should be recorded");
        assert_eq!(
            entry.source_dat_ids(),
            vec!["12345c01-f1", "12345c01-f2"],
            "flexion-only design must not list a non-existent base .dat",
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn flexion_variants_with_base_but_no_f1() {
        // The real library's `30191` shape: a plain base file plus `-f2` /
        // `-f3` with no `-f1`. The base still wins the entry, and both
        // present positions are grouped (no phantom f1).
        let root = temp_library("flexion-no-f1");
        write_dat(&root, "30191.dat", b"0 Spring Shock Absorber\n");
        write_dat(
            &root,
            "30191-f2.dat",
            b"0 Spring Shock Absorber (Compressed)\n",
        );
        write_dat(
            &root,
            "30191-f3.dat",
            b"0 Spring Shock Absorber (Extended)\n",
        );
        let catalog = PartCatalog::build(&root).expect("build catalog");
        let entry = catalog.get("30191").expect("canonical entry from base");
        assert_eq!(entry.name, "Spring Shock Absorber", "base file wins");
        assert_eq!(entry.flexion_variants, vec![2, 3], "no phantom f1");
        // Base + f2 + f3 all exist; source list includes the base and skips f1.
        assert!(entry.has_base_file);
        assert_eq!(
            entry.source_dat_ids(),
            vec!["30191", "30191-f2", "30191-f3"],
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plain_part_has_no_flexion_variants() {
        let root = temp_library("plain-no-flexion");
        write_dat(&root, "3001.dat", b"0 Brick 2 x 4\n0 !CATEGORY Brick\n");
        let catalog = PartCatalog::build(&root).expect("build catalog");
        let entry = catalog.get("3001").expect("3001 indexed");
        assert!(
            entry.flexion_variants.is_empty(),
            "an ordinary part has no flexion variants"
        );
        // An ordinary part is backed by exactly its own .dat.
        assert!(entry.has_base_file);
        assert_eq!(entry.source_dat_ids(), vec!["3001"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn builds_catalog_from_fixture_library() {
        let catalog = PartCatalog::build(fixtures_path()).expect("build catalog");
        assert!(!catalog.is_empty());

        // The fixture's reference parts must all be indexed.
        for id in ["3001", "3024", "3005"] {
            assert!(catalog.get(id).is_some(), "missing part {id}");
        }
    }

    #[test]
    fn name_whitespace_runs_are_collapsed_at_build() {
        // LDraw descriptions pad dimensions for column alignment
        // ("Brick  2 x  4"). The catalog stores the display name with
        // runs collapsed so the UI and search never see the artifact.
        let catalog = PartCatalog::build(fixtures_path()).expect("build catalog");
        let brick = catalog.get("3001").expect("part 3001");
        assert_eq!(brick.name, "Brick 2 x 4");
        let plate = catalog.get("15070").expect("part 15070");
        assert_eq!(plate.name, "Plate 1 x 1 with Tooth Perpendicular");
    }

    #[test]
    fn catalog_entry_carries_classified_metadata() {
        let catalog = PartCatalog::build(fixtures_path()).expect("build catalog");

        let brick = catalog.get("3001").expect("part 3001");
        assert!(brick.name.starts_with("Brick"), "name was {:?}", brick.name);
        assert_eq!(brick.subcategory.category(), Category::Bricks);
        assert_eq!(brick.dimensions, vec![2, 4]);
        assert!(!brick.is_decorated);
    }

    #[test]
    fn missing_parts_dir_errors() {
        let path = std::env::temp_dir().join("blockstar-catalog-test-missing-xyz");
        let err = PartCatalog::build(&path).unwrap_err();
        assert!(matches!(err, CoreError::LDrawLibraryNotFound(_)));
    }

    #[test]
    fn indexes_uppercase_dat_extension() {
        let root = temp_library("uppercase-ext");
        write_dat(&root, "9001.DAT", b"0 Brick 1 x 1\n0 !CATEGORY Brick\n");
        let catalog = PartCatalog::build(&root).expect("build catalog");
        assert!(catalog.get("9001").is_some(), "uppercase .DAT was skipped");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_header_skips_leading_blank_lines() {
        let root = temp_library("blank-leading");
        // Two blank lines, then the real description.
        write_dat(
            &root,
            "9002.dat",
            b"\n   \n0 Brick 1 x 2\n0 !CATEGORY Brick\n",
        );
        let catalog = PartCatalog::build(&root).expect("build catalog");
        let entry = catalog.get("9002").expect("9002 indexed");
        assert_eq!(entry.name, "Brick 1 x 2");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_header_strips_utf8_bom() {
        let root = temp_library("utf8-bom");
        // 0xEF 0xBB 0xBF is the UTF-8 BOM.
        let mut bytes: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"0 Tile 1 x 1\n0 !CATEGORY Tile\n");
        write_dat(&root, "9003.dat", &bytes);
        let catalog = PartCatalog::build(&root).expect("build catalog");
        let entry = catalog.get("9003").expect("9003 indexed");
        assert_eq!(entry.name, "Tile 1 x 1");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_unreadable_files_without_failing_the_build() {
        let root = temp_library("bad-file-skip");
        // One good file alongside one with invalid UTF-8 in a header
        // byte. The bad file must be skipped; the good one must land.
        write_dat(&root, "9004.dat", b"0 Plate 1 x 1\n0 !CATEGORY Plate\n");
        // Invalid UTF-8 sequence (0xFF is never valid UTF-8).
        write_dat(&root, "9005.dat", b"0 Bad \xFF\xFE header\n");

        let catalog = PartCatalog::build(&root).expect("build should not abort");
        assert!(catalog.get("9004").is_some(), "good file was lost");
        assert!(catalog.get("9005").is_none(), "bad file was indexed");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn skips_files_with_no_description() {
        let root = temp_library("no-description");
        // A real part, plus three files that should be dropped: a
        // completely empty file, a file with only blank lines, and a
        // file that jumps straight to geometry with no `0 …` meta.
        write_dat(&root, "9006.dat", b"0 Brick 1 x 1\n0 !CATEGORY Brick\n");
        write_dat(&root, "9007.dat", b"");
        write_dat(&root, "9008.dat", b"\n\n\n");
        write_dat(
            &root,
            "9009.dat",
            b"1 16 0 0 0 1 0 0 0 1 0 0 0 1 stud.dat\n",
        );

        let catalog = PartCatalog::build(&root).expect("build should not abort");
        assert!(catalog.get("9006").is_some(), "good file was lost");
        for id in ["9007", "9008", "9009"] {
            assert!(
                catalog.get(id).is_none(),
                "description-less file {id} was indexed",
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
