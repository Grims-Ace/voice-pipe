// ============================================================================
// ============================================================================
// Give one bot a gateway connection that stays open. It tracks voice channels,
// joins and leaves voice, and gives chat messages to the command handler.
// ============================================================================
// ============================================================================

mod protocol;
mod session;
mod state;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::sync::{Notify, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

pub use protocol::{
        INTENT_GUILD_MESSAGES, INTENT_GUILD_VOICE_STATES, INTENT_GUILDS,
        INTENT_MESSAGE_CONTENT,
};
pub use state::VoiceChannel;

use crate::platform::Platform;
use protocol::{
        ChannelData, GuildCreate, MessageData, ParseData, VoiceServerData,
        VoiceStateData,
};
use session::SessionTask;
use state::GatewayState;

// ============================================================================
// Constants
// ============================================================================

// Time limit for the server to send the voice server after a join request.
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
// Wait time between a leave and the second join request.
const REJOIN_DELAY: Duration = Duration::from_secs(2);
// Time limit for the server to confirm that an old voice state is gone.
const STALE_LEAVE_TIMEOUT: Duration = Duration::from_secs(5);
// Time limit for the session task to say goodbye to the server.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);
// Number of chat messages that can wait for the command handler.
const MESSAGE_QUEUE: usize = 64;

// ============================================================================
// Public Types
// ============================================================================

// What a gateway connection needs to log in.
#[derive(Clone)]
pub struct GatewaySettings {
        _label: String,
        _platform: Platform,
        _url: String,
        _token: String,
        _intents: u64,
        _botId: String,
        _hostHeader: Option<String>,
}

// The voice server that a voice library connects to after a join.
#[derive(Clone, Debug)]
pub struct VoiceServer {
        _endpoint: String,
        _token: String,
        _sessionId: Option<String>,
}

// A chat message in a guild text channel.
#[derive(Clone, Debug)]
pub struct ChatMessage {
        _id: String,
        _channelId: String,
        _guildId: String,
        _content: String,
        _authorId: String,
        _isAuthorBot: bool,
        _roleIds: Vec<String>,
}

// A join request that waits for its voice session and voice server.
struct PendingJoin {
        _guildId: String,
        _isSessionRequired: bool,
        _sessionId: Option<String>,
        _server: Option<(String, String)>,
        _sender: oneshot::Sender<VoiceServer>,
}

// State that the gateway handle and the session task share.
struct GatewayShared {
        _botId: String,
        _state: Mutex<GatewayState>,
        _pendingJoin: Mutex<Option<PendingJoin>>,
        _changed: Notify,
        _isClosed: AtomicBool,
}

// A gateway connection for one bot. Drop or close it to log out.
pub struct Gateway {
        _label: String,
        _isSessionRequired: bool,
        _shared: Arc<GatewayShared>,
        _outgoing: mpsc::UnboundedSender<String>,
        _stop: watch::Sender<bool>,
        _task: Mutex<Option<JoinHandle<()>>>,
        _messages: Mutex<Option<mpsc::Receiver<ChatMessage>>>,
}

// ============================================================================
// Gateway Settings
// ============================================================================

impl GatewaySettings {
        pub fn getLabel(&self) -> &str { &self._label }
        pub fn getUrl(&self) -> &str { &self._url }
        pub fn getToken(&self) -> &str { &self._token }
        pub fn getIntents(&self) -> u64 { self._intents }
        pub fn getPlatform(&self) -> Platform { self._platform }
        pub fn getHostHeader(&self) -> Option<&str> {
                self._hostHeader.as_deref()
        }

        // Discord joins need the voice session ID, because Songbird uses it.
        // Fluxer voice needs only the LiveKit server and token.
        pub fn isSessionRequired(&self) -> bool {
                self._platform == Platform::Discord
        }

