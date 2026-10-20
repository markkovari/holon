//! `secrets-vault` — store secrets envelope-encrypted, so an API key never sits in plaintext beside ordinary data
//! its own random nonce, so old versions stay decryptable after a rotation.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use chacha20poly1305::{aead::Aead, ChaCha20Poly1305, Key, KeyInit, Nonce};

use bindings::exports::secrets::vault::vault::{Guest, SecretMeta, VaultError};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::config::store as config;
use bindings::wasi::keyvalue::store as kv;
use bindings::wasi::random::random::get_random_bytes;

struct Component;

const BUCKET: &str = "default";
const INDEX_KEY: &str = "sv_index";
const NONCE_LEN: usize = 12;

fn now() -> u64 {
    wall_clock::now().seconds
}

/// Same byte scheme as idempotency-guard's `id_key`.
fn safe_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for b in name.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn meta_key(name: &str) -> String {
    format!("sv_meta_{}", safe_name(name))
}

fn version_key(name: &str, version: u32) -> String {
    format!("sv_{}_v{version}", safe_name(name))
}

/// Anything but 32 base64 bytes is a `crypto` error: the host mis-provisioned the vault.
fn cipher() -> Result<ChaCha20Poly1305, VaultError> {
    let raw = match config::get("master-key") {
        Ok(Some(v)) => v,
        Ok(None) => return Err(VaultError::Crypto("master key missing".into())),
        Err(e) => {
            return Err(VaultError::BackendUnavailable(format!("config: {e:?}")));
        }
    };
    let master = B64
        .decode(raw.trim())
        .map_err(|_| VaultError::Crypto("master key not valid base64".into()))?;
    if master.len() != 32 {
        return Err(VaultError::Crypto(format!(
            "master key must be 32 bytes, got {}",
            master.len()
        )));
    }
    // Not the deprecated `Key::from_slice`, which panics: a wrong length stays a `VaultError`.
    let key: [u8; 32] = master
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::Crypto("master key must be 32 bytes".into()))?;
    Ok(ChaCha20Poly1305::new(&Key::from(key)))
}

fn seal(plaintext: &[u8]) -> Result<String, VaultError> {
    let cipher = cipher()?;
    let nonce_bytes = get_random_bytes(NONCE_LEN as u64);
    let n: [u8; NONCE_LEN] = nonce_bytes
        .as_slice()
        .try_into()
        .map_err(|_| VaultError::Crypto("short nonce from the host".into()))?;
    let nonce = Nonce::from(n);
    let ct = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| VaultError::Crypto("encrypt failed".into()))?;
    let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ct);
    Ok(B64.encode(&blob))
}

fn unseal(blob: &str) -> Result<Vec<u8>, VaultError> {
    let cipher = cipher()?;
    let raw = B64
        .decode(blob.trim())
        .map_err(|_| VaultError::Crypto("stored value not valid base64".into()))?;
    if raw.len() < NONCE_LEN {
        return Err(VaultError::Crypto("stored value truncated".into()));
    }
    let (nonce_bytes, ct) = raw.split_at(NONCE_LEN);
    let n: [u8; NONCE_LEN] =
        nonce_bytes.try_into().map_err(|_| VaultError::Crypto("stored value truncated".into()))?;
    let nonce = Nonce::from(n);
    cipher.decrypt(&nonce, ct).map_err(|_| VaultError::Crypto("decrypt/authenticate failed".into()))
}

fn open() -> Result<kv::Bucket, VaultError> {
    kv::open(BUCKET).map_err(|e| VaultError::BackendUnavailable(format!("open: {e:?}")))
}

