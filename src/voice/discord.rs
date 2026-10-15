// ============================================================================
// ============================================================================
// Connect a bot to Discord voice with Songbird: play relayed audio, and when
// the bot listens, give each Discord speaker's audio to the bridge.
// ============================================================================
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use songbird::driver::{DecodeConfig, DecodeMode, Driver};
use songbird::events::context_data::VoiceData;
use songbird::events::{CoreEvent, Event, EventContext, EventHandler};
use songbird::id::{ChannelId, GuildId, UserId};
use songbird::input::{Input, RawAdapter};
use songbird::tracks::TrackHandle;
use songbird::{Config, ConnectionInfo};
use symphonia_core::io::MediaSource;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::{SEND_QUEUE, SpeakerFrame, VoiceTarget};
use crate::audio::{AudioFrame, CHANNELS, FRAME_LEN, SAMPLE_RATE};
use crate::gateway::VoiceServer;

// ============================================================================
// Constants
// ============================================================================

// Frames to collect before playback starts, so that uneven arrival times do
// not put gaps in words (about 60 ms).
const PLAYBACK_PREBUFFER: usize = 3;
// Remove the oldest frames above this depth, so that delay cannot grow
// (about 240 ms).
const PLAYBACK_MAX_DEPTH: usize = 12;

// ============================================================================
// Types
// ============================================================================

// A bot in a Discord voice channel.
pub struct DiscordVoice {
        _driver: Mutex<Driver>,
        _output: mpsc::Sender<AudioFrame>,
        _isListening: Arc<AtomicBool>,
        _isConnected: Arc<AtomicBool>,
}

// Records which user each audio stream (SSRC) belongs to.
struct SsrcTracker {
        _ssrcUsers: Arc<Mutex<HashMap<u32, String>>>,
}

// Gives the audio of each speaker to the bridge while the bot listens.
struct VoiceReceiver {
        _ssrcUsers: Arc<Mutex<HashMap<u32, String>>>,
        _ownBotIds: Arc<HashSet<String>>,
        _isListening: Arc<AtomicBool>,
        _heard: mpsc::Sender<SpeakerFrame>,
        _voiceThreshold: f32,
}

// Keeps the connection flag equal to the state of the Songbird driver.
struct ConnectionMonitor {
        _isConnected: Arc<AtomicBool>,
}

// A live PCM stream for Songbird. It turns queued frames into f32 bytes and
// plays silence while it waits for frames.
struct PcmSource {
        _frames: flume::Receiver<AudioFrame>,
        _bytes: Vec<u8>,
        _isPlaying: bool,
}

// ============================================================================
// Discord Voice
// ============================================================================

impl DiscordVoice {
        pub fn isConnected(&self) -> bool {
                self._isConnected.load(Ordering::Relaxed)
        }

        // Connect Songbird with the voice server from the gateway.
        pub async fn Connect(
                server: &VoiceServer,
                target: &VoiceTarget,
        ) -> Result<Self> {
                let info = BuildConnectionInfo(server, target)?;
                let is_listening = Arc::new(AtomicBool::new(false));
                let is_connected = Arc::new(AtomicBool::new(true));
                let mut driver = Driver::new(BuildConfig(false));
                AddReceiveEvents(&mut driver, target, is_listening.clone());
                AddConnectionEvents(&mut driver, is_connected.clone());
                driver.connect(info).await.map_err(|e| {
                        anyhow!("Discord voice connect failed: {e}")
                })?;
                let output = StartPlayback(
                        &mut driver,
                        target._tuning.getHangover(),
                );
                info!(
                        channel = %target._channelId,
                        bot = %target._botId,
                        "joined Discord voice",
                );
                Ok(Self {
                        _driver: Mutex::new(driver),
                        _output: output,
                        _isListening: is_listening,
                        _isConnected: is_connected,
                })
        }

        // Queue a frame to play. Drop it if playback is behind.
        pub fn SendFrame(&self, frame: AudioFrame) {
                let _ = self._output.try_send(frame);
        }

        // Decode received audio only while the bot listens, because decoding
        // costs CPU time and the bridge must not record audio without need.
        pub fn SetListening(&self, is_listening: bool) {
                if self._isListening.swap(is_listening, Ordering::Relaxed)
                        == is_listening
                {
                        return;
                }
                self._driver
                        .lock()
                        .unwrap()
                        .set_config(BuildConfig(is_listening));
        }

