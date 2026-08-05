#![no_main]

use libfuzzer_sys::fuzz_target;
use md_crdt::codec::{BinaryOpCodec, OpCodec};
use md_crdt::{decode_bin_v1_change_message_to_json_payloads, decode_change_message_bin_v1};

fuzz_target!(|data: &[u8]| {
    let _ = BinaryOpCodec.decode(data);
    let _ = decode_change_message_bin_v1(data);
    let _ = decode_bin_v1_change_message_to_json_payloads(data);
});
