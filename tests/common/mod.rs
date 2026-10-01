//! Test harness: an in-process mock of the Discord REST API, the `docsgpt`
//! mock server, gateway-event fixtures, and helpers that build real bots.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get};
use bytes::Bytes;
#[allow(unused_imports)]
pub use docsgpt::mock::{Call, MockDocsGpt, Recorder, Step, StreamReply, answer_steps, ev, reply_text, sse};
use docsgpt_bot::Shutdown;
use docsgpt_bot::storage::memory::MemoryStorage;
use docsgpt_discord::bot::DiscordBot;
use docsgpt_discord::config::{BotConfig, Config};
use serde_json::{Value, json};
use twilight_gateway::{Event, EventTypeFlags};

pub const WAIT: Duration = Duration::from_secs(10);
pub const BOT_USER: u64 = 900;
pub const APP_ID: u64 = 901;
pub const USER: u64 = 100;
pub const DM: u64 = 200;
pub const GUILD: u64 = 300;
pub const CHANNEL: u64 = 400;

/// The mock Discord REST API (`/api/v10/...`).
pub struct MockDiscord {
    /// `host:port`, for `ClientBuilder::proxy`.
    pub host: String,
    pub rec: Recorder,
    next_id: AtomicU64,
    faults: Mutex<HashMap<String, VecDeque<u16>>>,
    files: Mutex<HashMap<String, Bytes>>,
    /// Interaction token → the message posted as its response.
    originals: Mutex<HashMap<String, Value>>,
}

impl MockDiscord {
    pub async fn start() -> Arc<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let m = Arc::new(Self {
            host,
            rec: Recorder::default(),
            next_id: AtomicU64::new(10_000),
            faults: Mutex::new(HashMap::new()),
            files: Mutex::new(HashMap::new()),
            originals: Mutex::new(HashMap::new()),
        });
        let app = Router::new()
            .route("/api/v10/{*path}", any(api))
            .route("/cdn/{name}", get(cdn))
            .with_state(m.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        m
    }

    /// The next `times` calls named `name` (e.g. `create_thread`) fail with `status`.
    pub fn fail(&self, name: &str, status: u16, times: usize) {
        self.faults
            .lock()
            .unwrap()
            .entry(name.into())
            .or_default()
            .extend(std::iter::repeat_n(status, times));
    }

