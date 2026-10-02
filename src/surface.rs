//! How a turn looks in Discord: typing, a message edited as the answer is
//! written (continued in new messages past 2000 characters) with a Stop
//! button, then the final text with sources and 👍/👎 buttons.

use std::sync::Arc;

use async_trait::async_trait;
use docsgpt_bot::docsgpt::Download;
use docsgpt_bot::util::safe_filename;
use docsgpt_bot::{CancelGuard, Final, Progress, Surface, Turn};
use tokio_util::sync::CancellationToken;
use twilight_model::channel::message::component::Component;
use twilight_model::channel::message::{AllowedMentions, Embed};
use twilight_model::http::attachment::Attachment;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, MessageMarker};
use twilight_util::builder::embed::{EmbedBuilder, ImageSource};

use crate::bot::{DiscordBot, TYPING_EVERY};
use crate::render;

/// Where one question is answered.
pub struct DiscordSurface {
    pub bot: Arc<DiscordBot>,
    pub channel: Id<ChannelMarker>,
    /// Reply to this message (inline answers in servers).
    pub reply_to: Option<Id<MessageMarker>>,
}

/// The messages holding the answer so far.
pub struct DiscordDraft {
    messages: Vec<(Id<MessageMarker>, String)>,
    typing: CancellationToken,
    cancels: Vec<CancelGuard>,
}

fn platform(e: impl std::fmt::Display) -> docsgpt_bot::Error {
    docsgpt_bot::Error::platform(e.to_string())
}

/// Answers must never ping anyone.
fn no_pings() -> AllowedMentions {
    AllowedMentions::default()
}

impl DiscordSurface {
    pub fn key(channel: Id<ChannelMarker>, message: Id<MessageMarker>) -> String {
        format!("{channel}:{message}")
    }

    fn spawn_typing(&self) -> CancellationToken {
        let token = CancellationToken::new();
        let (http, channel, t) = (self.bot.http.clone(), self.channel, token.clone());
        tokio::spawn(async move {
            loop {
                if let Err(e) = http.create_typing_trigger(channel).await {
                    tracing::debug!(error = %e, "typing indicator failed");
                    break;
                }
                tokio::select! {
                    _ = t.cancelled() => break,
                    _ = tokio::time::sleep(TYPING_EVERY) => {}
                }
            }
        });
        token
    }

    async fn create(
        &self,
        content: &str,
        components: &[Component],
        embeds: &[Embed],
        first: bool,
    ) -> Result<Id<MessageMarker>, docsgpt_bot::Error> {
        let mentions = no_pings();
        let mut req = self
            .bot
            .http
            .create_message(self.channel)
            .content(content)
            .allowed_mentions(Some(&mentions));
        if !components.is_empty() {
            req = req.components(components);
        }
        if !embeds.is_empty() {
            req = req.embeds(embeds);
        }
        if first && let Some(m) = self.reply_to {
            req = req.reply(m).fail_if_not_exists(false);
        }
        let msg = req.await.map_err(platform)?.model().await.map_err(platform)?;
        Ok(msg.id)
    }

    async fn edit(
        &self,
        id: Id<MessageMarker>,
        content: &str,
        components: &[Component],
        embeds: Option<&[Embed]>,
    ) -> Result<(), docsgpt_bot::Error> {
        let mentions = no_pings();
        let mut req = self
            .bot
            .http
            .update_message(self.channel, id)
            .content(Some(content))
            .components(Some(components))
            .allowed_mentions(Some(&mentions));
        if let Some(e) = embeds {
            req = req.embeds(Some(e));
        }
        req.await.map_err(platform)?;
        Ok(())
    }

