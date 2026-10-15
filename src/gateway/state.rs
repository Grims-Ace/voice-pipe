// ============================================================================
// ============================================================================
// Keep a live view of the voice channels in each guild and of the users that
// are in them, from the events that the gateway sends.
// ============================================================================
// ============================================================================

use std::collections::{HashMap, HashSet};

use super::protocol::{ChannelData, GuildCreate, VoiceStateData};

// A voice channel and its name.
#[derive(Clone, Debug)]
pub struct VoiceChannel {
        _id: String,
        _name: String,
}

// The voice channels of one guild and the channel that each user is in.
#[derive(Default)]
struct GuildState {
        _channelNames: HashMap<String, String>,
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

        pub fn New(id: &str, name: &str) -> Self {
                Self { _id: id.to_string(), _name: name.to_string() }
        }
}

impl GatewayState {
        pub fn hasGuild(&self, guild_id: &str) -> bool {
                self._guilds.contains_key(guild_id)
        }

        pub fn getVoiceChannels(&self, guild_id: &str) -> Vec<VoiceChannel> {
                let Some(guild) = self._guilds.get(guild_id) else {
                        return Vec::new();
                };
                guild._channelNames
                        .iter()
                        .map(|(id, name)| VoiceChannel::New(id, name))
                        .collect()
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
                        let name = channel.name.clone().unwrap_or_default();
                        guild._channelNames.insert(channel.id.clone(), name);
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

        // Add or rename a voice channel. Remove it if it is no longer voice.
        pub fn ApplyChannelUpdate(&mut self, data: &ChannelData) {
                let Some(guild_id) = &data.guild_id else { return };
                let guild = self._guilds.entry(guild_id.clone()).or_default();
                if !data.isVoice() {
                        guild._channelNames.remove(&data.id);
                        return;
                }
                let name = data.name.clone().unwrap_or_default();
                guild._channelNames.insert(data.id.clone(), name);
        }

        pub fn ApplyChannelDelete(&mut self, data: &ChannelData) {
                let Some(guild_id) = &data.guild_id else { return };
                let Some(guild) = self._guilds.get_mut(guild_id) else {
                        return;
                };
                guild._channelNames.remove(&data.id);
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
}
