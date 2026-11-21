use sfgn_core::{Document, render_ascii_tree, render_mermaid};

#[test]
fn renders_ascii_tree_and_mermaid_for_dual_topology_graph() {
    let src = r###">:
  "project": "sfgn_core"
  > ["src", "crates"]
  > <"external_spec.sfgn">

"src":
  "kind": "directory"
  > ["lib.rs", "builder.rs"]

"lib.rs":
  "lines": 42
  > []
  >> "builder.rs"

"builder.rs":
  "lines": 380
  > []

"crates":
  "kind": "directory"
  > []
"###;

    let doc = Document::parse(src).expect("valid SFGN");

    let ascii = render_ascii_tree(&doc);
    assert!(ascii.contains(r###">:  {project: "sfgn_core"}"###));
    assert!(ascii.contains(r###"├── "src"  {kind: "directory"}"###));
    assert!(ascii.contains(r###"──>> ["builder.rs"]"###));
    assert!(ascii.contains(r###"──>  <"external_spec.sfgn">"###));

    let mermaid = render_mermaid(&doc);
    assert!(mermaid.starts_with("graph TD\n"));
    assert!(mermaid.contains("N0 --> N1"));
    assert!(mermaid.contains("N2 -. \">>\" .-> N3"));
}
