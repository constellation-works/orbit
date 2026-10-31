//! Advisory reads of the Grok CLI account's billing window.

use std::io::Read;
use std::path::PathBuf;

use orbit_exec::{
    EnvironmentMode, ExecRequest, NoSandbox, StdinMode, run_process_streaming_stdout,
};
use serde_json::Value;

const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const MAX_BYTES: u64 = 64 * 1024;

/// Account state and transport settings from the actual provider child environment.
pub(super) struct GrokUsageRequest {
    home: PathBuf,
    environment: Vec<(String, String)>,
}

impl GrokUsageRequest {
    pub(super) fn from_env(env: &[(String, String)]) -> Option<Self> {
        let value = |name: &str| {
            env.iter()
                .rev()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
                .filter(|value| !value.is_empty())
        };
        // These can select another account or endpoint. Do not attribute the
        // cached first-party login's reading to that run, or send it elsewhere.
        for name in [
            "XAI_API_KEY",
            "GROK_CODE_XAI_API_KEY",
            "GROK_AUTH",
            "GROK_DEPLOYMENT_KEY",
            "GROK_OAUTH2_ISSUER",
            "GROK_OAUTH2_CLIENT_ID",
            "GROK_CLI_CHAT_PROXY_BASE_URL",
            "GROK_XAI_API_BASE_URL",
            "GROK_AUTH_PROVIDER_COMMAND",
            "GROK_MODELS_BASE_URL",
            "GROK_OIDC_ISSUER",
        ] {
            if value(name).is_some() {
                return None;
            }
        }
        let home = value("GROK_HOME")
            .map(PathBuf::from)
            .or_else(|| value("HOME").map(|home| PathBuf::from(home).join(".grok")))?;
        // A relative root depends on the child's cwd, not the host's cwd.
        if !home.is_absolute() {
            return None;
        }
        let environment = [
            "PATH",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
            "NO_PROXY",
            "no_proxy",
        ]
        .into_iter()
        .filter_map(|name| value(name).map(|value| (name.to_string(), value.to_string())))
        .collect();
        Some(Self { home, environment })
    }

    /// Read only the existing CLI credential. Never refresh it, log it, put
    /// it in argv, follow redirects, or let an unavailable reading fail a run.
    pub(super) fn read(&self) -> Option<Vec<u8>> {
        let path = self.home.join("auth.json");
        if !path.metadata().ok()?.is_file() {
            return None;
        }
        let mut auth = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut auth)
            .ok()?;
        if auth.len() as u64 > MAX_BYTES {
            return None;
        }
        let auth: Value = serde_json::from_slice(&auth).ok()?;
        let auth = auth.as_object()?;
        // Without Grok's full configuration resolver, several cached logins
        // cannot be attributed safely, even if only one is first-party.
        if auth.len() != 1 {
            return None;
        }
        let (_, credential) = auth.iter().find(|(issuer, credential)| {
            issuer.as_str() == "https://accounts.x.ai/sign-in"
                || (issuer.starts_with("https://auth.x.ai::")
                    && credential.get("oidc_issuer").and_then(Value::as_str)
                        == Some("https://auth.x.ai"))
        })?;
        let token = credential.get("key")?.as_str()?;
        if token.is_empty() || !token.bytes().all(|byte| byte.is_ascii_graphic()) {
            return None;
        }
        let headers = format!("Authorization: Bearer {token}\nX-XAI-Token-Auth: xai-grok-cli\n");
        let request = ExecRequest {
            program: "curl".to_string(),
            args: [
                "--disable",
                "--silent",
                "--fail",
                "--proto",
                "=https",
                "--max-time",
                "3",
                "--max-filesize",
                "65536",
                "--header",
                "@-",
                "--write-out",
                "\n%{http_code}",
                "--",
                BILLING_URL,
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            current_dir: None,
            timeout_ms: Some(4_000),
            stdin_mode: StdinMode::Bytes(headers.into_bytes()),
            environment_mode: EnvironmentMode::ClearAndSet(self.environment.clone()),
            debug: false,
        };
        let (result, mut body) = run_process_streaming_stdout(&request, &NoSandbox, |stdout| {
            let mut body = Vec::new();
            stdout
                .take(MAX_BYTES + 1)
                .read_to_end(&mut body)
                .map_err(|_| {
                    orbit_common::OrbitError::Execution("read Grok billing".to_string())
                })?;
            Ok(body)
        })
        .ok()?;
        if !result.success || body.len() as u64 > MAX_BYTES || !body.ends_with(b"\n200") {
            return None;
        }
        body.truncate(body.len() - 4);
        Some(body)
    }
}
