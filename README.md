# Discord DocsGPT extension

Discord bots for your [DocsGPT](https://www.docsgpt.cloud/) agents. One small binary runs any number of bots, each connected to one or more agents.
- **Streaming:** answers are written into the message as they are generated, with a **Stop** button.
- **Threads:** in servers, each question gets its own thread.
- **Sources and feedback:** sources are linked under each answer, and 👍/👎 buttons record feedback in DocsGPT.
- **Files:** attachments go to the agent, and files that its tools produce come back.

Version 2 is a rewrite in Rust on [`docsgpt-rs`](https://github.com/arc53/docsgpt-rs), the crates shared with the [Slack](https://github.com/arc53/slack-bot-docsgpt-extenstion) and [Telegram](https://github.com/arc53/tg-bot-docsgpt-extenstion) bots. The Python bot lives on the [`legacy-python`](https://github.com/arc53/discord-docsgpt-extension/tree/legacy-python) branch and the `:1` image tag. See [Upgrading from version 1](#upgrading-from-version-1).

## Features

- **Streaming answers.** The answer is edited in as it's written, about once a second. Long answers continue in new messages, splitting between blocks so code blocks stay intact. Tool steps ("Running code") show as a small grey line, and **Stop** cuts the answer short.
- **A thread per question.** In a server, mention the bot and it opens a thread on your message and answers there. Keep talking in the thread without mentioning it again. If threads aren't allowed in that channel, it replies inline.
- **DMs.** Every message in a DM continues one conversation until `/new`.
- **Sources and feedback.** Sources are linked under the answer. The 👍/👎 buttons record feedback in DocsGPT for that exact answer; click again to take it back.
- **Files in.** Documents and images you attach are sent to the agent. Voice messages are transcribed and asked as questions.
- **Files out.** When the agent runs tools (code execution, document or image generation), the files are uploaded to the conversation.
- **Several agents.** `#sales question` asks one agent once, and a message that is just `#sales` switches the channel to it. `/agent` switches, `/agents` lists, and `/ask` asks.
- **Formatting.** Markdown renders natively, and tables become aligned code blocks. Answers never ping anyone.
- **Operations.** SQLite by default (or in memory), structured logs, graceful shutdown that lets answers finish, and a small distroless image for amd64 and arm64.

## Quick start

### 1. Create the Discord application

1. In the [Discord Developer Portal](https://discord.com/developers/applications), create an application and open **Bot**.
2. Copy the token (**Reset Token**).
3. Turn on **Message Content Intent** under *Privileged Gateway Intents*. The bot needs it to read follow-ups in its threads that don't mention it. If you'd rather not, set `FOLLOW_THREADS=false`; follow-ups then need a mention or `/ask`.
4. Run `docsgpt-discord --check` (below). It prints an invite link with the right scopes and permissions: send messages, send messages in threads, create public threads, read message history, embed links, attach files. Open it to add the bot to your server.

### 2. Run it

```bash
git clone https://github.com/arc53/discord-docsgpt-extension.git
cd discord-docsgpt-extension
cp .env.example .env      # fill in DISCORD_TOKEN and API_KEY
docker compose up -d
```

Or without compose:

```bash
docker run -d --name docsgpt-discord --env-file .env -v botdata:/app/data arc53/discord-docsgpt-extension:latest
```

To get the invite link and check the token: `docker run --rm --env-file .env arc53/discord-docsgpt-extension:latest --check`.

### From source

```bash
cargo build --release
./target/release/docsgpt-discord --check
./target/release/docsgpt-discord
```

Rust 1.89 or newer is required.

You need an agent API key from DocsGPT: Agents → your agent → API key.

## Configuration

### One bot: environment variables

| Variable | Purpose |
|---|---|
| `DISCORD_TOKEN` | Bot token. Required. |
| `API_KEY` | DocsGPT agent API key for the default agent. |
| `API_KEY_<NAME>` | Extra agents, addressed as `#name` or with `/agent`. |
| `API_BASE` | DocsGPT server URL (default `https://gptcloud.arc53.com`). |
| `SQLITE_PATH` / `STORAGE_TYPE` | SQLite file (default `data/docsgpt-discord.db`; `/app/data/…` in Docker), or `STORAGE_TYPE=memory`. |
| `THREADS` | `false` to answer mentions inline instead of opening threads. |
| `FOLLOW_THREADS` | `false` to need a mention in threads too (no privileged intent needed). |
| `STREAMING`, `FILES`, `SLASH_COMMANDS` | `false` turns that feature off. |
| `MAX_FILE_MB`, `ALLOWED_GUILDS` | File size limit (default 20); comma-separated server ids to answer in (DMs are always allowed). |
| `RUST_LOG`, `LOG_FORMAT=json` | Logging. |

### Many bots: `docsgpt-discord.toml`

Create `docsgpt-discord.toml` in the working directory, or set `DOCSGPT_DISCORD_CONFIG=/path/to/file`.
- `${VAR}` and `${VAR:-default}` are filled in from the environment, so secrets can stay in `.env`.
- [`docsgpt-discord.example.toml`](docsgpt-discord.example.toml) shows every option.
- In Docker, mount the file: `-v ./docsgpt-discord.toml:/app/docsgpt-discord.toml:ro`.

### Storage

DocsGPT keeps the conversation transcript. The bot only stores:
- which DocsGPT conversation each DM, thread or channel is in
- the chosen agent for each channel
- which threads it opened
- which message holds which answer (for 👍/👎)

Backends:
- `sqlite` (default): one file, kept in the `/app/data` volume in Docker.
- `memory`: lost on restart.

## Using the bot

- **In a server:** mention it (`@DocsGPT how do I deploy?`) or use `/ask`. The answer goes in a new thread; keep asking there.
- **In a DM:** just ask. `/new` starts over.
- **Press Stop** while it's writing to cut the answer short.
- **👍 / 👎** under an answer rate it.
- **Agents:** `/agents` lists them, `/agent sales` switches the channel, and `#sales question` asks one agent once.
- **Files:** attach them to your message. A voice message is transcribed first.

## Upgrading from version 1

- **Images and branches:** `:1` and the `legacy-python` branch keep the Python bot. `:latest` and `:2` are version 2.
- **Your `.env` works as is** (`DISCORD_TOKEN`, `API_KEY`, `API_BASE`), except for MongoDB:
  - `STORAGE_TYPE=mongodb` is no longer supported; remove it and the `MONGODB_*` variables, and keep `/app/data` on a volume.
  - Version 1 kept its own copy of every chat. DocsGPT keeps them now, so existing chats start fresh conversations.
- **Conversations are per DM and per thread**, no longer one per user across every server.
- **New:**
  - streaming, threads, buttons and slash commands
  - several agents
  - files
- **Re-invite the bot** with the link from `--check` so it gets the `applications.commands` scope and the thread permissions. Turn on the Message Content intent, or set `FOLLOW_THREADS=false`.
- **`!start` is gone.** Mention the bot, DM it, or use `/ask`.

## Development

```bash
cargo test            # unit + end-to-end tests against a mock Discord API and a mock DocsGPT
cargo clippy --all-targets -- -D warnings
```

The end-to-end tests feed gateway events (JSON) to the real dispatcher. They run against an in-process mock of Discord's REST API and the [`docsgpt`](https://crates.io/crates/docsgpt) crate's mock server.

Layout:
- `src/config.rs`: TOML and environment config.
- `src/gateway.rs`: the gateway connection.
- `src/events.rs`: what each message, command and button does.
- `src/surface.rs`: how an answer is streamed and finished.
- `src/render.rs`: Markdown and buttons.
- `src/files.rs`: attachments.
- `src/commands.rs`: slash commands.

## License

MIT
