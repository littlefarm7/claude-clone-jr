//! Administrator rights with the user's explicit authorization.
//!
//! - Linux/macOS (`sudo`): Claudio shows a screen with the exact command where
//!   the user types their password. The password goes straight to `sudo -S`
//!   through the child's stdin: it never reaches the model, the logs, the
//!   audit trail or the disk, and the sudo ticket is revoked (`sudo -k`) when
//!   the command ends.
//! - Linux without sudo (`pkexec`) and Windows (UAC): the system shows its own
//!   authorization screen.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use claudio_core::events::{ElevationKind, ElevationRequest, ElevationResponse};
use claudio_core::permissions::RiskAssessment;
use claudio_core::tool::{arg_str, Tool, ToolContext, ToolOutput, ToolRegistry};
use claudio_llm::ToolSpec;
use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;
use zeroize::Zeroizing;

use crate::shell::{build_command, run_command_with_input, which, ExecResult};

pub fn register(r: &mut ToolRegistry) {
    r.register(std::sync::Arc::new(AdminCommand));
}

/// Exit code used by the wrapper when sudo rejects the password.
const BAD_PASSWORD: i32 = 97;

/// Whether a Unix command line calls `sudo` or `doas` itself.
pub fn uses_sudo(command: &str) -> bool {
    if cfg!(windows) {
        return false;
    }
    let mut prev = ' ';
    let chars: Vec<char> = command.chars().collect();
    for (i, _) in chars.iter().enumerate() {
        let at_start = i == 0 || matches!(prev, ' ' | '\t' | ';' | '&' | '|' | '(' | '\n' | '`');
        prev = chars[i];
        if !at_start {
            continue;
        }
        let rest: String = chars[i..].iter().collect();
        for w in ["sudo", "/usr/bin/sudo", "/bin/sudo", "doas"] {
            if let Some(after) = rest.strip_prefix(w) {
                if after.is_empty() || after.starts_with([' ', '\t']) {
                    return true;
                }
            }
        }
    }
    false
}

/// How this computer grants administrator rights, if at all.
pub fn kind() -> Option<ElevationKind> {
    if cfg!(windows) {
        Some(ElevationKind::SystemPrompt)
    } else if which("sudo").is_some() {
        Some(ElevationKind::Password)
    } else if which("pkexec").is_some() {
        Some(ElevationKind::SystemPrompt)
    } else {
        None
    }
}

/// Unix script run by bash: reads the password from stdin once, validates it
/// with `sudo -v` (the ticket is shared by the sudo calls of this script) and
/// revokes the ticket at the end.
pub fn sudo_script(command: &str, wrap: bool) -> String {
    let inner = if wrap {
        format!("sudo -- bash -c {}", shell_words::quote(command))
    } else {
        command.to_string()
    };
    format!(
        "IFS= read -r __claudio_pw\n\
         if ! printf '%s\\n' \"$__claudio_pw\" | sudo -S -p '' -v 2>/dev/null; then\n\
         unset __claudio_pw\n\
         echo 'A senha de administrador não foi aceita (senha incorreta ou usuário sem permissão de sudo).' >&2\n\
         exit {BAD_PASSWORD}\n\
         fi\n\
         unset __claudio_pw\n\
         {inner}\n\
         __claudio_rc=$?\n\
         sudo -k\n\
         exit $__claudio_rc\n"
    )
}

/// Remove `sudo`/`doas` prefixes (used with pkexec, which elevates the whole command).
fn strip_sudo(command: &str) -> String {
    let re =
        regex::Regex::new(r"(^|[;&|(\s])(?:/usr/bin/|/bin/)?(?:sudo|doas)(?:\s+-\S+)*\s+").unwrap();
    re.replace_all(command, "$1").into_owned()
}

/// PowerShell that starts an elevated PowerShell (UAC screen), waits for it
/// and prints its output.
pub fn uac_script(command: &str, out_file: &Path) -> String {
    let out = out_file.to_string_lossy().replace('\'', "''");
    let inner = format!(
        "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8; & {{ {command} }} *>&1 | Out-File -Encoding utf8 -FilePath '{out}'; exit $LASTEXITCODE"
    );
    let utf16: Vec<u8> = inner.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, utf16);
    format!(
        "try {{ $p = Start-Process powershell -Verb RunAs -Wait -PassThru -WindowStyle Hidden \
         -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-EncodedCommand','{encoded}' }} \
         catch {{ Write-Error 'O usuário não autorizou na tela do Windows (UAC).'; exit 1223 }}; \
         if (Test-Path '{out}') {{ Get-Content -Raw '{out}'; Remove-Item '{out}' -Force }}; exit $p.ExitCode"
    )
}

