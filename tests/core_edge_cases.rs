use std::borrow::Cow;

use sfgn_core::{
    Document, ErrorCode, IndentStyle, Value, scan_tape, serialize_document_with_indent,
};

#[test]
fn e001_control_bytes_and_nul_in_multiline_and_comments() {
    // NUL (0x00) inside multiline block payload is illegal -> E001
    let nul_in_ml = ">:\n  \"payload\": \"\"\"txt\nhello\x00world\n\"\"\"\n";
    assert_eq!(
        Document::parse(nul_in_ml).unwrap_err().code,
        ErrorCode::E001
    );

    // DEL (0x7F) inside quoted string -> E001
    let del_in_str = ">:\n  \"k\": \"abc\x7Fdef\"\n";
    assert_eq!(
        Document::parse(del_in_str).unwrap_err().code,
        ErrorCode::E001
    );

    // Control byte inside comment -> E001
    let ctrl_in_comment = ">: # bad\x01comment\n";
    assert_eq!(
        Document::parse(ctrl_in_comment).unwrap_err().code,
        ErrorCode::E001
    );

    // Standalone CR inside multiline payload -> E001
    let cr_in_ml = ">:\n  \"payload\": \"\"\"txt\nline1\rline2\n\"\"\"\n";
    assert_eq!(Document::parse(cr_in_ml).unwrap_err().code, ErrorCode::E001);
}

#[test]
fn e007_and_e008_string_and_unicode_escape_boundaries() {
    // Unterminated string at EOF without trailing newline -> E007
    let eof_str = r###">:
  "k": "unterminated"###;
    assert_eq!(Document::parse(eof_str).unwrap_err().code, ErrorCode::E007);

    // Unterminated string in NodeHeader -> E007
    let header_str = r###">:
"UnclosedNode:
"###;
    assert_eq!(
        Document::parse(header_str).unwrap_err().code,
        ErrorCode::E007
    );

    // Unterminated string in ExternalRef -> E007
    let ext_str = r###">:
  > <"unclosed.sfgn
"###;
    assert_eq!(Document::parse(ext_str).unwrap_err().code, ErrorCode::E007);

    // Malformed \u{...} escapes -> E008
    let bad_escapes = [
        r###">:
  "k": "\u{}"
"###,
        r###">:
  "k": "\u{1234567}"
"###,
        r###">:
  "k": "\u{110000}"
"###,
        r###">:
  "k": "\u{DFFF}"
"###,
        r###">:
  "k": "\u0041"
"###,
    ];
    for case in bad_escapes {
        assert_eq!(
            Document::parse(case).unwrap_err().code,
            ErrorCode::E008,
            "expected E008 for:\n{}",
            case
        );
    }
}

#[test]
fn e009_e010_header_property_and_chained_index_malformations() {
    // Trailing token after `>:` -> E009
    let trailing_root = r###">: unexpected
"###;
    assert_eq!(
        Document::parse(trailing_root).unwrap_err().code,
        ErrorCode::E009
    );

    // Trailing token after `"Node":` -> E009
    let trailing_node = r###">:
  > "Node"
"Node": extra
"###;
    assert_eq!(
        Document::parse(trailing_node).unwrap_err().code,
        ErrorCode::E009
    );

    // Missing ':' after property key -> E009
    let missing_prop_colon = r###">:
  "key" "val"
"###;
    assert_eq!(
        Document::parse(missing_prop_colon).unwrap_err().code,
        ErrorCode::E009
    );

    // Invalid unquoted scalars -> E009
    for bad_scalar in ["NaN", "Infinity", "-Infinity", "undefined", "True", "NULL"] {
        let src = format!(
            r###">:
  "v": {}
"###,
            bad_scalar
        );
        assert_eq!(
            Document::parse(&src).unwrap_err().code,
            ErrorCode::E009,
            "expected E009 for scalar {}",
            bad_scalar
        );
    }

    // Malformed second ordinal index in chain -> E010
    let bad_chain = r###">:
  "matrix"#0#: [1, 2]
"###;
    assert_eq!(
        Document::parse(bad_chain).unwrap_err().code,
        ErrorCode::E010
    );
}

