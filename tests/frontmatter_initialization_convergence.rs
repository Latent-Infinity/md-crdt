use md_crdt::doc::{EquivalenceMode, Frontmatter, FrontmatterError};
use md_crdt::session::{CollaborativeDocument, DocumentDto, SessionError, SessionSnapshot};
use md_crdt::{
    ChangeMessage, CheckpointRequest, DocumentTombstonePolicy, OpId, StateVector, ValidationLimits,
};

fn deliver(session: &mut CollaborativeDocument, message: &ChangeMessage) {
    session
        .apply_remote(message.clone(), &ValidationLimits::default())
        .unwrap();
}

fn compact_restore(session: &mut CollaborativeDocument) {
    session
        .checkpoint_history(&CheckpointRequest {
            max_retained_ops: 0,
            active_peer_leases: Vec::new(),
            tombstones: DocumentTombstonePolicy::KeepAll,
        })
        .unwrap();
    let snapshot = session.save_snapshot().unwrap();
    assert!(snapshot.ops.is_empty());
    let bytes = snapshot.to_bytes().unwrap();
    *session =
        CollaborativeDocument::restore_from_snapshot(SessionSnapshot::from_bytes(&bytes).unwrap())
            .unwrap();
}

fn permutations(items: &mut [usize], start: usize, output: &mut Vec<Vec<usize>>) {
    if start == items.len() {
        output.push(items.to_vec());
        return;
    }
    for index in start..items.len() {
        items.swap(start, index);
        permutations(items, start + 1, output);
        items.swap(start, index);
    }
}

#[test]
fn competing_initializers_and_explicit_writes_converge_in_every_delivery_order() {
    let mut left = CollaborativeDocument::new(1);
    left.initialize_frontmatter(Frontmatter::parse(
        "# losing base\ntitle: left\nleft_only: no".into(),
    ))
    .unwrap();
    let left_init = left.encode_changes_since(&StateVector::new()).unwrap();
    let left_seen = left.state_vector();
    left.set_frontmatter_field("title", Some("edited".into()))
        .unwrap();
    let title_write = left.encode_changes_since(&left_seen).unwrap();

    let mut right = CollaborativeDocument::new(2);
    right
        .initialize_frontmatter(Frontmatter::parse(
            "# winning base\ntitle: right # title comment\nuntouched: kept # untouched comment\nremoved: old".into(),
        ))
        .unwrap();
    let right_init = right.encode_changes_since(&StateVector::new()).unwrap();
    let right_seen = right.state_vector();
    right.set_frontmatter_field("removed", None).unwrap();
    let deletion = right.encode_changes_since(&right_seen).unwrap();
    let messages = [left_init, title_write, right_init, deletion];

    deliver(&mut left, &messages[2]);
    deliver(&mut left, &messages[3]);
    deliver(&mut right, &messages[0]);
    deliver(&mut right, &messages[1]);
    assert_eq!(left.state_vector(), right.state_vector());
    assert_eq!(left.document(), right.document());
    let expected = left.document().frontmatter.as_ref().unwrap().clone();
    assert_eq!(expected.get("title"), Some("edited"));
    assert_eq!(expected.get("removed"), None);
    assert_eq!(expected.get("left_only"), None);
    assert_eq!(
        expected.render(),
        "# winning base\ntitle: edited # title comment\nuntouched: kept # untouched comment"
    );

    let mut orders = Vec::new();
    permutations(&mut [0, 1, 2, 3], 0, &mut orders);
    for order in orders {
        for restore_between in [false, true] {
            let mut receiver = CollaborativeDocument::new(9);
            for (position, index) in order.iter().enumerate() {
                deliver(&mut receiver, &messages[*index]);
                if restore_between && position == 1 {
                    compact_restore(&mut receiver);
                }
            }
            assert_eq!(receiver.state_vector(), left.state_vector(), "{order:?}");
            assert_eq!(receiver.document().frontmatter.as_ref(), Some(&expected));
            for mode in [EquivalenceMode::Exact, EquivalenceMode::Structural] {
                assert_eq!(
                    receiver.document().serialize(mode),
                    left.document().serialize(mode)
                );
            }
            compact_restore(&mut receiver);
            assert_eq!(receiver.document().frontmatter.as_ref(), Some(&expected));
        }
    }
}

