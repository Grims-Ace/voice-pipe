// ============================================================================
// ============================================================================
// Call the HTTP API of Discord or Fluxer for one bot: find bot IDs, read user
// profiles, copy names and avatars to proxies, and send chat replies.
// ============================================================================
// ============================================================================

use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use reqwest::header::{CONTENT_TYPE, HOST};
use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::{Value, json};
use tracing::warn;

use crate::config::PlatformSettings;
use crate::platform::Platform;

// ============================================================================
// Constants
// ============================================================================

// Discord and Fluxer limit nicknames to 32 characters.
pub const NICKNAME_MAX: usize = 32;
// Discord limits a chat message to 2000 characters.
pub const MESSAGE_MAX: usize = 2000;
// Longest rate limit wait that the client accepts before it gives up.
const RATE_LIMIT_MAX_WAIT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
// Largest avatar image that the client downloads. Avatars are much smaller,
// so a larger body is an error.
const AVATAR_MAX_BYTES: usize = 8 * 1024 * 1024;
const AVATAR_DEFAULT_TYPE: &str = "image/png";

// ============================================================================
// Types
// ============================================================================

// The name and avatar that a proxy copies from a user.
#[derive(Clone, Debug)]
pub struct UserProfile {
        _name: String,
        _avatarUrl: Option<String>,
}

// An API client that sends requests as one bot.
#[derive(Clone)]
pub struct RestClient {
        _http: reqwest::Client,
        _platform: Platform,
        _apiUrl: String,
        _token: String,
        _hostHeader: Option<String>,
        _avatarUrl: String,
        _guildAvatarUrl: Option<String>,
}

impl UserProfile {
        pub fn getName(&self) -> &str { &self._name }
        pub fn getAvatarUrl(&self) -> Option<&str> {
                self._avatarUrl.as_deref()
        }
}

impl RestClient {
        pub fn getPlatform(&self) -> Platform { self._platform }

        pub fn New(
                http: &reqwest::Client,
                platform: Platform,
                settings: &PlatformSettings,
                token: &str,
        ) -> Self {
                Self {
                        _http: http.clone(),
                        _platform: platform,
                        _apiUrl: settings.getApiUrl().to_string(),
                        _token: token.to_string(),
                        _hostHeader: settings.getHostHeader().map(String::from),
                        _avatarUrl: settings.getAvatarUrl().to_string(),
                        _guildAvatarUrl: settings
                                .getGuildAvatarUrl()
                                .map(String::from),
                }
        }

        // Make a client for another bot on the same platform.
        pub fn WithToken(&self, token: &str) -> Self {
                Self { _token: token.to_string(), ..self.clone() }
        }

        // ====================================================================
        // Bot Operations
        // ====================================================================

        // Find the user ID of the bot that owns the token.
        pub async fn FetchSelfId(&self) -> Result<String> {
                let user = self.Send(Method::GET, "/users/@me", None).await?;
                let id = user
                        .get("id")
                        .and_then(Value::as_str)
                        .context("response has no user ID")?;
                Ok(id.to_string())
        }

        // Read the name and avatar of a guild member. Use the user object if
        // the member request fails.
        pub async fn FetchProfile(
                &self,
                guild_id: &str,
                user_id: &str,
        ) -> Result<UserProfile> {
                let path = format!("/guilds/{guild_id}/members/{user_id}");
                let member = match self.Send(Method::GET, &path, None).await {
                        Ok(member) => member,
                        Err(e) => {
                                warn!(
                                        platform = %self._platform,
                                        error = %e,
                                        "member read failed",
                                );
                                self.FetchUserAsMember(user_id).await?
                        }
                };
                Ok(self.BuildProfile(guild_id, user_id, &member))
        }

        // Read the user object and put it in a member object that has no
        // guild values.
        async fn FetchUserAsMember(&self, user_id: &str) -> Result<Value> {
                let path = format!("/users/{user_id}");
                let user = self.Send(Method::GET, &path, None).await?;
                Ok(json!({ "user": user }))
        }

