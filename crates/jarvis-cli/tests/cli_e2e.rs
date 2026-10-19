//! Runs the real `jarvis` binary against a scripted OpenAI-compatible server.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

struct Server(Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

/// Start the scripted model server on an OS-assigned port (printed on stdout).
fn start_mock() -> (Server, u16) {
    use std::io::BufRead;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mock_llm.py");
    let mut child = Command::new("python3").arg(script).arg("0").stdout(std::process::Stdio::piped()).spawn().expect("python3");
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let port: u16 = line.trim().parse().expect("porta do mock");
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

#[test]
fn workflow_schedule_and_daemon() {
    let (_srv, port) = start_mock();
    let (home, cfg) = setup(port);
    let wf = home.path().join("wf.toml");
    std::fs::write(
        &wf,
        r#"name = "contagem"
allowed_tools = ["write_file"]
[vars]
arquivo = "dados.csv"
[[steps]]
tool = "shell"
args = { command = "wc -l < {{arquivo}}" }
save_as = "linhas"
[[steps]]
tool = "write_file"
args = { path = "contagem.txt", content = "{{linhas}}" }
[[steps]]
task = "oi"
"#,
    )
    .unwrap();
    let out = jarvis(home.path(), &cfg, &["workflow", wf.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    let written = std::fs::read_to_string(home.path().join("ws/contagem.txt")).unwrap();
    assert!(written.contains("exit code: 0") && written.contains('3'), "{written}");

    let out = jarvis(home.path(), &cfg, &["schedule", "add", "--name", "diario", "--cron", "0 8 * * *", "--task", "oi", "--now"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = jarvis(home.path(), &cfg, &["daemon", "--once"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("executando agendamento") && stdout.contains("tarefa: completed"), "{stdout}");
    // Not due again right after running.
    let out = jarvis(home.path(), &cfg, &["daemon", "--once"]);
    assert!(!String::from_utf8_lossy(&out.stdout).contains("executando agendamento"));
}