#[test]
fn field_writes_before_initialization_do_not_claim_the_base() {
    let mut editor = CollaborativeDocument::new(3);
    editor
        .set_frontmatter_field("added", Some("before initialization".into()))
        .unwrap();
    editor.set_frontmatter_field("deleted", None).unwrap();
    let fields = editor.encode_changes_since(&StateVector::new()).unwrap();
    let fields_seen = editor.state_vector();
    compact_restore(&mut editor);
    assert!(!editor.document().frontmatter.as_ref().unwrap().has_base());
    assert!(
        editor
            .initialize_frontmatter(Frontmatter::parse(
                "# preserved\nadded: base\ndeleted: base\nuntouched: yes".into(),
            ))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        editor.document().frontmatter_field("added"),
        Some("before initialization")
    );
    assert_eq!(editor.document().frontmatter_field("deleted"), None);
    let initializer = editor.encode_changes_since(&fields_seen).unwrap();
    for field_first in [false, true] {
        let mut receiver = CollaborativeDocument::new(4);
        let messages = if field_first {
            [&fields, &initializer]
        } else {
            [&initializer, &fields]
        };
        for message in messages {
            deliver(&mut receiver, message);
            compact_restore(&mut receiver);
        }
        assert_eq!(receiver.state_vector(), editor.state_vector());
        assert_eq!(receiver.document(), editor.document());
        assert!(
            receiver
                .document()
                .frontmatter
                .as_ref()
                .unwrap()
                .render()
                .contains("# preserved")
        );
    }
}

#[test]
fn initializer_history_recovers_own_clocks_even_for_a_losing_base() {
    let mut left = CollaborativeDocument::new(7);
    let left_id = left
        .initialize_frontmatter(Frontmatter::parse("title: left".into()))
        .unwrap()
        .unwrap();
    let mut right = CollaborativeDocument::new(8);
    for value in ["one", "two", "three"] {
        right
            .set_frontmatter_field("other", Some(value.into()))
            .unwrap();
    }
    let right_id = right
        .initialize_frontmatter(Frontmatter::parse("title: right".into()))
        .unwrap()
        .unwrap();
    deliver(
        &mut left,
        &right.encode_changes_since(&StateVector::new()).unwrap(),
    );
    compact_restore(&mut left);
    assert_eq!(
        left.peek_next_id(),
        OpId {
            peer: 7,
            counter: left_id.counter + 1
        }
    );
    assert_eq!(left.document().frontmatter_field("title"), Some("right"));
    for (peer, previous) in [(7, left_id), (8, right_id)] {
        let imported = CollaborativeDocument::import_state(
            DocumentDto::from_document(left.document()),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            peer,
            true,
        )
        .unwrap();
        assert_eq!(
            imported.peek_next_id(),
            OpId {
                peer,
                counter: previous.counter + 1
            }
        );
    }
}

