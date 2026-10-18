// ============================================================================
// ============================================================================
// Give the bridge one interface for a bot in a voice channel, on Discord
// (Songbird) or on Fluxer (LiveKit).
// ============================================================================
// ============================================================================

mod discord;
mod fluxer;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::mpsc;

use crate::audio::AudioFrame;
use crate::gateway::Gateway;
use crate::platform::Platform;
use discord::DiscordVoice;
use fluxer::FluxerVoice;

// One frame of audio from one speaker: (speaker user ID, frame).
pub type SpeakerFrame = (String, AudioFrame);

// Number of frames that can wait in a send queue. Older audio is dropped
// when a queue is full, because late audio is not useful.
const SEND_QUEUE: usize = 64;

// ============================================================================
// Types
// ============================================================================

// Values that control when audio counts as speech.
#[derive(Clone, Copy, Debug)]
pub struct AudioTuning {
        _hangover: Duration,
}

// Where a bot joins and where its received audio goes.
#[derive(Clone)]
pub struct VoiceTarget {
        _platform: Platform,
        _guildId: String,
        _channelId: String,
        _botId: String,
        _ownBotIds: Arc<HashSet<String>>,
        _heard: mpsc::Sender<SpeakerFrame>,
        _tuning: AudioTuning,
        _livekitUrl: Option<String>,
}

// A bot that is connected to a voice channel.
pub enum VoiceConnection {
        Discord(DiscordVoice),
        Fluxer(FluxerVoice),
}

impl AudioTuning {
        pub fn getHangover(&self) -> Duration { self._hangover }

        // `hangover` is how long a bot keeps sending after speech stops, so
        // that the ends of words are not cut.
        pub fn New(hangover: Duration) -> Self {
                Self { _hangover: hangover }
        }
}

impl VoiceTarget {
        pub fn getGuildId(&self) -> &str { &self._guildId }
        pub fn getChannelId(&self) -> &str { &self._channelId }
        pub fn getBotId(&self) -> &str { &self._botId }
        pub fn getTuning(&self) -> AudioTuning { self._tuning }
        pub fn getLivekitUrl(&self) -> Option<&str> {
                self._livekitUrl.as_deref()
        }
        pub fn getOwnBotIds(&self) -> Arc<HashSet<String>> {
                self._ownBotIds.clone()
        }
        pub fn getHeard(&self) -> mpsc::Sender<SpeakerFrame> {
                self._heard.clone()
        }

        // `own_bot_ids` are the IDs of all bridge bots on this platform. The
        // receiver ignores their audio, because they only replay audio from
        // the other platform. `heard` gets the audio of the other speakers.
        #[allow(clippy::too_many_arguments)]
        pub fn New(
                platform: Platform,
                guild_id: &str,
                channel_id: &str,
                bot_id: &str,
                own_bot_ids: Arc<HashSet<String>>,
                heard: mpsc::Sender<SpeakerFrame>,
                tuning: AudioTuning,
                livekit_url: Option<&str>,
        ) -> Self {
                Self {
                        _platform: platform,
                        _guildId: guild_id.to_string(),
                        _channelId: channel_id.to_string(),
                        _botId: bot_id.to_string(),
                        _ownBotIds: own_bot_ids,
                        _heard: heard,
                        _tuning: tuning,
                        _livekitUrl: livekit_url.map(String::from),
                }
        }

        // Make the same target for another bot.
        pub fn ForBot(&self, bot_id: &str) -> Self {
                Self { _botId: bot_id.to_string(), ..self.clone() }
        }
}

impl VoiceConnection {
        pub fn isConnected(&self) -> bool {
                match self {
                        VoiceConnection::Discord(voice) => voice.isConnected(),
                        VoiceConnection::Fluxer(voice) => voice.isConnected(),
                }
        }

        // Move the bot into the channel of the target and start its audio.
        // The bot does not listen until SetListening(true).
        pub async fn Join(
                gateway: &Gateway,
                target: &VoiceTarget,
        ) -> Result<VoiceConnection> {
                let joined =
                        gateway.JoinVoice(&target._guildId, &target._channelId);
                let server = match joined.await {
                        Ok(server) => server,
                        Err(e) => {
                                gateway.LeaveVoice(&target._guildId);
                                return Err(e);
                        }
                };
                let result = match target._platform {
                        Platform::Discord => {
                                DiscordVoice::Connect(&server, target)
                                        .await
                                        .map(VoiceConnection::Discord)
                        }
                        Platform::Fluxer => {
                                FluxerVoice::Connect(&server, target)
                                        .await
                                        .map(VoiceConnection::Fluxer)
                        }
                };
                if result.is_err() { gateway.LeaveVoice(&target._guildId); }
                result
        }

        // Queue one frame to play in the channel.
        pub fn SendFrame(&self, frame: AudioFrame) {
                match self {
                        VoiceConnection::Discord(voice) => {
                                voice.SendFrame(frame)
                        }
                        VoiceConnection::Fluxer(voice) => {
                                voice.SendFrame(frame)
                        }
                }
        }

        // Start or stop receiving the audio of the people in the channel.
        // Only one bot on each side of a link listens at a time.
        pub fn SetListening(&self, is_listening: bool) {
                match self {
                        VoiceConnection::Discord(voice) => {
                                voice.SetListening(is_listening)
                        }
                        VoiceConnection::Fluxer(voice) => {
                                voice.SetListening(is_listening)
                        }
                }
        }

        // Disconnect the voice media. The caller must also send a voice leave
        // on the gateway of the bot.
        pub async fn Close(self) {
                match self {
                        VoiceConnection::Discord(voice) => voice.Close(),
                        VoiceConnection::Fluxer(voice) => voice.Close().await,
                }
        }
}
