use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

pub fn client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("jarvis-core/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

/// POST JSON with retries on 429/5xx/network errors (exponential backoff).
pub async fn post_json(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
) -> Result<Value> {
    let mut delay = Duration::from_millis(500);
    let mut last_err = None;
    for attempt in 0..4 {
        if attempt > 0 {
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
        let mut rb = client.post(url).json(body);
        for (k, v) in headers {
            rb = rb.header(*k, v);
        }
        match rb.send().await {
            Ok(resp) => {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                if status.is_success() {
                    return serde_json::from_str(&text).map_err(|e| {
                        anyhow!("resposta JSON inválida: {e}: {}", truncate(&text, 300))
                    });
                }
                let err = anyhow!("HTTP {status}: {}", truncate(&text, 600));
                if status.as_u16() == 429 || status.is_server_error() {
                    last_err = Some(err);
                    continue;
                }
                return Err(err);
            }
            Err(e) => {
                let retryable = e.is_connect() || e.is_timeout() || e.is_request();
                let err = anyhow!("erro de rede: {e}");
                if retryable && attempt < 3 {
                    last_err = Some(err);
                    continue;
                }
                return Err(err);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("falha desconhecida")))
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}
