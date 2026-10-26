// ============================================================================
// ============================================================================
// Keep a live view of the voice channels in each guild and of the users that
// are in them, from the events that the gateway sends.
// ============================================================================
// ============================================================================

use std::collections::HashMap;

use super::protocol::{ChannelData, GuildCreate, GuildUpdate, VoiceStateData};

// A voice channel, its name, and its position in the channel list.
#[derive(Clone, Debug)]
pub struct VoiceChannel {
        _id: String,
        _idNumber: u128,
        _name: String,
        _position: i64,
}

// The voice state of a user that a proxy shows. The user counts as muted
// or deafened when the user or a moderator did it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VoiceFlags {
        _isMuted: bool,
        _isDeafened: bool,
}

// The voice channel of a user and their mute and deafen state.
struct UserVoice {
        _channelId: String,
        _flags: VoiceFlags,
}

// The voice channels of one guild and the voice state of each user in
// voice. Text channels are never stored.
#[derive(Default)]
struct GuildState {
        _name: Option<String>,
        _channels: HashMap<String, VoiceChannel>,
        _userVoices: HashMap<String, UserVoice>,
        _afkChannelId: Option<String>,
}

// The voice view of all guilds that one bot can see.
#[derive(Default)]
pub struct GatewayState {
        _guilds: HashMap<String, GuildState>,
}

impl VoiceChannel {
        pub fn getId(&self) -> &str { &self._id }
        pub fn getName(&self) -> &str { &self._name }

        pub fn New(id: &str, name: &str, position: i64) -> Self {
                Self {
                        _id: id.to_string(),
                        _idNumber: id.parse().unwrap_or(u128::MAX),
                        _name: name.to_string(),
                        _position: position,
                }
        }

        fn FromData(data: &ChannelData) -> Self {
                let name = data.name.as_deref().unwrap_or_default();
                Self::New(&data.id, name, data.position.unwrap_or_default())
        }

        // Order of the channel list in the app: position, then age. Older
        // IDs are smaller numbers.
        fn getSortKey(&self) -> (i64, u128, &str) {
                (self._position, self._idNumber, &self._id)
        }
}

impl VoiceFlags {
        pub fn isMuted(&self) -> bool { self._isMuted }
        pub fn isDeafened(&self) -> bool { self._isDeafened }

        pub fn New(is_muted: bool, is_deafened: bool) -> Self {
                Self { _isMuted: is_muted, _isDeafened: is_deafened }
        }

        fn FromData(data: &VoiceStateData) -> Self {
                Self::New(
                        data.mute || data.self_mute,
                        data.deaf || data.self_deaf,
                )
        }
}

impl UserVoice {
        fn FromData(channel_id: &str, data: &VoiceStateData) -> Self {
                Self {
                        _channelId: channel_id.to_string(),
                        _flags: VoiceFlags::FromData(data),
                }
        }
}

impl GuildState {
        fn RenameChannel(&mut self, data: &ChannelData) {
                let Some(channel) = self._channels.get_mut(&data.id) else {
                        return;
                };
                if let Some(name) = &data.name { channel._name = name.clone(); }
                if let Some(pos) = data.position { channel._position = pos; }
        }
}

impl GatewayState {
        pub fn hasGuild(&self, guild_id: &str) -> bool {
                self._guilds.contains_key(guild_id)
        }

        // Return the name of a guild, if the server sent it.
        pub fn getGuildName(&self, guild_id: &str) -> Option<String> {
                self._guilds.get(guild_id)?._name.clone()
        }

        // Return the AFK voice channel of a guild, if the guild has one.
        pub fn getAfkChannelId(&self, guild_id: &str) -> Option<String> {
                self._guilds.get(guild_id)?._afkChannelId.clone()
        }

        // Return the voice channels in the order of the channel list, so
        // that "the first channel" is always the same channel.
        pub fn getVoiceChannels(&self, guild_id: &str) -> Vec<VoiceChannel> {
                let Some(guild) = self._guilds.get(guild_id) else {
                        return Vec::new();
                };
                let mut channels: Vec<VoiceChannel> =
                        guild._channels.values().cloned().collect();
                channels.sort_by(|a, b| a.getSortKey().cmp(&b.getSortKey()));
                channels
        }

        // Return the voice channel that a user is in, if any.
        pub fn getUserChannel(
                &self,
                guild_id: &str,
                user: &str,
        ) -> Option<String> {
                let guild = self._guilds.get(guild_id)?;
                Some(guild._userVoices.get(user)?._channelId.clone())
        }

        // Return all users in a voice channel (bots included), with their
        // mute state.
        pub fn getChannelUsers(
                &self,
                guild_id: &str,
                channel_id: &str,
        ) -> HashMap<String, VoiceFlags> {
                let Some(guild) = self._guilds.get(guild_id) else {
                        return HashMap::new();
                };
                guild._userVoices
                        .iter()
                        .filter(|(_, voice)| voice._channelId == channel_id)
                        .map(|(user, voice)| (user.clone(), voice._flags))
                        .collect()
        }