        pub fn New(
                label: &str,
                platform: Platform,
                url: &str,
                token: &str,
                intents: u64,
                bot_id: &str,
                host_header: Option<&str>,
        ) -> Self {
                Self {
                        _label: label.to_string(),
                        _platform: platform,
                        _url: url.to_string(),
                        _token: token.to_string(),
                        _intents: intents,
                        _botId: bot_id.to_string(),
                        _hostHeader: host_header.map(String::from),
                }
        }
}

// ============================================================================
// Voice Server and Chat Message
// ============================================================================

impl VoiceServer {
        pub fn getEndpoint(&self) -> &str { &self._endpoint }
        pub fn getToken(&self) -> &str { &self._token }
        pub fn getSessionId(&self) -> Option<&str> {
                self._sessionId.as_deref()
        }
}

impl ChatMessage {
        pub fn getId(&self) -> &str { &self._id }
        pub fn getChannelId(&self) -> &str { &self._channelId }
        pub fn getGuildId(&self) -> &str { &self._guildId }
        pub fn getContent(&self) -> &str { &self._content }
        pub fn getAuthorId(&self) -> &str { &self._authorId }
        pub fn isAuthorBot(&self) -> bool { self._isAuthorBot }
        pub fn getRoleIds(&self) -> &[String] { &self._roleIds }

        // Make a message from a MESSAGE_CREATE payload. Return None for
        // direct messages, because commands work only in the guild.
        fn FromData(data: MessageData) -> Option<Self> {
                Some(Self {
                        _id: data.id,
                        _channelId: data.channel_id,
                        _guildId: data.guild_id?,
                        _content: data.content,
                        _authorId: data.author.id,
                        _isAuthorBot: data.author.bot,
                        _roleIds: data
                                .member
                                .map(|m| m.roles)
                                .unwrap_or_default(),
                })
        }
}

// ============================================================================
// Pending Join
// ============================================================================

impl PendingJoin {
        fn isComplete(&self) -> bool {
                self._server.is_some()
                        && (!self._isSessionRequired
                                || self._sessionId.is_some())
        }

        // Send the voice server to the caller that waits for it.
        fn Finish(self) {
                let Some((endpoint, token)) = self._server else { return };
                let server = VoiceServer {
                        _endpoint: endpoint,
                        _token: token,
                        _sessionId: self._sessionId,
                };
                let _ = self._sender.send(server);
        }
}

// ============================================================================
// Gateway Shared State
// ============================================================================

impl GatewayShared {
        fn isClosed(&self) -> bool { self._isClosed.load(Ordering::Acquire) }

        // Mark the gateway as closed and wake all tasks that wait for changes.
        fn MarkClosed(&self) {
                self._isClosed.store(true, Ordering::Release);
                self._changed.notify_waiters();
        }

        // Apply one dispatch event to the voice view, the join request, or
        // the message queue.
        fn ApplyDispatch(
                &self,
                kind: &str,
                data: &Value,
                messages: &mpsc::Sender<ChatMessage>,
        ) {
                match kind {
                        "GUILD_CREATE" => self.ApplyGuildCreate(data),
                        "VOICE_STATE_UPDATE" => self.ApplyVoiceState(data),
                        "VOICE_SERVER_UPDATE" => self.ApplyVoiceServer(data),
                        "CHANNEL_CREATE" | "CHANNEL_UPDATE" => {
                                self.ApplyChannel(data, false)
                        }
                        "CHANNEL_DELETE" => self.ApplyChannel(data, true),
                        "MESSAGE_CREATE" => QueueMessage(data, messages),
                        _ => return,
                }
                self._changed.notify_waiters();
        }

        fn ApplyGuildCreate(&self, data: &Value) {
                let Some(guild) = ParseData::<GuildCreate>(data) else {
                        return;
                };
                debug!(guild = %guild.id, "guild snapshot received");
                self._state.lock().unwrap().ApplyGuildCreate(&guild);
        }

        fn ApplyVoiceState(&self, data: &Value) {
                let Some(voice) = ParseData::<VoiceStateData>(data) else {
                        return;
                };
                if voice.user_id.as_deref() == Some(self._botId.as_str()) {
                        self.SetJoinSession(&voice);
                }
                self._state.lock().unwrap().ApplyVoiceState(&voice);
        }

        fn ApplyVoiceServer(&self, data: &Value) {
                let Some(server) = ParseData::<VoiceServerData>(data) else {
                        return;
                };
                let Some(endpoint) = server.endpoint else { return };
                let mut slot = self._pendingJoin.lock().unwrap();
                let Some(pending) = slot.as_mut() else { return };
                if server
                        .guild_id
                        .as_deref()
                        .is_some_and(|g| g != pending._guildId)
                {
                        return;
                }
                pending._server = Some((endpoint, server.token));
                FinishIfComplete(&mut slot);
        }

