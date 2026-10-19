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
use std::time::{Duration, Instant};

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
// RTP payload type of Opus audio on Discord. A camera sends video with other
// payload types on the same connection, and video decoded as audio is noise.
const OPUS_PAYLOAD_TYPE: u8 = 120;
// Shortest time between two reports of skipped packets, so that a camera
// that stays on does not fill the log.
const SKIP_REPORT_INTERVAL: Duration = Duration::from_secs(60);

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

// The user of each stream (SSRC), and the one audio stream of each user. A
// user can send more than one stream (for example a camera), and only the
// stream that the user names when they start to speak is audio.
#[derive(Default)]
struct StreamMap {
        _streamUsers: HashMap<u32, String>,
        _audioStreams: HashMap<String, u32>,
}

// Records the audio stream of each user, and forgets the streams of users
// that leave.
struct SsrcTracker {
        _streams: Arc<Mutex<StreamMap>>,
}

// Counts the packets that were skipped because they are not audio, and
// writes the counts to the log at most once each report interval.
struct SkipReport {
        _counts: HashMap<String, u64>,
        _lastReport: Instant,
}

// Gives the audio of each speaker to the bridge while the bot listens.
struct VoiceReceiver {
        _streams: Arc<Mutex<StreamMap>>,
        _skipped: Mutex<SkipReport>,
        _ownBotIds: Arc<HashSet<String>>,
        _isListening: Arc<AtomicBool>,
        _heard: mpsc::Sender<SpeakerFrame>,
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
        _readOffset: usize,
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
                let mut driver = Driver::new(BuildConfig());
                AddReceiveEvents(&mut driver, target, is_listening.clone());
                AddConnectionEvents(&mut driver, is_connected.clone());
                driver.connect(info).await.map_err(|e| {
                        anyhow!("Discord voice connect failed: {e}")
                })?;
                let output = StartPlayback(
                        &mut driver,
                        target.getTuning().getHangover(),
                );
                info!(
                        channel = %target.getChannelId(),
                        bot = %target.getBotId(),
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

        // Receive audio only while the bot listens. VoiceReceiver checks
        // this flag on every voice tick.
        pub fn SetListening(&self, is_listening: bool) {
                self._isListening.store(is_listening, Ordering::Relaxed);
        }

        pub fn Close(self) { self._driver.lock().unwrap().leave(); }
}

// ============================================================================
// Event Handlers
// ============================================================================

#[async_trait]
impl EventHandler for SsrcTracker {
        async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
                let mut streams = self._streams.lock().unwrap();
                match ctx {
                        EventContext::SpeakingStateUpdate(speaking) => {
                                let user = speaking.user_id?.0.to_string();
                                streams.SetAudioStream(speaking.ssrc, user);
                        }
                        EventContext::ClientDisconnect(left) => {
                                streams.RemoveUser(&left.user_id.0.to_string());
                        }
                        _ => {}
                }
                None
        }
}

#[async_trait]
impl EventHandler for VoiceReceiver {
        async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
                let EventContext::VoiceTick(tick) = ctx else { return None };
                if !self._isListening.load(Ordering::Relaxed) { return None; }
                let streams = self._streams.lock().unwrap();
                let mut skipped = self._skipped.lock().unwrap();
                for (ssrc, data) in &tick.speaking {
                        if !isOpus(data) {
                                skipped.Count(streams.DescribeStream(*ssrc));
                                continue;
                        }
                        self.Forward(&streams, *ssrc, data);
                }
                skipped.LogIfDue();
                None
        }
}

impl VoiceReceiver {
        // Give one speaker's audio frame to the bridge. Skip streams that are
        // not the audio stream of a user, bridge bots, and digital silence.
        fn Forward(&self, streams: &StreamMap, ssrc: u32, data: &VoiceData) {
                let Some(user) = streams.getAudioUser(ssrc) else { return };
                if self._ownBotIds.contains(user) { return; }
                let Some(pcm) = &data.decoded_voice else { return };
                let frame = AudioFrame::FromDecoded(pcm);
                if frame.isSilent() { return; }
                let _ = self._heard.try_send((user.clone(), frame));
        }
}