        pub fn Close(self) { self._driver.lock().unwrap().leave(); }
}

// ============================================================================
// Event Handlers
// ============================================================================

#[async_trait]
impl EventHandler for SsrcTracker {
        async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
                let EventContext::SpeakingStateUpdate(speaking) = ctx else {
                        return None;
                };
                let user = speaking.user_id?;
                self._ssrcUsers
                        .lock()
                        .unwrap()
                        .insert(speaking.ssrc, user.0.to_string());
                None
        }
}

#[async_trait]
impl EventHandler for VoiceReceiver {
        async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
                let EventContext::VoiceTick(tick) = ctx else { return None };
                if !self._isListening.load(Ordering::Relaxed) { return None; }
                let users = self._ssrcUsers.lock().unwrap();
                for (ssrc, data) in &tick.speaking {
                        self.Forward(&users, *ssrc, data);
                }
                None
        }
}

impl VoiceReceiver {
        // Give one speaker's frame to the bridge. Skip unknown streams, bridge
        // bots, and frames that are below the voice threshold.
        fn Forward(
                &self,
                users: &HashMap<u32, String>,
                ssrc: u32,
                data: &VoiceData,
        ) {
                let Some(user) = users.get(&ssrc) else { return };
                if self._ownBotIds.contains(user) { return; }
                let Some(pcm) = &data.decoded_voice else { return };
                let frame = AudioFrame::FromDecoded(pcm);
                if frame.isQuiet(self._voiceThreshold) { return; }
                let _ = self._heard.try_send((user.clone(), frame));
        }
}

#[async_trait]
impl EventHandler for ConnectionMonitor {
        async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
                match ctx {
                        EventContext::DriverConnect(_)
                        | EventContext::DriverReconnect(_) => {
                                self._isConnected
                                        .store(true, Ordering::Relaxed);
                        }
                        EventContext::DriverDisconnect(_) => {
                                self._isConnected
                                        .store(false, Ordering::Relaxed);
                                warn!("Discord voice disconnected");
                        }
                        _ => {}
                }
                None
        }
}

// ============================================================================
// PCM Source
// ============================================================================

impl PcmSource {
        fn New(frames: flume::Receiver<AudioFrame>) -> Self {
                Self { _frames: frames, _bytes: Vec::new(), _isPlaying: false }
        }

        // Load the next frame into the byte buffer. Return false when the
        // sender is gone, which ends the stream.
        fn Refill(&mut self) -> bool {
                while self._frames.len() > PLAYBACK_MAX_DEPTH {
                        let _ = self._frames.try_recv();
                }
                if !self._isPlaying && self._frames.len() < PLAYBACK_PREBUFFER {
                        self.LoadSilence();
                        return true;
                }
                self._isPlaying = true;
                match self._frames.try_recv() {
                        Ok(frame) => {
                                self._bytes.clear();
                                frame.WriteFloatBytes(&mut self._bytes);
                                true
                        }
                        Err(flume::TryRecvError::Empty) => {
                                self._isPlaying = false;
                                self.LoadSilence();
                                true
                        }
                        Err(flume::TryRecvError::Disconnected) => false,
                }
        }

        fn LoadSilence(&mut self) { self._bytes = vec![0u8; FRAME_LEN * 4]; }
}

impl Read for PcmSource {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self._bytes.is_empty() && !self.Refill() { return Ok(0); }
                let len = buf.len().min(self._bytes.len());
                buf[..len].copy_from_slice(&self._bytes[..len]);
                self._bytes.drain(..len);
                Ok(len)
        }
}

impl Seek for PcmSource {
        fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
                Err(std::io::ErrorKind::Unsupported.into())
        }
}

impl MediaSource for PcmSource {
        fn is_seekable(&self) -> bool { false }
        fn byte_len(&self) -> Option<u64> { None }
}

// ============================================================================
// Helper Functions
// ============================================================================

// Decode received audio to 48 kHz stereo only while the bot listens. Pass
// mode does no decryption and no decoding.
fn BuildConfig(is_listening: bool) -> Config {
        let mode = if is_listening {
                DecodeMode::Decode(DecodeConfig::default())
        } else {
                DecodeMode::Pass
        };
        Config::default().decode_mode(mode)
}

