use sfgn_core::{ErrorCode, PropertyKind, RelationKind, scan_tape};

const SPEC_REFERENCE_DOC: &str = r###">:
  "project": "atlas-kv"
  "version": "1.0.0"
  > "StorageEngine"
  > "ArchiveEngine"
  > "WAL"
  > "Compactor"
  > <"drivers/os_io.sfgn">

"StorageEngine":
  "kind": "subsystem"
  "thread_safe": true
  "metrics_weights"#0: [0.4, 0.6]
  "metrics_weights"#1: [0.8, 0.2]
  > ["MemTable", "StorageEngine">"BufferPool", "FlushWorker"#0, "FlushWorker"#1]
  >> "WAL"
  >> <"drivers/os_io.sfgn">

"MemTable":
  "kind": "struct"
  "capacity_mb": 64
  "flush_policy": ["size_threshold", 67108864, true]
  "step": "Accept write batch"
  "step": "Insert into skiplist"
  "step": "Signal Compactor when full"
  "sample_dump": """csv
key,seq,op
user:101,8841,PUT
user:102,8842,DEL
"""
  > []
  >> ["WAL", "Compactor", <"drivers/os_io.sfgn">"IoUring">"RingBuffer"#0]

"StorageEngine">"BufferPool":
  "role": "active_read_cache"
  "pages": 4096
  > []
  >> []

"FlushWorker"#0:
  "thread_id": 0
  > []
  >> "WAL"

"FlushWorker"#1:
  "thread_id": 1
  > []
  >> "WAL"

"ArchiveEngine":
  "kind": "subsystem"
  > "ArchiveEngine">"BufferPool"
  >> []

"ArchiveEngine">"BufferPool":
  "role": "cold_compaction_cache"
  "pages": 512
  > []
  >> []

"WAL":
  "kind": "subsystem"
  "sync_mode": "fdatasync"
  > []
  >> <"drivers/os_io.sfgn">"DiskWriter"

"Compactor":
  "kind": "worker"
  "max_threads": 4
  > []
  >> "StorageEngine"
"###;

#[test]
fn scans_complete_spec_reference_document() {
    let tape = scan_tape(SPEC_REFERENCE_DOC).expect("reference document must scan cleanly");
    assert_eq!(tape.nodes.len(), 10);

    let root = tape.root().unwrap();
    assert_eq!(root.path_span.slice_str(SPEC_REFERENCE_DOC), ">:");
    assert_eq!(tape.node_properties(root).len(), 2);
    assert_eq!(tape.node_relations(root).len(), 5);

    let memtable = &tape.nodes[2];
    assert_eq!(
        memtable.path_span.slice_str(SPEC_REFERENCE_DOC),
        r###""MemTable""###
    );
    let props = tape.node_properties(memtable);
    assert_eq!(props.len(), 7);
    assert_eq!(props[2].kind, PropertyKind::FlatCollection);
    assert_eq!(
        props[2].val_span.slice_str(SPEC_REFERENCE_DOC),
        r###"["size_threshold", 67108864, true]"###
    );

    let dump = &props[6];
    assert_eq!(dump.kind, PropertyKind::MultilineBlock);
    assert_eq!(dump.tag_span.unwrap().slice_str(SPEC_REFERENCE_DOC), "csv");
    assert_eq!(
        dump.val_span.slice_str(SPEC_REFERENCE_DOC),
        "key,seq,op\nuser:101,8841,PUT\nuser:102,8842,DEL\n"
    );

    let rels = tape.node_relations(memtable);
    assert_eq!(rels.len(), 2);
    assert_eq!(rels[0].kind, RelationKind::ChildArray);
    assert_eq!(rels[1].kind, RelationKind::RelativeArray);
    assert_eq!(
        rels[1].target_span.slice_str(SPEC_REFERENCE_DOC),
        r###"["WAL", "Compactor", <"drivers/os_io.sfgn">"IoUring">"RingBuffer"#0]"###
    );

    assert_eq!(tape.nodes.first().unwrap().full_span.start_byte, 0);
    for pair in tape.nodes.windows(2) {
        assert_eq!(pair[0].full_span.end_byte, pair[1].full_span.start_byte);
    }
    assert_eq!(
        tape.nodes.last().unwrap().full_span.end_byte,
        SPEC_REFERENCE_DOC.len()
    );
}

#[test]
fn enforces_root_node_and_column_zero_errors_e002_e003_e009() {
    let empty_doc = r###"# empty

"###;
    assert_eq!(scan_tape(empty_doc).unwrap_err().code, ErrorCode::E002);

    let node_before_root = r###""A":
  > []
>:
"###;
    assert_eq!(
        scan_tape(node_before_root).unwrap_err().code,
        ErrorCode::E002
    );

    let dup_root = r###">:
>:
"###;
    assert_eq!(scan_tape(dup_root).unwrap_err().code, ErrorCode::E002);

    let bad_col0 = r###">:
foo:
"###;
    assert_eq!(scan_tape(bad_col0).unwrap_err().code, ErrorCode::E003);

    let missing_colon = r###">:
"A"
"###;
    assert_eq!(scan_tape(missing_colon).unwrap_err().code, ErrorCode::E009);

    let trailing_gt = r###">:
"A">:
"###;
    assert_eq!(scan_tape(trailing_gt).unwrap_err().code, ErrorCode::E009);
}

#[test]
fn enforces_collection_multiline_and_external_ref_errors_e011_to_e015() {
    let nested_prop = r###">:
  "a": [[1, 2]]
"###;
    assert_eq!(scan_tape(nested_prop).unwrap_err().code, ErrorCode::E011);

    let nested_rel = r###">:
  > [{"A"}]
"###;
    assert_eq!(scan_tape(nested_rel).unwrap_err().code, ErrorCode::E011);

    let unterm_prop = r###">:
  "a": [1, 2
"###;
    assert_eq!(scan_tape(unterm_prop).unwrap_err().code, ErrorCode::E012);

    let unterm_rel = r###">:
  > ["A", "B"
"###;
    assert_eq!(scan_tape(unterm_rel).unwrap_err().code, ErrorCode::E012);

    let unterm_ml = r###">:
  "code": """rust
fn main() {}
"###;
    assert_eq!(scan_tape(unterm_ml).unwrap_err().code, ErrorCode::E013);

    let escaped_ml = r###">:
  "doc": """md
\"""
"""
"###;
    let tape = scan_tape(escaped_ml).expect("escaped triple quote inside payload is valid");
    assert_eq!(
        tape.properties[0].val_span.slice_str(escaped_ml),
        r###"\"""
"###
    );

    let unclosed_ext = r###">:
  > <"other.sfgn"
"###;
    assert_eq!(scan_tape(unclosed_ext).unwrap_err().code, ErrorCode::E015);

    let extra_gt_ext = r###">:
  > <"other.sfgn">>"Node"
"###;
    assert_eq!(scan_tape(extra_gt_ext).unwrap_err().code, ErrorCode::E015);
}
