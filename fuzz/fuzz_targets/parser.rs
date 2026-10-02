#![no_main]

use libfuzzer_sys::fuzz_target;
use md_crdt::doc::{EquivalenceMode, Parser};

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let Ok(doc) = Parser::try_parse(&input) else { return; };
    let _ = doc.serialize(EquivalenceMode::Structural);
});
