use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempCliWorkspace {
    path: PathBuf,
}

impl TempCliWorkspace {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("{}_{}_{}_{}", prefix, pid, nanos, seq));
        fs::create_dir_all(&path).expect("create temp cli dir");
        Self { path }
    }

    fn write(&self, rel: &str, content: &str) {
        let full = self.path.join(rel);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(full, content).expect("write file");
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.path.join(rel)).expect("read file")
    }
}

impl Drop for TempCliWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
fn end_to_end_cli_compile_check_slice_grep_viz_patch_and_gbnf() {
    let ws = TempCliWorkspace::new("sfgn_cli_e2e");

    ws.write(
        "src/math.rs",
        r###"/// Adds two integers.
pub fn add(a: i64, b: i64) -> i64 {
    a + b
}
"###,
    );

    ws.write(
        "README.md",
        "# Math Crate\n\nSee [Math Implementation](src/math.rs) for details.\n",
    );

    ws.write(
        "grammar.ebnf",
        "Root = \"ok\", { Digit } ;\nDigit = '0'..'9' ;\n",
    );

    let bin = env!("CARGO_BIN_EXE_sfgn");
    let out_dir = ws.path.join(".sfgn");

    // 1. `sfgn compile`
    let compile_out = Command::new(bin)
        .arg("compile")
        .arg(&ws.path)
        .arg("--out-dir")
        .arg(&out_dir)
        .output()
        .expect("run sfgn compile");
    assert!(
        compile_out.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&compile_out.stderr)
    );
    assert!(out_dir.join("fs.sfgn").exists());
    assert!(out_dir.join("code.sfgn").exists());
    assert!(out_dir.join("doc.sfgn").exists());

    // 2. `sfgn check`
    let check_out = Command::new(bin)
        .arg("check")
        .arg(out_dir.join("code.sfgn"))
        .output()
        .expect("run sfgn check");
    assert!(check_out.status.success());
    assert!(String::from_utf8_lossy(&check_out.stdout).starts_with("OK:"));

    // 3. `sfgn slice --follow-portals` (doc.sfgn -> fs.sfgn)
    let slice_out = Command::new(bin)
        .arg("slice")
        .arg(out_dir.join("doc.sfgn"))
        .arg(r###""Math Crate""###)
        .arg("--follow-portals")
        .output()
        .expect("run sfgn slice");
    assert!(
        slice_out.status.success(),
        "slice stderr: {}",
        String::from_utf8_lossy(&slice_out.stderr)
    );
    let slice_stdout = String::from_utf8_lossy(&slice_out.stdout);
    assert!(slice_stdout.contains(r###"Resolved Portal: <"fs.sfgn">"src">"math.rs""###));
    assert!(slice_stdout.contains(r###""path": "src/math.rs""###));

    // 4. `sfgn grep`
    let grep_out = Command::new(bin)
        .arg("grep")
        .arg(out_dir.join("code.sfgn"))
        .arg("Adds two integers")
        .output()
        .expect("run sfgn grep");
    assert!(grep_out.status.success());
    assert!(String::from_utf8_lossy(&grep_out.stdout).contains(r###""fn add""###));

    // 5. `sfgn viz --mermaid --lr`
    let viz_out = Command::new(bin)
        .arg("viz")
        .arg(out_dir.join("doc.sfgn"))
        .arg("--mermaid")
        .arg("--lr")
        .output()
        .expect("run sfgn viz");
    assert!(viz_out.status.success());
    assert!(String::from_utf8_lossy(&viz_out.stdout).starts_with("graph LR\n"));

    // 6. `sfgn patch`
    let updated_fn = "pub fn add(a: i64, b: i64) -> i64 {\n    a.saturating_add(b)\n}";
    let patch_out = Command::new(bin)
        .arg("patch")
        .arg(&ws.path)
        .arg(out_dir.join("code.sfgn"))
        .arg(r###""fn add""###)
        .arg("--body")
        .arg(updated_fn)
        .output()
        .expect("run sfgn patch");
    assert!(
        patch_out.status.success(),
        "patch stderr: {}",
        String::from_utf8_lossy(&patch_out.stderr)
    );
    assert!(ws.read("src/math.rs").contains("a.saturating_add(b)"));

    // 7. `sfgn gbnf`
    let gbnf_out = Command::new(bin)
        .arg("gbnf")
        .arg(ws.path.join("grammar.ebnf"))
        .output()
        .expect("run sfgn gbnf");
    assert!(gbnf_out.status.success());
    assert!(String::from_utf8_lossy(&gbnf_out.stdout).contains("root ::= \"ok\" (digit)*"));
}
