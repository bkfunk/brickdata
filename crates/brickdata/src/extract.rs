//! Archive helpers for the two asset encodings brickdata releases use:
//! gzip (Rebrickable `*.csv.gz`) and zip (the LDraw merged tree).

use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

/// Error extracting an archive.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// Reading the archive or writing extracted output failed. Corrupt gzip
    /// streams also surface here (on the source path).
    #[error("I/O error on {path}: {source}")]
    Io {
        /// The path being read or written.
        path: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The file is not a readable zip archive.
    #[error("invalid zip archive {path}: {source}")]
    Zip {
        /// Path of the bad archive.
        path: String,
        /// Underlying zip error.
        #[source]
        source: Box<zip::result::ZipError>,
    },
    /// A zip entry's name would write outside the destination (zip-slip).
    #[error("zip entry {name:?} escapes the destination directory")]
    UnsafeZipPath {
        /// The offending entry name.
        name: String,
    },
}

fn io_err(path: &Path) -> impl FnOnce(io::Error) -> ExtractError {
    let path = path.display().to_string();
    move |source| ExtractError::Io { path, source }
}

/// Decompress a `.gz` file to `dest`, returning `dest`.
pub fn gunzip_file(src: impl AsRef<Path>, dest: impl AsRef<Path>) -> Result<PathBuf, ExtractError> {
    let (src, dest) = (src.as_ref(), dest.as_ref());
    let reader = fs::File::open(src).map_err(io_err(src))?;
    let mut decoder = flate2::read::GzDecoder::new(io::BufReader::new(reader));
    let mut out = fs::File::create(dest).map_err(io_err(dest))?;
    // Decode errors surface on the source path: a truncated/corrupt gzip
    // stream is a source problem, not a destination one.
    io::copy(&mut decoder, &mut out).map_err(io_err(src))?;
    Ok(dest.to_path_buf())
}

/// Decompress a `.gz` file into memory.
pub fn gunzip_to_vec(src: impl AsRef<Path>) -> Result<Vec<u8>, ExtractError> {
    let src = src.as_ref();
    let reader = fs::File::open(src).map_err(io_err(src))?;
    let mut decoder = flate2::read::GzDecoder::new(io::BufReader::new(reader));
    let mut buf = Vec::new();
    decoder.read_to_end(&mut buf).map_err(io_err(src))?;
    Ok(buf)
}

/// Extract a zip archive into `dest_dir` (created if missing), returning
/// `dest_dir`. Entry paths are validated: absolute paths and `..` traversal
/// are rejected rather than written outside the destination.
pub fn unzip_to(
    zip_path: impl AsRef<Path>,
    dest_dir: impl AsRef<Path>,
) -> Result<PathBuf, ExtractError> {
    let (zip_path, dest_dir) = (zip_path.as_ref(), dest_dir.as_ref());
    let zip_err = |source: zip::result::ZipError| ExtractError::Zip {
        path: zip_path.display().to_string(),
        source: Box::new(source),
    };

    let file = fs::File::open(zip_path).map_err(io_err(zip_path))?;
    let mut archive = zip::ZipArchive::new(io::BufReader::new(file)).map_err(zip_err)?;
    fs::create_dir_all(dest_dir).map_err(io_err(dest_dir))?;

    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(zip_err)?;
        let rel = sanitize_entry_path(entry.name())?;
        let out_path = dest_dir.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out_path).map_err(io_err(&out_path))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        let mut out = fs::File::create(&out_path).map_err(io_err(&out_path))?;
        io::copy(&mut entry, &mut out).map_err(io_err(&out_path))?;
    }
    Ok(dest_dir.to_path_buf())
}

/// Reject absolute or parent-traversing entry names (zip-slip).
fn sanitize_entry_path(name: &str) -> Result<PathBuf, ExtractError> {
    let path = Path::new(name);
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(ExtractError::UnsafeZipPath {
                    name: name.to_string(),
                });
            }
        }
    }
    Ok(clean)
}
