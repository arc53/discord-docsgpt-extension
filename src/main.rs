use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use docsgpt_bot::Shutdown;
use docsgpt_bot::storage::MESSAGE_REF_KIND;
use docsgpt_discord::bot::DiscordBot;
use docsgpt_discord::config::{self, DEFAULT_SQLITE_PATH};
use docsgpt_discord::{commands, gateway};

/// Discord bots for DocsGPT agents.
#[derive(Parser, Debug)]
#[command(name = "docsgpt-discord", version)]
struct Cli {
    /// Config file (default: $DOCSGPT_DISCORD_CONFIG, then ./docsgpt-discord.toml, then environment variables).
    #[arg(short, long)]
    config: Option<PathBuf>,
    /// Check the config and tokens, print each bot's invite link, then exit.
    #[arg(long)]
    check: bool,
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,twilight_gateway=warn,twilight_http=warn"));
    if std::env::var("LOG_FORMAT").is_ok_and(|v| v == "json") {
        tracing_subscriber::fmt().with_env_filter(filter).json().init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    init_tracing();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cli = Cli::parse();
    let cfg = config::load(cli.config.as_deref())?;
    let storage = docsgpt_bot::storage::open(&cfg.storage, DEFAULT_SQLITE_PATH).await?;
    let web = reqwest::Client::builder()
        .user_agent(concat!("docsgpt-discord/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .build()
        .context("building http client")?;
    let shutdown = Shutdown::new();

    let mut bots = Vec::new();
    for b in cfg.bots.clone() {
        bots.push(DiscordBot::init(b, &cfg, storage.clone(), web.clone(), shutdown.clone()).await?);
    }
    if cli.check {
        for b in &bots {
            println!(
                "ok: {} → {} ({} agent(s))\n    invite: {}",
                b.cfg.name,
                b.user_name,
                b.core.agents.len(),
                b.invite_url()
            );
        }
        return Ok(());
    }

    let mut tasks = Vec::new();
    for b in &bots {
        if b.cfg.commands {
            let defs = commands::definitions(&b.cfg.agents);
            match b.http.interaction(b.application_id).set_global_commands(&defs).await {
                Ok(_) => tracing::info!(bot = %b.cfg.name, commands = defs.len(), "slash commands registered"),
                Err(e) => tracing::warn!(bot = %b.cfg.name, error = %e, "registering slash commands failed"),
            }
        }
        tracing::info!(bot = %b.cfg.name, invite = %b.invite_url(), "add the bot to a server with this link");
        tasks.push(tokio::spawn(gateway::run(b.clone())));
    }
    {
        // Message refs map an answer to its conversation for 👍/👎; drop old ones daily.
        let (storage, token) = (storage.clone(), shutdown.token().clone());
        tokio::spawn(async move {
            loop {
                if let Ok(n) = storage.prune_json(MESSAGE_REF_KIND, docsgpt_bot::MESSAGE_REF_TTL).await
                    && n > 0
                {
                    tracing::info!(pruned = n, "pruned old message refs");
                }
                tokio::select! {
                    _ = token.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_secs(24 * 3600)) => {}
                }
            }
        });
    }
    shutdown.wait_for_signal().await;
    for t in tasks {
        let _ = tokio::time::timeout(Duration::from_secs(5), t).await;
    }
    if !shutdown.drain(Duration::from_secs(30)).await {
        tracing::warn!("some answers were cut off by shutdown");
    }
    tracing::info!("bye");
    Ok(())
}