        fn ApplyChannel(&self, data: &Value, is_delete: bool) {
                let Some(channel) = ParseData::<ChannelData>(data) else {
                        return;
                };
                let mut state = self._state.lock().unwrap();
                if is_delete {
                        state.ApplyChannelDelete(&channel)
                } else {
                        state.ApplyChannelUpdate(&channel)
                }
        }

        // Save the voice session ID of this bot for the join that waits.
        fn SetJoinSession(&self, voice: &VoiceStateData) {
                if voice.channel_id.is_none() { return; }
                let mut slot = self._pendingJoin.lock().unwrap();
                let Some(pending) = slot.as_mut() else { return };
                pending._sessionId = voice.session_id.clone();
                FinishIfComplete(&mut slot);
        }
}

// ============================================================================
// Gateway
// ============================================================================

impl Gateway {
        pub fn isClosed(&self) -> bool { self._shared.isClosed() }

        fn hasGuild(&self, guild_id: &str) -> bool {
                self._shared._state.lock().unwrap().hasGuild(guild_id)
        }

        pub fn getVoiceChannels(&self, guild_id: &str) -> Vec<VoiceChannel> {
                self._shared._state.lock().unwrap().getVoiceChannels(guild_id)
        }

        // Return the IDs of all users in a voice channel, bots included.
        pub fn getChannelUsers(
                &self,
                guild_id: &str,
                channel_id: &str,
        ) -> HashSet<String> {
                self._shared
                        ._state
                        .lock()
                        .unwrap()
                        .getChannelUsers(guild_id, channel_id)
        }

        // Open the connection in a background task. The task logs in again
        // and resumes the session when the connection drops.
        pub fn Connect(settings: GatewaySettings) -> Gateway {
                let shared = Arc::new(GatewayShared {
                        _botId: settings._botId.clone(),
                        _state: Mutex::new(GatewayState::default()),
                        _pendingJoin: Mutex::new(None),
                        _changed: Notify::new(),
                        _isClosed: AtomicBool::new(false),
                });
                let (outgoing_tx, outgoing_rx) = mpsc::unbounded_channel();
                let (messages_tx, messages_rx) = mpsc::channel(MESSAGE_QUEUE);
                let (stop_tx, stop_rx) = watch::channel(false);
                let label = settings._label.clone();
                let is_session_required = settings.isSessionRequired();
                let task = SessionTask::New(
                        settings,
                        shared.clone(),
                        outgoing_rx,
                        messages_tx,
                        stop_rx,
                );
                Gateway {
                        _label: label,
                        _isSessionRequired: is_session_required,
                        _shared: shared,
                        _outgoing: outgoing_tx,
                        _stop: stop_tx,
                        _task: Mutex::new(Some(tokio::spawn(task.Run()))),
                        _messages: Mutex::new(Some(messages_rx)),
                }
        }

        // Wait until the server sends the snapshot of a guild.
        pub async fn WaitForGuild(
                &self,
                guild_id: &str,
                timeout: Duration,
        ) -> Result<()> {
                let wait = async {
                        loop {
                                let changed = self._shared._changed.notified();
                                if self.isClosed() {
                                        bail!("{} gateway closed", self._label);
                                }
                                if self.hasGuild(guild_id) { return Ok(()); }
                                changed.await;
                        }
                };
                tokio::time::timeout(timeout, wait).await.with_context(|| {
                        let label = &self._label;
                        format!("{label} did not send guild {guild_id}")
                })?
        }

        // Wait until a voice channel, a channel, or a voice user changes.
        pub async fn Changed(&self) { self._shared._changed.notified().await }

        // Take the chat message queue. Only the first caller gets it.
        pub fn TakeMessages(&self) -> Option<mpsc::Receiver<ChatMessage>> {
                self._messages.lock().unwrap().take()
        }

        // Move the bot into a voice channel and wait for the voice server.
        // Discord sends nothing when a join does not change the voice state,
        // so a bot that is still in voice from an old session leaves first,
        // and a join without an answer is tried once more after a leave.
        pub async fn JoinVoice(
                &self,
                guild_id: &str,
                channel_id: &str,
        ) -> Result<VoiceServer> {
                self.LeaveStaleVoice(guild_id).await;
                let error = match self.RequestJoin(guild_id, channel_id).await {
                        Ok(server) => return Ok(server),
                        Err(e) => e,
                };
                warn!(
                        gateway = %self._label,
                        error = %error,
                        "voice join failed, the bot leaves and tries again",
                );
                self.LeaveVoice(guild_id);
                tokio::time::sleep(REJOIN_DELAY).await;
                self.RequestJoin(guild_id, channel_id).await
        }

