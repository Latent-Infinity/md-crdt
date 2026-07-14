//! Guards the public façade of the split modules: parser/serialize (doc),
//! wire (session), and validation (sync). These references resolve at compile
//! time, so a dropped `pub use` re-export or a renamed public path fails to
//! build here — and each split is exercised end-to-end so the delegation, not
//! just the symbol paths, stays intact.

use md_crdt::core::StateVector;
use md_crdt::doc::{EquivalenceMode, Parser};
use md_crdt::session::CollaborativeDocument;
use md_crdt::sync::{
    ChangeMessage, MalformedKind, ValidationError, ValidationLimits, validate_changes,
};

#[test]
fn doc_parser_and_serializer_facade_round_trips() {
    // parser split: public `Parser` entry point still resolves and parses.
    let document = Parser::parse("# Title\n\n- a\n- b");
    // serialize split: public serialization surface still produces canonical text.
    let rendered = document.serialize(EquivalenceMode::Structural);
    assert_eq!(
        rendered,
        Parser::parse(&rendered).serialize(EquivalenceMode::Structural),
        "parse/serialize façade must remain idempotent after the module split"
    );
}

#[test]
fn session_wire_facade_still_applies_operations() {
    // session/wire split is internal; the caller-visible façade is
    // CollaborativeDocument, whose ops route through the extracted wire helpers.
    let mut a = CollaborativeDocument::new(1);
    a.insert_paragraph(None, "hi").expect("insert paragraph");
    let mut b = CollaborativeDocument::new(2);
    let msg = a.encode_changes_since(&b.state_vector()).unwrap();
    b.apply_remote(msg, &ValidationLimits::default())
        .expect("apply remote through wire translation");
    assert_eq!(
        a.document().serialize(EquivalenceMode::Structural),
        b.document().serialize(EquivalenceMode::Structural)
    );
}

#[test]
fn sync_validation_facade_is_reexported_with_stable_defaults() {
    // validation split: types and the entry point remain at md_crdt::sync::*.
    let limits = ValidationLimits::default();
    // Guard the security-relevant default bounds against silent drift in the move.
    assert_eq!(limits.max_ops_per_message, 10_000);
    assert_eq!(limits.max_payload_bytes, 10 * 1024 * 1024);
    assert_eq!(limits.max_pending_buffer, 100_000);

    let empty = ChangeMessage {
        since: StateVector::new(),
        ops: Vec::new(),
    };
    validate_changes(&empty, &limits, 0).expect("empty change set validates");

    // The error taxonomy is still reachable at the re-exported path.
    let err = ValidationError::MalformedOperation {
        op_id: md_crdt::core::OpId {
            counter: 1,
            peer: 1,
        },
        kind: MalformedKind::EmptyPayload,
    };
    assert!(matches!(err, ValidationError::MalformedOperation { .. }));
}
