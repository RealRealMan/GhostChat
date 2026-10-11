//! Persistence (PRD §13, ARCHITECTURE §9).
//!
//! - Identity secret key + contacts storage key: OS secure store (one entry),
//!   or a passphrase-encrypted `vault.bin` when no secure store works.
//! - Contacts: `contacts.bin`, XChaCha20-Poly1305 with the storage key.
//! - Settings: `settings.json`, plain.
//!
//! Nothing here ever touches message content.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, Generate, KeyInit},
};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::{contacts::ContactBook, ui::theme::ThemeName};

const KEYRING_USER: &str = "master";
const VAULT_MAGIC: &[u8; 8] = b"GCVAULT1";
const CONTACTS_MAGIC: &[u8; 8] = b"GCCONTS1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;

#[derive(Debug)]
pub enum StorageError {
    Io(std::io::Error),
    SecureStore(String),
    WrongPassphrase,
    Corrupt(&'static str),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::Io(e) => write!(f, "file error: {e}"),
            StorageError::SecureStore(e) => write!(f, "OS secure store error: {e}"),
            StorageError::WrongPassphrase => write!(f, "wrong passphrase"),
            StorageError::Corrupt(what) => write!(f, "{what} is damaged or was changed"),
        }
    }
}

impl std::error::Error for StorageError {}

impl From<std::io::Error> for StorageError {
    fn from(e: std::io::Error) -> Self {
        StorageError::Io(e)
    }
}

/// Identity secret key and the key that encrypts `contacts.bin`.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secrets {
    pub identity: [u8; 32],
    pub storage_key: [u8; 32],
}

impl Secrets {
    pub fn generate() -> Self {
        let mut s = Secrets {
            identity: [0; 32],
            storage_key: [0; 32],
        };
        rand::fill(&mut s.identity);
        rand::fill(&mut s.storage_key);
        s
    }

    fn to_bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut v = Zeroizing::new(Vec::with_capacity(64));
        v.extend_from_slice(&self.identity);
        v.extend_from_slice(&self.storage_key);
        v
    }

    fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() != 64 {
            return None;
        }
        let mut s = Secrets {
            identity: [0; 32],
            storage_key: [0; 32],
        };
        s.identity.copy_from_slice(&b[..32]);
        s.storage_key.copy_from_slice(&b[32..]);
        Some(s)
    }

    pub fn secret_key(&self) -> iroh::SecretKey {
        iroh::SecretKey::from_bytes(&self.identity)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub codename: Option<String>,
    pub theme: ThemeName,
    pub scanlines: bool,
    pub flicker: bool,
    /// Flash the taskbar / bounce the Dock icon when a message arrives while
    /// the window is in the background.
    pub flash: bool,
    /// Microphone and speakers for calls, by name. None = system default.
    pub mic: Option<String>,
    pub speaker: Option<String>,
    /// PRD §20.5: off by default.
    pub panic_wipes_identity: bool,
    pub auto_connect: bool,
    /// Minutes without input before the Matrix idle screen. 0 = never.
    pub idle_minutes: u32,
    pub pairing_minutes: u32,
    /// Empty = n0 public relays.
    pub relay_urls: Vec<String>,
    /// None = n0 DNS discovery.
    pub pkarr_relay: Option<String>,
    /// Route everything via the relay so contacts never see this IP.
    pub relay_only: bool,
    pub max_file_mb: u64,
    /// None = `<Downloads>/GhostChat`.
    pub downloads_dir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            codename: None,
            theme: ThemeName::Green,
            scanlines: true,
            flicker: false,
            flash: true,
            mic: None,
            speaker: None,
            panic_wipes_identity: false,
            auto_connect: true,
            idle_minutes: 5,
            pairing_minutes: 10,
            relay_urls: Vec::new(),
            pkarr_relay: None,
            relay_only: false,
            max_file_mb: 2048,
            downloads_dir: None,
        }
    }
}

/// Where this installation (or `--profile`) keeps its data.
#[derive(Debug, Clone)]
pub struct Profile {
    pub dir: PathBuf,
    /// OS secure-store service name.
    pub service: String,
}

impl Profile {
    /// `name` is used for running several instances on one machine (testing).
    pub fn new(name: Option<&str>) -> Result<Self, StorageError> {
        let base = directories::ProjectDirs::from("", "", "GhostChat")
            .ok_or_else(|| StorageError::Io(std::io::Error::other("no home directory")))?
            .data_dir()
            .to_path_buf();
        Ok(Self::at(base, name))
    }

