use std::collections::BTreeMap;
use std::env;
#[cfg(feature = "image")]
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

use futures_util::StreamExt;
use im::{Vector, hashmap};
use modular_agent_core::photon_rs::PhotonImage;
use modular_agent_core::{
    Agent, AgentContext, AgentData, AgentError, AgentOutput, AgentSpec, AgentValue, AsAgent,
    Message, ModularAgent, async_trait, modular_agent,
};
use tokio::sync::mpsc;
use tracing::error;

use crate::client::{CreatePostRequest, MattermostClient, MattermostPost, WsEvent};

static CATEGORY: &str = "Mattermost";

static PORT_RESULT: &str = "result";
static PORT_UNIT: &str = "unit";
static PORT_MESSAGE: &str = "message";
static PORT_VALUE: &str = "value";
static PORT_VALUES: &str = "values";
static PORT_CHANNELS: &str = "channels";

static CONFIG_CHANNEL_ID: &str = "channel_id";
static CONFIG_TEAM_ID: &str = "team_id";
static CONFIG_LIMIT: &str = "limit";
static CONFIG_MATTERMOST_TOKEN: &str = "mattermost_token";
static CONFIG_SERVER_URL: &str = "server_url";

// ---------------------------------------------------------------------------
// Client caching (keyed by server_url + token)
// ---------------------------------------------------------------------------

static CLIENT_MAP: OnceLock<Mutex<BTreeMap<String, MattermostClient>>> = OnceLock::new();

fn get_client_map() -> &'static Mutex<BTreeMap<String, MattermostClient>> {
    CLIENT_MAP.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn get_cached_client(server_url: &str, token: &str) -> Result<MattermostClient, AgentError> {
    let key = format!("{}\0{}", server_url, token);
    let mut map = get_client_map().lock().unwrap();
    if let Some(client) = map.get(&key) {
        return Ok(client.clone());
    }
    let client = MattermostClient::new(server_url, token)?;
    map.insert(key, client.clone());
    Ok(client)
}

// ---------------------------------------------------------------------------
// Config helpers
// ---------------------------------------------------------------------------

fn get_token(ma: &ModularAgent) -> Result<String, AgentError> {
    if let Some(global_token) = ma
        .get_global_configs(MattermostPostAgent::DEF_NAME)
        .and_then(|cfg| cfg.get_string(CONFIG_MATTERMOST_TOKEN).ok())
        .filter(|key| !key.is_empty())
    {
        Ok(global_token)
    } else {
        env::var("MATTERMOST_TOKEN")
            .map_err(|_| AgentError::InvalidValue("MATTERMOST_TOKEN not set".to_string()))
    }
}

fn get_server_url(ma: &ModularAgent) -> Result<String, AgentError> {
    if let Some(global_url) = ma
        .get_global_configs(MattermostPostAgent::DEF_NAME)
        .and_then(|cfg| cfg.get_string(CONFIG_SERVER_URL).ok())
        .filter(|url| !url.is_empty())
    {
        Ok(global_url)
    } else {
        env::var("MATTERMOST_URL")
            .map_err(|_| AgentError::InvalidValue("MATTERMOST_URL not set".to_string()))
    }
}

fn build_client(ma: &ModularAgent) -> Result<MattermostClient, AgentError> {
    let token = get_token(ma)?;
    let server_url = get_server_url(ma)?;
    get_cached_client(&server_url, &token)
}

// ---------------------------------------------------------------------------
// MattermostPostAgent
// ---------------------------------------------------------------------------

