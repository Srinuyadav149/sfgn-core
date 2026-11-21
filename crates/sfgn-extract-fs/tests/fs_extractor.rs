use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use sfgn_core::{Document, Number, Scalar, Value};
use sfgn_extract_fs::{FsExtractOptions, FsExtractor, extract_fs_to_sfgn};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{}_{}_{}_{}", prefix, pid, nanos, seq));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(full, content).expect("write file");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn respects_gitignore_and_applies_minimal_collision_qualification() {
    let dir = TempDir::new("sfgn_fs_ignore");
    dir.write(".gitignore", b"target/\n*.tmp\n");
    dir.write("src/main.rs", b"fn main() {\n    println!(\"hi\");\n}\n");
    dir.write("src/alpha/mod.rs", b"pub fn a() {}\n");
    dir.write("src/beta/mod.rs", b"pub fn b() {}\n");
    dir.write("binary.bin", b"\x00\x01\x02\x03");
    dir.write("target/ignored.rs", b"fn ignored() {}\n");
    dir.write("notes.tmp", b"ignored temp\n");

    let sfgn_text = extract_fs_to_sfgn(&dir.path).expect("extract fs.sfgn");
    let doc = Document::parse(&sfgn_text).expect("emitted fs.sfgn passes strict validation");

    assert!(doc.node_by_path(r###""ignored.rs""###).unwrap().is_none());
    assert!(doc.node_by_path(r###""notes.tmp""###).unwrap().is_none());

    let main_node = doc
        .node_by_path(r###""main.rs""###)
        .unwrap()
        .expect("bare main.rs");
    assert_eq!(
        main_node.property("ext").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String("rs".into()))
    );
    assert_eq!(
        main_node.property("lines").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::Number(Number::new("3")))
    );
    assert_eq!(
        main_node.property("git_status").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String("untracked_repo".into()))
    );

    assert!(
        doc.node_by_path(r###""alpha">"mod.rs""###)
            .unwrap()
            .is_some()
    );
    assert!(
        doc.node_by_path(r###""beta">"mod.rs""###)
            .unwrap()
            .is_some()
    );

    let bin_node = doc
        .node_by_path(r###""binary.bin""###)
        .unwrap()
        .expect("binary.bin");
    assert_eq!(
        bin_node.property("lines").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::Number(Number::new("0")))
    );
}

#[cfg(unix)]
#[test]
fn resolves_in_tree_symlinks_as_relative_edges() {
    use std::os::unix::fs::symlink;

    let dir = TempDir::new("sfgn_fs_symlink");
    dir.write("docs/spec.md", b"# SFGN Spec\n");
    symlink(
        dir.path.join("docs/spec.md"),
        dir.path.join("SPEC_ALIAS.md"),
    )
    .expect("create symlink");

    let sfgn_text = extract_fs_to_sfgn(&dir.path).expect("extract fs with symlink");
    let doc = Document::parse(&sfgn_text).expect("valid fs.sfgn");

    let alias = doc
        .node_by_path(r###""SPEC_ALIAS.md""###)
        .unwrap()
        .expect("symlink node exists");
    assert_eq!(
        alias.property("kind").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String("symlink".into()))
    );

    let rels: Vec<String> = alias
        .relatives()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert_eq!(rels, vec![r###""spec.md""###]);
}

#[test]
fn extracts_git_status_last_commit_and_co_change_relative_edges() {
    let dir = TempDir::new("sfgn_fs_git");
    if !init_git_repo(&dir.path) {
        return;
    }

    dir.write("src/lexer.rs", b"pub fn lex() {}\n");
    dir.write("src/parser.rs", b"pub fn parse() {}\n");
    dir.write("src/standalone.rs", b"pub fn solo() {}\n");
    git_commit_all(&dir.path, "c1");

    dir.write("src/lexer.rs", b"pub fn lex() { /* v2 */ }\n");
    dir.write("src/parser.rs", b"pub fn parse() { /* v2 */ }\n");
    git_commit_all(&dir.path, "c2");

    dir.write("src/lexer.rs", b"pub fn lex() { /* v3 */ }\n");
    dir.write("src/parser.rs", b"pub fn parse() { /* v3 */ }\n");
    git_commit_all(&dir.path, "c3");

    dir.write("src/standalone.rs", b"pub fn solo() { /* dirty */ }\n");
    dir.write("src/brand_new.rs", b"pub fn fresh() {}\n");

    let extractor = FsExtractor::new(FsExtractOptions {
        co_change_min_commits: 2,
        ..FsExtractOptions::default()
    });
    let sfgn_text = extractor.extract(&dir.path).expect("extract git repo");
    let doc = Document::parse(&sfgn_text).expect("valid git fs.sfgn");

    assert!(doc.root().property("git_head").is_some());

    let lexer = doc.node_by_path(r###""lexer.rs""###).unwrap().unwrap();
    assert_eq!(
        lexer.property("git_status").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String("clean".into()))
    );
    assert!(
        lexer.property("last_commit").unwrap().decode().unwrap() != Value::Scalar(Scalar::Null)
    );

    let lexer_rels: Vec<String> = lexer
        .relatives()
        .map(|n| n.canonical_path().unwrap().to_string())
        .collect();
    assert_eq!(lexer_rels, vec![r###""parser.rs""###]);

    let standalone = doc.node_by_path(r###""standalone.rs""###).unwrap().unwrap();
    assert_eq!(
        standalone.property("git_status").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String("modified".into()))
    );

    let brand_new = doc.node_by_path(r###""brand_new.rs""###).unwrap().unwrap();
    assert_eq!(
        brand_new.property("git_status").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::String("untracked".into()))
    );
    assert_eq!(
        brand_new.property("last_commit").unwrap().decode().unwrap(),
        Value::Scalar(Scalar::Null)
    );
}

fn init_git_repo(dir: &Path) -> bool {
    let ok = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return false;
    }
    let _ = Command::new("git")
        .args(["config", "user.email", "test@sfgn.local"])
        .current_dir(dir)
        .status();
    let _ = Command::new("git")
        .args(["config", "user.name", "SFGN Test"])
        .current_dir(dir)
        .status();
    true
}

fn git_commit_all(dir: &Path, msg: &str) {
    Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir)
        .status()
        .expect("git add");
    Command::new("git")
        .args(["commit", "-q", "-m", msg])
        .current_dir(dir)
        .status()
        .expect("git commit");
}
