//! Instance identity (ADR 0002 decisions 5/6, ticket #54).
//!
//! The daemon mints an ed25519 keypair silently on first launch. The node ID
//! (hex public key) is the instance's federation identity — the same value an
//! iroh endpoint will present, since iroh secret keys are ed25519 seeds.
//! Users see petnames, never keys; the only surfaces are `grimoire identity`
//! (show/export/import) and the optional fingerprint check in contact details.
//!
//! Storage: macOS Keychain (service `ie.null.grimoire`), falling back to a
//! 0600 file next to the database for machines where the keychain is
//! unavailable (CI, Linux without a secret service). Never in ks.db, never
//! logged.

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use std::path::{Path, PathBuf};

const KEYCHAIN_SERVICE: &str = "ie.null.grimoire";
const KEYCHAIN_USER: &str = "identity";
const KEY_FILE: &str = "identity.key";

pub struct Identity {
    signing: SigningKey,
}

impl Identity {
    /// Load the instance identity, minting one on first run. `db_dir` is the
    /// directory holding ks.db — the file fallback lives beside it.
    /// `GRIMOIRE_IDENTITY_FILE` bypasses the keychain entirely — the seam for
    /// running a second instance on one machine (testing federation locally).
    pub fn load_or_create(db_dir: &Path) -> Result<Self> {
        if let Some(path) = std::env::var_os("GRIMOIRE_IDENTITY_FILE") {
            return Self::load_or_create_file(Path::new(&path));
        }
        let file = db_dir.join(KEY_FILE);
        // a key file beside the db always wins: it is how a headless box (no
        // keychain) and a migrated identity are expressed
        if file.exists() {
            return Self::load_or_create_file(&file);
        }
        // Only macOS has a keychain backend compiled in (`apple-native`). On
        // any other OS `keyring` falls back to an IN-MEMORY mock that happily
        // "stores" the seed and forgets it at exit — a hub would mint a new
        // node id on every restart and orphan its members. File only there.
        if !cfg!(target_os = "macos") {
            return Self::load_or_create_file(&file);
        }
        match keychain_entry() {
            Ok(entry) => match entry.get_password() {
                Ok(hex_seed) => Self::from_hex(&hex_seed),
                Err(keyring::Error::NoEntry) => {
                    let id = Self::generate();
                    match entry.set_password(&id.seed_hex()) {
                        Ok(()) => Ok(id),
                        // keychain present but refusing (locked, headless
                        // session): persist to the file rather than minting a
                        // throwaway identity every start
                        Err(e) => {
                            tracing::warn!("keychain refused the identity ({e}); keeping it in {}", file.display());
                            write_secret_file(&file, &id.seed_hex())?;
                            Ok(id)
                        }
                    }
                }
                // Linux without a native backend compiled in, a locked
                // keychain, no session bus: none of these are "no identity",
                // so fall back to the file instead of disabling federation
                Err(e) => {
                    tracing::warn!("keychain unavailable ({e}); using {}", file.display());
                    Self::load_or_create_file(&file)
                }
            },
            Err(_) => Self::load_or_create_file(&file),
        }
    }

    fn load_or_create_file(path: &Path) -> Result<Self> {
        if path.exists() {
            let hex_seed = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            return Self::from_hex(hex_seed.trim());
        }
        let id = Self::generate();
        write_secret_file(path, &id.seed_hex())?;
        Ok(id)
    }

    fn generate() -> Self {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).expect("OS entropy");
        Self {
            signing: SigningKey::from_bytes(&seed),
        }
    }

    fn from_hex(hex_seed: &str) -> Result<Self> {
        let bytes = hex::decode(hex_seed).context("identity seed is not hex")?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("identity seed is not 32 bytes"))?;
        Ok(Self {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    fn seed_hex(&self) -> String {
        hex::encode(self.signing.to_bytes())
    }

    /// The federation identity: hex ed25519 public key (an iroh node ID).
    pub fn node_id(&self) -> String {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    /// Short human-checkable form for contact verification, grouped for
    /// reading aloud: `3fb2 91cc 04ad e77b`.
    pub fn fingerprint(&self) -> String {
        fingerprint_of(&self.node_id())
    }

    /// The raw seed for constructing an iroh SecretKey (ticket #56).
    #[allow(dead_code)]
    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// Write the seed to a file for machine migration ("move your identity
    /// with your data"). 0600; the user is choosing to hold a secret.
    pub fn export(&self, path: &Path) -> Result<()> {
        write_secret_file(path, &self.seed_hex())
    }

    /// Read an exported identity file without persisting anything.
    pub fn from_export(path: &Path) -> Result<Self> {
        let hex_seed = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::from_hex(hex_seed.trim())
    }

    /// Adopt an exported identity, replacing this machine's key in the
    /// keychain (or file fallback).
    pub fn import(path: &Path, db_dir: &Path) -> Result<Self> {
        let id = Self::from_export(path)?;
        match keychain_entry() {
            Ok(entry) => entry
                .set_password(&id.seed_hex())
                .context("storing imported identity in keychain")?,
            Err(_) => write_secret_file(&db_dir.join(KEY_FILE), &id.seed_hex())?,
        }
        Ok(id)
    }
}

/// Grouped short fingerprint of a hex node ID.
pub fn fingerprint_of(node_id: &str) -> String {
    node_id
        .chars()
        .take(16)
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

fn keychain_entry() -> Result<keyring::Entry> {
    Ok(keyring::Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_USER)?)
}

pub(crate) fn write_secret_file(path: &Path, contents: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Test/CI seam: a purely file-backed load that never touches the keychain.
#[allow(dead_code)]
pub fn load_or_create_file_only(db_dir: &Path) -> Result<Identity> {
    Identity::load_or_create_file(&PathBuf::from(db_dir).join(KEY_FILE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("grimoire-id-test-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn file_identity_is_stable_across_loads() {
        let dir = tmp();
        let a = load_or_create_file_only(&dir).unwrap();
        let b = load_or_create_file_only(&dir).unwrap();
        assert_eq!(a.node_id(), b.node_id());
        assert_eq!(a.node_id().len(), 64); // 32 bytes hex
    }

    #[test]
    fn export_import_round_trips() {
        let dir = tmp();
        let original = load_or_create_file_only(&dir).unwrap();
        let export_path = dir.join("exported.key");
        original.export(&export_path).unwrap();

        // from_export is import minus persistence — never touches the
        // keychain, so it is safe to exercise on a dev machine
        let imported = Identity::from_export(&export_path).unwrap();
        assert_eq!(imported.node_id(), original.node_id());
    }

    #[test]
    fn key_file_is_owner_only() {
        let dir = tmp();
        load_or_create_file_only(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn fingerprint_is_grouped_prefix() {
        let dir = tmp();
        let id = load_or_create_file_only(&dir).unwrap();
        let fp = id.fingerprint();
        assert_eq!(fp.len(), 19); // 4 groups of 4 + 3 spaces
        assert_eq!(fp.replace(' ', ""), id.node_id()[..16]);
    }
}
