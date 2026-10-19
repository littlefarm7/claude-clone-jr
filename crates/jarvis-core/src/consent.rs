//! Consent before sending data to a remote model provider.

use anyhow::{bail, Result};
use jarvis_llm::ModelConfig;
use serde_json::json;

use crate::config::PrivacyConfig;
use crate::events::{ApprovalDecision, ApprovalRequest, Approver};
use crate::permissions::Risk;
use crate::store::Store;

pub fn consent_key(m: &ModelConfig) -> String {
    format!("remote:{:?}:{}", m.provider, m.base_url()).to_lowercase()
}

/// Ensure the user agreed to send conversation data to this remote provider.
pub async fn ensure_remote_consent(store: &Store, approver: &dyn Approver, privacy: &PrivacyConfig, m: &ModelConfig) -> Result<()> {
    if m.is_local() {
        return Ok(());
    }
    if privacy.local_only {
        bail!("privacy.local_only está ativo: o modelo remoto '{}' não pode ser usado", m.name);
    }
    let key = consent_key(m);
    if store.has_consent(&key)? {
        return Ok(());
    }
    let req = ApprovalRequest {
        session_id: String::new(),
        tool: "remote_model".into(),
        risk: Risk::Network,
        summary: format!(
            "Enviar o conteúdo das conversas (tarefas, resultados de ferramentas e trechos de arquivos) para {} ({})",
            m.base_url(),
            m.model
        ),
        args: json!({"model": m.name, "endpoint": m.base_url()}),
        reason: "primeiro uso de um provedor remoto".into(),
        allow_session: true,
    };
    match approver.approve(req).await {
        ApprovalDecision::Deny => bail!("uso do provedor remoto '{}' não autorizado", m.name),
        ApprovalDecision::AllowSession => store.grant_consent(&key),
        ApprovalDecision::AllowOnce => Ok(()),
    }
}
