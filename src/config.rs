// ============================================================================
// ============================================================================
// Load config.toml, fill in default values, and make sure that the settings
// are valid before the bridge starts.
// ============================================================================
// ============================================================================

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde::de::IgnoredAny;
use tracing::warn;

use crate::platform::{PLATFORMS, Platform};

// ============================================================================
// Default Values
// ============================================================================

const DISCORD_API_URL: &str = "https://discord.com/api/v10";
const DISCORD_GATEWAY_URL: &str =
        "wss://gateway.discord.gg/?v=10&encoding=json";
const DISCORD_AVATAR_URL: &str =
        "https://cdn.discordapp.com/avatars/{user_id}/{hash}.png?size=128";
const DISCORD_GUILD_AVATAR_URL: &str = "https://cdn.discordapp.com/guilds/\
        {guild_id}/users/{user_id}/avatars/{hash}.png?size=128";
// Fluxer runs on the same machine, so the defaults use the local edge port
// and do not go through Cloudflare or nginx.
const FLUXER_API_URL: &str = "http://127.0.0.1:8080/api/v1";
const FLUXER_GATEWAY_URL: &str =
        "ws://127.0.0.1:8080/gateway?v=1&encoding=json";
const FLUXER_LIVEKIT_URL: &str = "ws://127.0.0.1:8080/livekit";
const FLUXER_AVATAR_URL: &str =
        "http://127.0.0.1:8080/media/avatars/{user_id}/{hash}.png?size=128";

const DEFAULT_PREFIX: &str = "vp!";
const DEFAULT_OVERFLOW_NAME: &str = "Multiple Users";
const DEFAULT_PROXY_IDLE_NAME: &str = "Voice Pipe Proxy {number}";
const DEFAULT_LOG_LEVEL: &str = "info";
const DEFAULT_STATE_FILE: &str = "voice-pipe-state.toml";
const DEFAULT_HANGOVER_MS: u64 = 300;
const DEFAULT_JITTER_FRAMES: usize = 3;
// Shortest hangover. Below one 20 ms frame, playback stops after each frame.
const HANGOVER_MIN_MS: u64 = 20;
// Hosts where a plain (not encrypted) URL does not expose the tokens.
const LOCAL_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

// ============================================================================
// Settings Types
// ============================================================================

// Settings that control the whole bridge.
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GeneralSettings {
        #[serde(rename = "command_prefix")]
        _commandPrefix: String,
        #[serde(rename = "start_enabled")]
        _isStartEnabled: bool,
        #[serde(rename = "state_file")]
        _stateFile: String,
        #[serde(rename = "match_by_name")]
        _isNameMatchEnabled: bool,
        #[serde(rename = "ignore_afk_channels")]
        _isAfkIgnored: bool,
        #[serde(rename = "overflow_name")]
        _overflowName: String,
        #[serde(rename = "proxy_idle_name")]
        _proxyIdleName: String,
        #[serde(rename = "log_level")]
        _logLevel: String,
        // Removed setting. It is still read, so that an old config file
        // loads, and Validate tells the operator to remove it.
        #[serde(rename = "voice_threshold")]
        _removedVoiceThreshold: Option<IgnoredAny>,
        #[serde(rename = "silence_hangover_ms")]
        _silenceHangoverMs: u64,
        #[serde(rename = "jitter_frames")]
        _jitterFrames: usize,
}

// Settings for the bots and the server on one platform.
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct PlatformSettings {
        #[serde(rename = "guild_id")]
        _guildId: String,
        #[serde(rename = "main_token")]
        _mainToken: String,
        #[serde(rename = "proxy_tokens")]
        _proxyTokens: Vec<String>,
        #[serde(rename = "api_url")]
        _apiUrl: String,
        #[serde(rename = "gateway_url")]
        _gatewayUrl: String,
        #[serde(rename = "livekit_url")]
        _livekitUrl: Option<String>,
        #[serde(rename = "avatar_url")]
        _avatarUrl: String,
        #[serde(rename = "guild_avatar_url")]
        _guildAvatarUrl: String,
        #[serde(rename = "host_header")]
        _hostHeader: String,
        #[serde(rename = "command_role_ids")]
        _commandRoleIds: Vec<String>,
}

// One fixed pair of voice channels to bridge.
#[derive(Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct LinkSettings {
        #[serde(rename = "discord_channel_id")]
        _discordChannelId: String,
        #[serde(rename = "fluxer_channel_id")]
        _fluxerChannelId: String,
}

// All settings from config.toml.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
        #[serde(rename = "general", default)]
        _general: GeneralSettings,
        #[serde(rename = "discord")]
        _discord: PlatformSettings,
        #[serde(rename = "fluxer")]
        _fluxer: PlatformSettings,
        #[serde(rename = "links", default)]
        _links: Vec<LinkSettings>,
}

