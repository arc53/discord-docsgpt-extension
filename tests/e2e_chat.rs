//! Answering in DMs and servers, end to end against mock Discord and mock DocsGPT.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use serde_json::json;

fn stream_body(text: &str, conv: &str) -> docsgpt::mock::StreamReply {
    sse(answer_steps(text, conv, &[]))
}

#[tokio::test]
async fn dm_streams_then_finishes_with_sources_and_feedback() {
    let t = start(|_| {}).await;
    t.docs.on_stream(|_| {
        sse(answer_steps(
            "Hello from DocsGPT.",
            "conv-1",
            &[("Guide", "https://g.example")],
        ))
    });
    t.send("MESSAGE_CREATE", dm("hi there")).await;

    let rec = &t.discord.rec;
    assert!(rec.count("typing") >= 1);
    let first = rec.calls("create_message").into_iter().next().expect("a message");
    assert_eq!(
        button_ids(&first.body["components"]),
        ["dg:stop"],
        "Stop while streaming"
    );
    assert_eq!(
        first.body["allowed_mentions"],
        json!({"parse": []}),
        "answers never ping"
    );
    let id = first.query["message"].clone();
    assert_eq!(
        t.discord.message_text(&id),
        "Hello from DocsGPT.\n-# Sources: [Guide](<https://g.example>)"
    );
    assert_eq!(t.discord.buttons(&id), ["dg:fb:up", "dg:fb:down"]);
    assert_eq!(t.docs.rec.last("/stream").unwrap().body["question"], "hi there");

    // 👍 records feedback and colours the button; a second 👍 takes it back.
    t.send("INTERACTION_CREATE", click("dg:fb:up", DM, &id, json!([])))
        .await;
    let fb = t.docs.rec.last("/api/feedback").unwrap().body;
    assert_eq!(
        fb,
        json!({"feedback": "like", "conversation_id": "conv-1", "question_index": 0, "api_key": "key-default"})
    );
    let cb = t.discord.rec.last("interaction_callback").unwrap().body;
    assert_eq!(cb["type"], 7);
    assert_eq!(
        cb["data"]["components"][0]["components"][0]["style"], 3,
        "👍 shown as selected"
    );

    let selected = cb["data"]["components"].clone();
    t.send("INTERACTION_CREATE", click("dg:fb:up", DM, &id, selected)).await;
    assert!(t.docs.rec.last("/api/feedback").unwrap().body["feedback"].is_null());

    // On a message holding no known answer.
    t.send("INTERACTION_CREATE", click("dg:fb:down", DM, "123", json!([])))
        .await;
    let cb = t.discord.rec.last("interaction_callback").unwrap().body;
    assert_eq!((cb["type"].as_u64(), cb["data"]["flags"].as_u64()), (Some(4), Some(64)));
}

#[tokio::test]
async fn dm_conversation_continues_until_new() {
    let t = start(|_| {}).await;
    t.docs
        .on_stream(|b| stream_body("ok", b["conversation_id"].as_str().unwrap_or("conv-1")));
    t.send("MESSAGE_CREATE", dm("one")).await;
    t.send("MESSAGE_CREATE", dm("two")).await;
    assert_eq!(t.docs.rec.last("/stream").unwrap().body["conversation_id"], "conv-1");

    t.send("INTERACTION_CREATE", slash("new", &[], DM, false)).await;
    let cb = t.discord.rec.last("interaction_callback").unwrap().body;
    assert_eq!(cb["data"]["content"], "Started a new conversation here.");
    t.send("MESSAGE_CREATE", dm("three")).await;
    assert!(
        t.docs
            .rec
            .last("/stream")
            .unwrap()
            .body
            .get("conversation_id")
            .is_none()
    );
}