        // Send one join request and wait for the voice server.
        async fn RequestJoin(
                &self,
                guild_id: &str,
                channel_id: &str,
        ) -> Result<VoiceServer> {
                let (sender, receiver) = oneshot::channel();
                *self._shared._pendingJoin.lock().unwrap() =
                        Some(PendingJoin {
                                _guildId: guild_id.to_string(),
                                _isSessionRequired: self._isSessionRequired,
                                _sessionId: None,
                                _server: None,
                                _sender: sender,
                        });
                let channel = Some(channel_id);
                let payload = protocol::BuildVoiceState(guild_id, channel);
                self.Send(payload)?;
                let label = &self._label;
                let waited = tokio::time::timeout(JOIN_TIMEOUT, receiver).await;
                let Ok(result) = waited else {
                        bail!("{label}: {}", self.DescribeJoinTimeout());
                };
                result.with_context(|| format!("{label} closed during a join"))
        }

        // Tell which part of a join did not arrive, and clear the request.
        fn DescribeJoinTimeout(&self) -> &'static str {
                let pending = self._shared._pendingJoin.lock().unwrap().take();
                let Some(pending) = pending else { return "join cancelled" };
                let has_session = pending._sessionId.is_some();
                match (has_session, pending._server.is_some()) {
                        (false, false) => {
                                "no answer to the join, check the Connect \
                                 permission and the user limit of the channel"
                        }
                        (true, false) => "joined, but got no voice server",
                        (false, true) => "got a voice server, but no session",
                        (true, true) => "the join finished too late",
                }
        }

        // If the server still shows this bot in voice (for example after a
        // crash), leave first and wait until the server confirms it.
        async fn LeaveStaleVoice(&self, guild_id: &str) {
                if self.getBotChannel(guild_id).is_none() { return; }
                info!(gateway = %self._label, "bot is in voice, leaves first");
                self.LeaveVoice(guild_id);
                let wait = async {
                        loop {
                                let changed = self._shared._changed.notified();
                                if self.getBotChannel(guild_id).is_none() {
                                        return;
                                }
                                changed.await;
                        }
                };
                let _ = tokio::time::timeout(STALE_LEAVE_TIMEOUT, wait).await;
        }

        fn getBotChannel(&self, guild_id: &str) -> Option<String> {
                let state = self._shared._state.lock().unwrap();
                state.getUserChannel(guild_id, &self._shared._botId)
        }

        // Take the bot out of voice in a guild.
        pub fn LeaveVoice(&self, guild_id: &str) {
                if let Err(e) =
                        self.Send(protocol::BuildVoiceState(guild_id, None))
                {
                        warn!(
                                gateway = %self._label,
                                error = %e,
                                "cannot send voice leave",
                        );
                }
        }

        // Send the queued messages, log out, and wait for the task to end.
        pub async fn Close(&self) {
                let _ = self._stop.send(true);
                let task = self._task.lock().unwrap().take();
                let Some(task) = task else { return };
                if tokio::time::timeout(CLOSE_TIMEOUT, task).await.is_err() {
                        warn!(
                                gateway = %self._label,
                                "gateway did not close in time",
                        );
                }
        }

        fn Send(&self, payload: String) -> Result<()> {
                if self.isClosed() {
                        bail!("{} gateway is closed", self._label);
                }
                self._outgoing
                        .send(payload)
                        .ok()
                        .context("gateway task stopped")
        }
}

impl Drop for Gateway {
        fn drop(&mut self) {
                if let Some(task) = self._task.lock().unwrap().take() {
                        task.abort();
                }
        }
}

// ============================================================================
// Helper Functions
// ============================================================================

// Complete the join that waits if it has all of its values.
fn FinishIfComplete(slot: &mut Option<PendingJoin>) {
        if !slot.as_ref().is_some_and(|p| p.isComplete()) { return; }
        if let Some(pending) = slot.take() { pending.Finish(); }
}

// Put a chat message in the queue. Drop it if the command handler is slow.
fn QueueMessage(data: &Value, messages: &mpsc::Sender<ChatMessage>) {
        let Some(message) =
                ParseData::<MessageData>(data).and_then(ChatMessage::FromData)
        else {
                return;
        };
        let _ = messages.try_send(message);
}