// ============================================================================
// General Settings
// ============================================================================

impl GeneralSettings {
        pub fn getCommandPrefix(&self) -> &str { &self._commandPrefix }
        pub fn isStartEnabled(&self) -> bool { self._isStartEnabled }

        // Return the file that keeps the switch state after a restart, or
        // None when the state is not saved.
        pub fn getStateFile(&self) -> Option<PathBuf> {
                let path = self._stateFile.trim();
                (!path.is_empty()).then(|| PathBuf::from(path))
        }
        pub fn isNameMatchEnabled(&self) -> bool { self._isNameMatchEnabled }
        pub fn isAfkIgnored(&self) -> bool { self._isAfkIgnored }
        pub fn getOverflowName(&self) -> &str { &self._overflowName }
        pub fn getLogLevel(&self) -> &str { &self._logLevel }
        pub fn getJitterFrames(&self) -> usize { self._jitterFrames }

        pub fn getSilenceHangover(&self) -> Duration {
                Duration::from_millis(self._silenceHangoverMs)
        }

        // Return the nickname of a free proxy. `number` is the position of
        // its token in proxy_tokens, from 1. None removes the nickname.
        pub fn getProxyIdleName(&self, number: usize) -> Option<String> {
                let number = number.to_string();
                let name = self._proxyIdleName.replace("{number}", &number);
                Some(name).filter(|n| !n.trim().is_empty())
        }

        // Correct values that are out of range, and tell the operator.
        fn Validate(&mut self) -> Result<()> {
                if self._commandPrefix.trim().is_empty() {
                        bail!("general.command_prefix must not be empty");
                }
                if self._removedVoiceThreshold.is_some() {
                        warn!(
                                "general.voice_threshold is no longer used, \
                                 remove it from the config"
                        );
                }
                if self._silenceHangoverMs < HANGOVER_MIN_MS {
                        warn!(
                                "general.silence_hangover_ms is below \
                                 {HANGOVER_MIN_MS}, using {HANGOVER_MIN_MS}"
                        );
                        self._silenceHangoverMs = HANGOVER_MIN_MS;
                }
                if self._jitterFrames == 0 {
                        warn!("general.jitter_frames is 0, using 1");
                        self._jitterFrames = 1;
                }
                Ok(())
        }
}

impl Default for GeneralSettings {
        fn default() -> Self {
                Self {
                        _commandPrefix: DEFAULT_PREFIX.into(),
                        _isStartEnabled: true,
                        _stateFile: DEFAULT_STATE_FILE.into(),
                        _isNameMatchEnabled: true,
                        _isAfkIgnored: true,
                        _overflowName: DEFAULT_OVERFLOW_NAME.into(),
                        _proxyIdleName: DEFAULT_PROXY_IDLE_NAME.into(),
                        _logLevel: DEFAULT_LOG_LEVEL.into(),
                        _removedVoiceThreshold: None,
                        _silenceHangoverMs: DEFAULT_HANGOVER_MS,
                        _jitterFrames: DEFAULT_JITTER_FRAMES,
                }
        }
}

// ============================================================================
// Platform Settings
// ============================================================================

impl PlatformSettings {
        pub fn getGuildId(&self) -> &str { &self._guildId }
        pub fn getMainToken(&self) -> &str { &self._mainToken }
        pub fn getProxyTokens(&self) -> &[String] { &self._proxyTokens }
        pub fn getApiUrl(&self) -> &str { &self._apiUrl }
        pub fn getGatewayUrl(&self) -> &str { &self._gatewayUrl }
        pub fn getAvatarUrl(&self) -> &str { &self._avatarUrl }
        pub fn getCommandRoleIds(&self) -> &[String] { &self._commandRoleIds }

        // Return the LiveKit URL to use instead of the URL that the server
        // sends. Return None to use the URL from the server.
        pub fn getLivekitUrl(&self) -> Option<&str> {
                self._livekitUrl.as_deref().filter(|u| !u.is_empty())
        }

        pub fn getGuildAvatarUrl(&self) -> Option<&str> {
                Some(self._guildAvatarUrl.as_str()).filter(|u| !u.is_empty())
        }

        pub fn getHostHeader(&self) -> Option<&str> {
                Some(self._hostHeader.as_str()).filter(|h| !h.is_empty())
        }