    /// Make the draft show `texts`: edit messages whose text changed, add
    /// messages for new pieces. `last` components go on the last message,
    /// every earlier message ends up with none.
    async fn show(
        &self,
        d: &mut DiscordDraft,
        texts: &[String],
        last: &[Component],
        embeds: &[Embed],
        turn: Option<&Turn>,
    ) -> Result<(), docsgpt_bot::Error> {
        let shown_before = d.messages.len();
        for (i, text) in texts.iter().enumerate() {
            let is_last = i + 1 == texts.len();
            let components: &[Component] = if is_last { last } else { &[] };
            let message_embeds: &[Embed] = if is_last { embeds } else { &[] };
            if i < shown_before {
                let (id, shown) = d.messages[i].clone();
                // Edit if the text changed, if it's the last message (its buttons
                // may change), or if it was the last and so still shows Stop.
                let was_last = i + 1 == shown_before;
                if shown != *text || is_last || was_last {
                    self.edit(id, text, components, Some(message_embeds)).await?;
                    d.messages[i].1 = text.clone();
                }
            } else {
                let id = self
                    .create(text, components, message_embeds, d.messages.is_empty())
                    .await?;
                if let Some(t) = turn {
                    d.cancels
                        .push(self.bot.cancels.insert(Self::key(self.channel, id), t.cancel.clone()));
                }
                d.messages.push((id, text.clone()));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Surface for DiscordSurface {
    type Draft = DiscordDraft;

    async fn begin(&self, _turn: &Turn) -> docsgpt_bot::Result<DiscordDraft> {
        Ok(DiscordDraft {
            messages: Vec::new(),
            typing: self.spawn_typing(),
            cancels: Vec::new(),
        })
    }

    async fn update(&self, turn: &Turn, d: &mut DiscordDraft, p: Progress<'_>) -> docsgpt_bot::Result<()> {
        if !self.bot.cfg.streaming || (p.answer.is_empty() && p.status.is_none()) {
            return Ok(());
        }
        let mut texts = render::pieces(&render::to_discord(p.answer));
        if let Some(s) = p.status {
            let line = render::status_line(s);
            match texts.last_mut() {
                Some(last) if last.chars().count() + line.chars().count() < 1990 => {
                    last.push('\n');
                    last.push_str(&line);
                }
                _ => texts.push(line),
            }
        }
        d.typing.cancel();
        self.show(d, &texts, &render::stop_row(), &[], Some(turn)).await
    }

    async fn finish(&self, _turn: &Turn, mut d: DiscordDraft, f: &Final) -> docsgpt_bot::Result<Option<String>> {
        d.typing.cancel();
        let body = if f.answer.is_empty() {
            f.display_text()
        } else {
            let mut t = render::to_discord(&f.answer);
            if let Some(n) = f.note() {
                t.push_str("\n\n");
                t.push_str(&n);
            }
            t
        };
        let mut texts = render::pieces(&body);
        if texts.is_empty() {
            texts.push(f.display_text());
        }
        if let Some(line) = render::sources_line(&f.sources) {
            match texts.last_mut() {
                Some(last) if last.chars().count() + line.chars().count() < 1990 => {
                    last.push('\n');
                    last.push_str(&line);
                }
                _ => texts.push(line),
            }
        }
        let embeds: Vec<Embed> = f
            .images
            .iter()
            .take(10)
            .filter_map(|u| ImageSource::url(u).ok())
            .map(|img| EmbedBuilder::new().image(img).build())
            .collect();
        let buttons = if f.can_rate() {
            render::feedback_row(None)
        } else {
            Vec::new()
        };
        self.show(&mut d, &texts, &buttons, &embeds, None).await?;
        // A shorter final text than the draft (rare): remove the leftovers.
        for (id, _) in d.messages.drain(texts.len()..) {
            let _ = self.bot.http.delete_message(self.channel, id).await;
        }
        let last = d.messages.last().map(|(id, _)| *id);
        Ok(last.filter(|_| f.can_rate()).map(|id| Self::key(self.channel, id)))
    }

    async fn send_file(&self, _turn: &Turn, file: Download) -> docsgpt_bot::Result<()> {
        let name = safe_filename(&file.filename, "file");
        let attachments = [Attachment::from_bytes(name, file.bytes.to_vec(), 1)];
        self.bot
            .http
            .create_message(self.channel)
            .attachments(&attachments)
            .await
            .map_err(platform)?;
        Ok(())
    }

    async fn notice(&self, text: &str) -> docsgpt_bot::Result<()> {
        self.create(text, &[], &[], true).await.map(drop)
    }

    fn update_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(1200)
    }
}
