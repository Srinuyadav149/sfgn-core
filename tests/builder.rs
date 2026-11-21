use std::borrow::Cow;

use sfgn_core::{
    Document, EdgeTarget, ErrorCode, IndentStyle, NodeHandle, SfgnBuilder, SfgnScalar, SfgnValue,
    Value,
};

#[test]
fn minimal_qualification_handles_unique_branch_and_sibling_collisions() {
    let mut b = SfgnBuilder::new();
    b.set_root_prop("project", "atlas-kv");

    // Unique top-level modules -> bare headers `"StorageEngine"` and `"ArchiveEngine"`
    let storage = b.add_node(NodeHandle::ROOT, "StorageEngine");
    let archive = b.add_node(NodeHandle::ROOT, "ArchiveEngine");

    // Unique leaf under StorageEngine -> bare header `"MemTable"`
    let memtable = b.add_node(storage, "MemTable");
    b.add_prop(memtable, "capacity_mb", 64);

    // Cross-branch collision: `"BufferPool"` under both StorageEngine and ArchiveEngine
    // -> `"StorageEngine">"BufferPool"` and `"ArchiveEngine">"BufferPool"`
    let storage_bp = b.add_node(storage, "BufferPool");
    b.add_prop(storage_bp, "pages", 4096);

    let archive_bp = b.add_node(archive, "BufferPool");
    b.add_prop(archive_bp, "pages", 512);

    // Sibling collision under same parent: two `"FlushWorker"` children under StorageEngine
    // -> `"FlushWorker"#0` and `"FlushWorker"#1` (no parent prefix needed since unique across parents)
    let fw0 = b.add_node(storage, "FlushWorker");
    b.add_prop(fw0, "thread_id", 0);

    let fw1 = b.add_node(storage, "FlushWorker");
    b.add_prop(fw1, "thread_id", 1);

    // Relative edge using `NodeHandle` automatically resolves to the qualified target path
    b.add_relative(memtable, storage_bp);
    b.add_relative(fw0, fw1);
    b.add_relative(
        memtable,
        EdgeTarget::external_node("drivers/os_io.sfgn", r###""IoUring">"RingBuffer"#0"###),
    );

    let sfgn_text = b.build().expect("builder produces valid SFGN");
    let doc = Document::parse(&sfgn_text).expect("emitted SFGN passes strict parsing");

    assert_eq!(doc.len(), 8);
    assert!(doc.node_by_path(r###""MemTable""###).unwrap().is_some());
    assert!(
        doc.node_by_path(r###""StorageEngine">"BufferPool""###)
            .unwrap()
            .is_some()
    );
    assert!(
        doc.node_by_path(r###""ArchiveEngine">"BufferPool""###)
            .unwrap()
            .is_some()
    );
    assert!(
        doc.node_by_path(r###""FlushWorker"#0"###)
            .unwrap()
            .is_some()
    );
    assert!(
        doc.node_by_path(r###""FlushWorker"#1"###)
            .unwrap()
            .is_some()
    );

    // Verify `memtable`'s resolved relative edge points to `"StorageEngine">"BufferPool"`
    let mt_view = doc.node_by_path(r###""MemTable""###).unwrap().unwrap();
    let rel_paths: Vec<String> = mt_view
        .relatives()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert_eq!(rel_paths, vec![r###""StorageEngine">"BufferPool""###]);
}

#[test]
fn multi_level_ancestor_qualification_when_immediate_parents_also_collide() {
    let mut b = SfgnBuilder::new();

    // Two top-level services, each with a `"Handler"` child, each with a `"validate"` leaf:
    // `"AuthService">"Handler">"validate"` vs `"TokenStore">"Handler">"validate"`
    let auth = b.add_node(NodeHandle::ROOT, "AuthService");
    let token = b.add_node(NodeHandle::ROOT, "TokenStore");

    let auth_handler = b.add_node(auth, "Handler");
    let token_handler = b.add_node(token, "Handler");

    let auth_validate = b.add_node(auth_handler, "validate");
    b.add_prop(auth_validate, "source_file", "src/auth.rs");

    let token_validate = b.add_node(token_handler, "validate");
    b.add_prop(token_validate, "source_file", "src/token.rs");

    let sfgn_text = b.build().expect("multi-level qualification succeeds");
    let doc = Document::parse(&sfgn_text).unwrap();

    assert!(
        doc.node_by_path(r###""AuthService">"Handler""###)
            .unwrap()
            .is_some()
    );
    assert!(
        doc.node_by_path(r###""TokenStore">"Handler""###)
            .unwrap()
            .is_some()
    );
    assert!(
        doc.node_by_path(r###""AuthService">"Handler">"validate""###)
            .unwrap()
            .is_some()
    );
    assert!(
        doc.node_by_path(r###""TokenStore">"Handler">"validate""###)
            .unwrap()
            .is_some()
    );
}

#[test]
fn combined_sibling_and_cross_branch_qualification() {
    let mut b = SfgnBuilder::new();

    // Two colliding `"Cluster"` siblings at root -> `"Cluster"#0` and `"Cluster"#1`
    let c0 = b.add_node(NodeHandle::ROOT, "Cluster");
    let c1 = b.add_node(NodeHandle::ROOT, "Cluster");

    // Two colliding `"Worker"` siblings under each cluster
    // -> `"Cluster"#0>"Worker"#0`, `"Cluster"#0>"Worker"#1`, `"Cluster"#1>"Worker"#0`, `"Cluster"#1>"Worker"#1`
    let _c0_w0 = b.add_node(c0, "Worker");
    let c0_w1 = b.add_node(c0, "Worker");
    let _c1_w0 = b.add_node(c1, "Worker");
    let c1_w1 = b.add_node(c1, "Worker");

    b.add_relative(c0_w1, c1_w1);

    let sfgn_text = b.build().expect("combined index + ancestor path builds");
    let doc = Document::parse(&sfgn_text).unwrap();

    let w_a = doc
        .node_by_path(r###""Cluster"#0>"Worker"#1"###)
        .unwrap()
        .expect("Cluster#0>Worker#1 exists");
    let rels: Vec<String> = w_a
        .relatives()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert_eq!(rels, vec![r###""Cluster"#1>"Worker"#1"###]);
}

#[test]
fn escapes_multiline_triple_quotes_indexed_props_and_tab_indent() {
    let mut b = SfgnBuilder::new().with_indent(IndentStyle::Tab);
    b.set_root_prop("note", "line1\nline2\t\"quoted\"");
    b.set_root_prop("nullable", SfgnValue::Null);

    let fn_node = b.add_node(NodeHandle::ROOT, "compile_ir");
    b.add_prop(
        fn_node,
        "line_span",
        vec![SfgnScalar::int(12), SfgnScalar::int(48)],
    );
    b.add_indexed_prop(
        fn_node,
        "weights",
        vec![0, 1],
        vec![SfgnScalar::float(0.25), SfgnScalar::float(0.75)],
    );
    b.add_multiline(fn_node, "code", Some("rust"), "fn demo() {\n\"\"\"\n}\n");

    let sfgn_text = b.build().expect("tab-indented multiline graph builds");
    assert!(sfgn_text.contains("\t\"line_span\": [12, 48]\n"));

    let doc = Document::parse(&sfgn_text).unwrap();
    let node = doc.node_by_path(r###""compile_ir""###).unwrap().unwrap();
    match node.property("code").unwrap().decode().unwrap() {
        Value::Multiline(ml) => {
            assert_eq!(ml.tag, Some("rust"));
            assert_eq!(ml.decode_text(), Cow::Borrowed("fn demo() {\n\"\"\"\n}\n"));
        }
        other => panic!("expected Multiline, got {:?}", other),
    }
}

#[test]
fn rejects_invalid_number_or_format_tag_in_build_validation_gate() {
    let mut bad_num = SfgnBuilder::new();
    bad_num.set_root_prop("bad", SfgnValue::number("NaN"));
    assert_eq!(bad_num.build().unwrap_err().code, ErrorCode::E009);

    let mut bad_tag = SfgnBuilder::new();
    let n = bad_tag.add_node(NodeHandle::ROOT, "Code");
    bad_tag.add_multiline(n, "body", Some("rust # bad"), "let x = 1;");
    assert_eq!(bad_tag.build().unwrap_err().code, ErrorCode::E014);
}
