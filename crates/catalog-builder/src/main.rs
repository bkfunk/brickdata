//! brickdata catalog builder CLI.
//!
//! A thin command-line shell over the `brickdata_catalog_builder` library.
//! Four subcommands:
//!
//! - `fetch`: download the pinned Rebrickable CSVs into the verified cache.
//! - `build`: build `catalog.sqlite` from the pinned CSVs + an LDraw
//!   library (either a local tree or a pinned snapshot).
//! - `refresh-color-names`: regenerate the committed color reference from
//!   the Rebrickable API (~250 one-off calls; rare, explicit, maintainer-
//!   only — never part of the routine build and never run in CI).
//! - `refresh-part-mappings`: regenerate `part_crossrefs.ron` from the
//!   Rebrickable parts API (~64 one-off calls; same caveats).

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use brickdata_catalog_builder::{build, fetch, refresh_colors, refresh_parts};

#[derive(Parser)]
#[command(name = "catalog-builder", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download the pinned Rebrickable CSVs into the verified cache. The pin
    /// is authored by `just mirror-rebrickable`; `fetch` never rewrites it.
    Fetch {
        /// Rebrickable pin file (pins/rebrickable-YYYY-MM-DD.ron).
        #[arg(long)]
        pin: PathBuf,
        /// Verified content-addressed cache directory.
        #[arg(long, default_value = "work/cache")]
        cache_dir: PathBuf,
    },

    /// Build catalog.sqlite from the pinned CSV snapshot and an LDraw library.
    Build {
        /// Rebrickable pin file (pins/rebrickable-YYYY-MM-DD.ron).
        #[arg(long)]
        pin: PathBuf,
        /// Path to a local LDraw library root (the dir containing `parts/`).
        /// Mutually exclusive with --ldraw-pin.
        #[arg(long, conflicts_with = "ldraw_pin")]
        ldraw_dir: Option<PathBuf>,
        /// LDraw snapshot pin (pins/ldraw-YYYY-MM-DD.ron): fetch + extract
        /// the pinned tree instead of using a local library.
        #[arg(long)]
        ldraw_pin: Option<PathBuf>,
        /// Where to write the SQLite DB.
        #[arg(long, default_value = "work/catalog.sqlite")]
        out: PathBuf,
        /// Verified content-addressed cache directory.
        #[arg(long, default_value = "work/cache")]
        cache_dir: PathBuf,
    },

    /// Refresh the committed color reference (colors.json + the compiled-in
    /// color_names.ron) from the Rebrickable API.
    RefreshColorNames {
        /// Rebrickable API key. Provide via `--api-key` or set the
        /// `REBRICKABLE_API_KEY` environment variable.
        #[arg(long, env = "REBRICKABLE_API_KEY")]
        api_key: String,
        /// Log what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
    },

    /// Refresh data/rebrickable/part_crossrefs.ron (part_num → external
    /// cross-refs) from the Rebrickable parts API.
    RefreshPartMappings {
        /// Rebrickable API key. Provide via `--api-key` or set the
        /// `REBRICKABLE_API_KEY` environment variable.
        #[arg(long, env = "REBRICKABLE_API_KEY")]
        api_key: String,
        /// Log the diff against the committed pin without writing it.
        #[arg(long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    match Cli::parse().command {
        Command::Fetch { pin, cache_dir } => fetch::run(&pin, &cache_dir),
        Command::Build {
            pin,
            ldraw_dir,
            ldraw_pin,
            out,
            cache_dir,
        } => {
            let ldraw_root = match (ldraw_dir, ldraw_pin) {
                (Some(dir), None) => dir,
                (None, Some(lp)) => fetch::pinned_ldraw_root(&lp, &cache_dir)?,
                _ => anyhow::bail!("exactly one of --ldraw-dir or --ldraw-pin is required"),
            };
            build::run(&pin, &cache_dir, &ldraw_root, &out)
        }
        Command::RefreshColorNames { api_key, dry_run } => refresh_colors::run(&api_key, dry_run),
        Command::RefreshPartMappings { api_key, dry_run } => refresh_parts::run(&api_key, dry_run),
    }
}
