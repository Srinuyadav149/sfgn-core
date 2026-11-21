use sfgn_core::{
    ErrorCode, LineCol, NodeId, OrphanPolicy, ParseOptions, RelationTarget, build_topology,
    parse_canonical_node_path, scan_tape,
};

#[test]
fn canonicalizes_whitespace_escapes_and_indices() {
    let p1 = parse_canonical_node_path(r###""StorageEngine" > "BufferPool" #0#2"###).unwrap();
    let p2 = parse_canonical_node_path(r###""StorageEngine">"BufferPool"#0#2"###).unwrap();
    let p3 = parse_canonical_node_path(r###""\u{0053}torageEngine">"BufferPool"#0#2"###).unwrap();

    assert_eq!(p1, p2);
    assert_eq!(p2, p3);
    assert_eq!(p1.to_string(), r###""StorageEngine">"BufferPool"#0#2"###);
}

#[test]
fn rejects_duplicate_canonical_identifiers_with_e016() {
    let doc = r###">:
  > ["Worker"#0, "Worker"#1]

"Worker"#0:
  "id": 0

"Worker" #0:
  "id": 99
"###;
    let tape = scan_tape(doc).unwrap();
    let err = build_topology(doc, &tape, ParseOptions::default()).unwrap_err();
    assert_eq!(err.code, ErrorCode::E016);
    assert_eq!(err.loc, LineCol::new(7, 1));
}

#[test]
fn rejects_orphan_nodes_with_e017_under_strict_policy() {
    let unreferenced = r###">:
  > "Connected"

"Connected":
  > []

"Lonely":
  > []
"###;
    let tape1 = scan_tape(unreferenced).unwrap();
    let err1 = build_topology(unreferenced, &tape1, ParseOptions::default()).unwrap_err();
    assert_eq!(err1.code, ErrorCode::E017);
    assert_eq!(err1.loc, LineCol::new(7, 1));

    let relative_only = r###">:
  > "Connected"

"Connected":
  > []
  >> "RelativeOnly"

"RelativeOnly":
  > []
"###;
    let tape2 = scan_tape(relative_only).unwrap();
    let err2 = build_topology(relative_only, &tape2, ParseOptions::default()).unwrap_err();
    assert_eq!(err2.code, ErrorCode::E017);
    assert_eq!(err2.loc, LineCol::new(8, 1));

    let cycle = r###">:
  > []

"CycleA":
  > "CycleB"

"CycleB":
  > "CycleA"
"###;
    let tape3 = scan_tape(cycle).unwrap();
    let err3 = build_topology(cycle, &tape3, ParseOptions::default()).unwrap_err();
    assert_eq!(err3.code, ErrorCode::E017);
    assert_eq!(err3.loc, LineCol::new(4, 1));
}

#[test]
fn attach_to_root_policy_adopts_orphans_and_preserves_subtrees() {
    let doc = r###">:
  > "Main"

"Main":
  > []

"OrphanParent":
  > "OrphanChild"

"OrphanChild":
  > []
"###;
    let tape = scan_tape(doc).unwrap();
    let opts = ParseOptions {
        orphan_policy: OrphanPolicy::AttachToRoot,
    };
    let topo = build_topology(doc, &tape, opts).expect("AttachToRoot must succeed");

    let orphan_parent = topo.lookup_str(r###""OrphanParent""###).unwrap().unwrap();
    let orphan_child = topo.lookup_str(r###""OrphanChild""###).unwrap().unwrap();

    assert_eq!(topo.adopted_orphans(), &[orphan_parent]);
    assert_eq!(topo.parent_of(orphan_parent), Some(NodeId::ROOT));
    assert_eq!(topo.parent_of(orphan_child), Some(orphan_parent));
    assert_eq!(
        topo.local_child_ids(NodeId::ROOT),
        &[NodeId(1), orphan_parent]
    );
}

#[test]
fn builds_full_topology_for_spec_reference_document() {
    let doc = r###">:
  "project": "atlas-kv"
  "version": "1.0.0"
  > "StorageEngine"
  > "ArchiveEngine"
  > "WAL"
  > "Compactor"
  > <"drivers/os_io.sfgn">

"StorageEngine":
  "kind": "subsystem"
  > ["MemTable", "StorageEngine">"BufferPool", "FlushWorker"#0, "FlushWorker"#1]
  >> "WAL"
  >> <"drivers/os_io.sfgn">

"MemTable":
  > []
  >> ["WAL", "Compactor", <"drivers/os_io.sfgn">"IoUring">"RingBuffer"#0]

"StorageEngine">"BufferPool":
  > []

"FlushWorker"#0:
  > []

"FlushWorker"#1:
  > []

"ArchiveEngine":
  > "ArchiveEngine">"BufferPool"

"ArchiveEngine">"BufferPool":
  > []

"WAL":
  > []
  >> <"drivers/os_io.sfgn">"DiskWriter"

"Compactor":
  > []
  >> "StorageEngine"
"###;
    let tape = scan_tape(doc).unwrap();
    let topo = build_topology(doc, &tape, ParseOptions::default()).unwrap();

    assert!(topo.adopted_orphans().is_empty());
    assert_eq!(topo.local_child_ids(NodeId::ROOT).len(), 4);

    let storage_id = topo.lookup_str(r###""StorageEngine""###).unwrap().unwrap();
    let storage_bp = topo
        .lookup_str(r###""StorageEngine" > "BufferPool""###)
        .unwrap()
        .unwrap();
    let archive_bp = topo
        .lookup_str(r###""ArchiveEngine">"BufferPool""###)
        .unwrap()
        .unwrap();

    assert_ne!(storage_bp, archive_bp);
    assert_eq!(topo.parent_of(storage_bp), Some(storage_id));

    let memtable_id = topo.lookup_str(r###""MemTable""###).unwrap().unwrap();
    let memtable_rels = topo.relatives(memtable_id);
    assert_eq!(memtable_rels.len(), 3);
    match &memtable_rels[2] {
        RelationTarget::External {
            file_path,
            sub_path,
            ..
        } => {
            assert_eq!(file_path, "drivers/os_io.sfgn");
            assert_eq!(
                sub_path.as_ref().unwrap().to_string(),
                r###""IoUring">"RingBuffer"#0"###
            );
        }
        other => panic!("expected External target, got {:?}", other),
    }
}
