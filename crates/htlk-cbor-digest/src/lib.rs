#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]

mod digest;

pub use digest::{Digest, ParseDigestError};

use sha2::{Digest as _, Sha256};

/// Computes SHA-256 of the exact supplied bytes.
///
/// Adds no domain label, version, length prefix, or encoding. The caller is
/// responsible for constructing its preimage and bounding input size. The hash
/// operation borrows the input and uses fixed-size hashing state.
///
/// ```
/// use htlk_cbor_digest::hash_bytes;
/// assert_eq!(hash_bytes(b"abc").to_string(),
///     "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
/// ```
pub fn hash_bytes(bytes: &[u8]) -> Digest {
    Digest::from_bytes(Sha256::digest(bytes).into())
}

/// Computes SHA-256 of a value's deterministic CBOR encoding.
///
/// Encodes exactly once using [`htlk_cbor::encode`] and fresh per-operation
/// accounting, then hashes the complete resulting bytes. No implicit domain or
/// version labels are added. Different valid limits do not change the digest.
/// The temporary encoded buffer is dropped before returning.
///
/// # Errors
/// Propagates the codec's configuration, limit, and allocation errors unchanged.
/// No digest is produced unless encoding succeeds.
pub fn hash_cbor(
    value: &htlk_cbor::Value,
    limits: &htlk_cbor::Limits,
) -> Result<Digest, htlk_cbor::Error> {
    let bytes = htlk_cbor::encode(value, limits)?;
    Ok(hash_bytes(&bytes))
}
