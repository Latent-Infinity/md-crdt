#![no_main]

use libfuzzer_sys::fuzz_target;
use md_crdt::doc::{EquivalenceMode, Parser};

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let doc = Parser::parse(&input);
    let _ = doc.serialize(EquivalenceMode::Structural);
});