/// Run `command` as administrator after the user authorizes it.
/// `wrap`: elevate the whole command (otherwise it already calls sudo).
pub async fn run_elevated(
    command: &str,
    reason: &str,
    wrap: bool,
    cwd: &Path,
    timeout: Duration,
    ctx: &ToolContext,
) -> Result<ToolOutput> {
    let Some(kind) = kind() else {
        bail!("este computador não tem sudo nem pkexec para conceder privilégios de administrador");
    };
    let Some(approver) = ctx.approver.clone() else {
        return Ok(ToolOutput::err(
            "Privilégios de administrador só podem ser pedidos com o usuário presente (modo interativo).",
        ));
    };
    let answer = approver
        .elevate(ElevationRequest {
            session_id: ctx.session_id.clone(),
            command: command.to_string(),
            reason: reason.to_string(),
            kind,
        })
        .await;
    let password = match answer {
        ElevationResponse::Denied => {
            return Ok(ToolOutput::err(
                "O usuário NÃO autorizou a execução como administrador. Não tente contornar; explique a alternativa ou pergunte.",
            ))
        }
        ElevationResponse::Confirmed => None,
        ElevationResponse::Password(p) => Some(p),
    };

    let res: ExecResult = if cfg!(windows) {
        let out_file = std::env::temp_dir().join(format!(
            "claudio-admin-{}.txt",
            uuid::Uuid::new_v4().simple()
        ));
        let script = uac_script(command, &out_file);
        let (prog, args) = crate::shell::shell_invocation(&script);
        let cmd = build_command(&prog, &args, cwd, ctx, false);
        let r = run_command_with_input(cmd, None, timeout, ctx).await;
        let _ = std::fs::remove_file(&out_file);
        r?
    } else if kind == ElevationKind::Password {
        let Some(pw) = password else {
            bail!("senha não informada");
        };
        let script = sudo_script(command, wrap);
        let cmd = build_command("bash", &["-c".into(), script], cwd, ctx, false);
        let mut input = Zeroizing::new(Vec::with_capacity(pw.len() + 1));
        input.extend_from_slice(pw.as_bytes());
        input.push(b'\n');
        drop(pw);
        run_command_with_input(cmd, Some(input), timeout, ctx).await?
    } else {
        let inner = strip_sudo(command);
        let cmd = build_command(
            "pkexec",
            &["bash".into(), "-c".into(), inner],
            cwd,
            ctx,
            false,
        );
        run_command_with_input(cmd, None, timeout, ctx).await?
    };

    let mut text = res.render(ctx.max_output);
    if res.code == Some(BAD_PASSWORD) && kind == ElevationKind::Password {
        text = "A senha de administrador não foi aceita (senha incorreta ou usuário sem permissão de sudo). \
                Nada foi executado. Pergunte ao usuário se quer tentar de novo."
            .into();
    } else if cfg!(windows) && res.code == Some(1223) {
        text =
            "O usuário cancelou a tela de autorização do Windows (UAC). Nada foi executado.".into();
    }
    let mut out = ToolOutput::ok(text);
    out.is_error = !res.ok();
    Ok(out)
}

/// Write `input` to the child's stdin (then close it).
pub(crate) async fn feed_stdin(child: &mut tokio::process::Child, input: Zeroizing<Vec<u8>>) {
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&input).await;
        let _ = stdin.shutdown().await;
    }
}

pub struct AdminCommand;

#[async_trait]
impl Tool for AdminCommand {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "admin_command".into(),
            description: format!(
                "Executa um comando {} COMO ADMINISTRADOR ({}). O usuário vê o comando numa tela de autorização e \
                 precisa digitar a senha/confirmar; ele pode recusar. Use só quando for realmente necessário \
                 (instalar programas, mudar configurações do sistema, serviços) e explique o motivo em 'reason'. \
                 Prefira comandos sem privilégios quando possível.",
                if cfg!(windows) { "PowerShell" } else { "bash" },
                if cfg!(windows) { "tela do UAC" } else { "sudo" }
            ),
            parameters: json!({"type":"object","properties":{
                "command":{"type":"string","description":"comando completo, sem 'sudo' na frente"},
                "reason":{"type":"string","description":"por que precisa de administrador, em linguagem simples"},
                "timeout_secs":{"type":"integer","description":"padrão 300, máximo 3600"}
            },"required":["command","reason"]}),
        }
    }
    fn assess(&self, args: &Value, _: &ToolContext) -> RiskAssessment {
        let cmd = arg_str(args, "command").unwrap_or("");
        RiskAssessment::new(
            claudio_core::permissions::Risk::Admin,
            format!("como administrador: $ {cmd}"),
        )
        .elevated()
    }
    async fn call(&self, args: Value, ctx: &ToolContext) -> Result<ToolOutput> {
        let command = arg_str(&args, "command")
            .context("command")?
            .trim()
            .to_string();
        let command = strip_sudo(&command);
        let reason = arg_str(&args, "reason").unwrap_or("").to_string();
        let timeout = Duration::from_secs(
            args.get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(300)
                .clamp(1, 3600),
        );
        let cwd = ctx.guard.cwd();
        std::fs::create_dir_all(&cwd).ok();
        run_elevated(&command, &reason, true, &cwd, timeout, ctx).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_sudo() {
        if cfg!(windows) {
            return;
        }
        assert!(uses_sudo("sudo apt install vlc"));
        assert!(uses_sudo("apt update && sudo apt upgrade -y"));
        assert!(uses_sudo("echo x | sudo tee /etc/x"));
        assert!(uses_sudo("/usr/bin/sudo ls"));
        assert!(!uses_sudo("echo pseudo sudoers"));
        assert!(!uses_sudo("cat /etc/sudoers.d/x"));
    }

    #[test]
    fn strips_sudo() {
        assert_eq!(strip_sudo("sudo apt install vlc"), "apt install vlc");
        assert_eq!(
            strip_sudo("sudo -E apt update && sudo apt upgrade"),
            "apt update && apt upgrade"
        );
        assert_eq!(strip_sudo("ls /etc/sudoers.d"), "ls /etc/sudoers.d");
    }

    #[test]
    fn scripts() {
        let s = sudo_script("apt install vlc", true);
        assert!(s.contains("sudo -S -p '' -v"));
        assert!(s.contains("sudo -- bash -c 'apt install vlc'"));
        assert!(s.contains("sudo -k"));
        let w = uac_script("Get-Service", Path::new("C:\\t\\o.txt"));
        assert!(w.contains("-Verb RunAs"));
        assert!(w.contains("-EncodedCommand"));
        assert!(!w.contains("Get-Service"), "the command travels encoded");
    }
}
