use std::borrow::Cow;

use sfgn_core::{
    Document, ErrorCode, ExternalPortal, OrphanPolicy, ParseOptions, PropertyView, RelationAxis,
    Scalar, Value,
};

const FULL_SPEC_EXAMPLE: &str = r###">:
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
fn progressive_retrieval_and_zero_copy_slicing() {
    let doc = Document::parse(FULL_SPEC_EXAMPLE).expect("spec example must parse");
    assert_eq!(doc.len(), 10);

    let root = doc.root();
    assert!(root.is_root());
    assert_eq!(root.raw_header_path(), ">:");
    assert_eq!(
        root.property("project").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String(Cow::Borrowed("atlas-kv")))
    );

    let root_children: Vec<String> = root
        .children()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert_eq!(
        root_children,
        vec![
            r###""StorageEngine""###,
            r###""ArchiveEngine""###,
            r###""WAL""###,
            r###""Compactor""###,
        ]
    );

    let root_portals: Vec<ExternalPortal<'_>> = root.external_portals().collect();
    assert_eq!(root_portals.len(), 1);
    assert_eq!(root_portals[0].axis, RelationAxis::Child);
    assert_eq!(root_portals[0].file_path, "drivers/os_io.sfgn");
    assert!(root_portals[0].sub_path.is_none());

    let raw_wal_slice = doc
        .node_slice_by_path(r###""WAL""###)
        .unwrap()
        .expect("WAL exists");
    assert!(raw_wal_slice.starts_with("\"WAL\":\n"));
    assert!(raw_wal_slice.contains(r###""sync_mode": "fdatasync""###));

    let base_ptr = FULL_SPEC_EXAMPLE.as_ptr() as usize;
    let slice_ptr = raw_wal_slice.as_ptr() as usize;
    assert!(slice_ptr > base_ptr && slice_ptr < base_ptr + FULL_SPEC_EXAMPLE.len());
}

#[test]
fn decodes_scalars_collections_repeated_keys_indexed_keys_and_multiline() {
    let doc = Document::parse(FULL_SPEC_EXAMPLE).unwrap();

    let storage = doc.node_by_path(r###""StorageEngine""###).unwrap().unwrap();
    let weight_props: Vec<PropertyView<'_>> = storage.properties_named("metrics_weights").collect();
    assert_eq!(weight_props.len(), 2);
    assert_eq!(weight_props[0].key_indices(), vec![0]);
    assert_eq!(weight_props[1].key_indices(), vec![1]);

    match weight_props[0].decode().unwrap() {
        Value::FlatCollection(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].as_number().unwrap().as_f64(), Some(0.4));
            assert_eq!(items[1].as_number().unwrap().as_f64(), Some(0.6));
        }
        other => panic!("expected FlatCollection, got {:?}", other),
    }

    let memtable = doc.node_by_path(r###""MemTable""###).unwrap().unwrap();
    match memtable.property("flush_policy").unwrap().decode().unwrap() {
        Value::FlatCollection(tuple) => {
            assert_eq!(tuple[0].as_str(), Some("size_threshold"));
            assert_eq!(tuple[1].as_number().unwrap().as_i64(), Some(67108864));
            assert_eq!(tuple[2].as_bool(), Some(true));
        }
        other => panic!("expected heterogeneous tuple, got {:?}", other),
    }

    let steps: Vec<String> = memtable
        .properties_named("step")
        .map(|p| match p.decode().unwrap() {
            Value::Scalar(Scalar::String(s)) => s.into_owned(),
            other => panic!("expected string scalar, got {:?}", other),
        })
        .collect();
    assert_eq!(
        steps,
        vec![
            "Accept write batch",
            "Insert into skiplist",
            "Signal Compactor when full"
        ]
    );

    match memtable.property("sample_dump").unwrap().decode().unwrap() {
        Value::Multiline(payload) => {
            assert_eq!(payload.tag, Some("csv"));
            assert_eq!(
                payload.decode_text(),
                Cow::Borrowed("key,seq,op\nuser:101,8841,PUT\nuser:102,8842,DEL\n")
            );
        }
        other => panic!("expected Multiline, got {:?}", other),
    }
}

#[test]
fn unescapes_strings_and_escaped_triple_quotes_lazily() {
    let src = r###">:
  "plain": "zero_copy"
  "escaped": "line1\n\t\u{1F680}"
  "nullable": null
  "code": """md
  \"""
  heading
"""
"###;
    let doc = Document::parse(src).unwrap();
    let root = doc.root();

    match root.property("plain").unwrap().decode().unwrap() {
        Value::Scalar(Scalar::String(cow)) => {
            assert!(matches!(cow, Cow::Borrowed("zero_copy")));
        }
        other => panic!("unexpected {:?}", other),
    }

    match root.property("escaped").unwrap().decode().unwrap() {
        Value::Scalar(Scalar::String(cow)) => {
            assert_eq!(cow, "line1\n\t🚀");
        }
        other => panic!("unexpected {:?}", other),
    }

    assert_eq!(
        root.property("nullable").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::Null)
    );

    match root.property("code").unwrap().decode().unwrap() {
        Value::Multiline(ml) => {
            assert_eq!(ml.tag, Some("md"));
            assert_eq!(ml.decode_text(), "  \"\"\"\n  heading\n");
        }
        other => panic!("unexpected {:?}", other),
    }
}

#[test]
fn neighborhood_union_and_parse_bytes_with_options() {
    let src = br###">:
  > "A"

"A":
  > "B"
  >> ["B", "C"]

"B":
  > []

"C":
  > []
"###;
    assert_eq!(
        Document::parse_bytes(src).unwrap_err().code,
        ErrorCode::E017
    );

    let doc = Document::parse_bytes_with_options(
        src,
        ParseOptions {
            orphan_policy: OrphanPolicy::AttachToRoot,
        },
    )
    .unwrap();

    let node_a = doc.node_by_path(r###""A""###).unwrap().unwrap();
    let neighbors: Vec<String> = node_a
        .neighbors()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert_eq!(neighbors, vec![r###""B""###, r###""C""###]);
}