    pub fn add_file(&self, name: &str, bytes: &[u8]) -> String {
        self.files
            .lock()
            .unwrap()
            .insert(name.into(), Bytes::copy_from_slice(bytes));
        format!("http://{}/cdn/{name}", self.host)
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Message bodies sent to a channel (created and edited), in order.
    pub fn contents(&self, channel: u64) -> Vec<String> {
        self.rec
            .all()
            .into_iter()
            .filter(|c| {
                matches!(c.method.as_str(), "create_message" | "update_message")
                    && c.query["channel"] == channel.to_string()
            })
            .filter_map(|c| c.body["content"].as_str().map(str::to_string))
            .collect()
    }

    /// The text a message shows now (its last create or edit).
    pub fn message_text(&self, message: &str) -> String {
        self.rec
            .all()
            .into_iter()
            .filter(|c| c.query.get("message").map(String::as_str) == Some(message) && c.body["content"].is_string())
            .last()
            .map(|c| c.body["content"].as_str().unwrap().to_string())
            .unwrap_or_default()
    }

    /// Ids of messages the bot created in `channel`, in order.
    pub fn created(&self, channel: u64) -> Vec<String> {
        self.rec
            .calls("create_message")
            .into_iter()
            .filter(|c| c.query["channel"] == channel.to_string())
            .map(|c| c.query["message"].clone())
            .collect()
    }

    /// custom_ids of the buttons a message shows now.
    pub fn buttons(&self, message: &str) -> Vec<String> {
        let last = self
            .rec
            .all()
            .into_iter()
            .filter(|c| {
                c.query.get("message").map(String::as_str) == Some(message) && c.body.get("components").is_some()
            })
            .last();
        last.map(|c| button_ids(&c.body["components"])).unwrap_or_default()
    }
}

pub fn button_ids(components: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for row in components.as_array().into_iter().flatten() {
        for b in row["components"].as_array().into_iter().flatten() {
            if let Some(id) = b["custom_id"].as_str() {
                out.push(id.to_string());
            }
        }
    }
    out
}

pub fn user(id: u64, name: &str, bot: bool) -> Value {
    json!({"id": id.to_string(), "username": name, "discriminator": "0", "avatar": null, "bot": bot, "public_flags": 0, "global_name": name})
}

pub fn message_json(id: u64, channel: u64, content: &str, author: Value) -> Value {
    json!({
        "id": id.to_string(), "channel_id": channel.to_string(), "author": author, "content": content,
        "timestamp": "2026-10-02T00:00:00.000000+00:00", "edited_timestamp": null, "tts": false,
        "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [], "embeds": [],
        "pinned": false, "type": 0, "components": [], "flags": 0
    })
}

async fn api(State(m): State<Arc<MockDiscord>>, Path(path): Path<String>, req: Request) -> Response {
    let method = req.method().clone();
    let (body, files) = docsgpt::mock::parse_body(req).await;
    // Multipart messages carry their JSON in payload_json.
    let body = match body.get("payload_json") {
        Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(Value::Null),
        Some(v @ Value::Object(_)) => v.clone(),
        _ => body,
    };
    let seg: Vec<&str> = path.split('/').collect();
    let (name, params): (&str, Vec<(&str, String)>) = match (method.clone(), seg.as_slice()) {
        (Method::GET, ["users", "@me"]) => ("current_user", vec![]),
        (Method::GET, ["applications", "@me"]) => ("current_app", vec![]),
        (Method::PUT, ["applications", _, "commands"]) => ("set_commands", vec![]),
        (Method::POST, ["channels", c, "messages"]) => ("create_message", vec![("channel", c.to_string())]),
        (Method::PATCH, ["channels", c, "messages", msg]) => (
            "update_message",
            vec![("channel", c.to_string()), ("message", msg.to_string())],
        ),
        (Method::DELETE, ["channels", c, "messages", msg]) => (
            "delete_message",
            vec![("channel", c.to_string()), ("message", msg.to_string())],
        ),
        (Method::POST, ["channels", c, "messages", msg, "threads"]) => (
            "create_thread",
            vec![("channel", c.to_string()), ("message", msg.to_string())],
        ),
        (Method::POST, ["channels", c, "typing"]) => ("typing", vec![("channel", c.to_string())]),
        (Method::POST, ["interactions", _, token, "callback"]) => {
            ("interaction_callback", vec![("token", token.to_string())])
        }
        (Method::GET, ["webhooks", _, token, "messages", "@original"]) => {
            ("get_original", vec![("token", token.to_string())])
        }
        _ => ("unknown", vec![("path", path.clone())]),
    };
    let mut call = Call::new(name, body.clone());
    call.files = files;
    for (k, v) in &params {
        call.query.insert(k.to_string(), v.clone());
    }
    let param = |k: &str| {
        params
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let fault = m.faults.lock().unwrap().get_mut(name).and_then(VecDeque::pop_front);
    if let Some(status) = fault {
        m.rec.record(call);
        return (
            StatusCode::from_u16(status).unwrap(),
            axum::Json(json!({"message": "Missing Permissions", "code": 50013})),
        )
            .into_response();
    }
    let reply = match name {
        "current_user" => {
            json!({"id": BOT_USER.to_string(), "username": "docsgpt", "discriminator": "0", "avatar": null, "bot": true, "mfa_enabled": false})
        }
        "current_app" => {
            json!({"id": APP_ID.to_string(), "name": "DocsGPT", "description": "", "bot_public": true, "bot_require_code_grant": false, "verify_key": "k", "icon": null})
        }
        "set_commands" => json!([]),
        "create_message" => {
            let id = m.id();
            call.query.insert("message".into(), id.to_string());
            let mut msg = message_json(
                id,
                param("channel").parse().unwrap(),
                body["content"].as_str().unwrap_or(""),
                user(BOT_USER, "docsgpt", true),
            );
            msg["components"] = body.get("components").cloned().unwrap_or(json!([]));
            msg
        }
        "update_message" => message_json(
            param("message").parse().unwrap(),
            param("channel").parse().unwrap(),
            body["content"].as_str().unwrap_or(""),
            user(BOT_USER, "docsgpt", true),
        ),
        "create_thread" => {
            let id = m.id();
            call.query.insert("thread".into(), id.to_string());
            json!({"id": id.to_string(), "type": 11, "parent_id": param("channel"), "name": body["name"], "owner_id": BOT_USER.to_string(), "guild_id": GUILD.to_string()})
        }
        "interaction_callback" => {
            if body["type"] == 4 && body["data"]["flags"].as_u64().unwrap_or(0) & 64 == 0 {
                // A visible reply: becomes the interaction's original message.
                let channel = m
                    .originals
                    .lock()
                    .unwrap()
                    .get(&format!("channel:{}", param("token")))
                    .cloned()
                    .unwrap_or(json!(CHANNEL));
                let id = m.id();
                let msg = message_json(
                    id,
                    channel.as_u64().unwrap(),
                    body["data"]["content"].as_str().unwrap_or(""),
                    user(BOT_USER, "docsgpt", true),
                );
                m.originals.lock().unwrap().insert(param("token"), msg);
            }
            m.rec.record(call);
            return StatusCode::NO_CONTENT.into_response();
        }
        "get_original" => m
            .originals
            .lock()
            .unwrap()
            .get(&param("token"))
            .cloned()
            .unwrap_or(Value::Null),
        "typing" | "delete_message" => {
            m.rec.record(call);
            return StatusCode::NO_CONTENT.into_response();
        }
        _ => json!({}),
    };
    m.rec.record(call);
    axum::Json(reply).into_response()
}

async fn cdn(State(m): State<Arc<MockDiscord>>, Path(name): Path<String>) -> Response {
    m.rec.record(Call::new("cdn", json!({"name": name})));
    match m.files.lock().unwrap().get(&name).cloned() {
        Some(b) => b.into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Bots and events
// ---------------------------------------------------------------------------

pub struct TestBot {
    pub bot: Arc<DiscordBot>,
    pub discord: Arc<MockDiscord>,
    pub docs: Arc<MockDocsGpt>,
}

static SEQ: AtomicU64 = AtomicU64::new(1);

pub fn next_id() -> u64 {
    50_000 + SEQ.fetch_add(1, Ordering::SeqCst)
}

pub fn agents(list: &[(&str, &str)]) -> Vec<docsgpt_bot::AgentConfig> {
    list.iter()
        .map(|(n, k)| docsgpt_bot::AgentConfig::new(*n, *k))
        .collect()
}

pub async fn start(tweak: impl FnOnce(&mut BotConfig)) -> TestBot {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let discord = MockDiscord::start().await;
    let docs = MockDocsGpt::start().await;
    let mut cfg: BotConfig = toml::from_str("name = \"test\"\ntoken = \"t0ken\"").unwrap();
    cfg.discord_api_host = Some(discord.host.clone());
    cfg.agents = agents(&[("default", "key-default")]);
    tweak(&mut cfg);
    let global = Config {
        api_base: docs.url.clone(),
        storage: Default::default(),
        bots: vec![],
    };
    let web = reqwest::Client::builder().no_proxy().build().unwrap();
    let bot = DiscordBot::init(cfg, &global, Arc::new(MemoryStorage::default()), web, Shutdown::new())
        .await
        .unwrap();
    TestBot { bot, discord, docs }
}

/// A gateway dispatch payload, parsed the way the shard parses it.
pub fn event(kind: &str, d: Value) -> Event {
    let raw = json!({"op": 0, "t": kind, "s": 1, "d": d}).to_string();
    let parsed = twilight_gateway::parse(raw, EventTypeFlags::all())
        .expect("valid gateway payload")
        .expect("event");
    Event::from(parsed)
}

pub fn dm(content: &str) -> Value {
    message_json(next_id(), DM, content, user(USER, "alice", false))
}

pub fn in_channel(channel: u64, content: &str, mention_bot: bool) -> Value {
    let mut m = message_json(next_id(), channel, content, user(USER, "alice", false));
    m["guild_id"] = json!(GUILD.to_string());
    if mention_bot {
        m["mentions"] = json!([user(BOT_USER, "docsgpt", true)]);
    }
    m
}

pub fn interaction(kind: u8, data: Value, channel: u64, guild: bool, message: Option<Value>) -> Value {
    let token = format!("tok{}", next_id());
    let mut i = json!({
        "id": next_id().to_string(), "application_id": APP_ID.to_string(), "type": kind, "token": token,
        "version": 1, "data": data, "channel": {"id": channel.to_string(), "type": if guild { 0 } else { 1 }},
        "entitlements": [], "authorizing_integration_owners": {}, "locale": "en-US",
    });
    if guild {
        i["guild_id"] = json!(GUILD.to_string());
        i["member"] = json!({"user": user(USER, "alice", false), "roles": [], "joined_at": "2026-01-01T00:00:00.000000+00:00", "deaf": false, "mute": false, "flags": 0});
    } else {
        i["user"] = user(USER, "alice", false);
    }
    if let Some(m) = message {
        i["message"] = m;
    }
    i
}

pub fn slash(name: &str, options: &[(&str, &str)], channel: u64, guild: bool) -> Value {
    let opts: Vec<Value> = options
        .iter()
        .map(|(n, v)| json!({"name": n, "type": 3, "value": v}))
        .collect();
    interaction(
        2,
        json!({"id": "1", "name": name, "type": 1, "options": opts}),
        channel,
        guild,
        None,
    )
}

/// A click on button `custom_id` of message `message_id` (showing `components`).
pub fn click(custom_id: &str, channel: u64, message_id: &str, components: Value) -> Value {
    let mut msg = message_json(
        message_id.parse().unwrap(),
        channel,
        "",
        user(BOT_USER, "docsgpt", true),
    );
    msg["components"] = components;
    interaction(
        3,
        json!({"custom_id": custom_id, "component_type": 2}),
        channel,
        channel != DM,
        Some(msg),
    )
}

impl TestBot {
    pub async fn send(&self, kind: &str, d: Value) {
        docsgpt_discord::events::dispatch(self.bot.clone(), event(kind, d)).await;
    }

    /// The interaction's original-message channel, for /ask.
    pub fn set_original_channel(&self, token: &str, channel: u64) {
        self.discord
            .originals
            .lock()
            .unwrap()
            .insert(format!("channel:{token}"), json!(channel));
    }
}
