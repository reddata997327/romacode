//! ADB key management — `~/.android/adbkey` and `adbkey.pub`.
//!
//! An ADB key is an ordinary RSA key; what makes it "an ADB key" is the
//! `RSAPublicKey` blob format the handshake exchanges (see [`crate::crypto`]).
//! We generate and manage these ourselves, so a device that has never seen a
//! real `adb` binary can still be authorised by `mandroid`.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rsa::RsaPrivateKey;

use crate::crypto;

#[derive(Clone)]
pub struct AdbKey {
    pub key: RsaPrivateKey,
    pub private_path: PathBuf,
    pub public_path: PathBuf,
}

impl std::fmt::Debug for AdbKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never let a private key reach a log line.
        f.debug_struct("AdbKey")
            .field("private_path", &self.private_path)
            .field("public_path", &self.public_path)
            .field("key", &"<redacted>")
            .finish()
    }
}

impl AdbKey {
    /// The `base64(RSAPublicKey)` text of an `adbkey.pub` file.
    pub fn public_text(&self) -> Result<String> {
        crypto::adbkey_pub_text(&self.key)
    }

    /// Sign an ADB AUTH token (SHA-1 / PKCS#1 v1.5, as the protocol requires).
    pub fn sign_token(&self, token: &[u8]) -> Result<Vec<u8>> {
        crypto::sign_sha1(&self.key, token)
    }
}

/// Default location, matching where the real ADB client keeps its identity.
pub fn default_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ANDROID_VENDOR_KEYS_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".android");
    }
    PathBuf::from(".android")
}

/// Load `adbkey` from `dir`, generating and persisting a fresh 2048-bit key if
/// there is not one there yet.
pub fn load_or_generate(dir: &Path) -> Result<AdbKey> {
    let private_path = dir.join("adbkey");
    let public_path = dir.join("adbkey.pub");
    if private_path.exists() {
        // Repair legacy permissions before reading secret key bytes.
        restrict_permissions(&private_path)?;
        let pem = std::fs::read_to_string(&private_path)
            .with_context(|| format!("reading {}", private_path.display()))?;
        let key = crypto::from_pkcs8_pem(&pem)
            .with_context(|| format!("parsing {}", private_path.display()))?;
        return Ok(AdbKey {
            key,
            private_path,
            public_path,
        });
    }
    let key = crypto::generate_key(crypto::RSA_BITS)?;
    persist(&key, &private_path, &public_path)?;
    Ok(AdbKey {
        key,
        private_path,
        public_path,
    })
}

/// Generate a new key, overwriting anything already at that location.
pub fn generate_at(dir: &Path) -> Result<AdbKey> {
    let private_path = dir.join("adbkey");
    let public_path = dir.join("adbkey.pub");
    let key = crypto::generate_key(crypto::RSA_BITS)?;
    persist(&key, &private_path, &public_path)?;
    Ok(AdbKey {
        key,
        private_path,
        public_path,
    })
}

fn persist(key: &RsaPrivateKey, private_path: &Path, public_path: &Path) -> Result<()> {
    if let Some(parent) = private_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    write_private_key(private_path, &crypto::to_pkcs8_pem(key)?)?;
    std::fs::write(public_path, format!("{}\n", crypto::adbkey_pub_text(key)?))
        .with_context(|| format!("writing {}", public_path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn write_private_key(path: &Path, pem: &str) -> Result<()> {
    if path.exists() {
        restrict_permissions(path)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Restrict permissions at creation time, before any secret bytes exist.
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(pem.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    file.sync_all()
        .with_context(|| format!("flushing {}", path.display()))?;
    restrict_permissions(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_persists_and_reloads() {
        let dir = std::env::temp_dir().join(format!(
            "mandroid-key-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let first = load_or_generate(&dir).unwrap();
        assert!(dir.join("adbkey").exists());
        assert!(dir.join("adbkey.pub").exists());

        // Reload yields the same key, not a new one.
        let second = load_or_generate(&dir).unwrap();
        assert_eq!(
            crypto::public_numbers(&first.key).0,
            crypto::public_numbers(&second.key).0
        );

        // The .pub file is base64 of the 524-byte blob.
        let text = std::fs::read_to_string(dir.join("adbkey.pub")).unwrap();
        assert_eq!(crypto::base64_decode(text.trim()).unwrap().len(), 524);

        // Debug never leaks the key material.
        let rendered = format!("{first:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("RsaPrivateKey"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn private_key_file_is_not_world_readable() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir =
                std::env::temp_dir().join(format!("mandroid-key-perm-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let key = generate_at(&dir).unwrap();
            let mode = std::fs::metadata(&key.private_path)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "private key must be owner-readable and inaccessible to group/other"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn loading_an_existing_private_key_repairs_its_permissions() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir =
                std::env::temp_dir().join(format!("mandroid-key-repair-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let first = generate_at(&dir).unwrap();
            std::fs::set_permissions(&first.private_path, std::fs::Permissions::from_mode(0o644))
                .unwrap();
            load_or_generate(&dir).unwrap();
            assert_eq!(
                std::fs::metadata(&first.private_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn token_signature_verifies_against_the_public_key() {
        use rsa::pkcs1v15::Pkcs1v15Sign;
        use rsa::RsaPublicKey;
        use sha1::Sha1;
        use sha2::Digest;

        let dir = std::env::temp_dir().join(format!("mandroid-key-sig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let adb = generate_at(&dir).unwrap();

        let token = [0x11u8; 20];
        let sig = adb.sign_token(&token).unwrap();
        let pub_key = RsaPublicKey::from(&adb.key);
        let digest = Sha1::digest(token);
        assert!(
            pub_key
                .verify(Pkcs1v15Sign::new::<Sha1>(), &digest, &sig)
                .is_ok(),
            "ADB AUTH signature must verify with SHA-1/PKCS#1 v1.5"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