        // Set the nickname of this bot in a guild. None removes the nickname.
        pub async fn SetNickname(
                &self,
                guild_id: &str,
                bot_id: &str,
                nickname: Option<&str>,
        ) -> Result<()> {
                let path = match self._platform {
                        // Discord bots change their own nickname with @me.
                        Platform::Discord => {
                                format!("/guilds/{guild_id}/members/@me")
                        }
                        // Fluxer accepts the explicit bot ID for this request.
                        Platform::Fluxer => {
                                format!("/guilds/{guild_id}/members/{bot_id}")
                        }
                };
                self.Send(
                        Method::PATCH,
                        &path,
                        Some(json!({ "nick": nickname })),
                )
                .await?;
                Ok(())
        }

        // Set the guild avatar of this bot from a data URI. None removes it.
        pub async fn SetAvatar(
                &self,
                guild_id: &str,
                image: Option<&str>,
        ) -> Result<()> {
                let path = format!("/guilds/{guild_id}/members/@me");
                self.Send(
                        Method::PATCH,
                        &path,
                        Some(json!({ "avatar": image })),
                )
                .await?;
                Ok(())
        }

        // Download an image and return it as a base64 data URI.
        pub async fn FetchImage(&self, url: &str) -> Result<String> {
                let response = self
                        .AddHost(self._http.get(url))
                        .timeout(REQUEST_TIMEOUT)
                        .send()
                        .await?
                        .error_for_status()?;
                let size = response.content_length().unwrap_or(0) as usize;
                if size > AVATAR_MAX_BYTES { bail!("image is too large"); }
                let kind = FindImageType(&response);
                let bytes = response.bytes().await?;
                if bytes.len() > AVATAR_MAX_BYTES {
                        bail!("image is too large");
                }
                let encoded = base64::engine::general_purpose::STANDARD
                        .encode(&bytes);
                Ok(format!("data:{kind};base64,{encoded}"))
        }

        // Send a chat message as a reply to another message.
        pub async fn SendReply(
                &self,
                channel_id: &str,
                reply_to: &str,
                content: &str,
        ) -> Result<()> {
                let reference = json!({
                        "message_id": reply_to,
                        "fail_if_not_exists": false,
                });
                let body = json!({
                        "content": Truncate(content, MESSAGE_MAX),
                        "message_reference": reference,
                        "allowed_mentions": { "parse": [] },
                });
                let path = format!("/channels/{channel_id}/messages");
                self.Send(Method::POST, &path, Some(body)).await?;
                Ok(())
        }

        // ====================================================================
        // Request Helpers
        // ====================================================================

        // Send one request. Wait and try again once if the API sends a rate
        // limit.
        async fn Send(
                &self,
                method: Method,
                path: &str,
                body: Option<Value>,
        ) -> Result<Value> {
                for _ in 0..2 {
                        let response = self
                                .BuildRequest(
                                        method.clone(),
                                        path,
                                        body.as_ref(),
                                )
                                .send()
                                .await?;
                        let status = response.status();
                        if status == StatusCode::TOO_MANY_REQUESTS {
                                let value: Value = response
                                        .json()
                                        .await
                                        .unwrap_or_default();
                                WaitForRateLimit(&value, path).await?;
                                continue;
                        }
                        let text = response.text().await.unwrap_or_default();
                        if !status.is_success() {
                                bail!("{method} {path}: HTTP {status}: {text}");
                        }
                        return Ok(serde_json::from_str(&text)
                                .unwrap_or(Value::Null));
                }
                bail!("{method} {path} is rate limited")
        }

        // Make an API request with the bot token and the optional JSON body.
        fn BuildRequest(
                &self,
                method: Method,
                path: &str,
                body: Option<&Value>,
        ) -> RequestBuilder {
                let request = self
                        ._http
                        .request(method, format!("{}{path}", self._apiUrl))
                        .header("Authorization", format!("Bot {}", self._token))
                        .timeout(REQUEST_TIMEOUT);
                let request = self.AddHost(request);
                match body {
                        Some(body) => request.json(body),
                        None => request,
                }
        }