fn get_str(bucket: &kv::Bucket, key: &str) -> Result<Option<String>, VaultError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| VaultError::BackendUnavailable("value not utf-8".into())),
        Ok(None) => Ok(None),
        Err(e) => Err(VaultError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

fn set_str(bucket: &kv::Bucket, key: &str, val: &str) -> Result<(), VaultError> {
    bucket
        .set(key, val.as_bytes())
        .map_err(|e| VaultError::BackendUnavailable(format!("set: {e:?}")))
}

fn delete_key(bucket: &kv::Bucket, key: &str) -> Result<(), VaultError> {
    bucket.delete(key).map_err(|e| VaultError::BackendUnavailable(format!("delete: {e:?}")))
}

fn read_meta(bucket: &kv::Bucket, name: &str) -> Result<Option<(u32, u64)>, VaultError> {
    match get_str(bucket, &meta_key(name))? {
        Some(s) => {
            let (v, u) = s.split_once(':').unwrap_or((s.as_str(), "0"));
            let version = v
                .parse()
                .map_err(|_| VaultError::BackendUnavailable("corrupt meta: version".into()))?;
            let updated = u.parse().unwrap_or(0);
            Ok(Some((version, updated)))
        }
        None => Ok(None),
    }
}

fn write_meta(
    bucket: &kv::Bucket,
    name: &str,
    version: u32,
    updated: u64,
) -> Result<(), VaultError> {
    set_str(bucket, &meta_key(name), &format!("{version}:{updated}"))
}

// Best-effort read-modify-write (no CAS in `wasi:keyvalue@0.2.0-draft`): concurrent first-puts
// of distinct names can clobber each other's entry.

fn read_index(bucket: &kv::Bucket) -> Result<Vec<String>, VaultError> {
    Ok(match get_str(bucket, INDEX_KEY)? {
        Some(s) => s.lines().filter(|l| !l.is_empty()).map(|l| l.to_string()).collect(),
        None => Vec::new(),
    })
}

fn write_index(bucket: &kv::Bucket, names: &[String]) -> Result<(), VaultError> {
    set_str(bucket, INDEX_KEY, &names.join("\n"))
}

fn index_add(bucket: &kv::Bucket, name: &str) -> Result<(), VaultError> {
    let mut names = read_index(bucket)?;
    if !names.iter().any(|n| n == name) {
        names.push(name.to_string());
        write_index(bucket, &names)?;
    }
    Ok(())
}

fn index_remove(bucket: &kv::Bucket, name: &str) -> Result<(), VaultError> {
    let mut names = read_index(bucket)?;
    let before = names.len();
    names.retain(|n| n != name);
    if names.len() != before {
        write_index(bucket, &names)?;
    }
    Ok(())
}

impl Guest for Component {
    fn put(name: String, value: Vec<u8>) -> Result<SecretMeta, VaultError> {
        let bucket = open()?;
        let current = read_meta(&bucket, &name)?.map(|(v, _)| v).unwrap_or(0);
        let new_version = current + 1;
        let blob = seal(&value)?;
        set_str(&bucket, &version_key(&name, new_version), &blob)?;
        let updated = now();
        write_meta(&bucket, &name, new_version, updated)?;
        if current == 0 {
            index_add(&bucket, &name)?;
        }
        Ok(SecretMeta { name, version: new_version, updated })
    }

    fn get(name: String) -> Result<Vec<u8>, VaultError> {
        let bucket = open()?;
        let (version, _) = read_meta(&bucket, &name)?.ok_or(VaultError::NotFound)?;
        let blob = get_str(&bucket, &version_key(&name, version))?.ok_or(VaultError::NotFound)?;
        unseal(&blob)
    }

    fn get_version(name: String, version: u32) -> Result<Vec<u8>, VaultError> {
        let bucket = open()?;
        let blob = get_str(&bucket, &version_key(&name, version))?.ok_or(VaultError::NotFound)?;
        unseal(&blob)
    }

    fn describe(name: String) -> Result<SecretMeta, VaultError> {
        let bucket = open()?;
        let (version, updated) = read_meta(&bucket, &name)?.ok_or(VaultError::NotFound)?;
        Ok(SecretMeta { name, version, updated })
    }

    fn rotate(name: String, new_value: Vec<u8>) -> Result<(u32, u32), VaultError> {
        let bucket = open()?;
        let prev = read_meta(&bucket, &name)?.map(|(v, _)| v).unwrap_or(0);
        let new_version = prev + 1;
        let blob = seal(&new_value)?;
        set_str(&bucket, &version_key(&name, new_version), &blob)?;
        write_meta(&bucket, &name, new_version, now())?;
        if prev == 0 {
            index_add(&bucket, &name)?;
        }
        Ok((new_version, prev))
    }

    fn delete(name: String) -> Result<(), VaultError> {
        let bucket = open()?;
        if let Some((current, _)) = read_meta(&bucket, &name)? {
            for version in 1..=current {
                delete_key(&bucket, &version_key(&name, version))?;
            }
            delete_key(&bucket, &meta_key(&name))?;
        }
        index_remove(&bucket, &name)?;
        Ok(())
    }

    fn list_names(max: u32) -> Result<Vec<String>, VaultError> {
        let bucket = open()?;
        let mut names = read_index(&bucket)?;
        names.truncate(max as usize);
        Ok(names)
    }
}

bindings::export!(Component with_types_in bindings);
