//! Configuration: `docsgpt-discord.toml` with `${ENV}` references, or the v1
//! environment layout (DISCORD_TOKEN / API_KEY, plus API_KEY_<NAME>).

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use docsgpt_bot::config::{
    AgentConfig, Backend, DEFAULT_API_BASE, StorageConfig, agents_from_env, expand_env, find_config, normalize_name,
};
use serde::Deserialize;

/// SQLite file used when the config names none.
pub const DEFAULT_SQLITE_PATH: &str = "data/docsgpt-discord.db";

const MONGODB_REMOVED: &str = "MongoDB storage was removed in version 2. Conversations now live in \
SQLite (the default; keep data/ on a volume) or in memory. Remove STORAGE_TYPE=mongodb and the MONGODB_* \
variables. DocsGPT keeps the conversation history, so nothing needs migrating; chats continue in new \
conversations. See \"Upgrading from version 1\" in the README.";

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_api_base")]
    pub api_base: String,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub bots: Vec<BotConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BotConfig {
    /// Short identifier used in storage keys and logs.
    pub name: String,
    /// Bot token from the Discord developer portal.
    pub token: String,
    /// In servers, answer a mention in a new thread on that message.
    #[serde(default = "default_true")]
    pub threads: bool,
    /// Answer every message in the bot's threads, not only mentions. Needs the
    /// privileged Message Content intent (Developer Portal → Bot).
    #[serde(default = "default_true")]
    pub follow_threads: bool,
    /// Edit the answer in as it is written.
    #[serde(default = "default_true")]
    pub streaming: bool,
    /// Accept files from users and send files tools produce.
    #[serde(default = "default_true")]
    pub files: bool,
    #[serde(default = "default_max_file_mb")]
    pub max_file_mb: u64,
    /// Restrict the bot to these server (guild) ids; DMs are always allowed. Empty = all.
    #[serde(default)]
    pub allowed_guilds: Vec<u64>,
    /// Register slash commands (/ask, /agents, /agent, /new) at startup.
    #[serde(default = "default_true")]
    pub commands: bool,
    /// Per-bot DocsGPT URL.
    #[serde(default)]
    pub api_base: Option<String>,
    /// Discord API host (tests point this at a mock, e.g. `127.0.0.1:PORT`).
    #[serde(default)]
    pub discord_api_host: Option<String>,
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
}

fn default_api_base() -> String {
    DEFAULT_API_BASE.into()
}
fn default_true() -> bool {
    true
}
fn default_max_file_mb() -> u64 {
    20
}

impl BotConfig {
    pub fn api_base<'a>(&'a self, global: &'a str) -> &'a str {
        self.api_base.as_deref().unwrap_or(global)
    }

    pub fn guild_allowed(&self, guild: Option<u64>) -> bool {
        match guild {
            None => true,
            Some(g) => self.allowed_guilds.is_empty() || self.allowed_guilds.contains(&g),
        }
    }
}

/// Load configuration: explicit path → `DOCSGPT_DISCORD_CONFIG` → `docsgpt-discord.toml` → environment.
pub fn load(explicit: Option<&Path>) -> Result<Config> {
    let mut cfg = match find_config(explicit, "DOCSGPT_DISCORD_CONFIG", "docsgpt-discord.toml") {
        Some(path) => {
            let raw = std::fs::read_to_string(&path).with_context(|| format!("reading config {}", path.display()))?;
            let cfg = parse(&raw).with_context(|| format!("parsing config {}", path.display()))?;
            tracing::info!(path = %path.display(), bots = cfg.bots.len(), "loaded config file");
            cfg
        }
        None => from_env(std::env::vars())?,
    };
    normalize(&mut cfg)?;
    Ok(cfg)
}

/// Expand `${VAR}` references and parse a TOML config.
pub fn parse(raw: &str) -> Result<Config> {
    let expanded = expand_env(raw)?;
    toml::from_str(&expanded).map_err(|e| anyhow!(e.to_string()))
}

