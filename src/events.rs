//! What the bot does with each gateway event.

use std::sync::Arc;

use docsgpt_bot::docsgpt::Feedback;
use docsgpt_bot::{Ask, Scope, StatePatch, run_turn, submit_feedback};
use serde_json::json;
use twilight_gateway::Event;
use twilight_model::application::interaction::application_command::{CommandData, CommandOptionValue};
use twilight_model::application::interaction::{Interaction, InteractionData, InteractionType};
use twilight_model::channel::Message;
use twilight_model::channel::message::MessageFlags;
use twilight_model::http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType};
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, MessageMarker};

use crate::bot::{DiscordBot, THREAD_KIND};
use crate::files;
use crate::render::{self, DISLIKE_ID, LIKE_ID, STOP_ID, clean_incoming, mentions_someone_else};
use crate::surface::DiscordSurface;

/// Handle one gateway event (spawned per event).
pub async fn dispatch(bot: Arc<DiscordBot>, event: Event) {
    let result = match event {
        Event::MessageCreate(m) => on_message(&bot, m.0).await,
        Event::InteractionCreate(i) => on_interaction(&bot, i.0).await,
        Event::Ready(r) => {
            tracing::info!(bot = %bot.cfg.name, user = %r.user.name, guilds = r.guilds.len(), "gateway ready");
            Ok(())
        }
        _ => Ok(()),
    };
    if let Err(e) = result {
        tracing::warn!(bot = %bot.cfg.name, error = %format!("{e:#}"), "event handling failed");
    }
}

fn space(id: impl std::fmt::Display) -> String {
    id.to_string()
}

/// A question to answer and where.
struct Question {
    /// Channel the answer goes to (DM, thread, or the channel itself).
    channel: Id<ChannelMarker>,
    reply_to: Option<Id<MessageMarker>>,
    scope: Scope,
    state_scope: Scope,
    text: String,
    attachments: Vec<twilight_model::channel::Attachment>,
    agent: Option<String>,
}

/// The parent channel of a thread the bot opened.
async fn bot_thread_parent(bot: &DiscordBot, channel: Id<ChannelMarker>) -> Option<String> {
    let v = bot
        .core
        .storage
        .get_json(&bot.cfg.name, THREAD_KIND, &channel.to_string())
        .await
        .ok()??;
    v["parent"].as_str().map(str::to_string)
}

async fn on_message(bot: &Arc<DiscordBot>, msg: Message) -> anyhow::Result<()> {
    if msg.author.bot || msg.author.id == bot.user_id {
        return Ok(());
    }
    let mentioned = msg.mentions.iter().any(|m| m.id == bot.user_id);
    let text = clean_incoming(&msg.content, bot.user_id.get());
    let name = &bot.cfg.name;
    let Some(guild) = msg.guild_id else {
        // A DM: every message, one conversation per DM until /new.
        let scope = Scope::new(name, space(msg.channel_id), "0");
        return answer(
            bot,
            Question {
                channel: msg.channel_id,
                reply_to: None,
                state_scope: scope.clone(),
                scope,
                text,
                attachments: msg.attachments,
                agent: None,
            },
        )
        .await;
    };
    if !bot.cfg.guild_allowed(Some(guild.get())) {
        return Ok(());
    }
    if let Some(parent) = bot_thread_parent(bot, msg.channel_id).await {
        // In one of the bot's threads: follow up without a mention (unless off).
        let follow = mentioned
            || (bot.cfg.follow_threads
                && !mentions_someone_else(&msg.content, bot.user_id.get())
                && !msg.content.trim_start().starts_with('!'));
        if !follow {
            return Ok(());
        }
        return answer(
            bot,
            Question {
                channel: msg.channel_id,
                reply_to: None,
                scope: Scope::new(name, space(msg.channel_id), "0"),
                state_scope: Scope::new(name, parent, "0"),
                text,
                attachments: msg.attachments,
                agent: None,
            },
        )
        .await;
    }
    if !mentioned {
        return Ok(());
    }
    start_question(
        bot,
        msg.channel_id,
        msg.id,
        msg.author.id.get(),
        text,
        msg.attachments,
        None,
    )
    .await
}

/// A new question in a server channel: open a thread on the message and
/// answer there, or answer inline when threads aren't possible.
async fn start_question(
    bot: &Arc<DiscordBot>,
    channel: Id<ChannelMarker>,
    message: Id<MessageMarker>,
    user: u64,
    text: String,
    attachments: Vec<twilight_model::channel::Attachment>,
    agent: Option<String>,
) -> anyhow::Result<()> {
    let name = &bot.cfg.name;
    let state_scope = Scope::new(name, space(channel), "0");
    if bot.cfg.threads {
        let title = render::thread_name(&text);
        match bot.http.create_thread_from_message(channel, message, &title).await {
            Ok(resp) => {
                let thread = resp.model().await?;
                bot.core
                    .storage
                    .put_json(
                        name,
                        THREAD_KIND,
                        &thread.id.to_string(),
                        &json!({"parent": channel.to_string()}),
                    )
                    .await?;
                return answer(
                    bot,
                    Question {
                        channel: thread.id,
                        reply_to: None,
                        scope: Scope::new(name, space(thread.id), "0"),
                        state_scope,
                        text,
                        attachments,
                        agent,
                    },
                )
                .await;
            }
            Err(e) => tracing::info!(error = %e, "can't open a thread here; answering inline"),
        }
    }
    let scope = Scope::new(name, space(channel), "0").with_namespace(format!("user:{user}"));
    answer(
        bot,
        Question {
            channel,
            reply_to: Some(message),
            scope,
            state_scope,
            text,
            attachments,
            agent,
        },
    )
    .await
}

