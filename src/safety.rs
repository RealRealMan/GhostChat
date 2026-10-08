//! Safety words and randomart derived from both public keys (ARCHITECTURE §6).

use iroh::PublicKey;

pub const WORDS_PER_SIDE: usize = 6;

/// Everything shown on the verification screen for one contact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Safety {
    /// Words this user reads aloud.
    pub mine: Vec<&'static str>,
    /// Words the contact reads aloud; the user types these. Never displayed.
    pub theirs: Vec<&'static str>,
    /// Identical on both screens.
    pub randomart: String,
}

fn digest(a: &PublicKey, b: &PublicKey) -> [u8; 32] {
    let (lo, hi) = if a.as_bytes() <= b.as_bytes() {
        (a, b)
    } else {
        (b, a)
    };
    let mut h = blake3::Hasher::new_derive_key("ghostchat safety words v1");
    h.update(lo.as_bytes());
    h.update(hi.as_bytes());
    *h.finalize().as_bytes()
}

/// The full 12-word list: first half read by the owner of the smaller key.
pub fn words(a: &PublicKey, b: &PublicKey) -> Vec<&'static str> {
    let d = digest(a, b);
    let list = bip39::Language::English.word_list();
    (0..WORDS_PER_SIDE * 2)
        .map(|i| list[bits11(&d, i * 11) as usize])
        .collect()
}

fn bits11(d: &[u8; 32], start: usize) -> u16 {
    let mut v = 0u16;
    for i in 0..11 {
        let bit = start + i;
        let b = (d[bit / 8] >> (7 - bit % 8)) & 1;
        v = (v << 1) | b as u16;
    }
    v
}

pub fn for_pair(me: &PublicKey, them: &PublicKey) -> Safety {
    let all = words(me, them);
    let (lo_half, hi_half) = all.split_at(WORDS_PER_SIDE);
    let (mine, theirs) = if me.as_bytes() <= them.as_bytes() {
        (lo_half, hi_half)
    } else {
        (hi_half, lo_half)
    };
    Safety {
        mine: mine.to_vec(),
        theirs: theirs.to_vec(),
        randomart: randomart(&digest(me, them), "SAFETY"),
    }
}

/// Checks what the user typed against the expected words. Accepts any case,
/// extra spaces/commas, and the unique 4-letter BIP39 prefixes.
pub fn check(expected: &[&str], typed: &str) -> bool {
    let typed: Vec<String> = typed
        .split(|c: char| c.is_whitespace() || c == ',' || c == '-')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_ascii_lowercase())
        .collect();
    typed.len() == expected.len()
        && expected.iter().zip(&typed).all(|(e, t)| {
            if t.len() >= 4 && t.len() <= e.len() {
                e.starts_with(t.as_str())
            } else {
                *e == t
            }
        })
}

/// OpenSSH "drunken bishop" randomart (17×9) over `data`.
pub fn randomart(data: &[u8], title: &str) -> String {
    const W: usize = 17;
    const H: usize = 9;
    const SYMBOLS: &[u8] = b" .o+=*BOX@%&#/^";
    let mut field = [[0u8; W]; H];
    let (mut x, mut y) = (W / 2, H / 2);
    let (sx, sy) = (x, y);
    for byte in data {
        let mut b = *byte;
        for _ in 0..4 {
            x = if b & 1 == 1 {
                (x + 1).min(W - 1)
            } else {
                x.saturating_sub(1)
            };
            y = if b & 2 == 2 {
                (y + 1).min(H - 1)
            } else {
                y.saturating_sub(1)
            };
            field[y][x] = field[y][x].saturating_add(1);
            b >>= 2;
        }
    }
    let mut out = String::new();
    let label = format!("[{title}]");
    let pad = W.saturating_sub(label.len());
    out.push('+');
    out.push_str(&"-".repeat(pad / 2));
    out.push_str(&label);
    out.push_str(&"-".repeat(pad - pad / 2));
    out.push_str("+\n");
    for (row, line) in field.iter().enumerate() {
        out.push('|');
        for (col, &v) in line.iter().enumerate() {
            let c = if (col, row) == (sx, sy) {
                'S'
            } else if (col, row) == (x, y) {
                'E'
            } else {
                SYMBOLS[(v as usize).min(SYMBOLS.len() - 1)] as char
            };
            out.push(c);
        }
        out.push_str("|\n");
    }
    out.push('+');
    out.push_str(&"-".repeat(W));
    out.push('+');
    out
}

/// Randomart for a single public key (used by `/mykey`).
pub fn key_art(key: &PublicKey) -> String {
    let d = blake3::derive_key("ghostchat key randomart v1", key.as_bytes());
    randomart(&d, "ED25519")
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::SecretKey;

    fn keys() -> (PublicKey, PublicKey) {
        (
            SecretKey::generate().public(),
            SecretKey::generate().public(),
        )
    }

    #[test]
    fn both_sides_derive_the_same_material() {
        for _ in 0..20 {
            let (a, b) = keys();
            assert_eq!(words(&a, &b), words(&b, &a));
            let sa = for_pair(&a, &b);
            let sb = for_pair(&b, &a);
            assert_eq!(sa.randomart, sb.randomart);
            // What I read aloud is exactly what my contact must type.
            assert_eq!(sa.mine, sb.theirs);
            assert_eq!(sa.theirs, sb.mine);
            assert_ne!(sa.mine, sa.theirs);
            assert_eq!(sa.mine.len(), WORDS_PER_SIDE);
        }
    }

    #[test]
    fn deterministic_for_fixed_keys() {
        let a = SecretKey::from_bytes(&[1; 32]).public();
        let b = SecretKey::from_bytes(&[2; 32]).public();
        let first = for_pair(&a, &b);
        for _ in 0..5 {
            assert_eq!(for_pair(&a, &b), first);
        }
    }

    #[test]
    fn different_keys_give_different_words() {
        let (a, b) = keys();
        let (c, _) = keys();
        assert_ne!(words(&a, &b), words(&a, &c));
    }

    #[test]
    fn check_accepts_case_spacing_and_prefixes() {
        let expected = ["abandon", "ability", "zoo", "about", "above", "absent"];
        assert!(check(&expected, "abandon ability zoo about above absent"));
        assert!(check(
            &expected,
            "  ABANDON, Ability  zoo about-above absent "
        ));
        assert!(check(&expected, "aban abil zoo abou abov abse"));
        assert!(!check(&expected, "abandon ability zoo about above"));
        assert!(!check(&expected, "abandon ability zoo about above absurd"));
        assert!(
            !check(&expected, "aba abi zoo abo abo abs"),
            "short prefixes are ambiguous"
        );
        assert!(!check(&expected, ""));
    }

    #[test]
    fn bip39_prefixes_are_unique() {
        let list = bip39::Language::English.word_list();
        let mut seen = std::collections::HashSet::new();
        for w in list {
            assert!(seen.insert(&w[..w.len().min(4)]), "{w}");
        }
    }

    #[test]
    fn randomart_shape() {
        let art = randomart(&[0xAB; 32], "SAFETY");
        let lines: Vec<&str> = art.lines().collect();
        assert_eq!(lines.len(), 11);
        assert!(lines.iter().all(|l| l.chars().count() == 19));
        assert!(art.contains('S'));
    }
}