    pub fn at(base: PathBuf, name: Option<&str>) -> Self {
        let (dir, service) = match name {
            Some(n) => (base.join("profiles").join(n), format!("GhostChat-{n}")),
            None => (base, "GhostChat".to_string()),
        };
        Profile { dir, service }
    }

    pub fn settings_path(&self) -> PathBuf {
        self.dir.join("settings.json")
    }
    pub fn contacts_path(&self) -> PathBuf {
        self.dir.join("contacts.bin")
    }
    pub fn vault_path(&self) -> PathBuf {
        self.dir.join("vault.bin")
    }

    pub fn load_settings(&self) -> Settings {
        fs::read(self.settings_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_settings(&self, s: &Settings) -> Result<(), StorageError> {
        let json = serde_json::to_vec_pretty(s).map_err(std::io::Error::other)?;
        write_atomic(&self.settings_path(), &json)
    }

    pub fn downloads_dir(&self, s: &Settings) -> PathBuf {
        s.downloads_dir.clone().unwrap_or_else(|| {
            directories::UserDirs::new()
                .and_then(|u| u.download_dir().map(Path::to_path_buf))
                .unwrap_or_else(|| self.dir.clone())
                .join("GhostChat")
        })
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Secret store

/// How the identity secrets are protected on this machine.
#[derive(Debug, Clone)]
pub enum SecretStore {
    /// OS secure store via `keyring`.
    Os { service: String },
    /// `vault.bin` encrypted with a passphrase (no OS store available).
    Passphrase { path: PathBuf },
    /// In-memory only; for tests.
    Memory(std::sync::Arc<std::sync::Mutex<Option<Vec<u8>>>>),
}

pub enum Lookup {
    Found(Secrets),
    /// No identity yet: first run.
    Missing,
    /// Identity exists in `vault.bin`; ask for the passphrase.
    NeedsPassphrase,
    /// OS store failed; fall back to a passphrase vault.
    OsUnavailable(String),
}

impl SecretStore {
    /// Finds the identity for a profile, choosing the backend.
    pub fn locate(profile: &Profile) -> (SecretStore, Lookup) {
        if profile.vault_path().exists() {
            return (
                SecretStore::Passphrase {
                    path: profile.vault_path(),
                },
                Lookup::NeedsPassphrase,
            );
        }
        let os = SecretStore::Os {
            service: profile.service.clone(),
        };
        match os.load_os() {
            Ok(Some(s)) => (os, Lookup::Found(s)),
            Ok(None) => (os, Lookup::Missing),
            Err(e) => (
                SecretStore::Passphrase {
                    path: profile.vault_path(),
                },
                Lookup::OsUnavailable(e.to_string()),
            ),
        }
    }

    fn entry(service: &str) -> Result<keyring::Entry, StorageError> {
        keyring::Entry::new(service, KEYRING_USER)
            .map_err(|e| StorageError::SecureStore(e.to_string()))
    }

    fn load_os(&self) -> Result<Option<Secrets>, StorageError> {
        let SecretStore::Os { service } = self else {
            unreachable!("load_os on non-OS store")
        };
        match Self::entry(service)?.get_secret() {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                Secrets::from_bytes(&bytes)
                    .map(Some)
                    .ok_or(StorageError::Corrupt("the identity in the OS secure store"))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(StorageError::SecureStore(e.to_string())),
        }
    }

    /// Stores new secrets. `passphrase` is required for the passphrase vault.
    pub fn save(&self, s: &Secrets, passphrase: Option<&str>) -> Result<(), StorageError> {
        match self {
            SecretStore::Os { service } => Self::entry(service)?
                .set_secret(&s.to_bytes())
                .map_err(|e| StorageError::SecureStore(e.to_string())),
            SecretStore::Passphrase { path } => {
                let pass = passphrase.ok_or(StorageError::WrongPassphrase)?;
                write_atomic(path, &vault_seal(pass, s)?)
            }
            SecretStore::Memory(m) => {
                *m.lock().expect("memory store") = Some(s.to_bytes().to_vec());
                Ok(())
            }
        }
    }

    pub fn unlock(&self, passphrase: &str) -> Result<Secrets, StorageError> {
        match self {
            SecretStore::Passphrase { path } => vault_open(passphrase, &fs::read(path)?),
            SecretStore::Memory(m) => m
                .lock()
                .expect("memory store")
                .as_deref()
                .and_then(Secrets::from_bytes)
                .ok_or(StorageError::Corrupt("memory store")),
            SecretStore::Os { .. } => self
                .load_os()?
                .ok_or(StorageError::Corrupt("the OS secure store")),
        }
    }

    pub fn delete(&self) -> Result<(), StorageError> {
        match self {
            SecretStore::Os { service } => match Self::entry(service)?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(StorageError::SecureStore(e.to_string())),
            },
            SecretStore::Passphrase { path } => remove_if_exists(path),
            SecretStore::Memory(m) => {
                if let Some(mut v) = m.lock().expect("memory store").take() {
                    v.zeroize();
                }
                Ok(())
            }
        }
    }

    pub fn describe(&self) -> &'static str {
        match self {
            SecretStore::Os { .. } if cfg!(windows) => "Windows Credential Manager",
            SecretStore::Os { .. } if cfg!(target_os = "macos") => "macOS Keychain",
            SecretStore::Os { .. } => "Secret Service",
            SecretStore::Passphrase { .. } => "passphrase-encrypted vault",
            SecretStore::Memory(_) => "memory",
        }
    }
}

fn remove_if_exists(path: &Path) -> Result<(), StorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn derive_vault_key(passphrase: &str, salt: &[u8]) -> Result<Zeroizing<[u8; 32]>, StorageError> {
    let mut key = Zeroizing::new([0u8; 32]);
    argon2::Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, key.as_mut())
        .map_err(|_| StorageError::Corrupt("vault parameters"))?;
    Ok(key)
}

fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let nonce = XNonce::generate();
    let ct = cipher
        .encrypt(
            &nonce,
            chacha20poly1305::aead::Payload {
                msg: plaintext,
                aad,
            },
        )
        .expect("encryption of in-memory buffer cannot fail");
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

fn open(key: &[u8; 32], aad: &[u8], data: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    if data.len() < NONCE_LEN {
        return None;
    }
    let (nonce, ct) = data.split_at(NONCE_LEN);
    let nonce = XNonce::try_from(nonce).ok()?;
    XChaCha20Poly1305::new(key.into())
        .decrypt(&nonce, chacha20poly1305::aead::Payload { msg: ct, aad })
        .ok()
        .map(Zeroizing::new)
}

fn vault_seal(passphrase: &str, s: &Secrets) -> Result<Vec<u8>, StorageError> {
    let mut salt = [0u8; SALT_LEN];
    rand::fill(&mut salt);
    let key = derive_vault_key(passphrase, &salt)?;
    let mut out = Vec::new();
    out.extend_from_slice(VAULT_MAGIC);
    out.extend_from_slice(&salt);
    out.extend_from_slice(&seal(&key, VAULT_MAGIC, &s.to_bytes()));
    Ok(out)
}

fn vault_open(passphrase: &str, data: &[u8]) -> Result<Secrets, StorageError> {
    let rest = data
        .strip_prefix(VAULT_MAGIC.as_slice())
        .ok_or(StorageError::Corrupt("vault.bin"))?;
    if rest.len() < SALT_LEN {
        return Err(StorageError::Corrupt("vault.bin"));
    }
    let (salt, sealed) = rest.split_at(SALT_LEN);
    let key = derive_vault_key(passphrase, salt)?;
    let plain = open(&key, VAULT_MAGIC, sealed).ok_or(StorageError::WrongPassphrase)?;
    Secrets::from_bytes(&plain).ok_or(StorageError::Corrupt("vault.bin"))
}

// ---------------------------------------------------------------------------
// Contacts

pub fn save_contacts(path: &Path, key: &[u8; 32], book: &ContactBook) -> Result<(), StorageError> {
    let json = Zeroizing::new(serde_json::to_vec(book).map_err(std::io::Error::other)?);
    let mut out = Vec::new();
    out.extend_from_slice(CONTACTS_MAGIC);
    out.extend_from_slice(&seal(key, CONTACTS_MAGIC, &json));
    write_atomic(path, &out)
}

pub fn load_contacts(path: &Path, key: &[u8; 32]) -> Result<ContactBook, StorageError> {
    let data = match fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ContactBook::default()),
        Err(e) => return Err(e.into()),
    };
    let sealed = data
        .strip_prefix(CONTACTS_MAGIC.as_slice())
        .ok_or(StorageError::Corrupt("contacts.bin"))?;
    let plain = open(key, CONTACTS_MAGIC, sealed).ok_or(StorageError::Corrupt("contacts.bin"))?;
    serde_json::from_slice(&plain).map_err(|_| StorageError::Corrupt("contacts.bin"))
}

