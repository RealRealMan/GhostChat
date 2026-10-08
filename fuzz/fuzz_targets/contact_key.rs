#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data)
        && let Ok(ck) = ghostchat::contact_key::ContactKey::parse(s)
    {
        assert_eq!(
            ghostchat::contact_key::ContactKey::parse(&ck.encode()).unwrap(),
            ck
        );
    }
});