        // Replace the view of a guild with the full snapshot from the server.
        pub fn ApplyGuildCreate(&mut self, data: &GuildCreate) {
                let mut guild = GuildState {
                        _name: data.name.clone(),
                        _afkChannelId: data.afk_channel_id.clone(),
                        ..Default::default()
                };
                for channel in data.channels.iter().filter(|c| c.isVoice()) {
                        let voice = VoiceChannel::FromData(channel);
                        guild._channels.insert(channel.id.clone(), voice);
                }
                for state in &data.voice_states {
                        let (Some(user), Some(channel)) =
                                (&state.user_id, &state.channel_id)
                        else {
                                continue;
                        };
                        let voice = UserVoice::FromData(channel, state);
                        guild._userVoices.insert(user.clone(), voice);
                }
                self._guilds.insert(data.id.clone(), guild);
        }

        // Change the name and AFK channel of a known guild. An update
        // without a key keeps the old value. An update for an unknown guild
        // does nothing, because only the snapshot can add a guild.
        pub fn ApplyGuildUpdate(&mut self, data: &GuildUpdate) {
                let Some(guild) = self._guilds.get_mut(&data.id) else {
                        return;
                };
                if let Some(name) = &data.name {
                        guild._name = Some(name.clone());
                }
                if let Some(afk) = &data.afk_channel_id {
                        guild._afkChannelId = afk.clone();
                }
        }

        // Move a user to a channel and record their mute state,
        // or take them out of voice when the channel is null.
        pub fn ApplyVoiceState(&mut self, data: &VoiceStateData) {
                let (Some(guild_id), Some(user)) =
                        (&data.guild_id, &data.user_id)
                else {
                        return;
                };
                let guild = self._guilds.entry(guild_id.clone()).or_default();
                let Some(channel) = &data.channel_id else {
                        guild._userVoices.remove(user);
                        return;
                };
                let voice = UserVoice::FromData(channel, data);
                guild._userVoices.insert(user.clone(), voice);
        }

        // Add or change a voice channel. An update without a type is partial,
        // so it can only rename a known voice channel. It must not remove
        // the channel, because that would stop its link.
        pub fn ApplyChannelUpdate(&mut self, data: &ChannelData) {
                let Some(guild_id) = &data.guild_id else { return };
                let guild = self._guilds.entry(guild_id.clone()).or_default();
                if data.kind.is_none() {
                        guild.RenameChannel(data);
                        return;
                }
                if !data.isVoice() {
                        guild._channels.remove(&data.id);
                        return;
                }
                let channel = VoiceChannel::FromData(data);
                guild._channels.insert(data.id.clone(), channel);
        }

        pub fn ApplyChannelDelete(&mut self, data: &ChannelData) {
                let Some(guild_id) = &data.guild_id else { return };
                let Some(guild) = self._guilds.get_mut(guild_id) else {
                        return;
                };
                guild._channels.remove(&data.id);
                let voices = &mut guild._userVoices;
                voices.retain(|_, voice| voice._channelId != data.id);
        }
}

#[cfg(test)]
mod tests {
        use serde_json::json;

        use super::*;

        fn VoiceState(user: &str, channel: Option<&str>) -> VoiceStateData {
                VoiceStateData {
                        guild_id: Some("g".into()),
                        channel_id: channel.map(String::from),
                        user_id: Some(user.into()),
                        ..Default::default()
                }
        }

        #[test]
        fn UsersMoveBetweenChannels() {
                let mut state = GatewayState::default();
                state.ApplyVoiceState(&VoiceState("u1", Some("c1")));
                state.ApplyVoiceState(&VoiceState("u1", Some("c2")));
                assert!(state.getChannelUsers("g", "c1").is_empty());
                assert!(state.getChannelUsers("g", "c2").contains_key("u1"));
                state.ApplyVoiceState(&VoiceState("u1", None));
                assert!(state.getChannelUsers("g", "c2").is_empty());
        }

        #[test]
        fn VoiceStateRecordsMute() {
                let mut state = GatewayState::default();
                let muted = VoiceStateData {
                        mute: true,
                        ..VoiceState("u1", Some("c1"))
                };
                state.ApplyVoiceState(&muted);
                let users = state.getChannelUsers("g", "c1");
                assert_eq!(users["u1"], VoiceFlags::New(true, false));
                state.ApplyVoiceState(&VoiceState("u1", Some("c1")));
                let users = state.getChannelUsers("g", "c1");
                assert_eq!(users["u1"], VoiceFlags::default());
        }

