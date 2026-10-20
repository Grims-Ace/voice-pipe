// ============================================================================
// ============================================================================
// Connect a bot to Discord voice with Songbird: play relayed audio, and when
// the bot listens, give each Discord speaker's audio to the bridge.
// ============================================================================
// ============================================================================

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use songbird::driver::{DecodeConfig, DecodeMode, Driver};
use songbird::events::context_data::{DisconnectReason, VoiceData};
use songbird::events::{CoreEvent, Event, EventContext, EventHandler};
use songbird::id::{ChannelId, GuildId, UserId};
use songbird::input::{Input, RawAdapter};
use songbird::tracks::TrackHandle;
use songbird::{Config, ConnectionInfo};
use symphonia_core::io::MediaSource;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::{SEND_QUEUE, SpeakerFrame, VoiceTarget};
use crate::audio::{AudioFrame, CHANNELS, FRAME_LEN, SAMPLE_RATE, TakeFrames};
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
// Time between two receive reports for one speaker.
const REPORT_INTERVAL: Duration = Duration::from_secs(10);
// Stereo samples in one millisecond of 48 kHz audio.
const SAMPLES_PER_MS: usize = SAMPLE_RATE as usize / 1000 * CHANNELS;
// A larger forward jump in RTP sequence numbers is a new start of the
// stream (or an old packet), not lost packets.
const SEQUENCE_JUMP_MAX: u16 = 1000;

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

// Numbers about the received audio of one speaker, for the receive report.
// They show if Songbird gives all of the audio: the packet lengths, the
// packets that never came (gaps in the RTP sequence numbers), and the frames
// that Songbird made up for lost packets.
#[derive(Default)]
struct ReceiveStats {
        _since: Option<Instant>,
        _audioMs: usize,
        _packets: u32,
        _packetLengths: BTreeMap<usize, u32>,
        _missingPackets: u32,
        _madeUpFrames: u32,
        _lastSequence: Option<u16>,
}

// What the receiver keeps for one speaker. `_pending` is the part of their
// audio that is shorter than one frame, until their next packet completes
// it.
#[derive(Default)]
struct SpeakerState {
        _pending: Vec<i16>,
        _stats: ReceiveStats,
}

// Gives the audio of each speaker to the bridge while the bot listens.
struct VoiceReceiver {
        _streams: Arc<Mutex<StreamMap>>,
        _speakers: Mutex<HashMap<String, SpeakerState>>,
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
                let mut speakers = self._speakers.lock().unwrap();
                for (ssrc, data) in &tick.speaking {
                        self.Forward(&streams, &mut speakers, *ssrc, data);
                }
                for (user, speaker) in speakers.iter_mut() {
                        speaker._stats.LogIfDue(user);
                }
                None
        }
}

impl VoiceReceiver {
        // Give all of one speaker's decoded audio to the bridge in 20 ms
        // frames. A packet can hold more than one frame: the web client and
        // Vesktop send 40 or 60 ms packets when a camera uses the bandwidth.
        // Skip streams that are not the audio stream of a user, bridge bots,
        // and digital silence.
        fn Forward(
                &self,
                streams: &StreamMap,
                speakers: &mut HashMap<String, SpeakerState>,
                ssrc: u32,
                data: &VoiceData,
        ) {
                let Some(user) = streams.getAudioUser(ssrc) else { return };
                if self._ownBotIds.contains(user) { return; }
                let speaker = speakers.entry(user.clone()).or_default();
                speaker._stats.Record(data);
                let Some(pcm) = &data.decoded_voice else { return };
                for frame in TakeFrames(&mut speaker._pending, pcm) {
                        if frame.isSilent() { continue; }
                        let _ = self._heard.try_send((user.clone(), frame));
                }
        }
}

// ============================================================================
// Receive Report
// ============================================================================

