//! Codename rules (PRD §7): 3–16 chars of `A–Z 0–9 - _`, shown uppercase.
//!
//! Codenames are not identities. Anything a peer sends is untrusted and must
//! pass [`validate`] before it is stored or displayed.

pub const MIN_LEN: usize = 3;
pub const MAX_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodenameError {
    Length,
    InvalidChar(char),
}

impl std::fmt::Display for CodenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodenameError::Length => write!(f, "codename must be {MIN_LEN}-{MAX_LEN} characters"),
            CodenameError::InvalidChar(_) => {
                write!(f, "codename may only use A-Z, 0-9, '-' and '_'")
            }
        }
    }
}

impl std::error::Error for CodenameError {}

/// Normalises user input (trim + uppercase) and validates it.
pub fn normalize(input: &str) -> Result<String, CodenameError> {
    validate(&input.trim().to_ascii_uppercase())
}

/// Strict validation: the string must already be uppercase. Used for peer data.
pub fn validate(name: &str) -> Result<String, CodenameError> {
    // Check characters first so a long string of garbage reports the real problem.
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '-' || *c == '_'))
    {
        return Err(CodenameError::InvalidChar(c));
    }
    if !(MIN_LEN..=MAX_LEN).contains(&name.len()) {
        return Err(CodenameError::Length);
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_examples_from_prd() {
        for n in [
            "NIGHTOWL",
            "GHOST-01",
            "ZER0COOL",
            "A_B",
            "ABCDEFGHIJKLMNOP",
        ] {
            assert_eq!(validate(n).unwrap(), n);
        }
    }

    #[test]
    fn normalizes_user_input() {
        assert_eq!(normalize("  nightowl ").unwrap(), "NIGHTOWL");
    }

    #[test]
    fn rejects_bad_length() {
        assert_eq!(validate("AB"), Err(CodenameError::Length));
        assert_eq!(validate("ABCDEFGHIJKLMNOPQ"), Err(CodenameError::Length));
        assert_eq!(validate(""), Err(CodenameError::Length));
    }

    #[test]
    fn rejects_bad_chars_and_lowercase_from_peers() {
        assert!(validate("night owl").is_err());
        assert!(validate("nightowl").is_err());
        assert!(validate("NIGHT.OWL").is_err());
        assert!(validate("NIGHT\u{202e}OWL").is_err());
        assert!(validate("ÉCLAIR").is_err());
        assert!(validate("NIGHT\x1b[31m").is_err());
    }
}