#[tokio::test]
async fn a_mention_opens_a_thread_that_the_bot_follows() {
    let t = start(|_| {}).await;
    t.docs
        .on_stream(|b| stream_body("Deploy with docker.", b["conversation_id"].as_str().unwrap_or("conv-t")));
    let q = in_channel(CHANNEL, "<@900> how do I deploy?", true);
    let qid = q["id"].as_str().unwrap().to_string();
    t.send("MESSAGE_CREATE", q).await;

    let th = t.discord.rec.last("create_thread").expect("thread opened");
    assert_eq!(
        (th.query["channel"].as_str(), th.query["message"].as_str()),
        (CHANNEL.to_string().as_str(), qid.as_str())
    );
    assert_eq!(th.body["name"], "how do I deploy?");
    let thread: u64 = th.query["thread"].parse().unwrap();
    assert!(!t.discord.created(thread).is_empty(), "answered in the thread");
    assert!(t.discord.created(CHANNEL).is_empty(), "nothing in the channel itself");
    assert_eq!(t.docs.rec.last("/stream").unwrap().body["question"], "how do I deploy?");

    // A follow-up in the thread, without a mention, continues the conversation.
    t.send("MESSAGE_CREATE", in_channel(thread, "and on kubernetes?", false))
        .await;
    let s = t.docs.rec.calls("/stream");
    assert_eq!(s.len(), 2);
    assert_eq!(s[1].body["conversation_id"], "conv-t");
    // Not: '!' messages, messages for someone else, messages in other channels.
    t.send("MESSAGE_CREATE", in_channel(thread, "!note to self", false))
        .await;
    t.send("MESSAGE_CREATE", in_channel(thread, "<@555> what do you think?", false))
        .await;
    t.send("MESSAGE_CREATE", in_channel(CHANNEL, "unrelated chat", false))
        .await;
    assert_eq!(t.docs.rec.count("/stream"), 2);
}

#[tokio::test]
async fn without_follow_threads_follow_ups_need_a_mention() {
    let t = start(|c| c.follow_threads = false).await;
    t.send("MESSAGE_CREATE", in_channel(CHANNEL, "<@900> q", true)).await;
    let thread: u64 = t.discord.rec.last("create_thread").unwrap().query["thread"]
        .parse()
        .unwrap();
    t.send("MESSAGE_CREATE", in_channel(thread, "follow up", false)).await;
    assert_eq!(t.docs.rec.count("/stream"), 1);
    t.send("MESSAGE_CREATE", in_channel(thread, "<@900> follow up", true))
        .await;
    assert_eq!(t.docs.rec.count("/stream"), 2);
    assert_eq!(t.discord.rec.count("create_thread"), 1, "no thread inside a thread");
}

#[tokio::test]
async fn answers_inline_when_a_thread_cant_be_opened() {
    let t = start(|_| {}).await;
    t.discord.fail("create_thread", 403, 1);
    let q = in_channel(CHANNEL, "<@900> q", true);
    let qid = q["id"].as_str().unwrap().to_string();
    t.send("MESSAGE_CREATE", q).await;
    let first = t
        .discord
        .rec
        .calls("create_message")
        .into_iter()
        .next()
        .expect("inline answer");
    assert_eq!(first.query["channel"], CHANNEL.to_string());
    assert_eq!(first.body["message_reference"]["message_id"], qid);
}