#[test]
fn opaque_winning_base_retains_supported_writes_without_mutating_yaml() {
    let mut editor = CollaborativeDocument::new(1);
    editor
        .initialize_frontmatter(Frontmatter::parse("title: old".into()))
        .unwrap();
    let editor_init = editor.encode_changes_since(&StateVector::new()).unwrap();
    let seen = editor.state_vector();
    editor
        .set_frontmatter_field("title", Some("edited".into()))
        .unwrap();
    let write = editor.encode_changes_since(&seen).unwrap();
    let mut opaque = CollaborativeDocument::new(2);
    let raw = "# opaque base\nparent:\n  child: value\ntitle: opaque";
    opaque
        .initialize_frontmatter(Frontmatter::parse(raw.into()))
        .unwrap();
    let opaque_init = opaque.encode_changes_since(&StateVector::new()).unwrap();
    let mut expected = None;
    for write_first in [false, true] {
        let mut receiver = CollaborativeDocument::new(9);
        deliver(&mut receiver, &editor_init);
        let messages = if write_first {
            [&write, &opaque_init]
        } else {
            [&opaque_init, &write]
        };
        for message in messages {
            deliver(&mut receiver, message);
            compact_restore(&mut receiver);
        }
        let frontmatter = receiver.document().frontmatter.as_ref().unwrap();
        assert_eq!(frontmatter.render(), raw);
        assert_eq!(frontmatter.get("title"), Some("opaque"));
        if let Some(previous) = &expected {
            assert_eq!(frontmatter, previous);
        } else {
            expected = Some(frontmatter.clone());
        }
        let before = receiver.save_snapshot().unwrap();
        assert!(matches!(
            receiver.set_frontmatter_field("title", Some("unsupported".into())),
            Err(SessionError::Frontmatter(FrontmatterError::Opaque))
        ));
        assert_eq!(receiver.save_snapshot().unwrap(), before);

        // A third peer proposes a higher-counter structured base without seeing
        // either initializer. Previously hidden writes must remain available.
        let mut structured = CollaborativeDocument::new(3);
        for _ in 0..3 {
            structured.set_frontmatter_field("advance", None).unwrap();
        }
        structured
            .initialize_frontmatter(Frontmatter::parse("# structured\ntitle: seed".into()))
            .unwrap();
        deliver(
            &mut receiver,
            &structured
                .encode_changes_since(&StateVector::new())
                .unwrap(),
        );
        compact_restore(&mut receiver);
        assert_eq!(
            receiver.document().frontmatter_field("title"),
            Some("edited")
        );
        assert!(
            receiver
                .document()
                .frontmatter
                .as_ref()
                .unwrap()
                .render()
                .contains("# structured")
        );
    }
}

#[test]
fn hidden_remote_write_preserves_an_imported_opaque_exact_source() {
    let original =
        "---\r\n# keep\r\nparent:\r\n  child: value\r\ntitle: opaque\r\n---\r\n\r\nbody\r\n";
    let parsed = md_crdt::doc::Parser::parse(original);
    let mut receiver = CollaborativeDocument::import_state(
        DocumentDto::from_document(&parsed),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        9,
        true,
    )
    .unwrap();
    let mut writer = CollaborativeDocument::new(1);
    writer
        .set_frontmatter_field("title", Some("edited".into()))
        .unwrap();
    deliver(
        &mut receiver,
        &writer.encode_changes_since(&StateVector::new()).unwrap(),
    );
    compact_restore(&mut receiver);
    assert_eq!(
        receiver.document().serialize(EquivalenceMode::Exact),
        original
    );
    assert_eq!(
        receiver.document().frontmatter_field("title"),
        Some("opaque")
    );
}

#[test]
fn initializer_embedded_field_clocks_do_not_become_document_authority() {
    let mut base = Frontmatter::parse("# supplied base\ntitle: old".into());
    base.set(
        "title".into(),
        Some("supplied projection".into()),
        OpId {
            peer: 7,
            counter: 9000,
        },
    )
    .unwrap();
    let mut source = CollaborativeDocument::new(7);
    let initializer = source.initialize_frontmatter(base).unwrap().unwrap();
    assert_eq!(
        initializer,
        OpId {
            peer: 7,
            counter: 1
        }
    );
    let message = source.encode_changes_since(&StateVector::new()).unwrap();
    let mut receiver = CollaborativeDocument::new(8);
    deliver(&mut receiver, &message);
    compact_restore(&mut source);
    compact_restore(&mut receiver);
    assert_eq!(source.document(), receiver.document());
    assert_eq!(
        source.document().frontmatter_field("title"),
        Some("supplied projection")
    );
    assert_eq!(
        source.peek_next_id(),
        OpId {
            peer: 7,
            counter: 2
        }
    );
    let imported = CollaborativeDocument::import_state(
        DocumentDto::from_document(receiver.document()),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        7,
        true,
    )
    .unwrap();
    assert_eq!(
        imported.peek_next_id(),
        OpId {
            peer: 7,
            counter: 2
        }
    );
}