fn BuildConnectionInfo(
        server: &VoiceServer,
        target: &VoiceTarget,
) -> Result<ConnectionInfo> {
        let session_id = server
                .getSessionId()
                .context("Discord sent no voice session ID")?;
        Ok(ConnectionInfo {
                channel_id: ChannelId(ParseId(&target._channelId)?),
                endpoint: server.getEndpoint().to_string(),
                guild_id: GuildId(ParseId(&target._guildId)?),
                session_id: session_id.to_string(),
                token: server.getToken().to_string(),
                user_id: UserId(ParseId(&target._botId)?),
        })
}

fn ParseId(value: &str) -> Result<NonZeroU64> {
        value.parse::<u64>()
                .ok()
                .and_then(NonZeroU64::new)
                .with_context(|| format!("\"{value}\" is not a Discord ID"))
}

fn AddReceiveEvents(
        driver: &mut Driver,
        target: &VoiceTarget,
        is_listening: Arc<AtomicBool>,
) {
        let ssrc_users = Arc::new(Mutex::new(HashMap::new()));
        let tracker = SsrcTracker { _ssrcUsers: ssrc_users.clone() };
        driver.add_global_event(
                Event::Core(CoreEvent::SpeakingStateUpdate),
                tracker,
        );
        let receiver = VoiceReceiver {
                _ssrcUsers: ssrc_users,
                _ownBotIds: target._ownBotIds.clone(),
                _isListening: is_listening,
                _heard: target._heard.clone(),
                _voiceThreshold: target._tuning.getVoiceThreshold(),
        };
        driver.add_global_event(Event::Core(CoreEvent::VoiceTick), receiver);
}

fn AddConnectionEvents(driver: &mut Driver, is_connected: Arc<AtomicBool>) {
        for event in [
                CoreEvent::DriverConnect,
                CoreEvent::DriverReconnect,
                CoreEvent::DriverDisconnect,
        ] {
                let monitor = ConnectionMonitor {
                        _isConnected: is_connected.clone(),
                };
                driver.add_global_event(Event::Core(event), monitor);
        }
}

// Start a paused playback track and the task that opens and closes it.
fn StartPlayback(
        driver: &mut Driver,
        hangover: Duration,
) -> mpsc::Sender<AudioFrame> {
        let (pcm_tx, pcm_rx) = flume::bounded(PLAYBACK_MAX_DEPTH * 2);
        let source = PcmSource::New(pcm_rx.clone());
        let input: Input =
                RawAdapter::new(source, SAMPLE_RATE, CHANNELS as u32).into();
        let track = driver.play_input(input);
        let _ = track.pause();
        let (output_tx, output_rx) = mpsc::channel(SEND_QUEUE);
        tokio::spawn(RunPlaybackGate(
                output_rx, pcm_tx, pcm_rx, track, hangover,
        ));
        output_tx
}

// Play while frames arrive and pause after the hangover. A paused track lets
// Songbird stop sending packets while nobody speaks.
async fn RunPlaybackGate(
        mut output: mpsc::Receiver<AudioFrame>,
        pcm: flume::Sender<AudioFrame>,
        pcm_drain: flume::Receiver<AudioFrame>,
        track: TrackHandle,
        hangover: Duration,
) {
        while let Some(frame) = output.recv().await {
                let _ = track.play();
                let _ = pcm.try_send(frame);
                if !ForwardUntilQuiet(&mut output, &pcm, hangover).await {
                        break;
                }
                let _ = track.pause();
                pcm_drain.drain();
        }
        let _ = track.stop();
}

// Forward frames until none arrive for the hangover time. Return false when
// the output queue closes.
async fn ForwardUntilQuiet(
        output: &mut mpsc::Receiver<AudioFrame>,
        pcm: &flume::Sender<AudioFrame>,
        hangover: Duration,
) -> bool {
        loop {
                match tokio::time::timeout(hangover, output.recv()).await {
                        Ok(Some(frame)) => {
                                let _ = pcm.try_send(frame);
                        }
                        Ok(None) => return false,
                        Err(_) => return true,
                }
        }
}