/// Agent for posting messages to Mattermost channels.
///
/// Sends text messages, threaded replies, and images to a specified channel.
/// Markdown is passed through as-is since Mattermost supports standard Markdown natively.
///
/// # Ports
/// - Input `message`: String, Message, object with `text`/`root_id` fields, array, or image
/// - Output `result`: Object containing `ok`, `post_id`, `channel` on success
///
/// # Configuration
/// - `channel_id`: The Mattermost channel ID to post to
///
/// # Global Configuration
/// - `mattermost_token`: Bot token or personal access token
/// - `server_url`: Mattermost server URL (e.g., "https://mattermost.example.com")
///
/// # Example
/// Given input `"Hello, world!"` with channel_id configured, posts the message
/// and outputs `{ok: true, post_id: "<post_id>", channel: "<channel_id>"}`.
#[modular_agent(
    title = "Post",
    category = CATEGORY,
    inputs = [PORT_MESSAGE],
    outputs = [PORT_RESULT],
    string_config(name = CONFIG_CHANNEL_ID),
    string_global_config(name = CONFIG_SERVER_URL, title = "Mattermost Server URL"),
    custom_global_config(name = CONFIG_MATTERMOST_TOKEN, type_ = "password", default = AgentValue::string(""), title = "Mattermost Token"),
)]
struct MattermostPostAgent {
    data: AgentData,
}

#[async_trait]
impl AsAgent for MattermostPostAgent {
    fn new(ma: ModularAgent, id: String, spec: AgentSpec) -> Result<Self, AgentError> {
        Ok(Self {
            data: AgentData::new(ma, id, spec),
        })
    }

    async fn process(
        &mut self,
        ctx: AgentContext,
        _port: String,
        value: AgentValue,
    ) -> Result<(), AgentError> {
        let config = self.configs()?;
        let channel_id = config.get_string(CONFIG_CHANNEL_ID)?;
        if channel_id.is_empty() {
            return Err(AgentError::InvalidValue(
                "Channel ID not configured".to_string(),
            ));
        }

        let client = build_client(self.ma())?;

        // Handle image upload
        #[cfg(feature = "image")]
        if let Some(image) = value.as_image() {
            let result = upload_image_and_post(&client, image, &channel_id, None, None).await?;
            return self.output(ctx, PORT_RESULT, result).await;
        }

        // Handle Message with image
        #[cfg(feature = "image")]
        if let Some(msg) = value.as_message()
            && let Some(ref image) = msg.image
        {
            let message_text = if msg.content.is_empty() {
                None
            } else {
                Some(msg.content.clone())
            };
            let result =
                upload_image_and_post(&client, image, &channel_id, message_text, None).await?;
            return self.output(ctx, PORT_RESULT, result).await;
        }

        let (text, root_id) = extract_message_content(&value)?;

        let request = CreatePostRequest {
            channel_id: channel_id.clone(),
            message: text,
            root_id,
            file_ids: vec![],
        };

        let post = client.create_post(&request).await?;

        let result = AgentValue::object(hashmap! {
            "ok".into() => AgentValue::boolean(true),
            "post_id".into() => AgentValue::string(post.id),
            "channel".into() => AgentValue::string(post.channel_id),
        });

        self.output(ctx, PORT_RESULT, result).await
    }
}

#[cfg(feature = "image")]
async fn upload_image_and_post(
    client: &MattermostClient,
    image: &PhotonImage,
    channel_id: &str,
    message: Option<String>,
    root_id: Option<String>,
) -> Result<AgentValue, AgentError> {
    let png_bytes = image.get_bytes();
    let filename = format!("image_{}.png", chrono::Utc::now().timestamp_millis());

    let upload_resp = client
        .upload_file(channel_id, &filename, png_bytes, "image/png")
        .await?;

    let file_ids: Vec<String> = upload_resp
        .file_infos
        .iter()
        .map(|f| f.id.clone())
        .collect();

    let request = CreatePostRequest {
        channel_id: channel_id.to_string(),
        message: message.unwrap_or_default(),
        root_id,
        file_ids,
    };

    let post = client.create_post(&request).await?;

    Ok(AgentValue::object(hashmap! {
        "ok".into() => AgentValue::boolean(true),
        "post_id".into() => AgentValue::string(post.id),
        "channel".into() => AgentValue::string(post.channel_id),
    }))
}

