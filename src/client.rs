use std::collections::HashMap;

use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use modular_agent_core::AgentError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::debug;
use url::Url;

// ---------------------------------------------------------------------------
// Serde types — Responses
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct MattermostPost {
    pub id: String,
    pub message: String,
    pub user_id: String,
    pub channel_id: String,
    #[serde(default)]
    pub root_id: String,
    pub create_at: i64,
    #[serde(default)]
    pub file_ids: Vec<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct PostList {
    pub order: Vec<String>,
    pub posts: HashMap<String, MattermostPost>,
}

#[derive(Debug, Deserialize)]
pub struct MattermostChannel {
    pub id: String,
    pub name: String,
    pub display_name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub purpose: String,
    pub delete_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct MattermostUser {
    pub id: String,
    pub username: String,
}

#[derive(Debug, Deserialize)]
pub struct MattermostTeam {
    pub id: String,
    pub name: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct FileUploadResponse {
    pub file_infos: Vec<FileInfo>,
}

#[derive(Debug, Deserialize)]
pub struct FileInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime_type: String,
}

// ---------------------------------------------------------------------------
// Serde types — Requests
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct CreatePostRequest {
    pub channel_id: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub file_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Serde types — WebSocket
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct WsAuthChallenge {
    seq: i64,
    action: String,
    data: WsAuthData,
}

#[derive(Debug, Serialize)]
struct WsAuthData {
    token: String,
}

#[derive(Debug, Deserialize)]
pub struct WsEvent {
    pub event: Option<String>,
    pub data: Option<serde_json::Value>,
    pub status: Option<String>,
    pub seq_reply: Option<i64>,
}

// ---------------------------------------------------------------------------
// MattermostClient
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct MattermostClient {
    http: Client,
    base_url: String,
    token: String,
}

impl MattermostClient {
    pub fn new(server_url: &str, token: &str) -> Result<Self, AgentError> {
        let base_url = server_url.trim_end_matches('/').to_string();
        let base_url = if base_url.ends_with("/api/v4") {
            base_url
        } else {
            format!("{}/api/v4", base_url)
        };

        let http = Client::builder()
            .build()
            .map_err(|e| AgentError::IoError(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            http,
            base_url,
            token: token.to_string(),
        })
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.token)
    }

    // -----------------------------------------------------------------------
    // REST API methods
    // -----------------------------------------------------------------------

    /// Get current authenticated user.
    pub async fn get_me(&self) -> Result<MattermostUser, AgentError> {
        let resp = self
            .http
            .get(self.url("/users/me"))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| AgentError::IoError(format!("GET /users/me failed: {}", e)))?;

        Self::check_response(&resp)?;
        resp.json::<MattermostUser>()
            .await
            .map_err(|e| AgentError::IoError(format!("Failed to parse user response: {}", e)))
    }

    /// Get teams for current user.
    pub async fn get_user_teams(&self) -> Result<Vec<MattermostTeam>, AgentError> {
        let resp = self
            .http
            .get(self.url("/users/me/teams"))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| AgentError::IoError(format!("GET /users/me/teams failed: {}", e)))?;

        Self::check_response(&resp)?;
        resp.json::<Vec<MattermostTeam>>()
            .await
            .map_err(|e| AgentError::IoError(format!("Failed to parse teams response: {}", e)))
    }

    /// Create a post in a channel.
    pub async fn create_post(
        &self,
        request: &CreatePostRequest,
    ) -> Result<MattermostPost, AgentError> {
        let resp = self
            .http
            .post(self.url("/posts"))
            .header("Authorization", self.auth_header())
            .json(request)
            .send()
            .await
            .map_err(|e| AgentError::IoError(format!("POST /posts failed: {}", e)))?;

        Self::check_response(&resp)?;
        resp.json::<MattermostPost>()
            .await
            .map_err(|e| AgentError::IoError(format!("Failed to parse post response: {}", e)))
    }

    /// Get posts for a channel, ordered by most recent first.
    pub async fn get_channel_posts(
        &self,
        channel_id: &str,
        per_page: u16,
    ) -> Result<Vec<MattermostPost>, AgentError> {
        let resp = self
            .http
            .get(self.url(&format!("/channels/{}/posts", channel_id)))
            .header("Authorization", self.auth_header())
            .query(&[("per_page", per_page.to_string())])
            .send()
            .await
            .map_err(|e| {
                AgentError::IoError(format!("GET /channels/{}/posts failed: {}", channel_id, e))
            })?;

        Self::check_response(&resp)?;
        let post_list = resp.json::<PostList>().await.map_err(|e| {
            AgentError::IoError(format!("Failed to parse post list response: {}", e))
        })?;

        // Reconstruct ordered list from order + posts map
        debug!(
            "Retrieved {} posts for channel {}",
            post_list.order.len(),
            channel_id
        );

        let mut posts_map = post_list.posts;
        let ordered: Vec<MattermostPost> = post_list
            .order
            .into_iter()
            .filter_map(|id| posts_map.remove(&id))
            .collect();

        Ok(ordered)
    }

    /// Get channels for a user in a team.
    pub async fn get_user_channels(
        &self,
        user_id: &str,
        team_id: &str,
    ) -> Result<Vec<MattermostChannel>, AgentError> {
        let resp = self
            .http
            .get(self.url(&format!("/users/{}/teams/{}/channels", user_id, team_id)))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| AgentError::IoError(format!("GET user channels failed: {}", e)))?;

        Self::check_response(&resp)?;
        resp.json::<Vec<MattermostChannel>>()
            .await
            .map_err(|e| AgentError::IoError(format!("Failed to parse channels response: {}", e)))
    }

    /// Upload a file to a channel (multipart).
    #[cfg(feature = "image")]
    pub async fn upload_file(
        &self,
        channel_id: &str,
        filename: &str,
        bytes: Vec<u8>,
        content_type: &str,
    ) -> Result<FileUploadResponse, AgentError> {
        let file_part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename.to_string())
            .mime_str(content_type)
            .map_err(|e| AgentError::IoError(format!("Failed to create multipart part: {}", e)))?;

        let form = reqwest::multipart::Form::new()
            .text("channel_id", channel_id.to_string())
            .part("files", file_part);

        let resp = self
            .http
            .post(self.url("/files"))
            .header("Authorization", self.auth_header())
            .multipart(form)
            .send()
            .await
            .map_err(|e| AgentError::IoError(format!("POST /files failed: {}", e)))?;

        Self::check_response(&resp)?;
        resp.json::<FileUploadResponse>().await.map_err(|e| {
            AgentError::IoError(format!("Failed to parse file upload response: {}", e))
        })
    }

    /// Download a file by its ID.
    #[cfg(feature = "image")]
    pub async fn download_file(&self, file_id: &str) -> Result<Vec<u8>, AgentError> {
        let resp = self
            .http
            .get(self.url(&format!("/files/{}", file_id)))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| AgentError::IoError(format!("GET /files/{} failed: {}", file_id, e)))?;

        Self::check_response(&resp)?;
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| AgentError::IoError(format!("Failed to read file bytes: {}", e)))
    }

    /// Get file info by its ID (to check mime type).
    #[cfg(feature = "image")]
    pub async fn get_file_info(&self, file_id: &str) -> Result<FileInfo, AgentError> {
        let resp = self
            .http
            .get(self.url(&format!("/files/{}/info", file_id)))
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| {
                AgentError::IoError(format!("GET /files/{}/info failed: {}", file_id, e))
            })?;

        Self::check_response(&resp)?;
        resp.json::<FileInfo>()
            .await
            .map_err(|e| AgentError::IoError(format!("Failed to parse file info response: {}", e)))
    }

    // -----------------------------------------------------------------------
    // WebSocket
    // -----------------------------------------------------------------------

    /// Connect to the Mattermost WebSocket and authenticate.
    /// Returns the authenticated read half of the stream.
    pub async fn connect_websocket(
        &self,
    ) -> Result<SplitStream<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>, AgentError>
    {
        let ws_url = self.websocket_url()?;

        let (ws_stream, _) = connect_async(&ws_url)
            .await
            .map_err(|e| AgentError::IoError(format!("WebSocket connection failed: {}", e)))?;

        let (mut write, mut read) = ws_stream.split();

        // Send authentication challenge
        let auth = WsAuthChallenge {
            seq: 1,
            action: "authentication_challenge".to_string(),
            data: WsAuthData {
                token: self.token.clone(),
            },
        };
        let auth_json = serde_json::to_string(&auth)
            .map_err(|e| AgentError::IoError(format!("Failed to serialize auth: {}", e)))?;

        write
            .send(WsMessage::Text(auth_json.into()))
            .await
            .map_err(|e| AgentError::IoError(format!("Failed to send auth challenge: {}", e)))?;

        // Read auth response
        if let Some(msg) = read.next().await {
            let msg =
                msg.map_err(|e| AgentError::IoError(format!("WebSocket read error: {}", e)))?;
            if let WsMessage::Text(text) = msg
                && let Ok(event) = serde_json::from_str::<WsEvent>(&text)
                && event.status.as_deref() == Some("FAIL")
            {
                return Err(AgentError::IoError(
                    "WebSocket authentication failed".to_string(),
                ));
            }
        }

        Ok(read)
    }

    fn websocket_url(&self) -> Result<String, AgentError> {
        let parsed = Url::parse(&self.base_url)
            .map_err(|e| AgentError::InvalidConfig(format!("Invalid server URL: {}", e)))?;

        let scheme = match parsed.scheme() {
            "https" => "wss",
            "http" => "ws",
            other => {
                return Err(AgentError::InvalidConfig(format!(
                    "Unsupported URL scheme: {}",
                    other
                )));
            }
        };

        let host = parsed
            .host_str()
            .ok_or_else(|| AgentError::InvalidConfig("Missing host in server URL".to_string()))?;

        let port = parsed.port().map(|p| format!(":{}", p)).unwrap_or_default();

        Ok(format!("{}://{}{}/api/v4/websocket", scheme, host, port))
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn check_response(resp: &reqwest::Response) -> Result<(), AgentError> {
        if resp.status().is_success() {
            return Ok(());
        }
        Err(AgentError::IoError(format!(
            "Mattermost API error: HTTP {}",
            resp.status()
        )))
    }
}