#[test]
fn e011_to_e015_collections_multiline_and_portals() {
    // Triple-quoted multiline block inside 1D collection -> E011
    let ml_in_array = r###">:
  "arr": ["""txt
hi
"""]
"###;
    assert_eq!(
        Document::parse(ml_in_array).unwrap_err().code,
        ErrorCode::E011
    );

    // Missing comma between collection items -> E012
    let missing_comma = r###">:
  "arr": [1 2]
"###;
    assert_eq!(
        Document::parse(missing_comma).unwrap_err().code,
        ErrorCode::E012
    );

    // EOF directly on opening `"""tag` line -> E013
    let eof_on_ml_open = r###">:
  "code": """rust"###;
    assert_eq!(
        Document::parse(eof_on_ml_open).unwrap_err().code,
        ErrorCode::E013
    );

    // Trailing tokens (not a comment) after closing `"""` -> E014
    let trailing_after_ml_close = r###">:
  "code": """rust
let x = 1;
""" trailing
"###;
    assert_eq!(
        Document::parse(trailing_after_ml_close).unwrap_err().code,
        ErrorCode::E014
    );

    // Unquoted file path inside `<...>` -> E015
    let unquoted_portal = r###">:
  > <other.sfgn>
"###;
    assert_eq!(
        Document::parse(unquoted_portal).unwrap_err().code,
        ErrorCode::E015
    );
}

#[test]
fn allows_optional_trailing_commas_in_collections() {
    let src = r###">:
  "arr": [1, 2,]
  > ["A",]
  >> ["A",]

"A":
  "tags": ["fast", "flat" , ]
  > []
  >> []
"###;
    let doc = Document::parse(src).expect("trailing commas in 1D collections must parse");

    match doc.root().property("arr").unwrap().decode().unwrap() {
        Value::FlatCollection(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].as_number().unwrap().as_i64(), Some(1));
            assert_eq!(items[1].as_number().unwrap().as_i64(), Some(2));
        }
        other => panic!("expected FlatCollection, got {:?}", other),
    }

    assert_eq!(doc.root().children().count(), 1);
    assert_eq!(doc.root().relatives().count(), 1);

    let node_a = doc.node_by_path(r###""A""###).unwrap().unwrap();
    match node_a.property("tags").unwrap().decode().unwrap() {
        Value::FlatCollection(tags) => {
            assert_eq!(tags.len(), 2);
            assert_eq!(tags[0].as_str(), Some("fast"));
            assert_eq!(tags[1].as_str(), Some("flat"));
        }
        other => panic!("expected FlatCollection, got {:?}", other),
    }
}

#[test]
fn e016_and_e017_multi_segment_duplicates_and_self_loops() {
    // Multi-segment duplicate with whitespace variation -> E016
    let dup_nested = r###">:
  > ["A", "A">"B"]

"A":
  > "A">"B"

"A">"B":
  "x": 1

"A" > "B":
  "x": 2
"###;
    assert_eq!(
        Document::parse(dup_nested).unwrap_err().code,
        ErrorCode::E016
    );

    // Disconnected self-loop unreachable from `>:` -> E017
    let self_loop = r###">:
  > []

"SelfLoop":
  > "SelfLoop"
"###;
    assert_eq!(
        Document::parse(self_loop).unwrap_err().code,
        ErrorCode::E017
    );
}

#[test]
fn empty_multiline_blocks_and_4_space_indentation_roundtrip() {
    let src = r###">:
    "empty_ml": """txt
    """
    "empty_arr": []
    "empty_str": ""
    > []
    >> []
"###;
    let tape = scan_tape(src).unwrap();
    assert_eq!(tape.properties[0].val_span.slice_str(src), "");

    let doc = Document::parse(src).unwrap();
    match doc.root().property("empty_ml").unwrap().decode().unwrap() {
        Value::Multiline(ml) => {
            assert_eq!(ml.tag, Some("txt"));
            assert_eq!(ml.decode_text(), Cow::Borrowed(""));
        }
        other => panic!("expected Multiline, got {:?}", other),
    }

    let reserialized = serialize_document_with_indent(&doc, IndentStyle::Spaces(4)).unwrap();
    let reparsed = Document::parse(&reserialized).unwrap();
    assert_eq!(reparsed.len(), 1);
}
