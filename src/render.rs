//! Discord text in, Discord messages out.

use std::sync::LazyLock;

use docsgpt_bot::docsgpt::{Feedback, Source};
use docsgpt_bot::markdown::{clean_title, render_table_monospace, split_markdown};
use regex::Regex;
use twilight_model::channel::message::EmojiReactionType;
use twilight_model::channel::message::component::{ActionRow, Button, ButtonStyle, Component};

/// Discord's message limit is 2000 characters; leave room for a status line.
pub const PIECE: usize = 1900;
pub const STOP_ID: &str = "dg:stop";
pub const LIKE_ID: &str = "dg:fb:up";
pub const DISLIKE_ID: &str = "dg:fb:down";

static MENTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<@!?(\d+)>").expect("valid regex"));

/// The question in a message: mentions of the bot removed, whitespace tidied.
pub fn clean_incoming(text: &str, bot_id: u64) -> String {
    let id = bot_id.to_string();
    let out = MENTION.replace_all(
        text,
        |c: &regex::Captures| if c[1] == id { String::new() } else { c[0].to_string() },
    );
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// True when someone other than the bot is mentioned.
pub fn mentions_someone_else(text: &str, bot_id: u64) -> bool {
    let id = bot_id.to_string();
    MENTION.captures_iter(text).any(|c| c[1] != id)
}

/// Markdown → Discord markdown. Discord has no tables: they become aligned
/// monospace blocks. Everything else (headings, lists, code, links) carries over.
pub fn to_discord(md: &str) -> String {
    let mut out = Vec::new();
    let lines: Vec<&str> = md.lines().collect();
    let mut in_fence = false;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        let is_row = |l: &str| l.trim_start().starts_with('|') && l.trim_end().ends_with('|');
        let is_sep = |l: &str| is_row(l) && l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '));
        if !in_fence && is_row(line) && lines.get(i + 1).is_some_and(|l| is_sep(l)) {
            let mut rows = vec![cells(line)];
            i += 2;
            while i < lines.len() && is_row(lines[i]) {
                rows.push(cells(lines[i]));
                i += 1;
            }
            out.push(format!("```\n{}\n```", render_table_monospace(&rows)));
            continue;
        }
        out.push(line.to_string());
        i += 1;
    }
    out.join("\n")
}

fn cells(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|')
        .map(|c| c.trim().replace("**", "").replace('`', ""))
        .collect()
}

/// Split into message-sized pieces without breaking code blocks.
pub fn pieces(text: &str) -> Vec<String> {
    split_markdown(text, PIECE)
}

/// A small grey line under the text.
pub fn subtext(s: &str) -> String {
    format!("-# {s}")
}

pub fn status_line(status: &str) -> String {
    subtext(&format!("⚙️ {status}…"))
}

/// Sources as one subtext line of links (angle brackets stop link previews).
pub fn sources_line(sources: &[Source]) -> Option<String> {
    let mut seen = Vec::new();
    let items: Vec<String> = sources
        .iter()
        .filter(|s| {
            let key = (s.title.clone(), s.url.clone());
            !seen.contains(&key) && {
                seen.push(key);
                true
            }
        })
        .take(10)
        .map(|s| {
            let title = clean_title(&s.title);
            match &s.url {
                Some(u) => format!("[{title}](<{u}>)"),
                None => title,
            }
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    Some(subtext(&docsgpt_bot::util::truncate_chars(
        &format!("Sources: {}", items.join(" · ")),
        1000,
    )))
}

fn button(id: &str, label: Option<&str>, emoji: Option<&str>, style: ButtonStyle) -> Component {
    Component::Button(Button {
        id: None,
        custom_id: Some(id.into()),
        disabled: false,
        emoji: emoji.map(|e| EmojiReactionType::Unicode { name: e.into() }),
        label: label.map(str::to_string),
        style,
        url: None,
        sku_id: None,
    })
}

fn row(components: Vec<Component>) -> Vec<Component> {
    vec![Component::ActionRow(ActionRow { id: None, components })]
}

/// The Stop button shown while an answer streams.
pub fn stop_row() -> Vec<Component> {
    row(vec![button(STOP_ID, Some("Stop"), Some("⏹️"), ButtonStyle::Secondary)])
}

/// 👍/👎 buttons; the chosen one is coloured.
pub fn feedback_row(selected: Option<Feedback>) -> Vec<Component> {
    let style = |want: Feedback, on: ButtonStyle| {
        if selected == Some(want) {
            on
        } else {
            ButtonStyle::Secondary
        }
    };
    row(vec![
        button(LIKE_ID, None, Some("👍"), style(Feedback::Like, ButtonStyle::Success)),
        button(
            DISLIKE_ID,
            None,
            Some("👎"),
            style(Feedback::Dislike, ButtonStyle::Danger),
        ),
    ])
}

/// Which feedback a message's buttons currently show.
pub fn selected_feedback(components: &[Component]) -> Option<Feedback> {
    let mut found = None;
    let mut visit = |c: &Component| {
        if let Component::Button(b) = c {
            match (b.custom_id.as_deref(), b.style) {
                (Some(LIKE_ID), ButtonStyle::Success) => found = Some(Feedback::Like),
                (Some(DISLIKE_ID), ButtonStyle::Danger) => found = Some(Feedback::Dislike),
                _ => {}
            }
        }
    };
    for c in components {
        match c {
            Component::ActionRow(r) => r.components.iter().for_each(&mut visit),
            other => visit(other),
        }
    }
    found
}

/// A thread name from the question: one line, at most 100 characters.
pub fn thread_name(question: &str) -> String {
    let line = question.trim().lines().next().unwrap_or("").trim();
    let name = if line.is_empty() { "Question" } else { line };
    docsgpt_bot::util::truncate_chars(name, 90)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_mentions() {
        assert_eq!(
            clean_incoming("<@42> what is  <@!42> this? <@7>", 42),
            "what is this? <@7>"
        );
        assert!(mentions_someone_else("hi <@7>", 42));
        assert!(!mentions_someone_else("hi <@42>", 42));
    }

    #[test]
    fn tables_become_monospace_and_code_is_untouched() {
        let md = "Intro\n\n| a | b |\n|---|---|\n| 1 | **2** |\n\n```\n| not | a table |\n|---|---|\n```";
        let out = to_discord(md);
        assert!(out.contains("```\na | b\n--|--\n1 | 2\n```"), "{out}");
        assert!(out.contains("```\n| not | a table |\n|---|---|\n```"), "{out}");
    }

    #[test]
    fn sources_and_buttons() {
        let s = vec![
            Source {
                title: "Guide [v2]".into(),
                url: Some("https://d/x".into()),
            },
            Source {
                title: "Guide [v2]".into(),
                url: Some("https://d/x".into()),
            },
            Source {
                title: "Notes".into(),
                url: None,
            },
        ];
        assert_eq!(
            sources_line(&s).unwrap(),
            "-# Sources: [Guide v2](<https://d/x>) · Notes"
        );
        assert!(sources_line(&[]).is_none());
        assert_eq!(
            selected_feedback(&feedback_row(Some(Feedback::Dislike))),
            Some(Feedback::Dislike)
        );
        assert_eq!(selected_feedback(&feedback_row(None)), None);
        assert_eq!(thread_name("  \nHow do I deploy?\nmore"), "How do I deploy?");
        assert_eq!(thread_name("   "), "Question");
        assert_eq!(thread_name("How do I deploy?\nmore"), "How do I deploy?");
    }
}
