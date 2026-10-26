// ============================================================================
// ============================================================================
// Define the gateway opcodes, intents, and payloads. Fluxer uses the same wire
// format as Discord, so one set of types works for both platforms.
// ============================================================================
// ============================================================================

// The field names of the payload types match the JSON keys of the gateway
// protocol, because these types are data contracts with the servers.

use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use super::state::VoiceFlags;
use crate::platform::Platform;

// ============================================================================
// Opcodes and Intents
// ============================================================================

pub const OP_DISPATCH: u8 = 0;
pub const OP_HEARTBEAT: u8 = 1;
pub const OP_IDENTIFY: u8 = 2;
pub const OP_PRESENCE_UPDATE: u8 = 3;
pub const OP_VOICE_STATE_UPDATE: u8 = 4;
pub const OP_RESUME: u8 = 6;
pub const OP_RECONNECT: u8 = 7;
pub const OP_INVALID_SESSION: u8 = 9;
pub const OP_HELLO: u8 = 10;
pub const OP_HEARTBEAT_ACK: u8 = 11;

pub const INTENT_GUILDS: u64 = 1 << 0;
pub const INTENT_GUILD_VOICE_STATES: u64 = 1 << 7;
pub const INTENT_GUILD_MESSAGES: u64 = 1 << 9;
// Privileged on Discord. Turn it on in the developer portal.
pub const INTENT_MESSAGE_CONTENT: u64 = 1 << 15;

// Discord channel types for voice and stage channels.
const VOICE_CHANNEL_TYPES: [u8; 2] = [2, 13];
// Longest custom status text on Fluxer (characters).
pub const STATUS_MAX: usize = 128;

// ============================================================================
// Incoming Payloads
// ============================================================================

// One frame from the gateway. The opcode decides which fields are present.
#[derive(Debug, Deserialize)]
pub struct Incoming {
        pub op: u8,
        #[serde(default)]
        pub d: Value,
        #[serde(default)]
        pub s: Option<u64>,
        #[serde(default)]
        pub t: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Hello {
        pub heartbeat_interval: u64,
}

#[derive(Debug, Deserialize)]
pub struct Ready {
        pub session_id: String,
        #[serde(default)]
        pub resume_gateway_url: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct UserData {
        pub id: String,
        #[serde(default)]
        pub bot: bool,
}

#[derive(Debug, Deserialize, Default)]
pub struct MemberData {
        #[serde(default)]
        pub roles: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChannelData {
        pub id: String,
        #[serde(default)]
        pub guild_id: Option<String>,
        #[serde(default)]
        pub name: Option<String>,
        // None when an update event does not include the type.
        #[serde(rename = "type", default)]
        pub kind: Option<u8>,
        #[serde(default)]
        pub position: Option<i64>,
}

// `mute` and `deaf` are set by a moderator. `self_mute` and `self_deaf` are
// set by the user. Only Fluxer sends `connection_id`, because one Fluxer user
// can have many voice connections.
#[derive(Debug, Default, Deserialize)]
pub struct VoiceStateData {
        #[serde(default)]
        pub guild_id: Option<String>,
        #[serde(default)]
        pub channel_id: Option<String>,
        #[serde(default)]
        pub user_id: Option<String>,
        #[serde(default)]
        pub session_id: Option<String>,
        #[serde(default)]
        pub connection_id: Option<String>,
        #[serde(default)]
        pub mute: bool,
        #[serde(default)]
        pub self_mute: bool,
        #[serde(default)]
        pub deaf: bool,
        #[serde(default)]
        pub self_deaf: bool,
}

#[derive(Debug, Deserialize)]
pub struct GuildCreate {
        pub id: String,
        #[serde(default)]
        pub name: Option<String>,
        #[serde(default)]
        pub afk_channel_id: Option<String>,
        #[serde(default)]
        pub channels: Vec<ChannelData>,
        #[serde(default)]
        pub voice_states: Vec<VoiceStateData>,
}

// Discord names the guild key "id". Fluxer names it "guild_id".
#[derive(Debug, Deserialize)]
pub struct GuildUpdate {
        #[serde(alias = "guild_id")]
        pub id: String,
        #[serde(default)]
        pub name: Option<String>,
        // None when the update does not include the key. Some(None) when
        // the guild removed its AFK channel.
        #[serde(default, deserialize_with = "ReadPresentValue")]
        pub afk_channel_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct VoiceServerData {
        pub token: String,
        #[serde(default)]
        pub guild_id: Option<String>,
        // Discord can send null while it selects a voice server.
        #[serde(default)]
        pub endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MessageData {
        pub id: String,
        pub channel_id: String,
        #[serde(default)]
        pub guild_id: Option<String>,
        #[serde(default)]
        pub content: String,
        #[serde(default)]
        pub author: UserData,
        #[serde(default)]
        pub member: Option<MemberData>,
}

impl ChannelData {
        pub fn isVoice(&self) -> bool {
                self.kind.is_some_and(|k| VOICE_CHANNEL_TYPES.contains(&k))
        }
}

// ============================================================================
// Outgoing Payloads
// ============================================================================

// Start a new session.
pub fn BuildIdentify(token: &str, intents: u64) -> String {
        let properties = json!({
                "os": std::env::consts::OS,
                "browser": "voice-pipe",
                "device": "voice-pipe",
        });
        let data = json!({
                "token": token,
                "intents": intents,
                "properties": properties,
        });
        json!({ "op": OP_IDENTIFY, "d": data }).to_string()
}

// Continue a session after the connection was lost.
pub fn BuildResume(
        token: &str,
        session_id: &str,
        sequence: Option<u64>,
) -> String {
        let data = json!({
                "token": token,
                "session_id": session_id,
                "seq": sequence,
        });
        json!({ "op": OP_RESUME, "d": data }).to_string()
}

pub fn BuildHeartbeat(sequence: Option<u64>) -> String {
        json!({ "op": OP_HEARTBEAT, "d": sequence }).to_string()
}

// Join a voice channel, or leave voice when `channel_id` is None. A second
// update for the same channel changes only the mute and deafen state. Fluxer
// needs `connection_id` to know which voice connection of the bot to change.
// Discord has no connection ID, so the key is sent only when it is known.
pub fn BuildVoiceState(
        guild_id: &str,
        channel_id: Option<&str>,
        flags: VoiceFlags,
        connection_id: Option<&str>,
) -> String {
        let mut data = json!({
                "guild_id": guild_id,
                "channel_id": channel_id,
                "self_mute": flags.isMuted(),
                "self_deaf": flags.isDeafened(),
        });
        if let Some(id) = connection_id {
                data["connection_id"] = json!(id);
        }
        json!({ "op": OP_VOICE_STATE_UPDATE, "d": data }).to_string()
}

// Show the bot as online with a custom status text. Discord sets it as an
// activity of type 4 (custom status). Fluxer has its own custom status
// object. Text longer than STATUS_MAX characters is cut.
pub fn BuildPresence(platform: Platform, text: &str) -> String {
        let text: String = text.chars().take(STATUS_MAX).collect();
        let data = match platform {
                Platform::Discord => json!({
                        "since": null,
                        "activities": [{
                                "name": "Custom Status",
                                "type": 4,
                                "state": text,
                        }],
                        "status": "online",
                        "afk": false,
                }),
                Platform::Fluxer => json!({
                        "status": "online",
                        "afk": false,
                        "mobile": false,
                        "custom_status": { "text": text },
                }),
        };
        json!({ "op": OP_PRESENCE_UPDATE, "d": data }).to_string()
}

// Tell a key that is missing (None, from the default) from a key that is
// null (Some(None)).
fn ReadPresentValue<'de, D: Deserializer<'de>>(
        deserializer: D,
) -> Result<Option<Option<String>>, D::Error> {
        Option::<String>::deserialize(deserializer).map(Some)
}

// Read the payload of a frame as a specific type.
pub fn ParseData<T: serde::de::DeserializeOwned>(data: &Value) -> Option<T> {
        T::deserialize(data).ok()
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn IdentifyHasDiscordShape() {
                let v: Value =
                        serde_json::from_str(&BuildIdentify("secret", 129))
                                .unwrap();
                assert_eq!(v["op"], 2);
                assert_eq!(v["d"]["token"], "secret");
                assert_eq!(v["d"]["intents"], 129);
        }

        #[test]
        fn LeaveSendsNullChannel() {
                let text =
                        BuildVoiceState("g", None, VoiceFlags::default(), None);
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["op"], 4);
                assert!(v["d"]["channel_id"].is_null());
                assert!(v["d"].get("connection_id").is_none());
        }

        #[test]
        fn MuteUpdateCarriesConnectionId() {
                let muted = VoiceFlags::New(true, false);
                let text = BuildVoiceState("g", Some("c"), muted, Some("x1"));
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["d"]["self_mute"], true);
                assert_eq!(v["d"]["self_deaf"], false);
                assert_eq!(v["d"]["connection_id"], "x1");
        }

