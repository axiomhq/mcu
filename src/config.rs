//! Load `~/.axiom.toml`.
//!
//! Expected shape (matches the axiom CLI):
//!
//! ```toml
//! active_deployments = "prod"   # optional
//!
//! [deployments.prod]
//! url = "https://api.axiom.co"
//! token = "xaat-..."
//! org_id = "..."
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Deployment {
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub org_id: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub active_deployments: Option<String>,
    #[serde(default)]
    pub deployments: HashMap<String, Deployment>,
}

impl Config {
    /// Load from `~/.axiom.toml`. Returns a clear error if the file is missing or malformed.
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).context("parsing axiom config")?;
        if cfg.deployments.is_empty() {
            bail!("no [deployments.*] entries in axiom config");
        }
        Ok(cfg)
    }

    /// Pick the deployment to use. Honors `active_deployments` when present.
    /// Falls back to a single deployment, or errors when ambiguous.
    pub fn active(&self) -> Result<(&str, &Deployment)> {
        if let Some(name) = self.active_deployments.as_deref().filter(|s| !s.is_empty()) {
            let dep = self
                .deployments
                .get(name)
                .ok_or_else(|| anyhow!("active_deployments=\"{name}\" not found"))?;
            return Ok((name, dep));
        }

        if self.deployments.len() == 1 {
            let (name, dep) = self.deployments.iter().next().unwrap();
            return Ok((name.as_str(), dep));
        }

        bail!(
            "multiple deployments configured but no active_deployments set; \
             pick one in ~/.axiom.toml"
        )
    }
}

fn config_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    let mut path = PathBuf::from(home);
    path.push(".axiom.toml");
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_deployment() {
        let text = r#"
            [deployments.prod]
            url = "https://api.axiom.co"
            token = "xaat-..."
            org_id = "heinz"
        "#;
        let cfg = Config::parse(text).unwrap();
        let (name, dep) = cfg.active().unwrap();
        assert_eq!(name, "prod");
        assert_eq!(dep.url, "https://api.axiom.co");
        assert_eq!(dep.org_id, "heinz");
    }

    #[test]
    fn honors_active_deployments() {
        let text = r#"
            active_deployments = "staging"

            [deployments.prod]
            url = "https://api.axiom.co"
            token = "p"
            org_id = "o"

            [deployments.staging]
            url = "https://staging.example.com"
            token = "s"
            org_id = "o"
        "#;
        let cfg = Config::parse(text).unwrap();
        let (name, dep) = cfg.active().unwrap();
        assert_eq!(name, "staging");
        assert_eq!(dep.url, "https://staging.example.com");
    }

    #[test]
    fn errors_when_multiple_deployments_and_no_active() {
        let text = r#"
            [deployments.a]
            url = "u"
            token = "t"
            org_id = "o"
            [deployments.b]
            url = "u"
            token = "t"
            org_id = "o"
        "#;
        let cfg = Config::parse(text).unwrap();
        assert!(cfg.active().is_err());
    }

    #[test]
    fn errors_when_no_deployments() {
        assert!(Config::parse("").is_err());
    }

    #[test]
    fn errors_when_active_deployment_missing() {
        let text = r#"
            active_deployments = "ghost"
            [deployments.prod]
            url = "u"
            token = "t"
            org_id = "o"
        "#;
        let cfg = Config::parse(text).unwrap();
        assert!(cfg.active().is_err());
    }
}
