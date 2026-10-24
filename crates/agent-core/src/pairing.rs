//! Pairing from the command line, shared by the agents: where the token
//! comes from, and `--pair`, which connects once, pins the hub certificate
//! and returns instead of serving (no Ctrl+C after "connected").

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use proto::NodeKind;
use tracing::info;

use crate::{handshake, resolve_hub, AgentConfig, AgentOptions};

/// How long `--pair` waits for the hub (mDNS, connection, `HelloAck`).
const PAIR_TIMEOUT: Duration = Duration::from_secs(20);

/// Pairing options from the command line.
#[derive(Debug, Default)]
pub struct PairArgs {
    /// Hub address (`host` or `host:port`).
    pub hub: Option<String>,
    /// The token itself (kept for compatibility: it stays in the shell
    /// history, so the docs use `token_file` or the prompt).
    pub token: Option<String>,
    /// A file holding the token.
    pub token_file: Option<PathBuf>,
    /// Forget the pinned certificate.
    pub unpin: bool,
    /// Pair now: forget the pin, ask for the token if there is none, and
    /// connect once ([`pair`]).
    pub pair: bool,
}

impl PairArgs {
    /// Apply the options to the config at `path` and save it if anything
    /// changed. With `pair` and no token anywhere, asks for it on the
    /// terminal without echo.
    pub fn apply(&self, path: &Path) -> Result<AgentConfig> {
        let mut cfg = AgentConfig::load_or_init(path)?;
        let mut changed = false;
        if let Some(hub) = &self.hub {
            cfg.hub = hub.trim().to_string();
            changed = true;
        }
        if let Some(token) = self.given_token()? {
            cfg.token = token;
            changed = true;
        }
        if self.pair && cfg.token.is_empty() {
            cfg.token = prompt_token()?;
            changed = true;
        }
        if (self.unpin || self.pair) && !cfg.hub_fingerprint.is_empty() {
            cfg.hub_fingerprint.clear();
            changed = true;
        }
        if changed {
            cfg.save(path)?;
            info!("saved {}", path.display());
        }
        Ok(cfg)
    }

    /// The token from `token` or `token_file`, checked and normalised.
    fn given_token(&self) -> Result<Option<String>> {
        let (text, what) = match (&self.token, &self.token_file) {
            (Some(_), Some(_)) => bail!("use either --token or --token-file, not both"),
            (Some(t), None) => (t.clone(), "--token".to_string()),
            (None, Some(f)) => (
                std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?,
                f.display().to_string(),
            ),
            (None, None) => return Ok(None),
        };
        parse(&text)
            .with_context(|| format!("{what}: the token is 64 hex characters"))
            .map(Some)
    }
}

/// The token, trimmed and in lower case, if it is 64 hex characters.
fn parse(text: &str) -> Result<String> {
    let t = text.trim().to_lowercase();
    proto::parse_token(&t).ok_or_else(|| anyhow!("not 64 hex characters"))?;
    Ok(t)
}

/// Ask for the token on the terminal, without echo.
pub fn prompt_token() -> Result<String> {
    if !std::io::stdin().is_terminal() {
        bail!("no pairing token: pass --token-file FILE (or run in a terminal to type it)");
    }
    loop {
        let text = prompt_hidden("Pairing token (shown by `rawkvm-hub --show-token`): ")?;
        match parse(&text) {
            Ok(t) => return Ok(t),
            Err(_) => eprintln!("That is not 64 hex characters; try again."),
        }
    }
}

/// Read a line from the terminal without echo.
pub fn prompt_hidden(prompt: &str) -> Result<String> {
    rpassword::prompt_password(prompt).context("reading from the terminal")
}

/// Connect once with the config of `opts`, present `kind`, pin the hub
/// certificate and return the hub's address once the hub accepted the
/// token. The session is closed at once; the service does the rest.
pub async fn pair(opts: &AgentOptions, kind: NodeKind) -> Result<std::net::SocketAddr> {
    let attempt = async {
        let mut cfg = AgentConfig::load_or_init(&opts.config_path)?;
        cfg.token_bytes()?;
        let addr = resolve_hub(&cfg).await?;
        let accepted = handshake(opts, &mut cfg, addr, kind)
            .await
            .map_err(|e| match e {
                crate::End::Fatal(e) | crate::End::Retry(e) => e,
            })?;
        if cfg.hub_fingerprint.is_empty() {
            return Err(anyhow!("the hub presented no certificate to pin"));
        }
        accepted.conn.close(0u32.into(), b"paired");
        // Let the close reach the hub before the endpoint goes away.
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(addr)
    };
    tokio::time::timeout(PAIR_TIMEOUT, attempt)
        .await
        .map_err(|_| anyhow!("no answer from the hub in {} s", PAIR_TIMEOUT.as_secs()))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_file_and_flags() {
        let dir = std::env::temp_dir().join(format!("rawkvm-pair-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent.toml");
        let file = dir.join("token");
        let token = "AB".repeat(32);
        std::fs::write(&file, format!("{token}\n")).unwrap();
        AgentConfig {
            hub_fingerprint: "cd".repeat(32),
            ..Default::default()
        }
        .save(&path)
        .unwrap();

        let cfg = PairArgs {
            hub: Some(" 192.168.1.28 ".into()),
            token_file: Some(file.clone()),
            ..Default::default()
        }
        .apply(&path)
        .unwrap();
        assert_eq!(cfg.hub, "192.168.1.28");
        assert_eq!(cfg.token, "ab".repeat(32));
        assert_eq!(cfg.hub_fingerprint, "cd".repeat(32), "kept without --pair");

        // --pair forgets the pin; the saved token is reused (no prompt).
        let cfg = PairArgs {
            pair: true,
            ..Default::default()
        }
        .apply(&path)
        .unwrap();
        assert!(cfg.hub_fingerprint.is_empty());
        assert_eq!(AgentConfig::load_or_init(&path).unwrap().token, cfg.token);

        std::fs::write(&file, "not a token").unwrap();
        let bad = PairArgs {
            token_file: Some(file.clone()),
            ..Default::default()
        };
        assert!(format!("{:#}", bad.apply(&path).unwrap_err()).contains("64 hex"));
        let both = PairArgs {
            token: Some(token),
            token_file: Some(file),
            ..Default::default()
        };
        assert!(both.apply(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
