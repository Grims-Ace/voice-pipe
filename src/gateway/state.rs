// ============================================================================
// ============================================================================
// Keep a live view of the voice channels in each guild and of the users that
// are in them, from the events that the gateway sends.
// ============================================================================
// ============================================================================

use std::collections::{HashMap, HashSet};

use super::protocol::{ChannelData, GuildCreate, VoiceStateData};

// A voice channel, its name, and its position in the channel list.
#[derive(Clone, Debug)]
pub struct VoiceChannel {
        _id: String,
        _name: String,
        _position: i64,
}

// The voice channels of one guild and the channel that each user is in.
// Text channels are never stored.
#[derive(Default)]
struct GuildState {
        _channels: HashMap<String, VoiceChannel>,
        _userChannels: HashMap<String, String>,
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
                Self { _id: id.to_string(), _name: name.to_string(), _position: position }
        }

        fn FromData(data: &ChannelData) -> Self {
                let name = data.name.as_deref().unwrap_or_default();
                Self::New(&data.id, name, data.position.unwrap_or_default())
        }

        // Order of the channel list in the app: position, then age. Older
        // IDs are smaller numbers.
        fn getSortKey(&self) -> (i64, u128, &str) {
                (self._position, self._id.parse().unwrap_or(u128::MAX), &self._id)
        }
}

impl GuildState {
        fn RenameChannel(&mut self, data: &ChannelData) {
                let Some(channel) = self._channels.get_mut(&data.id) else { return };
                if let Some(name) = &data.name { channel._name = name.clone(); }
                if let Some(position) = data.position { channel._position = position; }
        }
}

impl GatewayState {
        pub fn hasGuild(&self, guild_id: &str) -> bool {
                self._guilds.contains_key(guild_id)
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
                self._guilds.get(guild_id)?._userChannels.get(user).cloned()
        }

        // Return the IDs of all users in a voice channel, bots included.
        pub fn getChannelUsers(
                &self,
                guild_id: &str,
                channel_id: &str,
        ) -> HashSet<String> {
                let Some(guild) = self._guilds.get(guild_id) else {
                        return HashSet::new();
                };
                guild._userChannels
                        .iter()
                        .filter(|(_, channel)| *channel == channel_id)
                        .map(|(user, _)| user.clone())
                        .collect()
        }

        // Replace the view of a guild with the full snapshot from the server.
        pub fn ApplyGuildCreate(&mut self, data: &GuildCreate) {
                let mut guild = GuildState::default();
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
                        guild._userChannels
                                .insert(user.clone(), channel.clone());
                }
                self._guilds.insert(data.id.clone(), guild);
        }

        // Move a user to a channel, or out of voice when the channel is null.
        pub fn ApplyVoiceState(&mut self, data: &VoiceStateData) {
                let (Some(guild_id), Some(user)) =
                        (&data.guild_id, &data.user_id)
                else {
                        return;
                };
                let guild = self._guilds.entry(guild_id.clone()).or_default();
                match &data.channel_id {
                        Some(channel) => guild
                                ._userChannels
                                .insert(user.clone(), channel.clone()),
                        None => guild._userChannels.remove(user),
                };
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
                guild._channels.insert(data.id.clone(), VoiceChannel::FromData(data));
        }

        pub fn ApplyChannelDelete(&mut self, data: &ChannelData) {
                let Some(guild_id) = &data.guild_id else { return };
                let Some(guild) = self._guilds.get_mut(guild_id) else {
                        return;
                };
                guild._channels.remove(&data.id);
                guild._userChannels.retain(|_, channel| *channel != data.id);
        }
}

#[cfg(test)]
mod tests {
        use super::*;

        fn VoiceState(user: &str, channel: Option<&str>) -> VoiceStateData {
                VoiceStateData {
                        guild_id: Some("g".into()),
                        channel_id: channel.map(String::from),
                        user_id: Some(user.into()),
                        session_id: None,
                }
        }

        #[test]
        fn UsersMoveBetweenChannels() {
                let mut state = GatewayState::default();
                state.ApplyVoiceState(&VoiceState("u1", Some("c1")));
                state.ApplyVoiceState(&VoiceState("u1", Some("c2")));
                assert!(state.getChannelUsers("g", "c1").is_empty());
                assert!(state.getChannelUsers("g", "c2").contains("u1"));
                state.ApplyVoiceState(&VoiceState("u1", None));
                assert!(state.getChannelUsers("g", "c2").is_empty());
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
                assert!(state.getChannelUsers("g", "c1").contains("u1"));
        }

        fn Channel(text: &str) -> ChannelData { serde_json::from_str(text).unwrap() }

        fn StateWithGeneral() -> GatewayState {
                let mut state = GatewayState::default();
                let voice = r#"{"id":"1","guild_id":"g","name":"General","type":2}"#;
                state.ApplyChannelUpdate(&Channel(voice));
                state
        }

        #[test]
        fn TextChannelWithSameNameIsIgnored() {
                let mut state = StateWithGeneral();
                let text = r#"{"id":"2","guild_id":"g","name":"general","type":0}"#;
                state.ApplyChannelUpdate(&Channel(text));
                let channels = state.getVoiceChannels("g");
                assert_eq!(channels.len(), 1);
                assert_eq!(channels[0].getId(), "1");
        }

        #[test]
        fn UpdateWithoutTypeDoesNotRemoveChannel() {
                let mut state = StateWithGeneral();
                state.ApplyChannelUpdate(&Channel(r#"{"id":"1","guild_id":"g"}"#));
                assert_eq!(state.getVoiceChannels("g")[0].getName(), "General");
        }

        #[test]
        fn ChannelsAreInListOrder() {
                let mut state = GatewayState::default();
                for (id, position) in [("30", 2), ("20", 1), ("10", 1)] {
                        let text = format!(
                                r#"{{"id":"{id}","guild_id":"g","type":2,"position":{position}}}"#
                        );
                        state.ApplyChannelUpdate(&Channel(&text));
                }
                let ids: Vec<String> =
                        state.getVoiceChannels("g").iter().map(|c| c.getId().to_string()).collect();
                assert_eq!(ids, ["10", "20", "30"]);
        }
}
