//! Runs the real `jarvis` binary against a scripted OpenAI-compatible server.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::Duration;

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn start_mock() -> (Server, u16) {
    let port = free_port();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mock_llm.py");
    let child = Command::new("python3").arg(script).arg(port.to_string()).spawn().expect("python3");
    for _ in 0..50 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    (Server(child), port)
}

fn setup(port: u16) -> (tempfile::TempDir, PathBuf) {
    let home = tempfile::tempdir().unwrap();
    let ws = home.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("dados.csv"), "a,b\n1,2\n3,4\n").unwrap();
    let cfg = home.path().join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"default_model = "mock"
[permissions]
workspace_roots = ["{ws}"]
[[models]]
name = "mock"
provider = "openai_compat"
model = "mock"
base_url = "http://127.0.0.1:{port}/v1"
tags = ["fast"]
"#,
            ws = ws.display()
        ),
    )
    .unwrap();
    (home, cfg)
}

fn jarvis(home: &Path, cfg: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_jarvis"))
        .arg("--config")
        .arg(cfg)
        .args(args)
        .env("JARVIS_DATA_DIR", home.join("data"))
        .env("HOME", home)
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

#[test]
fn run_task_end_to_end_with_verification() {
    let (_srv, port) = start_mock();
    let (home, cfg) = setup(port);
    let out = jarvis(home.path(), &cfg, &["--yes", "run", "gere um relatório de dados.csv"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "stdout:\n{stdout}\nstderr:\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("concluído e verificado"), "{stdout}");
    let report = std::fs::read_to_string(home.path().join("ws/saida/relatorio.txt")).unwrap();
    assert_eq!(report, "relatorio: dados.csv tem 3 linhas\n");
    // history and audit are persisted
    let list = jarvis(home.path(), &cfg, &["sessions"]);
    assert!(String::from_utf8_lossy(&list.stdout).contains("completed"));
    let audit = jarvis(home.path(), &cfg, &["audit", "verify"]);
    assert!(String::from_utf8_lossy(&audit.stdout).contains("íntegras"), "{}", String::from_utf8_lossy(&audit.stdout));
}

#[test]
fn non_interactive_denies_writes_and_admin_is_never_auto_approved() {
    let (_srv, port) = start_mock();
    let (home, cfg) = setup(port);
    // Without --yes and without a TTY, the write needs approval and is denied.
    let out = jarvis(home.path(), &cfg, &["run", "gere um relatório de dados.csv"]);
    assert!(!home.path().join("ws/saida/relatorio.txt").exists(), "{}", String::from_utf8_lossy(&out.stdout));
    // Even with --yes, sudo is denied.
    let out = jarvis(home.path(), &cfg, &["--yes", "run", "instale com sudo"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("NEGOU") || stdout.contains("negado"), "{stdout}");
}

#[test]
fn json_events_and_memory_cli() {
    let (_srv, port) = start_mock();
    let (home, cfg) = setup(port);
    let out = jarvis(home.path(), &cfg, &["--json", "run", "oi"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.lines().any(|l| l.contains("\"kind\":\"finished\"")), "{stdout}");
    let out = jarvis(home.path(), &cfg, &["memory", "add", "prefiro", "PDF", "--kind", "preference"]);
    assert!(out.status.success());
    let out = jarvis(home.path(), &cfg, &["memory", "list"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("prefiro PDF"));
}