/// Deletes identity and contacts (panic-wipe option, `--wipe-identity`,
/// uninstaller). Settings other than the codename are kept.
pub fn wipe_identity(profile: &Profile) -> Result<(), StorageError> {
    let mut first_err = None;
    let os = SecretStore::Os {
        service: profile.service.clone(),
    };
    for r in [
        os.delete(),
        remove_if_exists(&profile.vault_path()),
        remove_if_exists(&profile.contacts_path()),
    ] {
        if let Err(e) = r {
            first_err.get_or_insert(e);
        }
    }
    let mut s = profile.load_settings();
    if s.codename.take().is_some() {
        let _ = profile.save_settings(&s);
    }
    first_err.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    #[test]
    fn contacts_roundtrip_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contacts.bin");
        let key = [7u8; 32];
        let mut book = ContactBook::default();
        book.add(SecretKey::generate().public(), "NIGHTOWL".into());
        save_contacts(&path, &key, &book).unwrap();

        let raw = fs::read(&path).unwrap();
        assert!(
            !raw.windows(8).any(|w| w == b"NIGHTOWL"),
            "must be encrypted at rest"
        );
        assert_eq!(load_contacts(&path, &key).unwrap(), book);
        assert!(matches!(
            load_contacts(&path, &[8u8; 32]),
            Err(StorageError::Corrupt(_))
        ));
    }

    #[test]
    fn contacts_tamper_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("contacts.bin");
        let key = [7u8; 32];
        save_contacts(&path, &key, &ContactBook::default()).unwrap();
        let mut raw = fs::read(&path).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 1;
        fs::write(&path, raw).unwrap();
        assert!(load_contacts(&path, &key).is_err());
    }

    #[test]
    fn missing_contacts_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let book = load_contacts(&dir.path().join("nope.bin"), &[0; 32]).unwrap();
        assert!(book.is_empty());
    }

    #[test]
    fn vault_roundtrip_and_wrong_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let store = SecretStore::Passphrase {
            path: dir.path().join("vault.bin"),
        };
        let s = Secrets::generate();
        store.save(&s, Some("correct horse")).unwrap();
        let raw = fs::read(dir.path().join("vault.bin")).unwrap();
        assert!(
            !raw.windows(32).any(|w| w == s.identity),
            "secret must not be stored in clear"
        );
        let back = store.unlock("correct horse").unwrap();
        assert_eq!(back.identity, s.identity);
        assert_eq!(back.storage_key, s.storage_key);
        assert!(matches!(
            store.unlock("wrong"),
            Err(StorageError::WrongPassphrase)
        ));
    }

    #[test]
    fn settings_roundtrip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::at(dir.path().to_path_buf(), Some("t"));
        assert_eq!(p.load_settings(), Settings::default());
        assert!(!Settings::default().panic_wipes_identity);
        let s = Settings {
            codename: Some("NIGHTOWL".into()),
            theme: ThemeName::Amber,
            ..Settings::default()
        };
        p.save_settings(&s).unwrap();
        assert_eq!(p.load_settings(), s);
        // Unknown/missing fields fall back to defaults.
        fs::write(p.settings_path(), br#"{"codename":"ABC"}"#).unwrap();
        assert_eq!(p.load_settings().codename.as_deref(), Some("ABC"));
    }

    #[test]
    fn locate_prefers_existing_vault() {
        let dir = tempfile::tempdir().unwrap();
        let p = Profile::at(dir.path().to_path_buf(), Some("vault-test"));
        fs::create_dir_all(&p.dir).unwrap();
        fs::write(p.vault_path(), b"x").unwrap();
        let (store, lookup) = SecretStore::locate(&p);
        assert!(matches!(store, SecretStore::Passphrase { .. }));
        assert!(matches!(lookup, Lookup::NeedsPassphrase));
    }

    /// Touches the real OS secure store; run manually:
    /// `cargo test os_secure_store_roundtrip -- --ignored`
    #[test]
    #[ignore = "uses the real OS secure store"]
    fn os_secure_store_roundtrip() {
        let store = SecretStore::Os {
            service: "GhostChat-selftest".into(),
        };
        let s = Secrets::generate();
        store.save(&s, None).unwrap();
        let back = store.unlock("").unwrap();
        assert_eq!(back.identity, s.identity);
        store.delete().unwrap();
        assert!(store.load_os().unwrap().is_none());
    }
}
