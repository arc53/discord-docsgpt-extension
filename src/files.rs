//! Files users attach: download from Discord's CDN, then transcribe (voice
//! messages) or upload to DocsGPT.

use std::time::Duration;

use bytes::Bytes;
use docsgpt_bot::docsgpt::{Error as DocsError, Upload};
use docsgpt_bot::util::safe_filename;
use docsgpt_bot::{AgentConfig, Routed, Scope};
use futures_util::StreamExt;
use twilight_model::channel::Attachment;

use crate::bot::DiscordBot;

const MAX_FILES: usize = 5;

/// What the attachments of one message became.
#[derive(Debug, Default)]
pub struct Prepared {
    pub attachments: Vec<String>,
    /// A transcript to ask when the message had no text.
    pub transcript: Option<String>,
    pub problems: Vec<String>,
}

/// The agent a message will go to, so files are uploaded with its key.
pub async fn agent_for(bot: &DiscordBot, text: &str, state_scope: &Scope) -> AgentConfig {
    let active = bot
        .core
        .storage
        .chat_state(state_scope)
        .await
        .ok()
        .and_then(|s| s.active_agent);
    match bot.core.agents.route(text, active.as_deref()) {
        Routed::Agent { agent, .. } => agent.clone(),
        Routed::UnknownTag { .. } => bot.core.agents.default_agent().clone(),
    }
}

fn is_audio(a: &Attachment) -> bool {
    a.duration_secs.is_some() || a.content_type.as_deref().is_some_and(|m| m.starts_with("audio/"))
}

async fn download(bot: &DiscordBot, url: &str, max: usize) -> Result<Bytes, String> {
    let resp = bot
        .web
        .get(url)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let mut body = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        if buf.len() + chunk.len() > max {
            return Err("too large".into());
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(Bytes::from(buf))
}

/// Download and process a message's attachments.
pub async fn prepare(bot: &DiscordBot, agent: &AgentConfig, files: &[Attachment], question_empty: bool) -> Prepared {
    let mut out = Prepared::default();
    let max = (bot.cfg.max_file_mb as usize) * 1024 * 1024;
    for a in files.iter().take(MAX_FILES) {
        let name = safe_filename(&a.filename, "file");
        if a.size as usize > max {
            out.problems
                .push(format!("{name} is larger than {} MB.", bot.cfg.max_file_mb));
            continue;
        }
        let bytes = match download(bot, &a.url, max).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(error = %e, file = %name, "attachment download failed");
                out.problems.push(format!("I couldn't download {name}."));
                continue;
            }
        };
        let mut upload = Upload::new(name.clone(), bytes);
        if let Some(m) = &a.content_type {
            upload = upload.mime(m.split(';').next().unwrap_or(m).trim());
        }
        if question_empty && out.transcript.is_none() && is_audio(a) {
            match bot.core.client.stt(&agent.api_key, &upload).await {
                Ok(text) => {
                    out.transcript = Some(text);
                    continue;
                }
                Err(DocsError::FeatureDisabled { .. }) => {
                    tracing::debug!("speech-to-text disabled; sending the audio as a file")
                }
                Err(e) => tracing::warn!(error = %e, "speech-to-text failed; sending the audio as a file"),
            }
        }
        match bot.core.client.upload_attachment(&agent.api_key, &upload).await {
            Ok(att) => {
                if let Some(task) = &att.task_id
                    && let Err(e) = bot.core.client.wait_for_task(task, Duration::from_secs(90)).await
                {
                    tracing::warn!(error = %e, file = %name, "attachment processing failed");
                    out.problems.push(format!("DocsGPT couldn't read {name}."));
                    continue;
                }
                out.attachments.push(att.id);
            }
            Err(e) => {
                tracing::warn!(error = %e, file = %name, "attachment upload failed");
                out.problems.push(format!("I couldn't attach {name}."));
            }
        }
    }
    out
}
