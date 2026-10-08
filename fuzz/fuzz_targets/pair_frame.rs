#![no_main]
use ghostchat::proto::{self, PairFrame};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = proto::decode::<PairFrame>(data);
});
