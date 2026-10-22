// ============================================================================
// ============================================================================
// Connect a bot to Fluxer voice (a LiveKit room): publish relayed audio, and
// when the bot listens, give each Fluxer speaker's audio to the bridge.
// ============================================================================
// ============================================================================

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use futures_util::StreamExt;
use livekit::options::TrackPublishOptions;
use livekit::prelude::*;
use livekit::webrtc::audio_source::RtcAudioSource;
use livekit::webrtc::audio_source::native::NativeAudioSource;
use livekit::webrtc::audio_stream::native::NativeAudioStream;
use livekit::webrtc::prelude::{AudioFrame as RtcFrame, AudioSourceOptions};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::{AudioTuning, SEND_QUEUE, SpeakerFrame, VoiceTarget};
use crate::audio::gate::SpeechGate;
use crate::audio::{
        AudioFrame, CHANNELS, FRAME_LEN, FRAME_TIME, SAMPLE_RATE,
        SAMPLES_PER_CHANNEL, mixer,
};
use crate::gateway::VoiceServer;

// ============================================================================
// Constants
// ============================================================================

// Audio that LiveKit can hold before a capture waits (must be a multiple of
// 10 ms).
const SOURCE_QUEUE_MS: u32 = 100;
// Time that the queued audio may run out before the publisher adds silence,
// so that short gaps in speech do not reset the jitter buffer of the
// listeners. The publisher measures from the end of the queued audio, not
// from the last frame, because frames can come in bursts (a 60 ms packet is
// three frames at once).
const GAP_MARGIN: Duration = Duration::from_millis(20);

// ============================================================================
// Types
// ============================================================================

// A bot in a Fluxer voice channel.
pub struct FluxerVoice {
        _room: Arc<Room>,
        _trackSid: TrackSid,
        _output: mpsc::Sender<AudioFrame>,
        _receive: Arc<ReceiveState>,
        _isConnected: Arc<AtomicBool>,
}

// What the room event task and the track readers need to receive audio.
struct ReceiveState {
        _isListening: AtomicBool,
        _ownBotIds: Arc<HashSet<String>>,
        _tuning: AudioTuning,
        _heard: mpsc::Sender<SpeakerFrame>,
}

// ============================================================================
// Fluxer Voice
// ============================================================================

impl FluxerVoice {
        pub fn isConnected(&self) -> bool {
                self._isConnected.load(Ordering::Relaxed)
        }

        // Connect to the LiveKit room. Use the local LiveKit URL from the
        // config when it is set, so that voice traffic stays on this machine.
        pub async fn Connect(
                server: &VoiceServer,
                target: &VoiceTarget,
        ) -> Result<Self> {
                let url =
                        target.getLivekitUrl().unwrap_or(server.getEndpoint());
                // The bridge subscribes only while it listens.
                let mut options = RoomOptions::default();
                options.auto_subscribe = false;
                let (room, events) =
                        Room::connect(url, server.getToken(), options).await?;
                let room = Arc::new(room);
                info!(url, bot = %target.getBotId(), "joined Fluxer voice");
                let (source, track_sid) = match PublishTrack(&room).await {
                        Ok(published) => published,
                        Err(e) => {
                                // Close the room, so that no connection stays
                                // open for a bot that never got audio.
                                let _ = room.close().await;
                                return Err(e);
                        }
                };
                let (output_tx, output_rx) = mpsc::channel(SEND_QUEUE);
                tokio::spawn(RunPublisher(
                        source,
                        output_rx,
                        target.getTuning().getHangover(),
                ));
                let receive = Arc::new(ReceiveState::New(target));
                let is_connected = Arc::new(AtomicBool::new(true));
                tokio::spawn(RunRoomEvents(
                        events,
                        receive.clone(),
                        is_connected.clone(),
                ));
                Ok(Self {
                        _room: room,
                        _trackSid: track_sid,
                        _output: output_tx,
                        _receive: receive,
                        _isConnected: is_connected,
                })
        }

        // Queue a frame to publish. Drop it if publishing is behind.
        pub fn SendFrame(&self, frame: AudioFrame) {
                let _ = self._output.try_send(frame);
        }

        // Subscribe to the audio tracks of the people in the room only while
        // the bot listens. Without a subscription, no audio comes to the bot.
        pub fn SetListening(&self, is_listening: bool) {
                if self._receive
                        ._isListening
                        .swap(is_listening, Ordering::Relaxed)
                        == is_listening
                {
                        return;
                }
                for participant in self._room.remote_participants().values() {
                        if self._receive.isOwnBot(participant) { continue; }
                        SetAudioSubscribed(participant, is_listening);
                }
        }

