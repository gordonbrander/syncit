//! Encryption of a remote vault's objects (store.md, section 6): keys and
//! object names derived from the master key, and XChaCha20-Poly1305 in the
//! STREAM construction. No I/O beyond `Read` and `Write`.

use aead_stream::{DecryptorBE32, EncryptorBE32};
use chacha20poly1305::XChaCha20Poly1305;
use hkdf::Hkdf;
use sha2::Sha256;
use std::io::{self, Read, Write};
use zeroize::Zeroizing;

pub type Key = Zeroizing<[u8; 32]>;

const CHUNK: u64 = 1 << 16;
const TAG: u64 = 16;
const NONCE: usize = 19;

/// A new master key: 32 random bytes.
pub fn new_key() -> io::Result<Key> {
    let mut key = Zeroizing::new([0u8; 32]);
    getrandom::fill(&mut *key).map_err(io::Error::other)?;
    Ok(key)
}

fn derive(key: &Key, info: &str) -> Key {
    let hk = Hkdf::<Sha256>::from_prk(&key[..]).expect("32 bytes is a valid PRK");
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(info.as_bytes(), &mut *out)
        .expect("32 bytes is a valid length");
    out
}

/// The value in `format` that tells a right key from a wrong one.
pub fn check(key: &Key) -> String {
    base16ct::lower::encode_string(derive(key, "syncit check").as_slice())
}

/// The object name of a segment or blob (`kind` is `log` or `blobs`).
pub fn name(key: &Key, kind: &str, hash: &str) -> String {
    base16ct::lower::encode_string(derive(key, &format!("syncit name {kind} {hash}")).as_slice())
}

/// The key an object named `name` is encrypted with.
pub fn object_key(key: &Key, kind: &str, name: &str) -> Key {
    derive(key, &format!("syncit key {kind} {name}"))
}

pub fn config_key(key: &Key) -> Key {
    derive(key, "syncit key config")
}

/// The length of the object that `n` plaintext bytes encrypt to.
pub fn encrypted_len(n: u64) -> u64 {
    NONCE as u64 + n + TAG * n.div_ceil(CHUNK).max(1)
}

/// The plaintext length of an object `len` bytes long; None if no
/// plaintext encrypts to that length.
pub fn plain_len(len: u64) -> Option<u64> {
    let body = len.checked_sub(NONCE as u64)?;
    let n = body.checked_sub(TAG * body.div_ceil(CHUNK + TAG).max(1))?;
    (encrypted_len(n) == len).then_some(n)
}

/// Read up to `size` bytes; fewer only at the end of the input.
fn read_full(input: &mut impl Read, buf: &mut Vec<u8>, size: u64) -> io::Result<()> {
    buf.clear();
    input.take(size).read_to_end(buf)?;
    Ok(())
}

fn bad() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "cannot decrypt")
}

/// Encrypt everything `input` holds to `output`: a random nonce, then the
/// plaintext in 64 KiB chunks, the last sealed as last.
pub fn encrypt(key: &Key, mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut nonce = [0u8; NONCE];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    output.write_all(&nonce)?;
    let mut stream = EncryptorBE32::<XChaCha20Poly1305>::new((&**key).into(), (&nonce).into());
    let (mut chunk, mut next) = (vec![], vec![]);
    read_full(&mut input, &mut chunk, CHUNK)?;
    loop {
        read_full(&mut input, &mut next, CHUNK)?;
        if next.is_empty() {
            let sealed = stream.encrypt_last(&chunk[..]).map_err(|_| bad())?;
            return output.write_all(&sealed);
        }
        output.write_all(&stream.encrypt_next(&chunk[..]).map_err(|_| bad())?)?;
        std::mem::swap(&mut chunk, &mut next);
    }
}