        #[test]
        fn DiscordPresenceIsCustomActivity() {
                let text = BuildPresence(Platform::Discord, "Standing By");
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["op"], 3);
                assert_eq!(v["d"]["status"], "online");
                assert_eq!(v["d"]["activities"][0]["type"], 4);
                assert_eq!(v["d"]["activities"][0]["state"], "Standing By");
        }

        #[test]
        fn FluxerPresenceHasCustomStatus() {
                let long = "x".repeat(200);
                let text = BuildPresence(Platform::Fluxer, &long);
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["op"], 3);
                assert_eq!(v["d"]["status"], "online");
                let status = v["d"]["custom_status"]["text"].as_str().unwrap();
                assert_eq!(status.chars().count(), STATUS_MAX);
        }

        #[test]
        fn DeafenUpdateSetsSelfDeaf() {
                let deafened = VoiceFlags::New(true, true);
                let text = BuildVoiceState("g", Some("c"), deafened, None);
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["d"]["self_mute"], true);
                assert_eq!(v["d"]["self_deaf"], true);
        }

        #[test]
        fn GuildUpdateTellsMissingFromNull() {
                let missing: GuildUpdate =
                        serde_json::from_str(r#"{"id":"g"}"#).unwrap();
                assert_eq!(missing.afk_channel_id, None);
                let text = r#"{"guild_id":"g","afk_channel_id":null}"#;
                let removed: GuildUpdate = serde_json::from_str(text).unwrap();
                assert_eq!(removed.id, "g");
                assert_eq!(removed.afk_channel_id, Some(None));
        }

        #[test]
        fn IncomingAcceptsMissingFields() {
                let text = r#"{"op":10,"d":{"heartbeat_interval":1000}}"#;
                let frame: Incoming = serde_json::from_str(text).unwrap();
                let hello: Hello = ParseData(&frame.d).unwrap();
                assert_eq!(hello.heartbeat_interval, 1000);
                assert!(frame.t.is_none());
        }
}