fn extract_message_content(value: &AgentValue) -> Result<(String, Option<String>), AgentError> {
    match value {
        AgentValue::String(s) => Ok((s.to_string(), None)),
        AgentValue::Message(msg) => Ok((msg.content.clone(), None)),
        AgentValue::Object(obj) => {
            let text = obj
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            // Extract root_id for threaded replies
            let root_id = obj
                .get("root_id")
                .and_then(|v| v.as_str())
                .map(String::from);
            Ok((text, root_id))
        }
        AgentValue::Array(arr) => {
            let texts: Vec<String> = arr
                .iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(String::from)
                        .or_else(|| v.as_message().map(|m| m.content.clone()))
                })
                .collect();
            Ok((texts.join("\n"), None))
        }
        _ => {
            let json = serde_json::to_string_pretty(&value.to_json()).unwrap_or_default();
            Ok((format!("```\n{}\n```", json), None))
        }
    }
}

// ---------------------------------------------------------------------------
// MattermostHistoryAgent
// ---------------------------------------------------------------------------

/// Agent for fetching message history from a Mattermost channel.
///
/// Retrieves recent posts from a channel, ordered by most recent first.
///
/// # Ports
/// - Input `unit`: Any value triggers fetching the history
/// - Output `values`: Array of post objects containing `text`, `user`, `post_id`, `root_id`, `create_at`
///
/// # Configuration
/// - `channel_id`: The Mattermost channel ID to fetch history from
/// - `limit`: Maximum number of posts to fetch (default: 10)
#[modular_agent(
    title = "History",
    category = CATEGORY,
    inputs = [PORT_UNIT],
    outputs = [PORT_VALUES],
    string_config(name = CONFIG_CHANNEL_ID),
    integer_config(name = CONFIG_LIMIT),
)]
struct MattermostHistoryAgent {
    data: AgentData,
}

#[async_trait]
impl AsAgent for MattermostHistoryAgent {
    fn new(ma: ModularAgent, id: String, spec: AgentSpec) -> Result<Self, AgentError> {
        Ok(Self {
            data: AgentData::new(ma, id, spec),
        })
    }

    async fn process(
        &mut self,
        ctx: AgentContext,
        _port: String,
        _value: AgentValue,
    ) -> Result<(), AgentError> {
        let config = self.configs()?;
        let channel_id = config.get_string(CONFIG_CHANNEL_ID)?;
        if channel_id.is_empty() {
            return Err(AgentError::InvalidValue(
                "Channel ID not configured".to_string(),
            ));
        }

        let limit = config.get_integer_or_default(CONFIG_LIMIT);
        let limit = if limit <= 0 { 10 } else { limit as u16 };

        let client = build_client(self.ma())?;
        let posts = client.get_channel_posts(&channel_id, limit).await?;

        let messages: Vector<AgentValue> = posts.iter().map(post_to_agent_value).collect();

        self.output(ctx, PORT_VALUES, AgentValue::array(messages))
            .await
    }
}

fn post_to_agent_value(post: &MattermostPost) -> AgentValue {
    let mut obj = im::HashMap::new();

    obj.insert("text".into(), AgentValue::string(post.message.clone()));
    obj.insert("user".into(), AgentValue::string(post.user_id.clone()));
    obj.insert("post_id".into(), AgentValue::string(post.id.clone()));

    if !post.root_id.is_empty() {
        obj.insert("root_id".into(), AgentValue::string(post.root_id.clone()));
    }

    obj.insert("create_at".into(), AgentValue::integer(post.create_at));

    AgentValue::object(obj)
}

// ---------------------------------------------------------------------------
// MattermostChannelsAgent
// ---------------------------------------------------------------------------

/// Agent for listing Mattermost channels in a team.
///
/// Lists channels the bot user belongs to. If `team_id` is not configured,
/// automatically uses the first team the user belongs to.
///
/// # Ports
/// - Input `unit`: Any value triggers fetching the channel list
/// - Output `channels`: Array of channel objects containing `id`, `name`, `is_private`, `is_archived`, `topic`, `purpose`
///
/// # Configuration
/// - `team_id`: The Mattermost team ID (auto-detected if empty)
/// - `limit`: Maximum number of channels to return (default: 100)
#[modular_agent(
    title = "Channels",
    category = CATEGORY,
    inputs = [PORT_UNIT],
    outputs = [PORT_CHANNELS],
    string_config(name = CONFIG_TEAM_ID),
    integer_config(name = CONFIG_LIMIT),
)]
struct MattermostChannelsAgent {
    data: AgentData,
}

