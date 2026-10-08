//! Property-based "fuzzing" on stable Rust (PRD §17): every parser that sees
//! peer or user input must never panic, and its output must stay safe.
//! Coverage-guided fuzz targets for nightly live in `fuzz/`.

use ghostchat::{
    codename,
    contact_key::ContactKey,
    proto::{self, Frame, PairFrame},
    safety, sanitize,
};
use proptest::prelude::*;

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 4000,
        ..ProptestConfig::default()
    }
}

/// Strings biased towards the characters that matter for these parsers.
fn nasty_string() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            any::<char>(),
            Just('\u{1b}'),
            Just('\u{9b}'),
            Just('['),
            Just(']'),
            Just('\u{7}'),
            Just('\u{202e}'),
            Just('\u{2066}'),
            Just('\u{200b}'),
            Just('.'),
            Just('/'),
            Just('\\'),
            Just(':'),
            Just('\n'),
            Just('\r'),
            Just('\0'),
            prop::char::range('A', 'Z'),
            prop::char::range('0', '9'),
        ],
        0..200,
    )
    .prop_map(|v| v.into_iter().collect())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn contact_key_parser_never_panics(s in nasty_string()) {
        if let Ok(ck) = ContactKey::parse(&s) {
            prop_assert_eq!(ContactKey::parse(&ck.encode()).unwrap(), ck);
        }
    }

    #[test]
    fn contact_key_mutations_are_rejected(i in 0usize..80, c in prop::char::range('A', 'Z')) {
        let ck = ContactKey::new(iroh::SecretKey::from_bytes(&[7; 32]).public(), "NIGHTOWL".into());
        let s = ck.encode();
        let i = i % s.len();
        if s.as_bytes()[i] != c as u8 && s.as_bytes()[i] != b'.' {
            let mut b = s.into_bytes();
            b[i] = c as u8;
            let m = String::from_utf8(b).unwrap();
            // Any single-character change must be caught (or yield the same key).
            if let Ok(parsed) = ContactKey::parse(&m) {
                prop_assert_eq!(parsed, ck);
            }
        }
    }

    #[test]
    fn chat_frame_decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        if let Ok(f) = proto::decode::<Frame>(&bytes) {
            let enc = proto::encode(&f).unwrap();
            prop_assert_eq!(proto::decode::<Frame>(&enc[4..]).unwrap(), f);
        }
    }

    #[test]
    fn pair_frame_decoder_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = proto::decode::<PairFrame>(&bytes);
    }

    #[test]
    fn framed_stream_reader_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async {
            let mut r: &[u8] = &bytes;
            // Read until the stream is exhausted or invalid.
            for _ in 0..64 {
                if proto::read_frame::<_, Frame>(&mut r).await.is_err() {
                    break;
                }
            }
        });
    }

    #[test]
    fn sanitized_text_has_no_controls_or_bidi(s in nasty_string()) {
        let out = sanitize::text(&s);
        for c in out.chars() {
            prop_assert!(c == '\n' || !c.is_control(), "control {c:?}");
            prop_assert!(!matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200b}'..='\u{200f}'), "bidi {c:?}");
        }
        prop_assert_eq!(sanitize::text(&out), out.clone(), "idempotent");
    }

    #[test]
    fn sanitized_filename_is_a_single_safe_component(s in nasty_string()) {
        if let Ok(n) = sanitize::filename(&s) {
            prop_assert!(!n.is_empty());
            prop_assert!(!n.contains('/') && !n.contains('\\') && !n.contains(':'));
            prop_assert!(n != "." && n != "..");
            prop_assert!(!n.ends_with('.') && !n.ends_with(' '));
            prop_assert!(n.chars().count() <= sanitize::MAX_FILENAME_CHARS);
            prop_assert!(n.chars().all(|c| !c.is_control()));
            prop_assert!(sanitize::filename(&n).is_ok());
            let p = std::path::Path::new("downloads").join(&n);
            prop_assert_eq!(p.parent(), Some(std::path::Path::new("downloads")));
        }
    }

    #[test]
    fn valid_codenames_follow_the_rules(s in nasty_string()) {
        if let Ok(n) = codename::validate(&s) {
            prop_assert!((3..=16).contains(&n.len()));
            prop_assert!(n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '-' || c == '_'));
        }
    }

    #[test]
    fn safety_check_never_panics(s in nasty_string()) {
        let a = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let b = iroh::SecretKey::from_bytes(&[2; 32]).public();
        let _ = safety::check(&safety::for_pair(&a, &b).theirs, &s);
    }
}