/// The single-bot layout of v1: DISCORD_TOKEN, API_KEY (and now API_KEY_<NAME>).
pub fn from_env(vars: impl IntoIterator<Item = (String, String)>) -> Result<Config> {
    let vars: Vec<(String, String)> = vars.into_iter().collect();
    let get = |k: &str| {
        vars.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    let backend = match get("STORAGE_TYPE").unwrap_or_default().to_ascii_lowercase().as_str() {
        "" | "sqlite" => Backend::Sqlite,
        "memory" => Backend::Memory,
        "mongodb" | "mongo" => bail!(MONGODB_REMOVED),
        other => bail!("unknown STORAGE_TYPE {other:?}; use sqlite or memory"),
    };
    let Some(token) = get("DISCORD_TOKEN") else {
        bail!("no docsgpt-discord.toml found and DISCORD_TOKEN is not set");
    };
    let flag = |k: &str, default: bool| match get(k).as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("false" | "0" | "no" | "off") => false,
        Some(_) => true,
        None => default,
    };
    let agents = agents_from_env(vars.iter().cloned());
    if agents.is_empty() {
        bail!("API_KEY (or API_KEY_<NAME>) is not set");
    }
    Ok(Config {
        api_base: get("API_BASE").unwrap_or_else(default_api_base),
        storage: StorageConfig {
            backend,
            path: get("SQLITE_PATH"),
        },
        bots: vec![BotConfig {
            name: get("BOT_NAME").unwrap_or_else(|| "docsgpt".into()),
            token,
            threads: flag("THREADS", true),
            follow_threads: flag("FOLLOW_THREADS", true),
            streaming: flag("STREAMING", true),
            files: flag("FILES", true),
            max_file_mb: get("MAX_FILE_MB")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(default_max_file_mb),
            allowed_guilds: get("ALLOWED_GUILDS")
                .map(|v| v.split(',').filter_map(|g| g.trim().parse().ok()).collect())
                .unwrap_or_default(),
            commands: flag("SLASH_COMMANDS", true),
            api_base: None,
            discord_api_host: get("DISCORD_API_HOST"),
            agents,
        }],
    })
}

fn normalize(cfg: &mut Config) -> Result<()> {
    if cfg.bots.is_empty() {
        bail!("no bots configured");
    }
    let mut names = HashSet::new();
    for bot in &mut cfg.bots {
        bot.name = normalize_name("bot", &bot.name)?;
        if !names.insert(bot.name.clone()) {
            bail!("duplicate bot name {:?}", bot.name);
        }
        if bot.token.trim().is_empty() {
            bail!("bot {:?}: token is empty", bot.name);
        }
        bot.token = bot.token.trim().trim_start_matches("Bot ").to_string();
        bot.agents = docsgpt_bot::Agents::new(std::mem::take(&mut bot.agents))
            .map_err(|e| anyhow!("bot {:?}: {e}", bot.name))?
            .iter()
            .cloned()
            .collect();
    }
    cfg.api_base = cfg.api_base.trim_end_matches('/').to_string();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn v1_env_layout_still_works() {
        let mut cfg = from_env(env(&[
            ("DISCORD_TOKEN", "Bot abc"),
            ("API_KEY", "k"),
            ("API_KEY_SALES", "k2"),
        ]))
        .unwrap();
        normalize(&mut cfg).unwrap();
        let bot = &cfg.bots[0];
        assert_eq!(bot.token, "abc");
        assert!(bot.threads && bot.follow_threads && bot.streaming);
        assert_eq!(
            bot.agents.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            ["default", "sales"]
        );
        assert_eq!(cfg.storage.backend, Backend::Sqlite);
    }

    #[test]
    fn mongodb_is_refused_with_the_upgrade_message() {
        let err = from_env(env(&[
            ("DISCORD_TOKEN", "t"),
            ("API_KEY", "k"),
            ("STORAGE_TYPE", "mongodb"),
        ]))
        .unwrap_err()
        .to_string();
        assert!(err.contains("MongoDB storage was removed"), "{err}");
        assert!(
            from_env(env(&[("DISCORD_TOKEN", "t")])).is_err(),
            "an agent key is required"
        );
    }

    #[test]
    fn example_config_parses() {
        let raw =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docsgpt-discord.example.toml")).unwrap();
        let raw = regex::Regex::new(r"\$\{[A-Z_]+\}")
            .unwrap()
            .replace_all(&raw, "placeholder");
        let mut cfg: Config = toml::from_str(&raw).unwrap();
        normalize(&mut cfg).unwrap();
        assert_eq!(cfg.bots.len(), 2);
        assert!(!cfg.bots[1].follow_threads);
    }
}
