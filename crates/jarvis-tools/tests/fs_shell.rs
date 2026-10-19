use jarvis_core::permissions::{Forced, Risk};
use jarvis_core::tool::Tool;
use jarvis_tools::{fs::*, shell::*, testutil::context};
use serde_json::json;

#[tokio::test]
async fn write_read_edit_glob_grep_trash() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let out = WriteFile.call(json!({"path":"docs/a.md","content":"linha um\nlinha dois\n"}), &ctx).await.unwrap();
    assert!(out.content.contains("Verificado"), "{}", out.content);
    let out = ReadFile.call(json!({"path":"docs/a.md"}), &ctx).await.unwrap();
    assert!(out.content.contains("     2\tlinha dois"));
    assert!(out.untrusted_source.is_none());
    let out = EditFile.call(json!({"path":"docs/a.md","old_text":"linha","new_text":"LINHA"}), &ctx).await.unwrap();
    assert!(out.is_error && out.content.contains("2 vezes"));
    let out = EditFile.call(json!({"path":"docs/a.md","old_text":"dois","new_text":"2"}), &ctx).await.unwrap();
    assert!(!out.is_error);
    assert_eq!(std::fs::read_to_string(dir.path().join("docs/a.md")).unwrap(), "linha um\nlinha 2\n");
    let out = Glob.call(json!({"pattern":"**/*.md"}), &ctx).await.unwrap();
    assert!(out.content.contains("a.md"));
    let out = Grep.call(json!({"pattern":"linha \\d"}), &ctx).await.unwrap();
    assert!(out.content.contains("a.md:2"), "{}", out.content);
    let out = ListDir.call(json!({"path":"docs"}), &ctx).await.unwrap();
    assert!(out.content.contains("a.md"));

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_DATA_HOME", home.path());
    let out = DeletePath.call(json!({"path":"docs/a.md"}), &ctx).await.unwrap();
    assert!(!out.is_error, "{}", out.content);
    assert!(!dir.path().join("docs/a.md").exists());
    assert!(home.path().join("Trash/files/a.md").exists());
    assert!(home.path().join("Trash/info/a.md.trashinfo").exists());
}

#[tokio::test]
async fn risk_assessments() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let ra = WriteFile.assess(&json!({"path":"/etc/passwd","content":""}), &ctx);
    assert!(matches!(ra.force, Some(Forced::Deny(_))));
    let ra = ReadFile.assess(&json!({"path":"/etc/hostname"}), &ctx);
    assert!(matches!(ra.force, Some(Forced::Ask(_))));
    let ra = DeletePath.assess(&json!({"path":"."}), &ctx);
    assert!(matches!(ra.force, Some(Forced::Deny(_))));
    let ra = Shell.assess(&json!({"command":"sudo reboot"}), &ctx);
    assert_eq!(ra.risk, Risk::Admin);
}

#[tokio::test]
async fn shell_runs_with_scrubbed_env_and_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    std::env::set_var("JARVIS_TEST_API_KEY", "should-not-leak");
    let out = Shell.call(json!({"command":"echo hi; pwd; echo ${JARVIS_TEST_API_KEY:-unset}; exit 3"}), &ctx).await.unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("exit code: 3"));
    assert!(out.content.contains("hi"));
    assert!(out.content.contains("unset"), "{}", out.content);
    assert!(out.content.contains(&dir.path().canonicalize().unwrap().display().to_string()));
    let t = std::time::Instant::now();
    let out = Shell.call(json!({"command":"sleep 30 & sleep 30","timeout_secs":1}), &ctx).await.unwrap();
    assert!(out.content.contains("TEMPO ESGOTADO"));
    assert!(t.elapsed().as_secs() < 10);
}

#[tokio::test]
async fn shell_cancel_and_background_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let c = ctx.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        c.cancel();
    });
    let out = Shell.call(json!({"command":"sleep 20"}), &ctx).await.unwrap();
    assert!(out.content.contains("CANCELADO"));

    let ctx = context(dir.path());
    let out = Shell.call(json!({"command":"echo começou; sleep 0.3; echo terminou","background":true}), &ctx).await.unwrap();
    let id = out.content.split_whitespace().find(|w| w.starts_with("job-")).unwrap().trim_end_matches('.').to_string();
    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
    let st = JobStatus.call(json!({"job_id": id}), &ctx).await.unwrap();
    assert!(st.content.contains("terminou com código 0"), "{}", st.content);
    assert!(st.content.contains("começou") && st.content.contains("terminou"));
}

#[tokio::test]
async fn system_info_reports_real_values() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    let out = SystemInfo.call(json!({}), &ctx).await.unwrap();
    assert!(out.content.contains("Kernel:"));
    assert!(out.content.contains("MB livres"));
}

#[tokio::test]
async fn bubblewrap_sandbox_confines_writes() {
    if jarvis_tools::shell::which("bwrap").is_none() {
        eprintln!("ignorado: bwrap ausente");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let ctx = context(dir.path());
    // Probe whether bwrap can create namespaces here (some containers forbid it).
    let probe = Shell.call(json!({"command": "true"}), &ctx).await.unwrap();
    if probe.is_error {
        eprintln!("ignorado: bwrap sem permissão de namespaces neste ambiente: {}", probe.content);
        return;
    }
    let target = outside.path().join("fuga.txt");
    let out = Shell.call(json!({"command": format!("echo dentro > ok.txt; echo x > {}", target.display())}), &ctx).await.unwrap();
    assert!(dir.path().join("ok.txt").exists(), "{}", out.content);
    assert!(!target.exists(), "escrita fora do workspace deveria falhar no sandbox: {}", out.content);
    assert!(out.content.contains("Read-only file system") || out.is_error, "{}", out.content);
}