        #[test]
        fn VoiceStateRecordsDeafen() {
                let mut state = GatewayState::default();
                let deafened = VoiceStateData {
                        self_mute: true,
                        self_deaf: true,
                        ..VoiceState("u1", Some("c1"))
                };
                state.ApplyVoiceState(&deafened);
                let users = state.getChannelUsers("g", "c1");
                assert_eq!(users["u1"], VoiceFlags::New(true, true));
                let moderated = VoiceStateData {
                        deaf: true,
                        ..VoiceState("u1", Some("c1"))
                };
                state.ApplyVoiceState(&moderated);
                let users = state.getChannelUsers("g", "c1");
                assert!(users["u1"].isDeafened());
        }

        #[test]
        fn GuildCreateKeepsOnlyVoiceChannels() {
                let text = r#"{"id":"g","channels":[
                        {"id":"c1","name":"General","type":2},
                        {"id":"c2","name":"chat","type":0}],
                        "voice_states":[{"user_id":"u1","channel_id":"c1"}]}"#;
                let data: GuildCreate = serde_json::from_str(text).unwrap();
                let mut state = GatewayState::default();
                state.ApplyGuildCreate(&data);
                assert_eq!(state.getVoiceChannels("g").len(), 1);
                assert!(state.getChannelUsers("g", "c1").contains_key("u1"));
        }

        fn Channel(value: serde_json::Value) -> ChannelData {
                serde_json::from_value(value).unwrap()
        }

        fn StateWithGeneral() -> GatewayState {
                let mut state = GatewayState::default();
                let voice =
                        json!({"id": "1", "guild_id": "g", "name": "General",
                               "type": 2});
                state.ApplyChannelUpdate(&Channel(voice));
                state
        }

        #[test]
        fn TextChannelWithSameNameIsIgnored() {
                let mut state = StateWithGeneral();
                let text =
                        json!({"id": "2", "guild_id": "g", "name": "general",
                               "type": 0});
                state.ApplyChannelUpdate(&Channel(text));
                let channels = state.getVoiceChannels("g");
                assert_eq!(channels.len(), 1);
                assert_eq!(channels[0].getId(), "1");
        }

        #[test]
        fn UpdateWithoutTypeDoesNotRemoveChannel() {
                let mut state = StateWithGeneral();
                let update = json!({"id": "1", "guild_id": "g"});
                state.ApplyChannelUpdate(&Channel(update));
                assert_eq!(state.getVoiceChannels("g")[0].getName(), "General");
        }

        #[test]
        fn GuildUpdateChangesAfkChannel() {
                let mut state = GatewayState::default();
                let snapshot: GuildCreate =
                        serde_json::from_value(json!({"id": "g"})).unwrap();
                state.ApplyGuildCreate(&snapshot);
                let update = |value| -> GuildUpdate {
                        serde_json::from_value(value).unwrap()
                };
                state.ApplyGuildUpdate(&update(
                        json!({"guild_id": "g", "afk_channel_id": "9"}),
                ));
                assert_eq!(state.getAfkChannelId("g").as_deref(), Some("9"));
                state.ApplyGuildUpdate(&update(json!({"id": "g"})));
                assert_eq!(state.getAfkChannelId("g").as_deref(), Some("9"));
                state.ApplyGuildUpdate(&update(
                        json!({"id": "g", "afk_channel_id": null}),
                ));
                assert_eq!(state.getAfkChannelId("g"), None);
                state.ApplyGuildUpdate(&update(json!({"id": "other"})));
                assert!(!state.hasGuild("other"));
        }

        #[test]
        fn GuildNameComesFromSnapshotAndUpdate() {
                let mut state = GatewayState::default();
                let snapshot: GuildCreate = serde_json::from_value(
                        json!({"id": "g", "name": "Lounge"}),
                )
                .unwrap();
                state.ApplyGuildCreate(&snapshot);
                assert_eq!(state.getGuildName("g").as_deref(), Some("Lounge"));
                let update: GuildUpdate = serde_json::from_value(
                        json!({"guild_id": "g", "name": "Den"}),
                )
                .unwrap();
                state.ApplyGuildUpdate(&update);
                assert_eq!(state.getGuildName("g").as_deref(), Some("Den"));
        }

        #[test]
        fn ChannelsAreInListOrder() {
                let mut state = GatewayState::default();
                for (id, position) in [("30", 2), ("20", 1), ("10", 1)] {
                        let channel = json!({"id": id, "guild_id": "g",
                                             "type": 2, "position": position});
                        state.ApplyChannelUpdate(&Channel(channel));
                }
                let ids: Vec<String> = state
                        .getVoiceChannels("g")
                        .iter()
                        .map(|c| c.getId().to_string())
                        .collect();
                assert_eq!(ids, ["10", "20", "30"]);
        }
}
