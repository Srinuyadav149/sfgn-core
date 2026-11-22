use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sfgn_core::{Document, Scalar, Value};
use sfgn_extract_code::{extract_code_to_sfgn, patch_node_source};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempCodeDir {
    path: PathBuf,
}

impl TempCodeDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{}_{}_{}_{}", prefix, pid, nanos, seq));
        fs::create_dir_all(&path).expect("create temp code dir");
        Self { path }
    }

    fn write(&self, rel: &str, content: &str) {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(full, content).expect("write source file");
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path.join(rel)).expect("read source file")
    }
}

impl Drop for TempCodeDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn extracts_rust_type_prefixed_nodes_precise_call_edges_and_splices_back() {
    let dir = TempCodeDir::new("sfgn_code_rust");

    dir.write(
        "src/token.rs",
        r###"/// Decoded authentication claims.
pub struct Claims {
    pub subject: String,
}

pub struct TokenCodec;

impl TokenCodec {
    pub fn new() -> Self {
        Self
    }

    pub fn decode_jwt(&self, raw: &str) -> Option<Claims> {
        Some(Claims { subject: raw.to_owned() })
    }
}
"###,
    );

    dir.write(
        "src/auth.rs",
        r###"use crate::token::{Claims, TokenCodec};

pub struct AuthService {
    codec: TokenCodec,
}

impl AuthService {
    pub fn new() -> Self {
        Self { codec: TokenCodec::new() }
    }

    /// Validates a bearer token against the codec.
    pub fn validate(&self, raw: &str) -> Option<Claims> {
        let claims = self.codec.decode_jwt(raw)?;
        self.check_expiry(&claims);
        Some(claims)
    }

    fn check_expiry(&self, _claims: &Claims) {}
}

pub struct TokenStore;

impl TokenStore {
    pub fn new() -> Self {
        Self
    }

    /// Colliding method name across structs -> disambiguated via type-prefixed parent.
    pub fn validate(&self, token_id: &str) -> bool {
        let _cmd = std::process::Command::new("echo");
        !token_id.is_empty()
    }
}
"###,
    );

    // Non-enabled Python file should be skipped gracefully when only `rust` feature is active
    dir.write("scripts/helper.py", "def ignored():\n    return 1\n");

    let code_sfgn = extract_code_to_sfgn(&dir.path).expect("extract Rust codebase");
    let doc = Document::parse(&code_sfgn).expect("emitted code.sfgn passes strict validation");

    // Type-prefixed unique structs and methods stay bare
    let claims = doc
        .node_by_path(r###""struct Claims""###)
        .unwrap()
        .expect("struct Claims");
    assert_eq!(
        claims.property("kind").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String("struct".into()))
    );

    let decode_fn = doc
        .node_by_path(r###""method decode_jwt""###)
        .unwrap()
        .expect("method decode_jwt");
    assert_eq!(
        decode_fn.property("kind").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String("method".into()))
    );

    // Colliding `method validate` nodes disambiguate via their type-prefixed parent struct
    let auth_validate = doc
        .node_by_path(r###""struct AuthService">"method validate""###)
        .unwrap()
        .expect("struct AuthService > method validate");
    let store_validate = doc
        .node_by_path(r###""struct TokenStore">"method validate""###)
        .unwrap()
        .expect("struct TokenStore > method validate");

    assert_eq!(
        auth_validate.property("doc").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String(
            "Validates a bearer token against the codec.".into()
        ))
    );

    // Verify `>>` call & type edges on `struct AuthService > method validate`
    let validate_rels: Vec<String> = auth_validate
        .relatives()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert!(validate_rels.contains(&r###""method decode_jwt""###.to_owned()));
    assert!(validate_rels.contains(&r###""method check_expiry""###.to_owned()));
    assert!(validate_rels.contains(&r###""struct Claims""###.to_owned()));

    // Verify `AuthService::new` links ONLY to `TokenCodec::new` (not AuthService::new or TokenStore::new)
    let auth_new = doc
        .node_by_path(r###""struct AuthService">"method new""###)
        .unwrap()
        .expect("struct AuthService > method new");
    let auth_new_rels: Vec<String> = auth_new
        .relatives()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert_eq!(
        auth_new_rels,
        vec![r###""struct TokenCodec">"method new""###.to_owned()]
    );

    // Verify `Command::new("echo")` in `TokenStore::validate` produces zero false-positive `::new` links
    let store_rels: Vec<String> = store_validate
        .relatives()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert!(store_rels.is_empty());

    // Bidirectional Source Mapping: splice an updated body into `struct TokenStore > method validate`
    let updated_method = r###"    pub fn validate(&self, token_id: &str) -> bool {
        token_id.len() >= 16
    }"###;
    patch_node_source(&dir.path, store_validate, updated_method)
        .expect("patch_node_source succeeds");

    let disk_after = dir.read("src/auth.rs");
    assert!(disk_after.contains("token_id.len() >= 16"));
    assert!(disk_after.contains("pub fn validate(&self, raw: &str) -> Option<Claims>"));
    assert!(disk_after.contains("fn check_expiry(&self, _claims: &Claims) {}"));
}
#[test]
fn extracts_single_file_with_lazy_external_portals() {
    struct TempDirGuard(std::path::PathBuf);
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    let unique = format!(
        "sfgn_code_single_file_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let root = std::env::temp_dir().join(unique);
    let _guard = TempDirGuard(root.clone());

    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo_cli\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub mod utils;\npub fn run_lsblk() {}\n",
    )
    .unwrap();
    std::fs::write(root.join("src/utils.rs"), "pub fn helper() {}\n").unwrap();
    std::fs::write(
        root.join("src/main.rs"),
        "use demo_cli::run_lsblk;\n\nfn local_init() {}\n\nfn main() {\n    local_init();\n    run_lsblk();\n}\n",
    )
    .unwrap();

    // Extract only `src/main.rs` without parsing `src/lib.rs` or `src/utils.rs`
    let main_sfgn = sfgn_extract_code::extract_code_file_to_sfgn(&root, "src/main.rs")
        .expect("extract single file src/main.rs");
    let main_doc = sfgn_core::Document::parse(&main_sfgn).expect("valid sfgn");

    assert!(main_sfgn.contains("\"kind\": \"code_file\""));
    assert!(main_sfgn.contains("\"source_file\": \"src/main.rs\""));
    assert!(main_sfgn.contains("\"fn main\":"));
    assert!(main_sfgn.contains("\"fn local_init\":"));

    let fn_main_slice = main_doc
        .node_slice_by_path("\"fn main\"")
        .unwrap()
        .expect("fn main node");
    // Intra-file call resolves to local node
    assert!(fn_main_slice.contains("\"fn local_init\""));
    // Cross-crate import call resolves to unhydrated flat portal
    assert!(fn_main_slice.contains("<\"sfgn-codes/src:lib.rs.sfgn\">"));

    // External `mod utils;` in `src/lib.rs` resolves to `sfgn-codes/src:utils.rs.sfgn`
    let lib_sfgn = sfgn_extract_code::extract_code_file_to_sfgn(&root, "src/lib.rs")
        .expect("extract single file src/lib.rs");
    assert!(lib_sfgn.contains("<\"sfgn-codes/src:utils.rs.sfgn\">"));
}