#[tokio::test]
async fn long_answers_continue_in_new_messages() {
    let t = start(|_| {}).await;
    let long = (1..=300)
        .map(|i| format!("Line {i}: some words here."))
        .collect::<Vec<_>>()
        .join("\n");
    let l2 = long.clone();
    t.docs.on_stream(move |_| {
        sse(vec![
            ev::step(ev::message_id("m", "c")),
            ev::step(ev::answer(&l2)),
            ev::step(ev::end()),
        ])
    });
    t.send("MESSAGE_CREATE", dm("q")).await;
    let ids = t.discord.created(DM);
    assert!(ids.len() >= 4, "{} messages", ids.len());
    let mut shown = String::new();
    for (i, id) in ids.iter().enumerate() {
        let text = t.discord.message_text(id);
        assert!(
            text.chars().count() <= 2000,
            "message {i} has {} chars",
            text.chars().count()
        );
        let buttons = t.discord.buttons(id);
        if i + 1 == ids.len() {
            assert_eq!(buttons, ["dg:fb:up", "dg:fb:down"]);
        } else {
            assert!(buttons.is_empty(), "message {i} still shows {buttons:?}");
        }
        shown.push_str(&text);
        shown.push('\n');
    }
    assert_eq!(
        shown.split_whitespace().collect::<Vec<_>>(),
        long.split_whitespace().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn stop_button_ends_the_answer() {
    let t = start(|_| {}).await;
    t.docs.on_stream(|_| {
        sse(vec![
            ev::step(ev::message_id("m", "c")),
            ev::step(ev::answer("Partial answer")),
            docsgpt::mock::Step::Sleep(Duration::from_secs(8)),
            ev::step(ev::answer(" never")),
            ev::step(ev::end()),
        ])
    });
    let bot = t.bot.clone();
    let turn =
        tokio::spawn(async move { docsgpt_discord::events::dispatch(bot, event("MESSAGE_CREATE", dm("q"))).await });
    let created = t.discord.rec.wait_any("create_message").await;
    let id = created.query["message"].clone();
    let started = Instant::now();
    t.send(
        "INTERACTION_CREATE",
        click("dg:stop", DM, &id, created.body["components"].clone()),
    )
    .await;
    assert_eq!(t.discord.rec.last("interaction_callback").unwrap().body["type"], 6);
    turn.await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
    assert_eq!(t.discord.message_text(&id), "Partial answer\n\n_Stopped._");
}

#[tokio::test]
async fn agents_by_tag_bare_tag_and_command() {
    let t = start(|c| c.agents = agents(&[("support", "k-support"), ("sales", "k-sales")])).await;
    t.send("MESSAGE_CREATE", dm("#sales what does it cost?")).await;
    let b = t.docs.rec.last("/stream").unwrap().body;
    assert_eq!(
        (b["api_key"].as_str(), b["question"].as_str()),
        (Some("k-sales"), Some("what does it cost?"))
    );

    t.send("MESSAGE_CREATE", dm("#nope hi")).await;
    assert!(
        t.discord
            .contents(DM)
            .iter()
            .any(|c| c == "Unknown agent #nope. Available: #support, #sales")
    );
    t.send("MESSAGE_CREATE", dm("#sales")).await;
    assert!(t.discord.contents(DM).iter().any(|c| c == "Now answering with #sales."));
    t.send("MESSAGE_CREATE", dm("and support?")).await;
    assert_eq!(t.docs.rec.last("/stream").unwrap().body["api_key"], "k-sales");

    t.send("INTERACTION_CREATE", slash("agent", &[("name", "support")], DM, false))
        .await;
    assert_eq!(
        t.discord.rec.last("interaction_callback").unwrap().body["data"]["content"],
        "Now answering with `#support` here."
    );
    t.send("MESSAGE_CREATE", dm("now?")).await;
    assert_eq!(t.docs.rec.last("/stream").unwrap().body["api_key"], "k-support");
    t.send("INTERACTION_CREATE", slash("agents", &[], DM, false)).await;
    let list = t.discord.rec.last("interaction_callback").unwrap().body["data"]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(list.contains("`#support` (current)"), "{list}");
}

#[tokio::test]
async fn errors_are_shown() {
    let t = start(|_| {}).await;
    t.docs.on_stream(|_| docsgpt::mock::StreamReply::Http(401, "{}".into()));
    t.send("MESSAGE_CREATE", dm("q")).await;
    let c = t.discord.rec.last("create_message").unwrap();
    assert_eq!(
        c.body["content"],
        "Sorry, I couldn't get an answer right now.\n\nThe agent's API key was rejected."
    );
    assert!(
        t.discord.buttons(&c.query["message"]).is_empty(),
        "no feedback buttons on an apology"
    );

    let t = start(|_| {}).await;
    t.docs.on_stream(|_| {
        sse(vec![
            ev::step(ev::message_id("m", "c")),
            ev::step(ev::answer("Half")),
            ev::step(ev::error("LLM overloaded")),
        ])
    });
    t.send("MESSAGE_CREATE", dm("q")).await;
    let id = t.discord.created(DM)[0].clone();
    assert_eq!(
        t.discord.message_text(&id),
        "Half\n\n_The answer was cut short: LLM overloaded_"
    );
}

#[tokio::test]
async fn ask_command_in_a_server_opens_a_thread() {
    let t = start(|_| {}).await;
    let i = slash("ask", &[("question", "what is DocsGPT?")], CHANNEL, true);
    t.set_original_channel(i["token"].as_str().unwrap(), CHANNEL);
    t.send("INTERACTION_CREATE", i).await;
    let cb = t.discord.rec.calls("interaction_callback")[0].body.clone();
    assert_eq!(
        (cb["type"].as_u64(), cb["data"]["content"].as_str()),
        (Some(4), Some("> what is DocsGPT?"))
    );
    let th = t.discord.rec.last("create_thread").expect("thread from the reply");
    let thread: u64 = th.query["thread"].parse().unwrap();
    assert!(!t.discord.created(thread).is_empty());
    assert_eq!(t.docs.rec.last("/stream").unwrap().body["question"], "what is DocsGPT?");
}

#[tokio::test]
async fn ignores_bots_and_other_servers() {
    let t = start(|c| c.allowed_guilds = vec![1]).await;
    t.send("MESSAGE_CREATE", in_channel(CHANNEL, "<@900> hi", true)).await;
    let mut from_bot = dm("hi");
    from_bot["author"] = user(77, "otherbot", true);
    t.send("MESSAGE_CREATE", from_bot).await;
    assert_eq!(t.docs.rec.count("/stream"), 0);
    t.send("MESSAGE_CREATE", dm("DMs always work")).await;
    assert_eq!(t.docs.rec.count("/stream"), 1);
    let _ = Arc::strong_count(&t.bot);
}

#[tokio::test]
async fn when_an_answer_outgrows_a_message_the_earlier_one_loses_stop() {
    let t = start(|_| {}).await;
    let a = "A".repeat(1500);
    let b = "B".repeat(1500);
    let (a2, b2) = (a.clone(), b.clone());
    t.docs.on_stream(move |_| {
        sse(vec![
            ev::step(ev::message_id("m", "c")),
            ev::step(ev::answer(&a2)),
            docsgpt::mock::Step::Sleep(Duration::from_millis(1500)),
            ev::step(ev::answer(&format!("\n\n{b2}"))),
            docsgpt::mock::Step::Sleep(Duration::from_millis(1500)),
            ev::step(ev::end()),
        ])
    });
    t.send("MESSAGE_CREATE", dm("q")).await;
    let ids = t.discord.created(DM);
    assert_eq!(ids.len(), 2);
    assert_eq!(t.discord.message_text(&ids[0]), a);
    assert!(
        t.discord.buttons(&ids[0]).is_empty(),
        "first message still shows {:?}",
        t.discord.buttons(&ids[0])
    );
    assert_eq!(t.discord.buttons(&ids[1]), ["dg:fb:up", "dg:fb:down"]);
}

#[tokio::test]
async fn mentioning_the_bots_role_counts_as_mentioning_the_bot() {
    // Discord's autocomplete often picks the bot's managed role, not the bot user.
    let t = start(|_| {}).await;
    let mut q = in_channel(CHANNEL, "<@&777> how do I deploy?", false);
    q["mention_roles"] = json!(["777"]);
    t.send("MESSAGE_CREATE", q).await;
    assert_eq!(t.docs.rec.count("/stream"), 1, "answered");
    assert_eq!(t.docs.rec.last("/stream").unwrap().body["question"], "how do I deploy?");
    // Another role is not the bot.
    let mut other = in_channel(CHANNEL, "<@&555> hello team", false);
    other["mention_roles"] = json!(["555"]);
    t.send("MESSAGE_CREATE", other).await;
    assert_eq!(t.docs.rec.count("/stream"), 1);
}