impl ReceiveStats {
        // Count one tick of a speaker's audio from Songbird.
        fn Record(&mut self, data: &VoiceData) {
                self._since.get_or_insert_with(Instant::now);
                let samples = data.decoded_voice.as_ref().map_or(0, Vec::len);
                let length_ms = samples / SAMPLES_PER_MS;
                self._audioMs += length_ms;
                let Some(rtp) = &data.packet else {
                        self._madeUpFrames += 1;
                        return;
                };
                self._packets += 1;
                *self._packetLengths.entry(length_ms).or_default() += 1;
                let Some(sequence) = ReadSequence(&rtp.packet) else { return };
                if let Some(last) = self._lastSequence {
                        self._missingPackets += CountMissing(last, sequence);
                }
                self._lastSequence = Some(sequence);
        }

        // Write the report and start a new one, if the interval has passed.
        // Keep the last sequence number, so that the next report continues
        // to count gaps.
        fn LogIfDue(&mut self, user: &str) {
                let Some(since) = self._since else { return };
                if since.elapsed() < REPORT_INTERVAL { return; }
                let lengths: Vec<String> = self
                        ._packetLengths
                        .iter()
                        .map(|(ms, count)| format!("{ms}ms:{count}"))
                        .collect();
                info!(
                        %user,
                        audio_ms = self._audioMs,
                        packets = self._packets,
                        packet_lengths = %lengths.join(" "),
                        missing_packets = self._missingPackets,
                        made_up_frames = self._madeUpFrames,
                        "receive report",
                );
                let last = self._lastSequence;
                *self = Self { _lastSequence: last, ..Default::default() };
        }
}

// The RTP sequence number is the third and fourth byte of the header.
fn ReadSequence(packet: &[u8]) -> Option<u16> {
        let bytes = packet.get(2..4)?;
        Some(u16::from_be_bytes([bytes[0], bytes[1]]))
}

// Return the number of packets between two packets that came one after the
// other. A large jump means that the stream started again, not loss.
fn CountMissing(last: u16, sequence: u16) -> u32 {
        let gap = sequence.wrapping_sub(last).wrapping_sub(1);
        if gap >= SEQUENCE_JUMP_MAX { 0 } else { u32::from(gap) }
}

// ============================================================================
// Audio Streams
// ============================================================================

impl StreamMap {
        // Return the user of a stream, but only if it is the audio stream of
        // that user. An older stream of the same user is not audio now.
        fn getAudioUser(&self, ssrc: u32) -> Option<&String> {
                let user = self._streamUsers.get(&ssrc)?;
                let audio = self._audioStreams.get(user);
                (audio == Some(&ssrc)).then_some(user)
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

#[async_trait]
impl EventHandler for ConnectionMonitor {
        async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
                match ctx {
                        EventContext::DriverConnect(_)
                        | EventContext::DriverReconnect(_) => {
                                self._isConnected
                                        .store(true, Ordering::Relaxed);
                        }
                        EventContext::DriverDisconnect(data) => {
                                self._isConnected
                                        .store(false, Ordering::Relaxed);
                                LogDisconnect(data.reason);
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

// A disconnect that the bridge asked for (`Requested`) is normal. Any other
// reason means that the connection was lost.
fn LogDisconnect(reason: Option<DisconnectReason>) {
        match reason {
                None | Some(DisconnectReason::Requested) => {
                        info!("Discord voice closed")
                }
                Some(reason) => warn!(?reason, "Discord voice disconnected"),
        }
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
                _speakers: Mutex::new(HashMap::new()),
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
        fn SequenceGapsCountMissingPackets() {
                assert_eq!(CountMissing(10, 11), 0);
                assert_eq!(CountMissing(10, 13), 2);
                // The 16-bit counter wraps around.
                assert_eq!(CountMissing(65535, 1), 1);
                // An old packet or a restart is not loss.
                assert_eq!(CountMissing(10, 9), 0);
                assert_eq!(CountMissing(10, 5000), 0);
                let header = [0x80, 120, 0x12, 0x34];
                assert_eq!(ReadSequence(&header), Some(0x1234));
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
