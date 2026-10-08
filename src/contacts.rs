//! Contacts and trust states (PRD §8, ARCHITECTURE §8).

use iroh::PublicKey;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trust {
    /// Paired but not yet verified with safety words.
    Added,
    Verified,
    /// A new key claimed the codename of a verified contact. Blocked until verified.
    KeyChanged,
}

impl Trust {
    pub fn label(self) -> &'static str {
        match self {
            Trust::Added => "UNVERIFIED",
            Trust::Verified => "VERIFIED",
            Trust::KeyChanged => "KEY CHANGED",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    pub key: PublicKey,
    /// Last codename the peer announced (validated).
    pub codename: String,
    /// Local display-name override chosen by this user.
    pub alias: Option<String>,
    pub trust: Trust,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome {
    /// A brand new contact in state `Added`.
    New,
    /// The key was already a contact; only the codename was refreshed.
    Known,
    /// The key is new but claims the codename of a verified contact. The old
    /// contact was replaced by this key in state `KeyChanged`.
    KeyChanged { old_key: PublicKey },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustError {
    UnknownContact,
}

impl std::fmt::Display for TrustError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown contact")
    }
}

impl std::error::Error for TrustError {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactBook {
    contacts: Vec<Contact>,
}

impl ContactBook {
    pub fn iter(&self) -> impl Iterator<Item = &Contact> {
        self.contacts.iter()
    }

    pub fn len(&self) -> usize {
        self.contacts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.contacts.is_empty()
    }

    pub fn get(&self, key: &PublicKey) -> Option<&Contact> {
        self.contacts.iter().find(|c| &c.key == key)
    }

    fn get_mut(&mut self, key: &PublicKey) -> Option<&mut Contact> {
        self.contacts.iter_mut().find(|c| &c.key == key)
    }

    /// Allowlist check used before any application data is exchanged.
    pub fn can_connect(&self, key: &PublicKey) -> bool {
        self.get(key)
            .is_some_and(|c| matches!(c.trust, Trust::Added | Trust::Verified))
    }

    /// What [`Self::add`] would do, without changing anything. Lets the UI warn
    /// before a key change is applied.
    pub fn classify(&self, key: &PublicKey, codename: &str) -> AddOutcome {
        if self.get(key).is_some() {
            return AddOutcome::Known;
        }
        match self
            .contacts
            .iter()
            .find(|c| c.trust == Trust::Verified && c.codename == codename)
        {
            Some(old) => AddOutcome::KeyChanged { old_key: old.key },
            None => AddOutcome::New,
        }
    }

    /// Adds a contact from a Contact Key or an accepted pair request.
    /// `codename` must already be validated.
    pub fn add(&mut self, key: PublicKey, codename: String) -> AddOutcome {
        let outcome = self.classify(&key, &codename);
        match &outcome {
            AddOutcome::Known => {
                if let Some(c) = self.get_mut(&key) {
                    c.codename = codename;
                }
            }
            AddOutcome::New => self.contacts.push(Contact {
                key,
                codename,
                alias: None,
                trust: Trust::Added,
            }),
            AddOutcome::KeyChanged { old_key } => {
                let pos = self
                    .contacts
                    .iter()
                    .position(|c| &c.key == old_key)
                    .expect("classified contact exists");
                let alias = self.contacts[pos].alias.take();
                self.contacts[pos] = Contact {
                    key,
                    codename,
                    alias,
                    trust: Trust::KeyChanged,
                };
            }
        }
        outcome
    }

    /// Marks a contact verified after the safety words matched.
    pub fn verify(&mut self, key: &PublicKey) -> Result<(), TrustError> {
        let c = self.get_mut(key).ok_or(TrustError::UnknownContact)?;
        c.trust = Trust::Verified;
        Ok(())
    }

    pub fn remove(&mut self, key: &PublicKey) -> bool {
        let before = self.contacts.len();
        self.contacts.retain(|c| &c.key != key);
        before != self.contacts.len()
    }

    /// Records a codename announced by the peer. Returns the previous codename
    /// if it changed.
    pub fn update_codename(&mut self, key: &PublicKey, codename: String) -> Option<String> {
        let c = self.get_mut(key)?;
        if c.codename == codename {
            return None;
        }
        Some(std::mem::replace(&mut c.codename, codename))
    }

    pub fn set_alias(&mut self, key: &PublicKey, alias: Option<String>) -> bool {
        match self.get_mut(key) {
            Some(c) => {
                c.alias = alias;
                true
            }
            None => false,
        }
    }

    /// Name shown in the UI. Adds a short key suffix when two contacts would
    /// otherwise look identical (PRD §7).
    pub fn display_name(&self, key: &PublicKey) -> String {
        let Some(c) = self.get(key) else {
            return format!("UNKNOWN·{}", short_id(key));
        };
        let base = c.alias.clone().unwrap_or_else(|| c.codename.clone());
        let clashes = self
            .contacts
            .iter()
            .filter(|o| o.alias.as_ref().unwrap_or(&o.codename) == &base)
            .count();
        if clashes > 1 {
            format!("{base}·{}", short_id(key))
        } else {
            base
        }
    }

