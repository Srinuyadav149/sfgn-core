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
fn end_to_end_cli_lazy_flat_compile_hydrate_slice_grep_viz_patch_and_gbnf() {
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

    // 1. `sfgn compile <ROOT_DIR>` creates root `fs.sfgn`, `sfgn-codes/`, and `sfgn-docs/`
    let compile_out = Command::new(bin)
        .arg("compile")
        .arg(&ws.path)
        .output()
        .expect("run sfgn compile");
    assert!(
        compile_out.status.success(),
        "compile stderr: {}",
        String::from_utf8_lossy(&compile_out.stderr)
    );
    assert!(ws.path.join("fs.sfgn").exists());
    assert!(ws.path.join("sfgn-codes").is_dir());
    assert!(ws.path.join("sfgn-docs").is_dir());
    // Lazy by default: neither code nor doc artifact exists before hydration
    assert!(!ws.path.join("sfgn-codes/src:math.rs.sfgn").exists());
    assert!(!ws.path.join("sfgn-docs/README.md.sfgn").exists());

    // 2. `sfgn check` on `fs.sfgn`
    let check_out = Command::new(bin)
        .arg("check")
        .arg(ws.path.join("fs.sfgn"))
        .output()
        .expect("run sfgn check");
    assert!(check_out.status.success());
    assert!(String::from_utf8_lossy(&check_out.stdout).starts_with("OK:"));

    // 3. `sfgn hydrate` lazily compiles `sfgn-codes/src:math.rs.sfgn`
    let hydrate_out = Command::new(bin)
        .arg("hydrate")
        .arg(&ws.path)
        .arg("src/math.rs")
        .output()
        .expect("run sfgn hydrate");
    assert!(
        hydrate_out.status.success(),
        "hydrate stderr: {}",
        String::from_utf8_lossy(&hydrate_out.stderr)
    );
    assert!(String::from_utf8_lossy(&hydrate_out.stdout).starts_with("Hydrated "));
    assert!(ws.path.join("sfgn-codes/src:math.rs.sfgn").exists());

    // Second hydrate without source modification hits mtime cache
    let hydrate_cached = Command::new(bin)
        .arg("hydrate")
        .arg(&ws.path)
        .arg("src/math.rs")
        .output()
        .expect("run cached sfgn hydrate");
    assert!(hydrate_cached.status.success());
    assert!(String::from_utf8_lossy(&hydrate_cached.stdout).starts_with("Up-to-date "));

    // 4. `sfgn slice --follow-portals` on `fs.sfgn` auto-hydrates `sfgn-docs/README.md.sfgn`
    let slice_out = Command::new(bin)
        .arg("slice")
        .arg(ws.path.join("fs.sfgn"))
        .arg(r###""README.md""###)
        .arg("--follow-portals")
        .output()
        .expect("run sfgn slice");
    assert!(
        slice_out.status.success(),
        "slice stderr: {}",
        String::from_utf8_lossy(&slice_out.stderr)
    );
    let slice_stdout = String::from_utf8_lossy(&slice_out.stdout);
    assert!(slice_stdout.contains(r###"Resolved Portal: <"sfgn-docs/README.md.sfgn">"###));
    assert!(slice_stdout.contains(r###""source_file": "README.md""###));
    assert!(ws.path.join("sfgn-docs/README.md.sfgn").exists());

    // 5. `sfgn grep` on hydrated `sfgn-codes/src:math.rs.sfgn`
    let grep_out = Command::new(bin)
        .arg("grep")
        .arg(ws.path.join("sfgn-codes/src:math.rs.sfgn"))
        .arg("Adds two integers")
        .output()
        .expect("run sfgn grep");
    assert!(grep_out.status.success());
    assert!(String::from_utf8_lossy(&grep_out.stdout).contains(r###""fn add""###));

    // 6. `sfgn viz --mermaid --lr` on hydrated `sfgn-docs/README.md.sfgn`
    let viz_out = Command::new(bin)
        .arg("viz")
        .arg(ws.path.join("sfgn-docs/README.md.sfgn"))
        .arg("--mermaid")
        .arg("--lr")
        .output()
        .expect("run sfgn viz");
    assert!(viz_out.status.success());
    assert!(String::from_utf8_lossy(&viz_out.stdout).starts_with("graph LR\n"));

    // 7. `sfgn patch` splices `src/math.rs` and refreshes `sfgn-codes/src:math.rs.sfgn`
    let updated_fn = "pub fn add(a: i64, b: i64) -> i64 {\n    a.saturating_add(b)\n}";
    let patch_out = Command::new(bin)
        .arg("patch")
        .arg(&ws.path)
        .arg(ws.path.join("sfgn-codes/src:math.rs.sfgn"))
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
    assert!(
        ws.read("sfgn-codes/src:math.rs.sfgn")
            .contains("a.saturating_add(b)")
    );

    // 8. `sfgn gbnf`
    let gbnf_out = Command::new(bin)
        .arg("gbnf")
        .arg(ws.path.join("grammar.ebnf"))
        .output()
        .expect("run sfgn gbnf");
    assert!(gbnf_out.status.success());
    assert!(String::from_utf8_lossy(&gbnf_out.stdout).contains("root ::= \"ok\" (digit)*"));
}
