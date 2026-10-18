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

// ============================================================================
// Opcodes and Intents
// ============================================================================

pub const OP_DISPATCH: u8 = 0;
pub const OP_HEARTBEAT: u8 = 1;
pub const OP_IDENTIFY: u8 = 2;
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
// set by the user.
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
        pub mute: bool,
        #[serde(default)]
        pub deaf: bool,
        #[serde(default)]
        pub self_mute: bool,
        #[serde(default)]
        pub self_deaf: bool,
}

#[derive(Debug, Deserialize)]
pub struct GuildCreate {
        pub id: String,
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
// update for the same channel changes only the mute and deafen state.
pub fn BuildVoiceState(
        guild_id: &str,
        channel_id: Option<&str>,
        is_muted: bool,
        is_deafened: bool,
) -> String {
        let data = json!({
                "guild_id": guild_id,
                "channel_id": channel_id,
                "self_mute": is_muted,
                "self_deaf": is_deafened,
        });
        json!({ "op": OP_VOICE_STATE_UPDATE, "d": data }).to_string()
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
                let text = BuildVoiceState("g", None, false, false);
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["op"], 4);
                assert!(v["d"]["channel_id"].is_null());
        }

        #[test]
        fn VoiceStateCarriesFlags() {
                let text = BuildVoiceState("g", Some("c"), true, false);
                let v: Value = serde_json::from_str(&text).unwrap();
                assert_eq!(v["d"]["self_mute"], true);
                assert_eq!(v["d"]["self_deaf"], false);
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