        // Fill empty values with the defaults for the platform.
        fn ApplyDefaults(&mut self, platform: Platform) {
                let (api, gateway, avatar, guild_avatar) = match platform {
                        Platform::Discord => (
                                DISCORD_API_URL,
                                DISCORD_GATEWAY_URL,
                                DISCORD_AVATAR_URL,
                                DISCORD_GUILD_AVATAR_URL,
                        ),
                        Platform::Fluxer => (
                                FLUXER_API_URL,
                                FLUXER_GATEWAY_URL,
                                FLUXER_AVATAR_URL,
                                "",
                        ),
                };
                FillEmpty(&mut self._apiUrl, api);
                FillEmpty(&mut self._gatewayUrl, gateway);
                FillEmpty(&mut self._avatarUrl, avatar);
                FillEmpty(&mut self._guildAvatarUrl, guild_avatar);
                if platform == Platform::Fluxer && self._livekitUrl.is_none() {
                        self._livekitUrl = Some(FLUXER_LIVEKIT_URL.into());
                }
                self._apiUrl = self._apiUrl.trim_end_matches('/').to_string();
        }

        // Make sure that the bot can log in, and clean the token list.
        fn Validate(&mut self, platform: Platform) -> Result<()> {
                let label = platform.getLabel();
                if self._guildId.trim().is_empty() {
                        bail!("{label}.guild_id must be set");
                }
                self._mainToken = CleanToken(&self._mainToken);
                if self._mainToken.is_empty() {
                        bail!("{label}.main_token must be set");
                }
                self._proxyTokens = self.FindUniqueProxyTokens(label);
                if self._proxyTokens.is_empty() {
                        warn!("{label} has no proxy tokens");
                }
                if platform == Platform::Discord {
                        RequireNumber(&self._guildId, "discord.guild_id")?;
                        self.WarnOpenCommands();
                }
                WarnPlainUrl(&self._apiUrl, &format!("{label}.api_url"));
                WarnPlainUrl(
                        &self._gatewayUrl,
                        &format!("{label}.gateway_url"),
                );
                Ok(())
        }

        // Without command roles, every member of the guild can turn the
        // bridge off.
        fn WarnOpenCommands(&self) {
                if !self._commandRoleIds.is_empty() { return; }
                warn!(
                        "discord.command_role_ids is empty, so every member \
                         can use the toggle command"
                );
        }

        // Remove empty and repeated proxy tokens, and the main token.
        fn FindUniqueProxyTokens(&self, label: &str) -> Vec<String> {
                let mut seen = HashSet::from([self._mainToken.clone()]);
                let mut tokens = Vec::new();
                for raw in &self._proxyTokens {
                        let token = CleanToken(raw);
                        if token.is_empty() { continue; }
                        if !seen.insert(token.clone()) {
                                warn!("{label}.proxy_tokens repeats a token");
                                continue;
                        }
                        tokens.push(token);
                }
                tokens
        }
}

// Read the channel IDs of one fixed link.
impl LinkSettings {
        pub fn getChannelId(&self, platform: Platform) -> &str {
                match platform {
                        Platform::Discord => &self._discordChannelId,
                        Platform::Fluxer => &self._fluxerChannelId,
                }
        }
}

// ============================================================================
// Config
// ============================================================================

impl Config {
        pub fn getGeneral(&self) -> &GeneralSettings { &self._general }
        pub fn getLinks(&self) -> &[LinkSettings] { &self._links }

        pub fn getPlatform(&self, platform: Platform) -> &PlatformSettings {
                match platform {
                        Platform::Discord => &self._discord,
                        Platform::Fluxer => &self._fluxer,
                }
        }

        // Read the config file. Call Prepare before the config is used.
        pub fn Read(path: &Path) -> Result<Config> {
                let text = fs::read_to_string(path).with_context(|| {
                        let name = path.display();
                        format!("cannot read {name}, copy config.example.toml")
                })?;
                toml::from_str(&text).with_context(|| {
                        format!("{} is not valid", path.display())
                })
        }

        fn getPlatformMut(
                &mut self,
                platform: Platform,
        ) -> &mut PlatformSettings {
                match platform {
                        Platform::Discord => &mut self._discord,
                        Platform::Fluxer => &mut self._fluxer,
                }
        }

        // Apply defaults and validate each part of the config.
        pub fn Prepare(&mut self) -> Result<()> {
                self._general.Validate()?;
                for platform in PLATFORMS {
                        let settings = self.getPlatformMut(platform);
                        settings.ApplyDefaults(platform);
                        settings.Validate(platform)?;
                }
                for (i, link) in self._links.iter().enumerate() {
                        let name = format!("links[{i}].discord_channel_id");
                        RequireNumber(&link._discordChannelId, &name)?;
                        if link._fluxerChannelId.trim().is_empty() {
                                bail!("links[{i}].fluxer_channel_id is empty");
                        }
                }
                Ok(())
        }
}

// ============================================================================
// Helper Functions
// ============================================================================

