//! One Discord bot: its REST client, identity, DocsGPT core and runtime state.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use docsgpt_bot::{Agents, BotCore, CancelRegistry, Shutdown, Storage};
use twilight_http::Client;
use twilight_model::id::Id;
use twilight_model::id::marker::{ApplicationMarker, GuildMarker, RoleMarker, UserMarker};

use crate::config::{BotConfig, Config};

pub struct DiscordBot {
    pub cfg: BotConfig,
    pub core: BotCore,
    pub http: Arc<Client>,
    /// For downloading attachments from Discord's CDN.
    pub web: reqwest::Client,
    pub user_id: Id<UserMarker>,
    pub user_name: String,
    pub application_id: Id<ApplicationMarker>,
    /// Running answers by `"{channel}:{message}"`, for the Stop button.
    pub cancels: CancelRegistry,
    pub shutdown: Shutdown,
    /// The bot's managed role per server (Discord's autocomplete often
    /// mentions that role instead of the bot).
    managed_roles: Mutex<HashMap<Id<GuildMarker>, Option<Id<RoleMarker>>>>,
}

/// Storage record kind for threads the bot opened (id → parent channel).
pub const THREAD_KIND: &str = "thread";

impl DiscordBot {
    /// Check the token and build the bot.
    pub async fn init(
        cfg: BotConfig,
        global: &Config,
        storage: Arc<dyn Storage>,
        web: reqwest::Client,
        shutdown: Shutdown,
    ) -> Result<Arc<Self>> {
        let mut builder = Client::builder()
            .token(cfg.token.clone())
            .timeout(Duration::from_secs(30));
        if let Some(host) = &cfg.discord_api_host {
            // Tests: plain HTTP to a mock, which doesn't rate limit.
            builder = builder.proxy(host.clone(), true).ratelimiter(None);
        }
        let http = Arc::new(builder.build());
        let me = http
            .current_user()
            .await
            .with_context(|| format!("bot {:?}: Discord rejected the token", cfg.name))?
            .model()
            .await?;
        let app = http.current_user_application().await?.model().await?;
        let client = docsgpt_bot::docsgpt::Client::builder(cfg.api_base(&global.api_base))
            .http_client(web.clone())
            .build()
            .context("DocsGPT client")?;
        let core = BotCore::new(cfg.name.clone(), client, Agents::new(cfg.agents.clone())?, storage);
        tracing::info!(bot = %cfg.name, user = %me.name, agents = core.agents.len(), "connected to Discord");
        Ok(Arc::new(Self {
            cfg,
            core,
            http,
            web,
            user_id: me.id,
            user_name: me.name,
            application_id: app.id,
            cancels: CancelRegistry::default(),
            shutdown,
            managed_roles: Mutex::new(HashMap::new()),
        }))
    }

    /// The bot's own managed role in `guild`, looked up once.
    pub async fn managed_role(&self, guild: Id<GuildMarker>) -> Option<Id<RoleMarker>> {
        if let Some(cached) = self.managed_roles.lock().unwrap().get(&guild) {
            return *cached;
        }
        let roles = match self.http.roles(guild).await {
            Ok(resp) => resp.model().await.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        let role = match roles {
            Ok(roles) => roles
                .into_iter()
                .find(|r| r.tags.as_ref().and_then(|t| t.bot_id) == Some(self.user_id))
                .map(|r| r.id),
            Err(e) => {
                // Not cached: try again on the next role mention.
                tracing::warn!(error = %e, %guild, "could not list the server's roles");
                return None;
            }
        };
        self.managed_roles.lock().unwrap().insert(guild, role);
        role
    }

    /// OAuth2 URL that adds the bot to a server with the permissions it needs.
    pub fn invite_url(&self) -> String {
        invite_url(self.application_id.get())
    }
}

/// Send messages, send messages in threads, create public threads, embed
/// links, attach files, read message history, view channels.
pub const PERMISSIONS: u64 = (1 << 11) | (1 << 38) | (1 << 35) | (1 << 14) | (1 << 15) | (1 << 16) | (1 << 10);

pub fn invite_url(application_id: u64) -> String {
    format!(
        "https://discord.com/oauth2/authorize?client_id={application_id}&scope=bot+applications.commands&permissions={PERMISSIONS}"
    )
}

/// Time between typing indicators (each lasts 10 s).
pub const TYPING_EVERY: Duration = Duration::from_secs(8);
