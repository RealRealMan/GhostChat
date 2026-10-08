#![no_main]
use ghostchat::proto::{self, Frame};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(f) = proto::decode::<Frame>(data) {
        let enc = proto::encode(&f).unwrap();
        assert_eq!(proto::decode::<Frame>(&enc[4..]).unwrap(), f);
    }
    // File names and text from peers go through the sanitizers.
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = ghostchat::sanitize::text(s);
        let _ = ghostchat::sanitize::filename(s);
    }
});
