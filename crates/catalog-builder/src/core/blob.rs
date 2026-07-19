//! Little-endian `u32` BLOB codec for catalog columns that store a list of
//! `u32`s (e.g. `ldraw_part.dimensions`, `ldraw_part.flexion_variants`).
//!
//! The catalog builder *packs* these lists into SQLite BLOBs and the cache
//! reader (and the build tests) *unpack* them. Keeping both halves here means
//! the two sides of one wire format cannot silently drift: changing the
//! encoding is a change to one function, checked by one round-trip test.

use thiserror::Error;

/// A BLOB whose byte length isn't a whole number of `u32`s — i.e. corrupt or
/// mis-encoded packing. Returned rather than silently dropping the trailing
/// partial value.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("u32 BLOB length {len} is not a multiple of 4 (corrupt packing)")]
pub struct BlobLengthError {
    /// The offending byte length.
    pub len: usize,
}

/// Pack a slice of `u32`s into a little-endian byte BLOB, or `None` when the
/// slice is empty.
///
/// An empty list encodes as SQL NULL (`None`), never a zero-length BLOB, so
/// the reader never has to distinguish "absent" from "present but empty" —
/// absence is the column being NULL.
pub fn pack_u32_le(values: &[u32]) -> Option<Vec<u8>> {
    if values.is_empty() {
        return None;
    }
    // Reserve the exact final size: `flat_map().collect()` has no exact size
    // hint and could reallocate as the buffer grows.
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for value in values {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    Some(bytes)
}

/// Decode a little-endian `u32` BLOB produced by [`pack_u32_le`].
///
/// Returns [`BlobLengthError`] when `bytes` isn't a multiple of 4 long: a
/// trailing partial chunk means a packing or storage bug, which should fail
/// loudly rather than be silently truncated the way `chunks_exact` would.
pub fn unpack_u32_le(bytes: &[u8]) -> Result<Vec<u32>, BlobLengthError> {
    if bytes.len() % 4 != 0 {
        return Err(BlobLengthError { len: bytes.len() });
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_empty_is_none() {
        assert_eq!(pack_u32_le(&[]), None);
    }

    #[test]
    fn round_trips_nonempty() {
        let values = vec![1u32, 2, 300, 70_000, u32::MAX];
        let blob = pack_u32_le(&values).expect("a non-empty list packs to Some");
        assert_eq!(blob.len(), values.len() * 4);
        assert_eq!(unpack_u32_le(&blob).unwrap(), values);
    }

    #[test]
    fn unpack_zero_length_blob_is_empty_vec() {
        // `pack_u32_le` never emits this (empty -> None), but a reader handed a
        // zero-length BLOB should decode it as an empty list, not an error.
        assert_eq!(unpack_u32_le(&[]).unwrap(), Vec::<u32>::new());
    }

    #[test]
    fn unpack_rejects_partial_trailing_value() {
        let err = unpack_u32_le(&[1, 0, 0, 0, 9]).expect_err("5 bytes must error");
        assert_eq!(err, BlobLengthError { len: 5 });
    }

    #[test]
    fn decodes_little_endian_byte_order() {
        // 0x04030201 is stored low byte first.
        assert_eq!(unpack_u32_le(&[1, 2, 3, 4]).unwrap(), vec![0x0403_0201]);
    }
}