    /// Resolves a name typed by the user (display name, codename or alias,
    /// case-insensitive). Ambiguous names resolve to `None`.
    pub fn find(&self, name: &str) -> Option<PublicKey> {
        let name = name.trim().to_uppercase();
        if let Some(c) = self
            .contacts
            .iter()
            .find(|c| self.display_name(&c.key).to_uppercase() == name)
        {
            return Some(c.key);
        }
        let mut matches = self.contacts.iter().filter(|c| {
            c.codename == name || c.alias.as_ref().is_some_and(|a| a.to_uppercase() == name)
        });
        let first = matches.next()?;
        matches.next().is_none().then_some(first.key)
    }
}

/// First 4 hex characters of a public key, for disambiguation.
pub fn short_id(key: &PublicKey) -> String {
    data_encoding::HEXUPPER.encode(&key.as_bytes()[..2])
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn key() -> PublicKey {
        SecretKey::generate().public()
    }

    #[test]
    fn add_then_verify() {
        let mut b = ContactBook::default();
        let k = key();
        assert_eq!(b.add(k, "NIGHTOWL".into()), AddOutcome::New);
        assert_eq!(b.get(&k).unwrap().trust, Trust::Added);
        assert!(b.can_connect(&k));
        b.verify(&k).unwrap();
        assert_eq!(b.get(&k).unwrap().trust, Trust::Verified);
    }

    #[test]
    fn unknown_key_cannot_connect() {
        let b = ContactBook::default();
        assert!(!b.can_connect(&key()));
    }

    #[test]
    fn re_adding_known_key_updates_codename_only() {
        let mut b = ContactBook::default();
        let k = key();
        b.add(k, "NIGHTOWL".into());
        b.verify(&k).unwrap();
        assert_eq!(b.add(k, "GHOST-01".into()), AddOutcome::Known);
        let c = b.get(&k).unwrap();
        assert_eq!(c.codename, "GHOST-01");
        assert_eq!(c.trust, Trust::Verified);
    }

    #[test]
    fn new_key_with_verified_codename_is_key_change_and_blocked() {
        let mut b = ContactBook::default();
        let old = key();
        b.add(old, "NIGHTOWL".into());
        b.verify(&old).unwrap();
        b.set_alias(&old, Some("CONTACT-1".into()));

        let new = key();
        assert_eq!(
            b.add(new, "NIGHTOWL".into()),
            AddOutcome::KeyChanged { old_key: old }
        );
        assert!(b.get(&old).is_none(), "old key must no longer be trusted");
        let c = b.get(&new).unwrap();
        assert_eq!(c.trust, Trust::KeyChanged);
        assert_eq!(c.alias.as_deref(), Some("CONTACT-1"));
        assert!(
            !b.can_connect(&new),
            "changed key is blocked until verified"
        );
        assert!(!b.can_connect(&old));

        b.verify(&new).unwrap();
        assert!(b.can_connect(&new));
    }

    #[test]
    fn same_codename_as_unverified_contact_is_a_separate_contact() {
        let mut b = ContactBook::default();
        let a = key();
        let c = key();
        b.add(a, "NIGHTOWL".into());
        assert_eq!(b.add(c, "NIGHTOWL".into()), AddOutcome::New);
        assert_eq!(b.len(), 2);
        assert_ne!(b.display_name(&a), b.display_name(&c));
        assert!(b.display_name(&a).starts_with("NIGHTOWL·"));
        assert_eq!(b.find("NIGHTOWL"), None, "ambiguous name");
        assert_eq!(b.find(&b.display_name(&a).to_lowercase()), Some(a));
    }

    #[test]
    fn remove_blocks_connection() {
        let mut b = ContactBook::default();
        let k = key();
        b.add(k, "NIGHTOWL".into());
        assert!(b.remove(&k));
        assert!(!b.can_connect(&k));
        assert_eq!(b.verify(&k), Err(TrustError::UnknownContact));
    }

    #[test]
    fn codename_change_is_reported() {
        let mut b = ContactBook::default();
        let k = key();
        b.add(k, "NIGHTOWL".into());
        assert_eq!(b.update_codename(&k, "NIGHTOWL".into()), None);
        assert_eq!(
            b.update_codename(&k, "GHOST-01".into()),
            Some("NIGHTOWL".into())
        );
        assert_eq!(b.find("ghost-01"), Some(k));
    }

    #[test]
    fn alias_overrides_display() {
        let mut b = ContactBook::default();
        let k = key();
        b.add(k, "NIGHTOWL".into());
        b.set_alias(&k, Some("DAD".into()));
        assert_eq!(b.display_name(&k), "DAD");
        assert_eq!(b.find("dad"), Some(k));
        assert_eq!(b.find("nightowl"), Some(k));
    }
}