        // Add the configured Host header, for local servers that route by
        // host name.
        fn AddHost(&self, request: RequestBuilder) -> RequestBuilder {
                match &self._hostHeader {
                        Some(host) => request.header(HOST, host),
                        None => request,
                }
        }

        // Read the display name and avatar URL from a member object.
        fn BuildProfile(
                &self,
                guild_id: &str,
                user_id: &str,
                member: &Value,
        ) -> UserProfile {
                let user = &member["user"];
                let name = [
                        &member["nick"],
                        &user["global_name"],
                        &user["display_name"],
                        &user["username"],
                ]
                .into_iter()
                .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
                .unwrap_or("Unknown User");
                let guild_avatar = member["avatar"]
                        .as_str()
                        .zip(self._guildAvatarUrl.as_deref());
                let avatar_url = match (guild_avatar, user["avatar"].as_str()) {
                        (Some((hash, template)), _) => Some(FillTemplate(
                                template, guild_id, user_id, hash,
                        )),
                        (None, Some(hash)) => Some(FillTemplate(
                                &self._avatarUrl,
                                guild_id,
                                user_id,
                                hash,
                        )),
                        (None, None) => None,
                };
                UserProfile {
                        _name: Truncate(name, NICKNAME_MAX),
                        _avatarUrl: avatar_url,
                }
        }
}

// ============================================================================
// Helper Functions
// ============================================================================

// Wait for the time in a rate limit response, if the time is short.
async fn WaitForRateLimit(body: &Value, path: &str) -> Result<()> {
        let seconds = body["retry_after"].as_f64().unwrap_or(1.0).max(0.0);
        let wait = Duration::from_secs_f64(seconds);
        if wait > RATE_LIMIT_MAX_WAIT {
                bail!("{path} is rate limited for {seconds:.0} s");
        }
        warn!(path, wait_ms = wait.as_millis() as u64, "rate limited, waiting");
        tokio::time::sleep(wait).await;
        Ok(())
}

// Return the image type from the response. Use PNG for a type that is not
// an image, so that the data URI always contains an image type.
fn FindImageType(response: &reqwest::Response) -> String {
        response.headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next().unwrap_or_default().trim())
                .filter(|v| IsImageType(v))
                .unwrap_or(AVATAR_DEFAULT_TYPE)
                .to_string()
}

fn IsImageType(kind: &str) -> bool {
        kind.strip_prefix("image/").is_some_and(|sub| {
                !sub.is_empty()
                        && sub.chars().all(|c| {
                                c.is_ascii_alphanumeric() || "+-.".contains(c)
                        })
        })
}

// Put the IDs and avatar hash into an avatar URL template.
fn FillTemplate(
        template: &str,
        guild_id: &str,
        user_id: &str,
        hash: &str,
) -> String {
        template.replace("{guild_id}", guild_id)
                .replace("{user_id}", user_id)
                .replace("{hash}", hash)
}

// Cut a text to a maximum number of characters.
pub fn Truncate(text: &str, max: usize) -> String {
        text.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn TemplateGetsAllValues() {
                let url = FillTemplate(
                        "x/{guild_id}/{user_id}/{hash}.png",
                        "g",
                        "u",
                        "h",
                );
                assert_eq!(url, "x/g/u/h.png");
        }

        #[test]
        fn OnlyImageTypesAreAccepted() {
                assert!(IsImageType("image/png"));
                assert!(IsImageType("image/svg+xml"));
                assert!(!IsImageType("text/html"));
                assert!(!IsImageType("image/"));
                assert!(!IsImageType("image/png,base64"));
        }

        #[test]
        fn TruncateCountsCharacters() {
                assert_eq!(Truncate("ÄÖÜabc", 4), "ÄÖÜa");
        }
}