// ============================================================================
// Streams and Skipped Packets
// ============================================================================

impl StreamMap {
        // Return the user of a stream, but only if it is the audio stream of
        // that user. An older stream of the same user is not audio now.
        fn getAudioUser(&self, ssrc: u32) -> Option<&String> {
                let user = self._streamUsers.get(&ssrc)?;
                let audio = self._audioStreams.get(user);
                (audio == Some(&ssrc)).then_some(user)
        }

        // Name a stream for the log: its user, or its number if unknown.
        fn DescribeStream(&self, ssrc: u32) -> String {
                match self._streamUsers.get(&ssrc) {
                        Some(user) => format!("user {user}"),
                        None => format!("stream {ssrc}"),
                }
        }

        // Record the stream that a user names when they start to speak. It
        // replaces the older audio stream of that user.
        fn SetAudioStream(&mut self, ssrc: u32, user: String) {
                self._streamUsers.insert(ssrc, user.clone());
                self._audioStreams.insert(user, ssrc);
        }

        fn RemoveUser(&mut self, user: &str) {
                self._streamUsers.retain(|_, u| u != user);
                self._audioStreams.remove(user);
        }
}

impl SkipReport {
        fn New() -> Self {
                Self { _counts: HashMap::new(), _lastReport: Instant::now() }
        }

        fn Count(&mut self, source: String) {
                *self._counts.entry(source).or_default() += 1;
        }

        // Write the counts and start again, if the interval has passed.
        fn LogIfDue(&mut self) {
                if self._counts.is_empty() { return; }
                if self._lastReport.elapsed() < SKIP_REPORT_INTERVAL { return; }
                for (source, count) in self._counts.drain() {
                        info!(%source, count, "skipped non-audio packets");
                }
                self._lastReport = Instant::now();
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
                        // Songbird gives no reason when the bridge asked
                        // for the disconnect, which is normal.
                        EventContext::DriverDisconnect(data) => {
                                self._isConnected
                                        .store(false, Ordering::Relaxed);
                                match &data.reason {
                                        None => info!("Discord voice closed"),
                                        Some(reason) => warn!(
                                                ?reason,
                                                "Discord voice disconnected",
                                        ),
                                }
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
                Self {
                        _frames: frames,
                        _bytes: Vec::with_capacity(FRAME_LEN * 4),
                        _readOffset: 0,
                        _isPlaying: false,
                }
        }

        // Load the next frame into the byte buffer. Return false when the
        // sender is gone, which ends the stream.
        fn Refill(&mut self) -> bool {
                self._readOffset = 0;
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
                        Err(flume::TryRecvError::Disconnected) => {
                                self._bytes.clear();
                                false
                        }
                }
        }

        fn LoadSilence(&mut self) {
                self._bytes.clear();
                self._bytes.resize(FRAME_LEN * 4, 0);
        }

        fn hasUnreadBytes(&self) -> bool {
                self._readOffset < self._bytes.len()
        }
}

