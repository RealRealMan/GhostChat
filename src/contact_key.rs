//! Contact Key: `GHOSTKEY1.<CODENAME>.<PUBKEY_B32>.<CHECKSUM_B32>` (ARCHITECTURE §5).
//!
//! Only public data goes in. The checksum catches typos and truncation; it is
//! not an authenticity check — that is what safety-word verification is for.

use data_encoding::BASE32_NOPAD;
use iroh::PublicKey;

use crate::codename;

pub const PREFIX: &str = "GHOSTKEY1";
pub const FILE_EXTENSION: &str = "ghostkey";
/// Generous upper bound on input before parsing (a valid key is ~80 chars).
const MAX_INPUT: usize = 400;
const CHECKSUM_LEN: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactKey {
    pub key: PublicKey,
    pub codename: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    TooLong,
    Format,
    UnsupportedVersion,
    Codename,
    PublicKey,
    Checksum,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let msg = match self {
            ParseError::TooLong => "that is too long to be a Contact Key",
            ParseError::Format => "that does not look like a Contact Key (GHOSTKEY1....)",
            ParseError::UnsupportedVersion => {
                "this Contact Key is from a newer GhostChat version — please update"
            }
            ParseError::Codename => "the codename inside the Contact Key is invalid",
            ParseError::PublicKey => "the public key inside the Contact Key is invalid",
            ParseError::Checksum => {
                "checksum mismatch — the Contact Key was copied incompletely or changed"
            }
        };
        f.write_str(msg)
    }
}

impl std::error::Error for ParseError {}

fn checksum(key: &PublicKey, codename: &str) -> String {
    let mut h = blake3::Hasher::new_derive_key("ghostchat contact key v1 checksum");
    h.update(PREFIX.as_bytes());
    h.update(key.as_bytes());
    h.update(codename.as_bytes());
    BASE32_NOPAD.encode(&h.finalize().as_bytes()[..CHECKSUM_LEN])
}

impl ContactKey {
    pub fn new(key: PublicKey, codename: String) -> Self {
        ContactKey { key, codename }
    }

    pub fn encode(&self) -> String {
        format!(
            "{PREFIX}.{}.{}.{}",
            self.codename,
            BASE32_NOPAD.encode(self.key.as_bytes()),
            checksum(&self.key, &self.codename)
        )
    }

    /// Contents of a `.ghostkey` file.
    pub fn to_file_contents(&self) -> String {
        format!("{}\n", self.encode())
    }

    /// Parses pasted text or `.ghostkey` file contents. Tolerates whitespace
    /// and line breaks inserted by chat apps, and lowercase.
    pub fn parse(input: &str) -> Result<Self, ParseError> {
        if input.len() > MAX_INPUT {
            return Err(ParseError::TooLong);
        }
        let s: String = input
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_ascii_uppercase();
        let parts: Vec<&str> = s.split('.').collect();
        let [prefix, name, key, sum] = parts.as_slice() else {
            return Err(if s.starts_with("GHOSTKEY") && !s.starts_with(PREFIX) {
                ParseError::UnsupportedVersion
            } else {
                ParseError::Format
            });
        };
        if *prefix != PREFIX {
            return Err(if prefix.starts_with("GHOSTKEY") {
                ParseError::UnsupportedVersion
            } else {
                ParseError::Format
            });
        }
        let codename = codename::validate(name).map_err(|_| ParseError::Codename)?;
        let raw = BASE32_NOPAD
            .decode(key.as_bytes())
            .map_err(|_| ParseError::PublicKey)?;
        let raw: [u8; 32] = raw.try_into().map_err(|_| ParseError::PublicKey)?;
        let key = PublicKey::from_bytes(&raw).map_err(|_| ParseError::PublicKey)?;
        if *sum != checksum(&key, &codename) {
            return Err(ParseError::Checksum);
        }
        Ok(ContactKey { key, codename })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data_encoding::{BASE32_NOPAD, BASE64, HEXLOWER, HEXUPPER};
    use iroh::SecretKey;

    fn sample() -> (SecretKey, ContactKey) {
        let sk = SecretKey::generate();
        let ck = ContactKey::new(sk.public(), "NIGHTOWL".into());
        (sk, ck)
    }

    #[test]
    fn roundtrip() {
        let (_, ck) = sample();
        let s = ck.encode();
        assert!(s.starts_with("GHOSTKEY1.NIGHTOWL."));
        assert_eq!(ContactKey::parse(&s).unwrap(), ck);
        assert_eq!(ContactKey::parse(&ck.to_file_contents()).unwrap(), ck);
    }

    #[test]
    fn tolerates_chat_app_mangling() {
        let (_, ck) = sample();
        let s = ck.encode();
        let (a, b) = s.split_at(30);
        let mangled = format!("  {}\n {}  ", a.to_lowercase(), b);
        assert_eq!(ContactKey::parse(&mangled).unwrap(), ck);
    }

    #[test]
    fn never_contains_private_key() {
        for _ in 0..50 {
            let (sk, ck) = sample();
            let s = ck.encode();
            let secret = sk.to_bytes();
            for enc in [
                BASE32_NOPAD.encode(&secret),
                HEXLOWER.encode(&secret),
                HEXUPPER.encode(&secret),
                BASE64.encode(&secret),
            ] {
                assert!(!s.to_uppercase().contains(&enc.to_uppercase()));
                // Not even a prefix long enough to be meaningful.
                assert!(!s.to_uppercase().contains(&enc.to_uppercase()[..16]));
            }
            // The only key material inside is the public key.
            let parsed = ContactKey::parse(&s).unwrap();
            assert_eq!(parsed.key, sk.public());
        }
    }

    #[test]
    fn detects_single_char_typo() {
        let (_, ck) = sample();
        let s = ck.encode();
        let key_start = "GHOSTKEY1.NIGHTOWL.".len();
        let mut bytes = s.into_bytes();
        bytes[key_start + 10] = if bytes[key_start + 10] == b'A' {
            b'B'
        } else {
            b'A'
        };
        let typo = String::from_utf8(bytes).unwrap();
        assert!(matches!(
            ContactKey::parse(&typo),
            Err(ParseError::Checksum | ParseError::PublicKey)
        ));
    }

    #[test]
    fn detects_codename_swap() {
        let (_, ck) = sample();
        let s = ck.encode().replace("NIGHTOWL", "GHOST-01");
        assert_eq!(ContactKey::parse(&s), Err(ParseError::Checksum));
    }

    #[test]
    fn rejects_truncation_and_garbage() {
        let (_, ck) = sample();
        let s = ck.encode();
        assert!(ContactKey::parse(&s[..s.len() - 3]).is_err());
        assert_eq!(ContactKey::parse("hello"), Err(ParseError::Format));
        assert_eq!(ContactKey::parse(""), Err(ParseError::Format));
        assert_eq!(
            ContactKey::parse(&"A".repeat(1000)),
            Err(ParseError::TooLong)
        );
    }

    #[test]
    fn rejects_other_versions() {
        let (_, ck) = sample();
        let s = ck.encode().replacen("GHOSTKEY1", "GHOSTKEY2", 1);
        assert_eq!(ContactKey::parse(&s), Err(ParseError::UnsupportedVersion));
    }

    #[test]
    fn rejects_invalid_codename() {
        let (_, ck) = sample();
        let s = ck.encode().replace("NIGHTOWL", "X");
        assert_eq!(ContactKey::parse(&s), Err(ParseError::Codename));
    }
}
