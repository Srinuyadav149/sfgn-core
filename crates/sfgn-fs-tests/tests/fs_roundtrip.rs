use std::borrow::Cow;

use sfgn_core::{
    Document, ErrorCode, GraphBuilder, IndentStyle, NodeBuilder, OrphanPolicy, OwnedScalar,
    OwnedTarget, OwnedValue, ParseOptions, Scalar, Value, serialize_document,
    serialize_document_with_indent,
};
use sfgn_fs_tests::{FsPortalResolver, TempWorkspace, assert_documents_semantically_equal};

#[test]
fn programmatic_build_serialize_write_load_deserialize_loop() {
    let ws = TempWorkspace::new("sfgn_loop").expect("create temp workspace");

    let graph = GraphBuilder::new(
        NodeBuilder::root()
            .prop_str("repo", "query-engine")
            .prop_str("language", "rust")
            .prop_bool("strict_mode", true)
            .prop_null("deprecated_flag")
            .child(OwnedTarget::local(r###""Planner""###).unwrap())
            .child(OwnedTarget::local(r###""Catalog""###).unwrap())
            .child(OwnedTarget::external("execution_engine.sfgn", None).unwrap()),
    )
    .add_node(
        NodeBuilder::new(r###""Planner""###)
            .unwrap()
            .prop_str("kind", "module")
            .prop_str("path", "src/planner/mod.rs")
            .prop_collection(
                "priority_tuple",
                vec![
                    OwnedScalar::String("P1".to_string()),
                    OwnedScalar::Number("3600".to_string()),
                    OwnedScalar::Bool(true),
                    OwnedScalar::Null,
                ],
            )
            .prop_indexed(
                "cost_matrix",
                vec![0, 1],
                OwnedValue::FlatCollection(vec![
                    OwnedScalar::Number("0.25".to_string()),
                    OwnedScalar::Number("0.75".to_string()),
                ]),
            )
            .child(OwnedTarget::local(r###""LogicalPlan""###).unwrap())
            .child(OwnedTarget::local(r###""optimize""###).unwrap())
            .relative(OwnedTarget::local(r###""Catalog""###).unwrap())
            .relative(
                OwnedTarget::external("execution_engine.sfgn", Some(r###""PhysicalOperator""###))
                    .unwrap(),
            ),
    )
    .add_node(
        NodeBuilder::new(r###""LogicalPlan""###)
            .unwrap()
            .prop_str("kind", "enum")
            .prop_collection(
                "variants",
                vec![
                    OwnedScalar::String("Scan".to_string()),
                    OwnedScalar::String("Filter".to_string()),
                    OwnedScalar::String("Join".to_string()),
                ],
            ),
    )
    .add_node(
        NodeBuilder::new(r###""optimize""###)
            .unwrap()
            .prop_str("kind", "fn")
            .prop_str("note", "Escaped quotes \"here\" and unicode \u{1F680}")
            .prop_multiline(
                "body",
                Some("rust"),
                r###"let pruned = predicate_pushdown(plan);
"""
cost_based_join_order(pruned, catalog)
"###,
            )
            .relative(OwnedTarget::local(r###""LogicalPlan""###).unwrap())
            .relative(OwnedTarget::local(r###""Catalog""###).unwrap()),
    )
    .add_node(
        NodeBuilder::new(r###""Catalog""###)
            .unwrap()
            .prop_str("kind", "struct")
            .prop_number("max_entries", "65536")
            .prop_bool("thread_safe", true),
    );

    let initial_bytes = graph.serialize_bytes().expect("serialize graph");
    let file_path = ws
        .write_bytes("graph/planner.sfgn", &initial_bytes)
        .expect("write planner.sfgn to disk");
    assert!(file_path.exists());

    let disk_bytes = ws
        .read_bytes("graph/planner.sfgn")
        .expect("read planner.sfgn from disk");
    let doc1 = Document::parse_bytes(&disk_bytes).expect("parse disk bytes");

    let tab_serialized =
        serialize_document_with_indent(&doc1, IndentStyle::Tab).expect("serialize with tabs");
    ws.write_sfgn("graph/planner_tab.sfgn", &tab_serialized)
        .expect("write tab-indented copy");

    let disk_tab_bytes = ws
        .read_bytes("graph/planner_tab.sfgn")
        .expect("read tab-indented copy");
    let doc2 = Document::parse_bytes(&disk_tab_bytes).expect("parse tab-indented copy");

    assert_documents_semantically_equal(&doc1, &doc2);

    let opt_node = doc2.node_by_path(r###""optimize""###).unwrap().unwrap();
    match opt_node.property("body").unwrap().decode().unwrap() {
        Value::Multiline(ml) => {
            assert_eq!(ml.tag, Some("rust"));
            assert_eq!(
                ml.decode_text(),
                "let pruned = predicate_pushdown(plan);\n\"\"\"\ncost_based_join_order(pruned, catalog)\n"
            );
        }
        other => panic!("expected Multiline payload, got {:?}", other),
    }
}

#[test]
fn federated_multi_file_subgraph_portals_on_disk() {
    let ws = TempWorkspace::new("sfgn_portals").expect("create temp workspace");

    let storage_primitives = r###">:
  "domain": "storage-primitives"
  > ["WriteAheadLog", "PageCache"]

"WriteAheadLog":
  "durability": "fdatasync"
  "segment_bytes": 67108864
  > []
  >> []

"PageCache":
  "eviction": "clock-pro"
  > []
  >> []
"###;

    let consensus = r###">:
  "domain": "distributed-consensus"
  "updated": "2026-11-20"
  > ["Raft", "ReplicatedLog"]
  > <"storage_primitives.sfgn">

"Raft":
  "kind": "protocol"
  "fault_model": "crash-recovery"
  > []
  >> "ReplicatedLog"

"ReplicatedLog":
  "kind": "data_structure"
  "invariant": "Log Matching Property"
  > []
  >> <"storage_primitives.sfgn">"WriteAheadLog"
"###;

    let consensus_path = ws
        .write_sfgn("kb/consensus.sfgn", consensus)
        .expect("write consensus.sfgn");
    ws.write_sfgn("kb/storage_primitives.sfgn", storage_primitives)
        .expect("write storage_primitives.sfgn");

    let mut resolver = FsPortalResolver::new(ParseOptions::default());
    let consensus_bytes = resolver.load_buffer(&consensus_path).expect("io ok");
    let consensus_doc = resolver
        .parse_buffer(&consensus_bytes)
        .expect("parse consensus.sfgn");

    let root_portal = consensus_doc
        .root()
        .external_portals()
        .next()
        .expect("root portal exists");
    let target_root_slice = resolver
        .resolve_portal_slice(&consensus_path, root_portal.file_path, root_portal.sub_path)
        .expect("io ok")
        .expect("parse target")
        .expect("root slice found");
    assert!(target_root_slice.starts_with(">:\n"));
    assert!(target_root_slice.contains(r###""domain": "storage-primitives""###));

    let log_node = consensus_doc
        .node_by_path(r###""ReplicatedLog""###)
        .unwrap()
        .unwrap();
    let wal_portal = log_node
        .external_portals()
        .next()
        .expect("WAL portal exists");
    let wal_slice = resolver
        .resolve_portal_slice(&consensus_path, wal_portal.file_path, wal_portal.sub_path)
        .expect("io ok")
        .expect("parse target")
        .expect("WriteAheadLog slice found");
    assert!(wal_slice.starts_with(r###""WriteAheadLog":"###));
    assert!(wal_slice.contains(r###""durability": "fdatasync""###));

    let reserialized_consensus = serialize_document(&consensus_doc).unwrap();
    let mirror_path = ws
        .write_sfgn("kb_mirror/consensus.sfgn", &reserialized_consensus)
        .unwrap();
    let mirror_bytes = std::fs::read(mirror_path).unwrap();
    let mirror_doc = Document::parse_bytes(&mirror_bytes).unwrap();
    assert_documents_semantically_equal(&consensus_doc, &mirror_doc);
}

#[test]
fn persistent_memory_crlf_and_orphan_policy_disk_roundtrip() {
    let ws = TempWorkspace::new("sfgn_memory").expect("create temp workspace");

    let crlf_memory = ">:\r\n  \"agent\": \"architect-v2\"\r\n  > \"Decision\"#0\r\n\r\n\"Decision\"#0:\r\n  \"meta\": [\"2026-11-20\", 1.0, \"locked\"]\r\n  > []\r\n\r\n\"ScratchNote\":\r\n  \"status\": \"ephemeral\"\r\n  > []\r\n";
    ws.write_bytes("memory/session.sfgn", crlf_memory.as_bytes())
        .expect("write CRLF session.sfgn");

    let disk_bytes = ws.read_bytes("memory/session.sfgn").unwrap();

    assert_eq!(
        Document::parse_bytes(&disk_bytes).unwrap_err().code,
        ErrorCode::E017
    );

    let adopted_doc = Document::parse_bytes_with_options(
        &disk_bytes,
        ParseOptions {
            orphan_policy: OrphanPolicy::AttachToRoot,
        },
    )
    .expect("AttachToRoot succeeds");

    let normalized_sfgn = serialize_document(&adopted_doc).expect("serialize adopted graph");
    ws.write_sfgn("memory/session_normalized.sfgn", &normalized_sfgn)
        .expect("write normalized session");

    let reloaded_bytes = ws.read_bytes("memory/session_normalized.sfgn").unwrap();
    let strict_reloaded = Document::parse_bytes(&reloaded_bytes)
        .expect("normalized file passes strict validation on disk");
    assert_documents_semantically_equal(&adopted_doc, &strict_reloaded);

    let scratch = strict_reloaded
        .node_by_path(r###""ScratchNote""###)
        .unwrap()
        .unwrap();
    assert_eq!(
        scratch.property("status").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String(Cow::Borrowed("ephemeral")))
    );
}

#[test]
fn filesystem_fail_fast_diagnostics_on_corrupt_files() {
    let ws = TempWorkspace::new("sfgn_errors").expect("create temp workspace");

    let mut bom_bytes = vec![0xEF, 0xBB, 0xBF];
    bom_bytes.extend_from_slice(b">:\n");
    ws.write_bytes("bad/bom.sfgn", &bom_bytes).unwrap();
    let err = Document::parse_bytes(&ws.read_bytes("bad/bom.sfgn").unwrap()).unwrap_err();
    assert_eq!(err.code, ErrorCode::E001);

    let depth2 = r###">:
  "a": 1
    "b": 2
"###;
    ws.write_sfgn("bad/depth2.sfgn", depth2).unwrap();
    let err = Document::parse_bytes(&ws.read_bytes("bad/depth2.sfgn").unwrap()).unwrap_err();
    assert_eq!(err.code, ErrorCode::E006);
    assert_eq!(err.loc.line, 3);

    let dup = r###">:
  > "Node"#0

"Node"#0:
  "v": 1

"Node" #0:
  "v": 2
"###;
    ws.write_sfgn("bad/dup.sfgn", dup).unwrap();
    let err = Document::parse_bytes(&ws.read_bytes("bad/dup.sfgn").unwrap()).unwrap_err();
    assert_eq!(err.code, ErrorCode::E016);
    assert_eq!(err.loc.line, 7);
}
