use sfgn_core::{
    ByteRange, Diagnostic, ErrorCode, LineCol, NodeEntry, NodeId, PropertyEntry, PropertyKind,
    RelationEntry, RelationKind, Span, StructuralTape, TapeRange,
};

#[test]
fn error_catalog_codes_and_names_match_spec() {
    let cases = [
        (ErrorCode::E001, "E001", "InvalidEncodingOrControlByte"),
        (ErrorCode::E002, "E002", "MissingOrDuplicateRootNode"),
        (ErrorCode::E003, "E003", "UnexpectedColumnZeroToken"),
        (ErrorCode::E004, "E004", "InvalidInitialIndent"),
        (ErrorCode::E005, "E005", "MismatchedIndentation"),
        (ErrorCode::E006, "E006", "DepthTwoIndentation"),
        (ErrorCode::E007, "E007", "UnterminatedQuotedString"),
        (ErrorCode::E008, "E008", "InvalidEscapeSequence"),
        (ErrorCode::E009, "E009", "MalformedNodeHeader"),
        (ErrorCode::E010, "E010", "MalformedIndexChain"),
        (ErrorCode::E011, "E011", "NestedCollectionError"),
        (ErrorCode::E012, "E012", "UnterminatedCollection"),
        (ErrorCode::E013, "E013", "UnterminatedMultilineBlock"),
        (ErrorCode::E014, "E014", "InvalidMultilineHeader"),
        (ErrorCode::E015, "E015", "MalformedExternalRef"),
        (ErrorCode::E016, "E016", "DuplicateNodeIdentifier"),
        (ErrorCode::E017, "E017", "OrphanNodeError"),
    ];

    for (variant, expected_code, expected_name) in cases {
        assert_eq!(variant.code(), expected_code);
        assert_eq!(variant.name(), expected_name);
    }

    let diag = Diagnostic::new(
        ErrorCode::E017,
        LineCol::new(14, 1),
        128,
        r###"node "Orphan" is unreachable from >:"###,
    );
    assert_eq!(
        diag.to_string(),
        r###"[E017] 14:1 OrphanNodeError: node "Orphan" is unreachable from >:"###
    );
}

#[test]
fn spans_and_structural_tape_slice_zero_copy() {
    let src = r###">:
  "project": "atlas-kv"
  > "WAL"
"WAL":
  > []
"###;
    let wal_start = src.find(r###""WAL":"###).expect("WAL header present");
    assert_eq!(wal_start, 37);

    let mut tape = StructuralTape::with_capacity(2, 1, 2);

    tape.nodes.push(NodeEntry {
        path_span: Span::new(0, 2),
        full_span: ByteRange::new(0, wal_start),
        prop_range: TapeRange::new(0, 1),
        rel_range: TapeRange::new(0, 1),
        header_loc: LineCol::new(1, 1),
    });
    tape.nodes.push(NodeEntry {
        path_span: Span::new(wal_start, 5),
        full_span: ByteRange::new(wal_start, src.len()),
        prop_range: TapeRange::new(1, 0),
        rel_range: TapeRange::new(1, 1),
        header_loc: LineCol::new(4, 1),
    });

    let key_start = src.find(r###""project""###).unwrap();
    let val_start = src.find(r###""atlas-kv""###).unwrap();
    tape.properties.push(PropertyEntry {
        key_span: Span::new(key_start, 9),
        val_span: Span::new(val_start, 10),
        tag_span: None,
        kind: PropertyKind::Scalar,
        loc: LineCol::new(2, 3),
    });

    let rel1_start = src.find("\"WAL\"\n").unwrap();
    let rel2_start = src.find("[]").unwrap();
    tape.relations.push(RelationEntry {
        kind: RelationKind::ChildSingle,
        target_span: Span::new(rel1_start, 5),
        loc: LineCol::new(3, 3),
    });
    tape.relations.push(RelationEntry {
        kind: RelationKind::ChildArray,
        target_span: Span::new(rel2_start, 2),
        loc: LineCol::new(5, 3),
    });

    let root = tape.root().expect("root node exists");
    assert_eq!(root.path_span.slice_str(src), ">:");
    assert_eq!(
        root.full_span.slice_str(src),
        r###">:
  "project": "atlas-kv"
  > "WAL"
"###
    );

    let root_props = tape.node_properties(root);
    assert_eq!(root_props.len(), 1);
    assert_eq!(root_props[0].key_span.slice_str(src), r###""project""###);
    assert_eq!(root_props[0].val_span.slice_str(src), r###""atlas-kv""###);

    let root_rels = tape.node_relations(root);
    assert_eq!(root_rels.len(), 1);
    assert!(root_rels[0].kind.is_child());
    assert!(!root_rels[0].kind.is_array());
    assert_eq!(root_rels[0].target_span.slice_str(src), r###""WAL""###);

    let wal = tape.node(NodeId(1)).expect("WAL node exists");
    assert_eq!(wal.path_span.slice_str(src), r###""WAL""###);
    assert_eq!(
        wal.full_span.slice_str(src),
        r###""WAL":
  > []
"###
    );
    assert!(tape.node_properties(wal).is_empty());
    assert_eq!(tape.node_relations(wal)[0].target_span.slice_str(src), "[]");
}