#[async_trait]
impl AsAgent for MattermostChannelsAgent {
    fn new(ma: ModularAgent, id: String, spec: AgentSpec) -> Result<Self, AgentError> {
        Ok(Self {
            data: AgentData::new(ma, id, spec),
        })
    }

    async fn process(
        &mut self,
        ctx: AgentContext,
        _port: String,
        _value: AgentValue,
    ) -> Result<(), AgentError> {
        let config = self.configs()?;
        let limit = config.get_integer_or_default(CONFIG_LIMIT);
        let limit = if limit <= 0 { 100 } else { limit as usize };

        let client = build_client(self.ma())?;

        // Resolve team_id: use config or auto-detect from first team
        let team_id = config.get_string_or_default(CONFIG_TEAM_ID);
        let team_id = if team_id.is_empty() {
            let teams = client.get_user_teams().await?;
            teams
                .first()
                .map(|t| t.id.clone())
                .ok_or_else(|| AgentError::InvalidValue("No teams found for user".to_string()))?
        } else {
            team_id
        };

        let all_channels = client.get_user_channels("me", &team_id).await?;

        let channels: Vector<AgentValue> = all_channels
            .iter()
            .take(limit)
            .map(|ch| {
                let mut obj = im::HashMap::new();
                obj.insert("id".into(), AgentValue::string(ch.id.clone()));
                obj.insert("name".into(), AgentValue::string(ch.name.clone()));
                obj.insert(
                    "is_private".into(),
                    AgentValue::boolean(ch.channel_type == "P"),
                );
                obj.insert("is_archived".into(), AgentValue::boolean(ch.delete_at > 0));
                if !ch.header.is_empty() {
                    obj.insert("topic".into(), AgentValue::string(ch.header.clone()));
                }
                if !ch.purpose.is_empty() {
                    obj.insert("purpose".into(), AgentValue::string(ch.purpose.clone()));
                }
                AgentValue::object(obj)
            })
            .collect();

        self.output(ctx, PORT_CHANNELS, AgentValue::array(channels))
            .await
    }
}

// ---------------------------------------------------------------------------
// MattermostListenerAgent
// ---------------------------------------------------------------------------

/// Agent for listening to Mattermost messages in real-time via WebSocket.
///
/// Connects to the Mattermost WebSocket API and outputs messages as they arrive.
/// Automatically reconnects with exponential backoff on disconnection.
///
/// # Ports
/// - Output `value`: Object containing `message` (Message), `user`, `channel`, `post_id`, `root_id`
///
/// # Configuration
/// - `channel_id`: Optional channel filter. If empty, listens to all channels.
///
/// # Global Configuration
/// - `mattermost_token`: Bot token or personal access token
/// - `server_url`: Mattermost server URL
#[modular_agent(
    title = "Listener",
    category = CATEGORY,
    outputs = [PORT_VALUE],
    string_config(name = CONFIG_CHANNEL_ID),
)]
struct MattermostListenerAgent {
    data: AgentData,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

struct ListenerState {
    client: MattermostClient,
    ma: ModularAgent,
    id: String,
    channel_filter: Option<String>,
    bot_user_id: String,
}

#[async_trait]
impl AsAgent for MattermostListenerAgent {
    fn new(ma: ModularAgent, id: String, spec: AgentSpec) -> Result<Self, AgentError> {
        Ok(Self {
            data: AgentData::new(ma, id, spec),
            shutdown_tx: None,
        })
    }