async fn answer(bot: &Arc<DiscordBot>, q: Question) -> anyhow::Result<()> {
    let surface = DiscordSurface {
        bot: bot.clone(),
        channel: q.channel,
        reply_to: q.reply_to,
    };
    let mut text = q.text;
    let mut attachment_ids = Vec::new();
    let mut agent = q.agent;
    if bot.cfg.files && !q.attachments.is_empty() {
        // Uploads need the key of the agent that will answer.
        let chosen = match &agent {
            Some(name) => bot
                .core
                .agents
                .get(name)
                .cloned()
                .unwrap_or_else(|| bot.core.agents.default_agent().clone()),
            None => files::agent_for(bot, &text, &q.state_scope).await,
        };
        let prepared = files::prepare(bot, &chosen, &q.attachments, text.is_empty()).await;
        for p in &prepared.problems {
            let _ = docsgpt_bot::Surface::notice(&surface, p).await;
        }
        if text.is_empty()
            && let Some(t) = prepared.transcript
        {
            text = t;
        }
        attachment_ids = prepared.attachments;
        if !attachment_ids.is_empty() && agent.is_none() {
            // Route now, so the question goes to the agent the files belong to.
            if let docsgpt_bot::Routed::Agent { agent: a, question, .. } =
                bot.core.agents.route(&text, Some(&chosen.name))
            {
                agent = Some(a.name.clone());
                text = question;
            }
        }
    }
    if text.is_empty() && attachment_ids.is_empty() {
        return Ok(());
    }
    let mut ask = Ask::new(q.scope, text).attachments(attachment_ids);
    ask.state_scope = Some(q.state_scope);
    ask.agent = agent;
    match run_turn(&bot.core, &surface, ask).await {
        Ok(r) => tracing::debug!(report = ?r, "turn done"),
        Err(e) => tracing::warn!(error = %e, "turn failed"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Interactions
// ---------------------------------------------------------------------------

fn reply(kind: InteractionResponseType, data: InteractionResponseData) -> InteractionResponse {
    InteractionResponse { kind, data: Some(data) }
}

fn ephemeral(text: &str) -> InteractionResponse {
    reply(
        InteractionResponseType::ChannelMessageWithSource,
        InteractionResponseData {
            content: Some(text.into()),
            flags: Some(MessageFlags::EPHEMERAL),
            ..Default::default()
        },
    )
}

async fn respond(bot: &DiscordBot, i: &Interaction, r: &InteractionResponse) -> anyhow::Result<()> {
    bot.http
        .interaction(bot.application_id)
        .create_response(i.id, &i.token, r)
        .await?;
    Ok(())
}

async fn on_interaction(bot: &Arc<DiscordBot>, i: Interaction) -> anyhow::Result<()> {
    match (&i.kind, &i.data) {
        (InteractionType::ApplicationCommand, Some(InteractionData::ApplicationCommand(cmd))) => {
            let cmd = cmd.as_ref().clone();
            on_command(bot, &i, &cmd).await
        }
        (InteractionType::MessageComponent, Some(InteractionData::MessageComponent(data))) => {
            on_button(bot, &i, &data.custom_id).await
        }
        _ => Ok(()),
    }
}

fn option(cmd: &CommandData, name: &str) -> Option<String> {
    cmd.options
        .iter()
        .find(|o| o.name == name)
        .and_then(|o| match &o.value {
            CommandOptionValue::String(s) => Some(s.clone()),
            _ => None,
        })
}

/// Conversation and agent-choice scopes for the channel an interaction came from.
async fn scopes_for(bot: &DiscordBot, i: &Interaction) -> Option<(Scope, Scope)> {
    let channel = i.channel.as_ref().map(|c| c.id)?;
    let name = &bot.cfg.name;
    if i.guild_id.is_none() {
        let s = Scope::new(name, space(channel), "0");
        return Some((s.clone(), s));
    }
    if let Some(parent) = bot_thread_parent(bot, channel).await {
        return Some((Scope::new(name, space(channel), "0"), Scope::new(name, parent, "0")));
    }
    let user = i.author_id().map(|u| u.get()).unwrap_or(0);
    Some((
        Scope::new(name, space(channel), "0").with_namespace(format!("user:{user}")),
        Scope::new(name, space(channel), "0"),
    ))
}

async fn on_command(bot: &Arc<DiscordBot>, i: &Interaction, cmd: &CommandData) -> anyhow::Result<()> {
    let Some((scope, state_scope)) = scopes_for(bot, i).await else {
        return respond(bot, i, &ephemeral("I can't tell which channel this is.")).await;
    };
    match cmd.name.as_str() {
        "ask" => {
            let question = option(cmd, "question").unwrap_or_default();
            let agent = option(cmd, "agent");
            // Post the question as the command's reply, then answer from that message.
            let shown = format!("> {}", question.replace('\n', "\n> "));
            let data = InteractionResponseData {
                content: Some(docsgpt_bot::util::truncate_chars(&shown, 1990)),
                allowed_mentions: Some(Default::default()),
                ..Default::default()
            };
            respond(bot, i, &reply(InteractionResponseType::ChannelMessageWithSource, data)).await?;
            let original = bot
                .http
                .interaction(bot.application_id)
                .response(&i.token)
                .await?
                .model()
                .await?;
            let user = i.author_id().map(|u| u.get()).unwrap_or(0);
            if i.guild_id.is_some() && bot_thread_parent(bot, original.channel_id).await.is_none() {
                return start_question(bot, original.channel_id, original.id, user, question, Vec::new(), agent).await;
            }
            answer(
                bot,
                Question {
                    channel: original.channel_id,
                    reply_to: None,
                    scope,
                    state_scope,
                    text: question,
                    attachments: Vec::new(),
                    agent,
                },
            )
            .await
        }
        "agents" => {
            let active = bot
                .core
                .storage
                .chat_state(&state_scope)
                .await
                .ok()
                .and_then(|s| s.active_agent);
            let current = active
                .as_deref()
                .and_then(|a| bot.core.agents.get(a))
                .unwrap_or_else(|| bot.core.agents.default_agent());
            let lines: Vec<String> = bot
                .core
                .agents
                .iter()
                .map(|a| {
                    let mark = if a.name == current.name { " (current)" } else { "" };
                    let about = a.description.as_deref().map(|d| format!(" — {d}")).unwrap_or_default();
                    format!("• `#{}`{mark}{about}", a.name)
                })
                .collect();
            let text = format!(
                "{}\n\nSwitch with `/agent`, or start a message with `#name` to ask one agent once.",
                lines.join("\n")
            );
            respond(bot, i, &ephemeral(&text)).await
        }
        "agent" => {
            let wanted = option(cmd, "name").unwrap_or_default();
            let text = match bot.core.agents.get(&wanted) {
                Some(a) => {
                    bot.core
                        .storage
                        .update_chat_state(&state_scope, StatePatch::active_agent(Some(&a.name)))
                        .await?;
                    format!("Now answering with `#{}` here.", a.name)
                }
                None => format!("Unknown agent `{wanted}`."),
            };
            respond(bot, i, &ephemeral(&text)).await
        }
        "new" => {
            for a in bot.core.agents.iter() {
                bot.core.storage.clear_conversation(&scope, &a.name).await?;
            }
            respond(bot, i, &ephemeral("Started a new conversation here.")).await
        }
        _ => respond(bot, i, &ephemeral("Unknown command.")).await,
    }
}

async fn on_button(bot: &Arc<DiscordBot>, i: &Interaction, custom_id: &str) -> anyhow::Result<()> {
    let Some(message) = &i.message else {
        return Ok(());
    };
    let key = DiscordSurface::key(message.channel_id, message.id);
    match custom_id {
        STOP_ID => {
            respond(
                bot,
                i,
                &InteractionResponse {
                    kind: InteractionResponseType::DeferredUpdateMessage,
                    data: None,
                },
            )
            .await?;
            let stopped = bot.cancels.cancel(&key);
            tracing::info!(bot = %bot.cfg.name, %key, stopped, "stop pressed");
            Ok(())
        }
        LIKE_ID | DISLIKE_ID => {
            let clicked = if custom_id == LIKE_ID {
                Feedback::Like
            } else {
                Feedback::Dislike
            };
            // Clicking the selected button again takes the feedback back.
            let now = if render::selected_feedback(&message.components) == Some(clicked) {
                None
            } else {
                Some(clicked)
            };
            match submit_feedback(&bot.core, &key, now.unwrap_or(Feedback::Clear)).await {
                Ok(true) => {
                    tracing::info!(bot = %bot.cfg.name, %key, feedback = ?now, "feedback sent");
                    let data = InteractionResponseData {
                        components: Some(render::feedback_row(now)),
                        ..Default::default()
                    };
                    respond(bot, i, &reply(InteractionResponseType::UpdateMessage, data)).await
                }
                Ok(false) => respond(bot, i, &ephemeral("This answer can't be rated anymore.")).await,
                Err(e) => {
                    tracing::warn!(error = %e, "feedback failed");
                    respond(
                        bot,
                        i,
                        &ephemeral("Couldn't record that right now; try again in a moment."),
                    )
                    .await
                }
            }
        }
        _ => Ok(()),
    }
}
