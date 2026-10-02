//! The gateway connection: one shard per bot (fine below 2,500 servers).

use std::sync::Arc;

use twilight_gateway::{CloseFrame, EventTypeFlags, Intents, Shard, ShardId, StreamExt};

use crate::bot::DiscordBot;
use crate::events;

/// What the bot needs to hear about. Message Content is privileged and only
/// requested when the bot follows its threads without mentions.
pub fn intents(follow_threads: bool) -> Intents {
    let base = Intents::GUILDS | Intents::GUILD_MESSAGES | Intents::DIRECT_MESSAGES;
    if follow_threads {
        base | Intents::MESSAGE_CONTENT
    } else {
        base
    }
}

/// Receive events until shutdown; each is handled in its own task.
pub async fn run(bot: Arc<DiscordBot>) {
    let mut shard = Shard::new(ShardId::ONE, bot.cfg.token.clone(), intents(bot.cfg.follow_threads));
    let wanted = EventTypeFlags::READY | EventTypeFlags::MESSAGE_CREATE | EventTypeFlags::INTERACTION_CREATE;
    let token = bot.shutdown.token().clone();
    loop {
        let item = tokio::select! {
            _ = token.cancelled() => {
                shard.close(CloseFrame::NORMAL);
                break;
            }
            item = shard.next_event(wanted) => item,
        };
        match item {
            None => break,
            Some(Ok(event)) => {
                let b = bot.clone();
                bot.shutdown.spawn(events::dispatch(b, event));
            }
            Some(Err(e)) => {
                // twilight reconnects by itself; 4014 means a privileged intent isn't enabled.
                tracing::warn!(bot = %bot.cfg.name, error = %e, "gateway error");
            }
        }
    }
    tracing::info!(bot = %bot.cfg.name, "gateway closed");
}