    async fn start(&mut self) -> Result<(), AgentError> {
        let client = build_client(self.ma())?;

        let me = client.get_me().await?;
        let bot_user_id = me.id;

        let config = self.configs()?;
        let channel_filter = config.get_string_or_default(CONFIG_CHANNEL_ID);
        let channel_filter = if channel_filter.is_empty() {
            None
        } else {
            Some(channel_filter)
        };

        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        let state = ListenerState {
            client,
            ma: self.ma().clone(),
            id: self.id().to_string(),
            channel_filter,
            bot_user_id,
        };

        tokio::spawn(async move {
            let mut backoff_ms: u64 = 1000;
            const MAX_BACKOFF_MS: u64 = 30_000;

            loop {
                match state.client.connect_websocket().await {
                    Ok(mut read) => {
                        backoff_ms = 1000; // Reset on successful connect

                        loop {
                            tokio::select! {
                                msg = read.next() => {
                                    match msg {
                                        Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) => {
                                            handle_ws_message(&state, &text);
                                        }
                                        Some(Ok(_)) => {} // Ignore non-text messages
                                        Some(Err(e)) => {
                                            error!("WebSocket error: {}", e);
                                            break; // Reconnect
                                        }
                                        None => {
                                            error!("WebSocket stream ended");
                                            break; // Reconnect
                                        }
                                    }
                                }
                                _ = shutdown_rx.recv() => {
                                    return; // Clean shutdown
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let err_msg = e.to_string();
                        // Stop retrying on auth failures
                        if err_msg.contains("authentication failed") || err_msg.contains("HTTP 401")
                        {
                            error!("WebSocket auth failed, stopping listener: {}", e);
                            return;
                        }
                        error!("WebSocket connection failed: {}", e);
                    }
                }

                // Exponential backoff before reconnect
                tokio::select! {
                    _ = tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)) => {}
                    _ = shutdown_rx.recv() => { return; }
                }
                backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            }
        });

        Ok(())
    }

    async fn stop(&mut self) -> Result<(), AgentError> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }
        Ok(())
    }
}

fn handle_ws_message(state: &ListenerState, text: &str) {
    let Ok(event) = serde_json::from_str::<WsEvent>(text) else {
        return;
    };

    // Only handle "posted" events
    if event.event.as_deref() != Some("posted") {
        return;
    }

    let Some(ref data) = event.data else {
        return;
    };

    // data.post is a JSON string (double-encoded) — parse it
    let Some(post_str) = data.get("post").and_then(|v| v.as_str()) else {
        return;
    };
    let Ok(post) = serde_json::from_str::<MattermostPost>(post_str) else {
        error!("Failed to parse post from WebSocket event");
        return;
    };

    // Skip bot's own messages
    if post.user_id == state.bot_user_id {
        return;
    }

    // Apply channel filter
    if let Some(ref filter) = state.channel_filter
        && post.channel_id != *filter
    {
        return;
    }

    // Download image if present
    #[cfg(feature = "image")]
    let image = download_first_image_sync(&state.client, &post);
    #[cfg(not(feature = "image"))]
    let image: Option<PhotonImage> = None;

    if let Some(output) = post_to_listener_output(&post, image)
        && let Err(e) = state.ma.try_send_agent_out(
            state.id.clone(),
            AgentContext::new(),
            PORT_VALUE.to_string(),
            output,
        )
    {
        error!("Failed to output message: {}", e);
    }
}

#[cfg(feature = "image")]
fn download_first_image_sync(
    client: &MattermostClient,
    post: &MattermostPost,
) -> Option<PhotonImage> {
    if post.file_ids.is_empty() {
        return None;
    }

    let client = client.clone();
    let file_ids = post.file_ids.clone();

    // Use block_in_place to call async from sync context within tokio
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async {
            for file_id in &file_ids {
                let Ok(info) = client.get_file_info(file_id).await else {
                    continue;
                };
                if !info.mime_type.starts_with("image/") {
                    continue;
                }
                match client.download_file(file_id).await {
                    Ok(bytes) => {
                        let image = PhotonImage::new_from_byteslice(bytes);
                        return Some(image);
                    }
                    Err(e) => {
                        error!("Failed to download image: {}", e);
                        continue;
                    }
                }
            }
            None
        })
    })
}