impl Read for PcmSource {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if !self.hasUnreadBytes() && !self.Refill() { return Ok(0); }
                let unread = &self._bytes[self._readOffset..];
                let len = buf.len().min(unread.len());
                buf[..len].copy_from_slice(&unread[..len]);
                self._readOffset += len;
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

// Decode received audio to 48 kHz stereo.
fn BuildConfig() -> Config {
        Config::default()
                .decode_mode(DecodeMode::Decode(DecodeConfig::default()))
}

fn BuildConnectionInfo(
        server: &VoiceServer,
        target: &VoiceTarget,
) -> Result<ConnectionInfo> {
        let session_id = server
                .getSessionId()
                .context("Discord sent no voice session ID")?;
        Ok(ConnectionInfo {
                channel_id: ChannelId(ParseId(target.getChannelId())?),
                endpoint: server.getEndpoint().to_string(),
                guild_id: GuildId(ParseId(target.getGuildId())?),
                session_id: session_id.to_string(),
                token: server.getToken().to_string(),
                user_id: UserId(ParseId(target.getBotId())?),
        })
}

fn ParseId(value: &str) -> Result<NonZeroU64> {
        value.parse::<u64>()
                .ok()
                .and_then(NonZeroU64::new)
                .with_context(|| format!("\"{value}\" is not a Discord ID"))
}

// Return true if the packet behind a frame is Opus audio. A lost packet has
// no header, and its frame continues the audio stream of its user.
fn isOpus(data: &VoiceData) -> bool {
        let Some(rtp) = &data.packet else { return true };
        ReadPayloadType(&rtp.packet) == Some(OPUS_PAYLOAD_TYPE)
}

// The payload type is the low 7 bits of the second byte of an RTP header.
fn ReadPayloadType(packet: &[u8]) -> Option<u8> {
        packet.get(1).map(|byte| byte & 0x7F)
}

fn AddReceiveEvents(
        driver: &mut Driver,
        target: &VoiceTarget,
        is_listening: Arc<AtomicBool>,
) {
        let streams = Arc::new(Mutex::new(StreamMap::default()));
        let events =
                [CoreEvent::SpeakingStateUpdate, CoreEvent::ClientDisconnect];
        for event in events {
                let tracker = SsrcTracker { _streams: streams.clone() };
                driver.add_global_event(Event::Core(event), tracker);
        }
        let receiver = VoiceReceiver {
                _streams: streams,
                _skipped: Mutex::new(SkipReport::New()),
                _ownBotIds: target.getOwnBotIds(),
                _isListening: is_listening,
                _heard: target.getHeard(),
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

#[cfg(test)]
mod tests {
        use super::*;

        fn ReadAll(source: &mut PcmSource, size: usize) -> Vec<u8> {
                let mut out = Vec::new();
                let mut buf = vec![0u8; size];
                loop {
                        let len = source.read(&mut buf).unwrap();
                        if len == 0 { return out; }
                        out.extend_from_slice(&buf[..len]);
                }
        }

        #[test]
        fn SourcePlaysFramesThenEnds() {
                let (tx, rx) = flume::bounded(PLAYBACK_MAX_DEPTH);
                let tone = AudioFrame::FromStereo(&[16384; FRAME_LEN]);
                for _ in 0..PLAYBACK_PREBUFFER { tx.send(tone.clone()).ok(); }
                drop(tx);
                let mut source = PcmSource::New(rx);
                let bytes = ReadAll(&mut source, 1000);
                assert_eq!(bytes.len(), PLAYBACK_PREBUFFER * FRAME_LEN * 4);
                assert_eq!(&bytes[..4], &0.5f32.to_le_bytes());
                assert_eq!(ReadAll(&mut source, 1000).len(), 0);
        }

        #[test]
        fn PayloadTypeIgnoresMarkerBit() {
                // Version 2 header. The marker bit (0x80) is set on the
                // first packet after silence and is not part of the type.
                assert_eq!(ReadPayloadType(&[0x80, 120]), Some(120));
                assert_eq!(ReadPayloadType(&[0x80, 0x80 | 120]), Some(120));
                assert_eq!(ReadPayloadType(&[0x80, 101]), Some(101));
                assert_eq!(ReadPayloadType(&[0x80]), None);
        }

        #[test]
        fn OnlyTheNewestAudioStreamIsAudio() {
                let mut streams = StreamMap::default();
                let user = "u".to_string();
                streams.SetAudioStream(1, user.clone());
                assert_eq!(streams.getAudioUser(1), Some(&user));
                streams.SetAudioStream(2, user.clone());
                assert_eq!(streams.getAudioUser(1), None);
                assert_eq!(streams.getAudioUser(2), Some(&user));
                assert_eq!(streams.getAudioUser(3), None);
                streams.RemoveUser("u");
                assert_eq!(streams.getAudioUser(2), None);
        }
}
