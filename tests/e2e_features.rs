//! Files in and out.

mod common;

use common::*;
use serde_json::json;

fn with_attachment(mut msg: serde_json::Value, url: &str, name: &str, mime: &str, voice: bool) -> serde_json::Value {
    let mut a = json!({"id": next_id().to_string(), "filename": name, "size": 12, "url": url, "proxy_url": url, "content_type": mime});
    if voice {
        a["duration_secs"] = json!(3.5);
        a["waveform"] = json!("AAAA");
        msg["flags"] = json!(1 << 13);
    }
    msg["attachments"] = json!([a]);
    msg
}

#[tokio::test]
async fn attachments_go_to_the_agent_that_answers() {
    let t = start(|c| c.agents = agents(&[("support", "k-support"), ("sales", "k-sales")])).await;
    let url = t.discord.add_file("notes.md", b"# Q3 pricing");
    t.send(
        "MESSAGE_CREATE",
        with_attachment(dm("#sales summarize this"), &url, "notes.md", "text/markdown", false),
    )
    .await;
    let up = t.docs.rec.last("/api/store_attachment").expect("uploaded to DocsGPT");
    assert_eq!(up.body["api_key"], "k-sales");
    assert_eq!(&up.file("file").bytes[..], b"# Q3 pricing");
    let s = t.docs.rec.last("/stream").unwrap().body;
    assert_eq!(
        (s["attachments"].clone(), s["api_key"].as_str(), s["question"].as_str()),
        (json!(["att-1"]), Some("k-sales"), Some("summarize this"))
    );
}

#[tokio::test]
async fn a_voice_message_is_transcribed() {
    let t = start(|_| {}).await;
    t.docs.set_stt("how do I reset my password");
    let url = t.discord.add_file("voice-message.ogg", b"OggS");
    t.send(
        "MESSAGE_CREATE",
        with_attachment(dm(""), &url, "voice-message.ogg", "audio/ogg", true),
    )
    .await;
    assert_eq!(t.docs.rec.count("/api/stt"), 1);
    assert_eq!(t.docs.rec.count("/api/store_attachment"), 0);
    assert_eq!(
        t.docs.rec.last("/stream").unwrap().body["question"],
        "how do I reset my password"
    );
}

#[tokio::test]
async fn file_problems_are_reported() {
    let t = start(|c| c.max_file_mb = 1).await;
    let mut msg = dm("look at these");
    msg["attachments"] = json!([
        {"id": "1", "filename": "big.pdf", "size": 5 * 1024 * 1024, "url": "http://x/big", "proxy_url": "http://x/big"},
        {"id": "2", "filename": "gone.txt", "size": 3, "url": format!("http://{}/cdn/missing", t.discord.host), "proxy_url": "x"},
    ]);
    t.send("MESSAGE_CREATE", msg).await;
    let texts = t.discord.contents(DM);
    assert!(texts.contains(&"big.pdf is larger than 1 MB.".to_string()), "{texts:?}");
    assert!(
        texts.contains(&"I couldn't download gone.txt.".to_string()),
        "{texts:?}"
    );
    assert_eq!(t.docs.rec.count("/stream"), 1, "the question is still asked");
}

#[tokio::test]
async fn tool_files_are_uploaded() {
    let t = start(|_| {}).await;
    t.docs.add_artifact("a1", "report.csv", "text/csv", &b"a,b\n1,2\n"[..]);
    t.docs.on_stream(|_| {
        sse(vec![
            ev::step(ev::message_id("m", "conv-f")),
            ev::step(ev::tool_call(
                json!({"tool_name": "code_executor", "call_id": "1", "status": "completed", "artifact_id": "a1"}),
            )),
            ev::step(ev::answer("Here is the report.")),
            ev::step(ev::end()),
        ])
    });
    t.send("MESSAGE_CREATE", dm("make a report")).await;
    let upload = t
        .discord
        .rec
        .calls("create_message")
        .into_iter()
        .find(|c| !c.files.is_empty())
        .expect("file message");
    let (_, file) = upload.files.iter().next().unwrap();
    assert_eq!(
        (file.filename.as_str(), &file.bytes[..]),
        ("report.csv", &b"a,b\n1,2\n"[..])
    );
}