fn post_to_listener_output(
    post: &MattermostPost,
    #[allow(unused_variables)] image: Option<PhotonImage>,
) -> Option<AgentValue> {
    let text = post.message.clone();
    let channel = post.channel_id.clone();
    let post_id = post.id.clone();
    let root_id = if post.root_id.is_empty() {
        None
    } else {
        Some(post.root_id.clone())
    };
    let user = post.user_id.clone();

    #[cfg(feature = "image")]
    {
        let mut message = Message::user(text);
        message.image = image.map(Arc::new);

        let mut obj = im::HashMap::new();
        obj.insert("message".into(), AgentValue::message(message));
        obj.insert("user".into(), AgentValue::string(user));
        obj.insert("channel".into(), AgentValue::string(channel));
        obj.insert("post_id".into(), AgentValue::string(post_id));
        if let Some(root_id) = root_id {
            obj.insert("root_id".into(), AgentValue::string(root_id));
        }
        Some(AgentValue::object(obj))
    }

    #[cfg(not(feature = "image"))]
    {
        let message = Message::user(text);

        let mut obj = im::HashMap::new();
        obj.insert("message".into(), AgentValue::message(message));
        obj.insert("user".into(), AgentValue::string(user));
        obj.insert("channel".into(), AgentValue::string(channel));
        obj.insert("post_id".into(), AgentValue::string(post_id));
        if let Some(root_id) = root_id {
            obj.insert("root_id".into(), AgentValue::string(root_id));
        }
        Some(AgentValue::object(obj))
    }
}

// ---------------------------------------------------------------------------
// MattermostToMessageAgent
// ---------------------------------------------------------------------------

/// Agent for converting Mattermost messages to LLM Message format.
///
/// Converts Mattermost message objects (with `text`, `user`, `channel`, `post_id` fields)
/// into AgentValue::Message format suitable for LLM agents.
///
/// # Ports
/// - Input `value`: Single message object or array of message objects
/// - Output `message`: AgentValue::Message or array of AgentValue::Message
#[modular_agent(
    title = "ToMessage",
    category = CATEGORY,
    inputs = [PORT_VALUE],
    outputs = [PORT_MESSAGE],
)]
struct MattermostToMessageAgent {
    data: AgentData,
}

#[async_trait]
impl AsAgent for MattermostToMessageAgent {
    fn new(ma: ModularAgent, id: String, spec: AgentSpec) -> Result<Self, AgentError> {
        Ok(Self {
            data: AgentData::new(ma, id, spec),
        })
    }

    async fn process(
        &mut self,
        ctx: AgentContext,
        _port: String,
        value: AgentValue,
    ) -> Result<(), AgentError> {
        if value.is_array() {
            let arr = value.as_array().unwrap();
            let messages: im::Vector<AgentValue> = arr
                .iter()
                .filter_map(|v| value_to_message(v).ok())
                .map(AgentValue::message)
                .collect();
            self.output(ctx, PORT_MESSAGE, AgentValue::array(messages))
                .await
        } else {
            let message = value_to_message(&value)?;
            self.output(ctx, PORT_MESSAGE, AgentValue::message(message))
                .await
        }
    }
}

fn value_to_message(value: &AgentValue) -> Result<Message, AgentError> {
    match value {
        AgentValue::String(s) => Ok(Message::user(s.to_string())),
        AgentValue::Message(msg) => Ok(Message::clone(msg)),
        AgentValue::Object(obj) => {
            // New format: check for "message" field first
            if let Some(msg) = obj.get("message").and_then(|v| v.as_message()) {
                return Ok(Message::clone(msg));
            }
            // Legacy format: use "text" field
            let text = obj
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(Message::user(text))
        }
        _ => Err(AgentError::InvalidValue(
            "Expected string, message, or object for Mattermost message".to_string(),
        )),
    }
}
