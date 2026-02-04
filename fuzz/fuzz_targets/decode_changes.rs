#![no_main]

use libfuzzer_sys::fuzz_target;
use md_crdt_sync::{ChangeMessage, Document, ValidationLimits, validate_changes};

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = postcard::from_bytes::<ChangeMessage>(data) {
        let _ = validate_changes(&message, &ValidationLimits::default(), 0);
        let mut doc = Document::new();
        let _ = doc.apply_changes(message);
    }
});