        // Stop the audio track, then leave the room. LiveKit expects the
        // tracks to stop before the room closes. A room that closes with a
        // live track can try to negotiate on a connection that is closed.
        // Without the leave, the bot stays in the channel list until
        // LiveKit times out.
        pub async fn Close(self) {
                drop(self._output);
                let participant = self._room.local_participant();
                let stopped = participant.unpublish_track(&self._trackSid);
                if let Err(e) = stopped.await {
                        debug!(error = %e, "cannot stop the audio track");
                }
                if let Err(e) = self._room.close().await {
                        warn!(error = %e, "cannot close LiveKit room");
                }
        }
}

// ============================================================================
// Receive State
// ============================================================================

impl ReceiveState {
        fn isListening(&self) -> bool {
                self._isListening.load(Ordering::Relaxed)
        }

        fn isOwnBot(&self, participant: &RemoteParticipant) -> bool {
                self._ownBotIds
                        .contains(&ParseUserId(&participant.identity().0))
        }

        fn New(target: &VoiceTarget) -> Self {
                Self {
                        _isListening: AtomicBool::new(false),
                        _ownBotIds: target.getOwnBotIds(),
                        _tuning: target.getTuning(),
                        _heard: target.getHeard(),
                }
        }

        // Give a frame to the bridge. Skip audio that is not speech, so that
        // background noise does not keep the mirrors speaking.
        fn Forward(
                &self,
                user: &str,
                frame: AudioFrame,
                gate: &mut SpeechGate,
        ) {
                if !gate.Pass(&frame) { return; }
                let _ = self._heard.try_send((user.to_string(), frame));
        }
}

// ============================================================================
// Room Tasks
// ============================================================================

// Turn off echo cancellation, noise suppression, and gain control. They are
// for microphones and damage audio that is already clean. Return the source
// and the track ID, which Close needs to stop the track.
async fn PublishTrack(room: &Room) -> Result<(NativeAudioSource, TrackSid)> {
        let options = AudioSourceOptions {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
        };
        let source = NativeAudioSource::new(
                options,
                SAMPLE_RATE,
                CHANNELS as u32,
                SOURCE_QUEUE_MS,
        );
        let track = LocalAudioTrack::create_audio_track(
                "voice-pipe",
                RtcAudioSource::Native(source.clone()),
        );
        // DTX lets the encoder send almost nothing during silence.
        let options = TrackPublishOptions {
                source: TrackSource::Microphone,
                dtx: true,
                ..Default::default()
        };
        let publication = room
                .local_participant()
                .publish_track(LocalTrack::Audio(track), options)
                .await?;
        Ok((source, publication.sid()))
}

// Publish only while frames arrive. After the hangover the track goes idle
// and LiveKit sends no audio.
async fn RunPublisher(
        source: NativeAudioSource,
        mut output: mpsc::Receiver<AudioFrame>,
        hangover: Duration,
) {
        while let Some(frame) = output.recv().await {
                Capture(&source, &frame).await;
                if !PublishUntilQuiet(&source, &mut output, hangover).await {
                        break;
                }
        }
        debug!("Fluxer publisher stopped");
}

// Publish frames and fill short gaps with silence until no audio comes for
// the hangover time. The caller has just captured one frame. Silence is
// added only when the queued audio has played out, so that a burst of
// frames is never cut up with silence. Return false when the output queue
// closes.
async fn PublishUntilQuiet(
        source: &NativeAudioSource,
        output: &mut mpsc::Receiver<AudioFrame>,
        hangover: Duration,
) -> bool {
        let mut queued_until = Instant::now() + FRAME_TIME;
        let mut quiet = Duration::ZERO;
        while quiet < hangover {
                let deadline = queued_until + GAP_MARGIN;
                let next = tokio::time::timeout_at(deadline, output.recv());
                match next.await {
                        Ok(Some(frame)) => {
                                quiet = Duration::ZERO;
                                Capture(source, &frame).await;
                        }
                        Ok(None) => return false,
                        Err(_) => {
                                quiet += FRAME_TIME;
                                Capture(source, &AudioFrame::Silence()).await;
                        }
                }
                queued_until = queued_until.max(Instant::now()) + FRAME_TIME;
        }
        true
}

// Limit the peaks of a frame and give it to LiveKit.
async fn Capture(source: &NativeAudioSource, frame: &AudioFrame) {
        let limited = mixer::Limit(frame);
        let rtc_frame = RtcFrame {
                data: Cow::Borrowed(limited.getSamples()),
                sample_rate: SAMPLE_RATE,
                num_channels: CHANNELS as u32,
                samples_per_channel: SAMPLES_PER_CHANNEL as u32,
        };
        if let Err(e) = source.capture_frame(&rtc_frame).await {
                debug!(error = %e, "capture failed");
        }
}

