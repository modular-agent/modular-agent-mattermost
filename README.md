# Mattermost Agents for Modular Agent

Mattermost integration agents for Modular Agent. Post messages, fetch history, list channels, and listen to real-time events via WebSocket.

English | [日本語](README_ja.md)

## Features

- **Post** — Send text messages, threaded replies, and images to Mattermost channels
- **History** — Fetch recent message history from a channel
- **Channels** — List channels the bot user belongs to in a team
- **Listener** — Listen to messages in real-time via WebSocket with auto-reconnect
- **ToMessage** — Convert Mattermost message objects to LLM Message format

## Installation

Two changes to add this package to [`modular-agent-desktop`](https://github.com/modular-agent/modular-agent-desktop):

1. **`modular-agent-desktop/src-tauri/Cargo.toml`** — add dependency:

   ```toml
   modular-agent-mattermost = { path = "../../modular-agent-mattermost" }
   ```

2. **`modular-agent-desktop/src-tauri/src/lib.rs`** — add import:

   ```rust
   #[allow(unused_imports)]
   use modular_agent_mattermost;
   ```

## Setup

### Global Config or Environment Variables

| Global Config | Environment Variable | Description |
| ------------- | -------------------- | ----------- |
| `mattermost_token` | `MATTERMOST_TOKEN` | Bot token or personal access token |
| `server_url` | `MATTERMOST_URL` | Mattermost server URL (e.g., `https://mattermost.example.com`) |

### Creating a Bot Account

1. Go to **System Console > Integrations > Bot Accounts** and ensure bot accounts are enabled
2. Go to **Integrations > Bot Accounts > Add Bot Account** to create a bot
3. Copy the generated access token — this is your `mattermost_token`

Alternatively, create a personal access token at **Profile > Security > Personal Access Tokens**.

### Finding IDs

- **Channel ID**: Open the channel, click the channel name header, and copy the ID from the dialog. Or use the Channels agent to list all channels with their IDs.
- **Team ID**: Use the API (`GET /api/v4/teams` with your token) or check **System Console > Teams** (admin only).

## Feature Flags

| Feature | Default | Description |
| ------- | ------- | ----------- |
| `image` | Yes | Image upload/download support for Post and Listener agents |

## Mattermost/Post

Posts messages to Mattermost channels. Markdown is passed through as-is since Mattermost supports standard Markdown natively.

### Configuration

| Config | Type | Default | Description |
| ------ | ---- | ------- | ----------- |
| `channel_id` | string | "" | The Mattermost channel ID to post to |

### Ports

- **Input**: `message` — String, Message, object with `text`/`root_id` fields, array, or image (AgentValue::Image)
- **Output**: `result` — Object containing `ok`, `post_id`, `channel` on success

### Image Upload

When an image (AgentValue::Image) or a Message with an attached image is received, the image is uploaded as a PNG file and attached to the post. Requires the `image` feature flag (enabled by default).

### Thread Replies

- Input objects accept `root_id` to specify the parent post ID for threaded replies
- Output uses `post_id` key with the post ID value

## Mattermost/History

Fetches message history from a Mattermost channel, ordered by most recent first.

### Configuration

| Config | Type | Default | Description |
| ------ | ---- | ------- | ----------- |
| `channel_id` | string | "" | The Mattermost channel ID to fetch history from |
| `limit` | integer | 10 | Maximum number of posts to fetch |

### Ports

- **Input**: `unit` — Any value triggers fetching the history
- **Output**: `values` — Array of post objects with `text`, `user`, `post_id`, `root_id`, `create_at` fields

> **Note:** The output port is named `values` because the output includes Mattermost-specific fields like `create_at` in addition to standard message fields.

The output includes `create_at` (milliseconds timestamp) since Mattermost post IDs are not time-sortable.

## Mattermost/Channels

Lists channels the bot user belongs to. If `team_id` is not configured, automatically uses the first team the user belongs to.

### Configuration

| Config | Type | Default | Description |
| ------ | ---- | ------- | ----------- |
| `team_id` | string | "" | The Mattermost team ID (auto-detected if empty) |
| `limit` | integer | 100 | Maximum number of channels to return |

### Ports

- **Input**: `unit` — Any value triggers fetching the channel list
- **Output**: `channels` — Array of channel objects with `id`, `name`, `is_private`, `is_archived`, `topic`, `purpose` fields

## Mattermost/Listener

Source agent (no inputs). Listens to Mattermost messages in real-time via WebSocket. Starts listening when the workflow starts and outputs messages as they arrive. Automatically reconnects with exponential backoff (1s to 30s) on disconnection. Stops retrying on authentication failure.

### Configuration

| Config | Type | Default | Description |
| ------ | ---- | ------- | ----------- |
| `channel_id` | string | "" | Optional channel filter. If empty, listens to all channels |

### Ports

- **Output**: `value` — Object containing `message` (Message), `user`, `channel`, `post_id`, `root_id`

## Mattermost/ToMessage

Converts Mattermost message objects into AgentValue::Message format suitable for LLM agents.

### Ports

- **Input**: `value` — Single message object or array of message objects
- **Output**: `message` — AgentValue::Message or array of AgentValue::Message

## Architecture

- REST API v4 via `reqwest` (no third-party Mattermost SDK)
- WebSocket via `tokio-tungstenite` for real-time events
- `MattermostClient` is cached per `(server_url, token)` pair using `OnceLock<Mutex<BTreeMap>>`

## Key Dependencies

- [reqwest](https://crates.io/crates/reqwest) — HTTP client for REST API v4
- [tokio-tungstenite](https://crates.io/crates/tokio-tungstenite) — WebSocket client for real-time events
- [chrono](https://crates.io/crates/chrono) — Timestamp formatting for image filenames

## License

Apache-2.0 OR MIT