/// Decrypt an object from `input` to `output`. An error, never a panic, on a
/// wrong key, a changed byte, a truncated object or a short nonce; what was
/// written to `output` before then is not to be used.
pub fn decrypt(key: &Key, mut input: impl Read, mut output: impl Write) -> io::Result<()> {
    let mut nonce = vec![];
    read_full(&mut input, &mut nonce, NONCE as u64)?;
    if nonce.len() != NONCE {
        return Err(bad());
    }
    let nonce: [u8; NONCE] = nonce.try_into().map_err(|_| bad())?;
    let mut stream = DecryptorBE32::<XChaCha20Poly1305>::new((&**key).into(), (&nonce).into());
    let (mut chunk, mut next) = (vec![], vec![]);
    read_full(&mut input, &mut chunk, CHUNK + TAG)?;
    loop {
        read_full(&mut input, &mut next, CHUNK + TAG)?;
        if next.is_empty() {
            let plain = stream.decrypt_last(&chunk[..]).map_err(|_| bad())?;
            return output.write_all(&plain);
        }
        output.write_all(&stream.decrypt_next(&chunk[..]).map_err(|_| bad())?)?;
        std::mem::swap(&mut chunk, &mut next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::sha256;

    fn key(byte: u8) -> Key {
        Zeroizing::new([byte; 32])
    }

    fn seal(key: &Key, plain: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        encrypt(key, plain, &mut out).unwrap();
        out
    }

    fn open(key: &Key, sealed: &[u8]) -> io::Result<Vec<u8>> {
        let mut out = vec![];
        decrypt(key, sealed, &mut out).map(|_| out)
    }

    #[test]
    fn crypt_round_trip() {
        let k = key(7);
        for n in [0, 1, 65535, 65536, 65537, 131072, 1 << 20] {
            let plain: Vec<u8> = (0..n).map(|i| (i * 31 % 251) as u8).collect();
            let sealed = seal(&k, &plain);
            assert_eq!(sealed.len() as u64, encrypted_len(n as u64), "{n}");
            assert_eq!(plain_len(sealed.len() as u64), Some(n as u64), "{n}");
            assert_eq!(open(&k, &sealed).unwrap(), plain, "{n}");
        }
        assert_ne!(seal(&k, b"x"), seal(&k, b"x"), "the nonce is random");
    }

    #[test]
    fn crypt_rejects() {
        let k = key(1);
        let plain = vec![9u8; 200_000];
        let hash = sha256(&plain);
        let n = name(&k, "blobs", &hash);
        let sealed = {
            let mut out = vec![];
            encrypt(&object_key(&k, "blobs", &n), &plain[..], &mut out).unwrap();
            out
        };
        let right = object_key(&k, "blobs", &n);
        assert_eq!(open(&right, &sealed).unwrap(), plain);
        let wrong_key = object_key(&key(2), "blobs", &n);
        let wrong_name = object_key(&k, "blobs", &name(&k, "blobs", &sha256(b"other")));
        let wrong_kind = object_key(&k, "log", &n);
        for other in [&wrong_key, &wrong_name, &wrong_kind] {
            assert!(open(other, &sealed).is_err());
        }
        for at in [0, 5, NONCE, NONCE + 70_000, sealed.len() - 1] {
            let mut changed = sealed.clone();
            changed[at] ^= 1;
            assert!(open(&right, &changed).is_err(), "byte {at}");
        }
        let boundary = NONCE + (CHUNK + TAG) as usize;
        for cut in [
            boundary,
            2 * boundary - NONCE,
            sealed.len() - 1,
            NONCE,
            10,
            0,
        ] {
            assert!(open(&right, &sealed[..cut]).is_err(), "cut at {cut}");
        }
        assert!(open(&right, &[]).is_err());
    }

    #[test]
    fn names_are_keyed() {
        let hash = sha256(b"content");
        let (a, b) = (key(1), key(2));
        assert_eq!(name(&a, "blobs", &hash), name(&a, "blobs", &hash));
        assert_ne!(name(&a, "blobs", &hash), name(&b, "blobs", &hash));
        assert_ne!(name(&a, "blobs", &hash), name(&a, "log", &hash));
        for n in [name(&a, "blobs", &hash), name(&a, "log", &hash)] {
            assert_ne!(n, hash);
            assert!(crate::entry::is_hash(&n));
        }
        assert_ne!(check(&a), check(&b));
    }
}