// Subscribe to new tracks while listening, start a reader for each audio
// track, and mark the connection as lost when the room disconnects.
async fn RunRoomEvents(
        mut events: mpsc::UnboundedReceiver<RoomEvent>,
        receive: Arc<ReceiveState>,
        is_connected: Arc<AtomicBool>,
) {
        while let Some(event) = events.recv().await {
                match event {
                        RoomEvent::TrackPublished {
                                publication,
                                participant,
                        } => {
                                SubscribeIfListening(
                                        &publication,
                                        &participant,
                                        &receive,
                                );
                        }
                        RoomEvent::TrackSubscribed {
                                track,
                                participant,
                                ..
                        } => {
                                StartTrackReader(track, &participant, &receive);
                        }
                        RoomEvent::Disconnected { reason } => {
                                LogDisconnect(reason);
                                break;
                        }
                        _ => {}
                }
        }
        is_connected.store(false, Ordering::Relaxed);
}

// Subscribe to a new audio track of a person while the bot listens.
fn SubscribeIfListening(
        publication: &RemoteTrackPublication,
        participant: &RemoteParticipant,
        receive: &ReceiveState,
) {
        if !receive.isListening() || receive.isOwnBot(participant) { return; }
        if publication.kind() == TrackKind::Audio {
                publication.set_subscribed(true);
        }
}

fn StartTrackReader(
        track: RemoteTrack,
        participant: &RemoteParticipant,
        receive: &Arc<ReceiveState>,
) {
        let RemoteTrack::Audio(audio) = track else { return };
        if receive.isOwnBot(participant) { return; }
        let user = ParseUserId(&participant.identity().0);
        debug!(%user, "reading Fluxer audio track");
        tokio::spawn(RunTrackReader(audio, user, receive.clone()));
}

// Cut the track audio into 20 ms frames and give them to the bridge. Stop
// when the bot stops listening.
async fn RunTrackReader(
        audio: RemoteAudioTrack,
        user: String,
        receive: Arc<ReceiveState>,
) {
        let mut stream = NativeAudioStream::new(
                audio.rtc_track(),
                SAMPLE_RATE as i32,
                CHANNELS as i32,
        );
        let mut pending: Vec<i16> = Vec::with_capacity(FRAME_LEN * 2);
        let mut gate = receive._tuning.NewGate();
        while let Some(chunk) = stream.next().await {
                if !receive.isListening() { break; }
                pending.extend_from_slice(&chunk.data);
                while pending.len() >= FRAME_LEN {
                        let frame =
                                AudioFrame::FromStereo(&pending[..FRAME_LEN]);
                        pending.drain(..FRAME_LEN);
                        receive.Forward(&user, frame, &mut gate);
                }
        }
        debug!(%user, "Fluxer track reader stopped");
}

// ============================================================================
// Helper Functions
// ============================================================================

// A disconnect that the bridge asked for is normal. Any other reason means
// that the connection was lost.
fn LogDisconnect(reason: livekit::DisconnectReason) {
        if reason == livekit::DisconnectReason::ClientInitiated {
                info!("Fluxer voice closed");
        } else {
                warn!(?reason, "Fluxer voice disconnected");
        }
}

fn SetAudioSubscribed(participant: &RemoteParticipant, is_subscribed: bool) {
        for publication in participant.track_publications().values() {
                if publication.kind() == TrackKind::Audio {
                        publication.set_subscribed(is_subscribed);
                }
        }
}

// Fluxer names LiveKit participants "user_<id>_<suffix>". Return the user ID,
// or the full identity if it has another shape.
fn ParseUserId(identity: &str) -> String {
        identity.strip_prefix("user_")
                .and_then(|rest| rest.split_once('_'))
                .map(|(id, _)| id.to_string())
                .unwrap_or_else(|| identity.to_string())
}

#[cfg(test)]
mod tests {
        use super::*;
        use crate::audio::FRAME_TIME;

        #[test]
        fn IdentityGivesUserId() {
                assert_eq!(ParseUserId("user_12345_abc"), "12345");
                assert_eq!(ParseUserId("user_12345_ab_cd"), "12345");
                assert_eq!(ParseUserId("other"), "other");
        }

        #[test]
        fn FrameTimeMatchesFrameLength() {
                assert_eq!(
                        FRAME_TIME.as_millis() as usize * 48 * CHANNELS,
                        FRAME_LEN
                );
        }
}
