//! Consumer API for [brickdata](https://github.com/bkfunk/brickdata)
//! snapshots: pinnable, immutable releases of brick-ecosystem data
//! (Rebrickable bulk CSVs, the LDraw parts library, built catalogs).
//!
//! The mirror side (cutting releases) stays pure shell in the repo's
//! `justfile`; this crate is the consumer side — every downstream build
//! shares one implementation of:
//!
//! - **[`pin`]** — parsing the `pins/*.ron` fingerprint records
//! - **[`fetch`]** — downloading assets with mandatory sha256/size
//!   verification and a local content-addressed cache (a verified cache hit
//!   does no network I/O)
//! - **[`extract`]** — gunzip/unzip helpers for the release asset encodings
//!
//! # Example
//!
//! ```no_run
//! # #[cfg(feature = "http")]
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use brickdata::fetch::Fetcher;
//! use brickdata::pin::RebrickablePin;
//!
//! let pin = RebrickablePin::from_path("pins/rebrickable-2026-06-01.ron")?;
//! let fetcher = Fetcher::new("/tmp/brickdata-cache");
//! let tables = fetcher.fetch_rebrickable(&pin)?; // name -> verified local path
//! let parts_csv = brickdata::extract::gunzip_to_vec(&tables["parts.csv.gz"])?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "http"))]
//! # fn main() {}
//! ```
//!
//! Verification failures ([`fetch::FetchError::HashMismatch`],
//! [`fetch::FetchError::SizeMismatch`]) are hard errors: no unverified bytes
//! are ever exposed through the cache.
//!
//! Typed row structs for the cleaned catalog tables will join this crate
//! once the catalog builder migration lands (repo issues #3/#4).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod extract;
pub mod fetch;
pub mod pin;