fn FillEmpty(value: &mut String, default: &str) {
        if value.trim().is_empty() { *value = default.to_string(); }
}

// Remove spaces and a "Bot " prefix, because the bridge adds that prefix.
fn CleanToken(token: &str) -> String {
        let trimmed = token.trim();
        trimmed.strip_prefix("Bot ").unwrap_or(trimmed).trim().to_string()
}

// Warn when a URL sends the tokens without encryption to another machine.
fn WarnPlainUrl(url: &str, name: &str) {
        if !isPlainRemoteUrl(url) { return; }
        warn!("{name} is not encrypted and not local, tokens are visible");
}

fn isPlainRemoteUrl(url: &str) -> bool {
        let Some(rest) = url
                .strip_prefix("http://")
                .or_else(|| url.strip_prefix("ws://"))
        else {
                return false;
        };
        let authority = rest.split(['/', '?']).next().unwrap_or_default();
        let host = match authority.rsplit_once(':') {
                Some((host, port)) if !port.contains(']') => host,
                _ => authority,
        };
        !LOCAL_HOSTS.contains(&host.to_ascii_lowercase().as_str())
}

// Discord IDs must be numbers, because the voice library uses u64 values.
fn RequireNumber(value: &str, name: &str) -> Result<()> {
        if value.trim().parse::<u64>().is_err() {
                bail!("{name} must be a number, found \"{value}\"");
        }
        Ok(())
}

#[cfg(test)]
mod tests {
        use super::*;

        const MINIMAL: &str = r#"
                [discord]
                guild_id = "123"
                main_token = "Bot abc"
                proxy_tokens = ["p1", "p1", "", "abc"]

                [fluxer]
                guild_id = "456"
                main_token = "xyz"
        "#;

        fn Parse(text: &str) -> Config {
                let mut config: Config = toml::from_str(text).unwrap();
                config.Prepare().unwrap();
                config
        }

        #[test]
        fn DefaultsPointFluxerAtLocalHost() {
                let config = Parse(MINIMAL);
                let fluxer = config.getPlatform(Platform::Fluxer);
                assert!(fluxer.getApiUrl().starts_with("http://127.0.0.1"));
                assert_eq!(fluxer.getLivekitUrl(), Some(FLUXER_LIVEKIT_URL));
        }

        #[test]
        fn TokensAreCleanAndUnique() {
                let config = Parse(MINIMAL);
                let discord = config.getPlatform(Platform::Discord);
                assert_eq!(discord.getMainToken(), "abc");
                assert_eq!(discord.getProxyTokens(), ["p1".to_string()]);
        }

        #[test]
        fn ExampleConfigIsValid() {
                let text = include_str!("../config.example.toml")
                        .replace("guild_id = \"\"", "guild_id = \"1\"")
                        .replace("main_token = \"\"", "main_token = \"t\"");
                let config = Parse(&text);
                assert_eq!(config.getGeneral().getCommandPrefix(), "vp!");
        }

        #[test]
        fn IdleNameGetsNumber() {
                let general = GeneralSettings::default();
                let name = general.getProxyIdleName(2);
                assert_eq!(name.as_deref(), Some("Voice Pipe Proxy 2"));
        }

        #[test]
        fn PlainUrlIsRemoteOnlyOffLocalHost() {
                assert!(!isPlainRemoteUrl("http://127.0.0.1:8080/api/v1"));
                assert!(!isPlainRemoteUrl("ws://localhost/gateway"));
                assert!(!isPlainRemoteUrl("ws://[::1]:8080/gateway"));
                assert!(!isPlainRemoteUrl("https://chat.example.com/api"));
                assert!(isPlainRemoteUrl("http://chat.example.com/api"));
                assert!(isPlainRemoteUrl("ws://10.0.0.5:8080/gateway"));
        }

        #[test]
        fn ShortHangoverIsRaised() {
                let text = MINIMAL.replace(
                        "[discord]",
                        "[general]\nsilence_hangover_ms = 0\n[discord]",
                );
                let config = Parse(&text);
                assert_eq!(
                        config.getGeneral().getSilenceHangover(),
                        Duration::from_millis(HANGOVER_MIN_MS)
                );
        }

        #[test]
        fn OldVoiceThresholdStillLoads() {
                let text = MINIMAL.replace(
                        "[discord]",
                        "[general]\nvoice_threshold = 200\n[discord]",
                );
                Parse(&text);
        }

        #[test]
        fn EmptyLivekitUrlUsesServerUrl() {
                let text = format!("{MINIMAL}\nlivekit_url = \"\"\n");
                let config = Parse(&text);
                assert_eq!(
                        config.getPlatform(Platform::Fluxer).getLivekitUrl(),
                        None
                );
        }
}
