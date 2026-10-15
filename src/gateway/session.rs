// ============================================================================
// ============================================================================
// Run the gateway connection of one bot: log in, send heartbeats, read events,
// and connect again (with resume) when the connection drops.
// ============================================================================
// ============================================================================

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as SocketError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{error, info, warn};

use super::protocol::{self, Hello, Incoming, ParseData, Ready};
use super::{ChatMessage, GatewaySettings, GatewayShared};
use crate::platform::Platform;

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

// ============================================================================
// Constants
// ============================================================================

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const HELLO_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_BACKOFF: Duration = Duration::from_secs(60);

// ============================================================================
// Types
// ============================================================================

// Values that let a new connection resume the same session.
#[derive(Default)]
struct SessionInfo {
        _sessionId: Option<String>,
        _resumeUrl: Option<String>,
        _sequence: Option<u64>,
}

// Why a connection ended.
enum ConnectionEnd {
        Reconnect,
        Stop,
        Fatal(String),
}

// The reason that the connection loop woke up.
enum Wake {
        Heartbeat,
        Outgoing(String),
        Stop,
        Frame(Option<Result<Message, SocketError>>),
}

// What the connection loop must do next.
enum LoopAction {
        Continue,
        Send(String),
        Acknowledge,
        End(ConnectionEnd),
}

// The background task that owns the gateway socket of one bot.
pub(super) struct SessionTask {
        _settings: GatewaySettings,
        _shared: Arc<GatewayShared>,
        _outgoing: mpsc::UnboundedReceiver<String>,
        _messages: mpsc::Sender<ChatMessage>,
        _stop: watch::Receiver<bool>,
        _session: SessionInfo,
        _failures: u32,
}

impl SessionTask {
        pub(super) fn New(
                settings: GatewaySettings,
                shared: Arc<GatewayShared>,
                outgoing: mpsc::UnboundedReceiver<String>,
                messages: mpsc::Sender<ChatMessage>,
                stop: watch::Receiver<bool>,
        ) -> Self {
                Self {
                        _settings: settings,
                        _shared: shared,
                        _outgoing: outgoing,
                        _messages: messages,
                        _stop: stop,
                        _session: SessionInfo::default(),
                        _failures: 0,
                }
        }

        // ====================================================================
        // Connection Life Cycle
        // ====================================================================

        // Keep the session open until a stop request or a fatal error.
        pub(super) async fn Run(mut self) {
                let label = self._settings.getLabel().to_string();
                loop {
                        match self.RunConnection().await {
                                ConnectionEnd::Stop => break,
                                ConnectionEnd::Fatal(reason) => {
                                        error!(
                                                gateway = %label,
                                                %reason,
                                                "gateway stopped",
                                        );
                                        break;
                                }
                                ConnectionEnd::Reconnect => {}
                        }
                        self._failures += 1;
                        let wait = Backoff(self._failures);
                        warn!(
                                gateway = %label,
                                wait_secs = wait.as_secs(),
                                "gateway connects again",
                        );
                        if self.WaitOrStop(wait).await { break; }
                }
                self._shared.MarkClosed();
        }

        // Open one socket, log in, and read events until the socket ends.
        async fn RunConnection(&mut self) -> ConnectionEnd {
                let mut socket = match self.Open().await {
                        Ok(socket) => socket,
                        Err(e) => {
                                warn!(
                                        gateway = %self._settings.getLabel(),
                                        error = %e,
                                        "cannot connect",
                                );
                                return ConnectionEnd::Reconnect;
                        }
                };
                let interval = match ReadHello(&mut socket).await {
                        Ok(interval) => interval,
                        Err(e) => {
                                warn!(
                                        gateway = %self._settings.getLabel(),
                                        error = %e,
                                        "no hello",
                                );
                                return ConnectionEnd::Reconnect;
                        }
                };
                if socket.send(Message::text(self.BuildLogin())).await.is_err()
                {
                        return ConnectionEnd::Reconnect;
                }
                let end = self.RunLoop(&mut socket, interval).await;
                if matches!(end, ConnectionEnd::Stop) {
                        self.SayGoodbye(&mut socket).await;
                }
                end
        }

        // Read events, send heartbeats, and send queued payloads.
        async fn RunLoop(
                &mut self,
                socket: &mut Socket,
                interval: Duration,
        ) -> ConnectionEnd {
                let mut heartbeat = tokio::time::interval_at(
                        Instant::now() + interval,
                        interval,
                );
                heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
                let mut is_acked = true;
                loop {
                        let wake = tokio::select! {
                                _ = heartbeat.tick() => Wake::Heartbeat,
                                Some(payload) = self._outgoing.recv() => {
                                        Wake::Outgoing(payload)
                                }
                                _ = self._stop.wait_for(|s| *s) => Wake::Stop,
                                frame = socket.next() => Wake::Frame(frame),
                        };
                        let action = match wake {
                                Wake::Heartbeat => {
                                        self.OnHeartbeatDue(&mut is_acked)
                                }
                                Wake::Outgoing(payload) => {
                                        LoopAction::Send(payload)
                                }
                                Wake::Stop => {
                                        LoopAction::End(ConnectionEnd::Stop)
                                }
                                Wake::Frame(frame) => {
                                        self.HandleSocketFrame(frame)
                                }
                        };
                        match action {
                                LoopAction::Continue => {}
                                LoopAction::Acknowledge => is_acked = true,
                                LoopAction::End(end) => return end,
                                LoopAction::Send(payload) => {
                                        if socket
                                                .send(Message::text(payload))
                                                .await
                                                .is_err()
                                        {
                                                return ConnectionEnd::Reconnect;
                                        }
                                }
                        }
                }
        }

        // Send the queued payloads (such as a voice leave) and close the
        // socket with a normal code, so that the server ends the session.
        async fn SayGoodbye(&mut self, socket: &mut Socket) {
                while let Ok(payload) = self._outgoing.try_recv() {
                        let _ = socket.send(Message::text(payload)).await;
                }
                let frame = CloseFrame {
                        code: CloseCode::Normal,
                        reason: "".into(),
                };
                let _ = socket.close(Some(frame)).await;
        }

        // Wait for the backoff time. Return true if a stop request came.
        async fn WaitOrStop(&mut self, wait: Duration) -> bool {
                tokio::select! {
                        _ = tokio::time::sleep(wait) => false,
                        _ = self._stop.wait_for(|stop| *stop) => true,
                }
        }

        // ====================================================================
        // Frame Handling
        // ====================================================================

        // Stop with a reconnect if the server did not answer the last
        // heartbeat, because the connection is probably dead.
        fn OnHeartbeatDue(&self, is_acked: &mut bool) -> LoopAction {
                if !*is_acked {
                        warn!(
                                gateway = %self._settings.getLabel(),
                                "no heartbeat answer",
                        );
                        return LoopAction::End(ConnectionEnd::Reconnect);
                }
                *is_acked = false;
                LoopAction::Send(protocol::BuildHeartbeat(
                        self._session._sequence,
                ))
        }

        fn HandleSocketFrame(
                &mut self,
                frame: Option<Result<Message, SocketError>>,
        ) -> LoopAction {
                let message = match frame {
                        Some(Ok(message)) => message,
                        Some(Err(e)) => {
                                warn!(
                                        gateway = %self._settings.getLabel(),
                                        error = %e,
                                        "read error",
                                );
                                return LoopAction::End(
                                        ConnectionEnd::Reconnect,
                                );
                        }
                        None => {
                                return LoopAction::End(
                                        ConnectionEnd::Reconnect,
                                );
                        }
                };
                match message {
                        Message::Text(text) => self.HandleText(text.as_str()),
                        Message::Binary(data) => {
                                match std::str::from_utf8(&data) {
                                        Ok(text) => self.HandleText(text),
                                        Err(_) => LoopAction::Continue,
                                }
                        }
                        Message::Close(frame) => self.HandleClose(frame),
                        _ => LoopAction::Continue,
                }
        }

        fn HandleText(&mut self, text: &str) -> LoopAction {
                let Ok(frame) = serde_json::from_str::<Incoming>(text) else {
                        warn!(
                                gateway = %self._settings.getLabel(),
                                "frame is not valid JSON",
                        );
                        return LoopAction::Continue;
                };
                if frame.s.is_some() { self._session._sequence = frame.s; }
                match frame.op {
                        protocol::OP_DISPATCH => self.HandleDispatch(&frame),
                        protocol::OP_HEARTBEAT => {
                                LoopAction::Send(protocol::BuildHeartbeat(
                                        self._session._sequence,
                                ))
                        }
                        protocol::OP_HEARTBEAT_ACK => LoopAction::Acknowledge,
                        protocol::OP_RECONNECT => {
                                LoopAction::End(ConnectionEnd::Reconnect)
                        }
                        protocol::OP_INVALID_SESSION => {
                                self.HandleInvalidSession(&frame.d)
                        }
                        _ => LoopAction::Continue,
                }
        }

        fn HandleDispatch(&mut self, frame: &Incoming) -> LoopAction {
                let kind = frame.t.as_deref().unwrap_or_default();
                match kind {
                        "READY" => self.HandleReady(&frame.d),
                        "RESUMED" => {
                                self._failures = 0;
                                info!(
                                        gateway = %self._settings.getLabel(),
                                        "gateway resumed",
                                );
                        }
                        _ => self._shared.ApplyDispatch(
                                kind,
                                &frame.d,
                                &self._messages,
                        ),
                }
                LoopAction::Continue
        }

        fn HandleReady(&mut self, data: &Value) {
                let Some(ready) = ParseData::<Ready>(data) else { return };
                self._session._sessionId = Some(ready.session_id);
                self._session._resumeUrl = ready.resume_gateway_url;
                self._failures = 0;
                info!(
                        gateway = %self._settings.getLabel(),
                        "gateway logged in",
                );
        }

        // A session that cannot resume must log in again from the start.
        fn HandleInvalidSession(&mut self, data: &Value) -> LoopAction {
                if !data.as_bool().unwrap_or(false) {
                        self._session = SessionInfo::default();
                }
                LoopAction::End(ConnectionEnd::Reconnect)
        }

        // Decide from the close code if the bridge can connect again.
        fn HandleClose(&mut self, frame: Option<CloseFrame>) -> LoopAction {
                let code = frame.map(|f| u16::from(f.code)).unwrap_or(1000);
                let reason = match code {
                        4004 => "the token is not valid",
                        4010..=4012 => "the API version is not accepted",
                        4013 => "the gateway does not accept the intents",
                        // Discord sends this when Message Content is off.
                        4014 => "an intent is not allowed, see the README",
                        4007 | 4009 => {
                                self._session = SessionInfo::default();
                                return LoopAction::End(
                                        ConnectionEnd::Reconnect,
                                );
                        }
                        _ => return LoopAction::End(ConnectionEnd::Reconnect),
                };
                LoopAction::End(ConnectionEnd::Fatal(format!(
                        "close code {code}: {reason}"
                )))
        }

        // ====================================================================
        // Login Helpers
        // ====================================================================

        fn BuildLogin(&self) -> String {
                let token = self._settings.getToken();
                match &self._session._sessionId {
                        Some(id) => protocol::BuildResume(
                                token,
                                id,
                                self._session._sequence,
                        ),
                        None => protocol::BuildIdentify(
                                token,
                                self._settings.getIntents(),
                        ),
                }
        }

        // Use the resume URL from Discord. Fluxer sends its public URL, and
        // the bridge must stay on the local URL, so Fluxer always uses the
        // configured URL.
        fn getConnectUrl(&self) -> String {
                let base = self._settings.getUrl();
                let is_discord =
                        self._settings.getPlatform() == Platform::Discord;
                match (&self._session._sessionId, &self._session._resumeUrl) {
                        (Some(_), Some(resume)) if is_discord => {
                                BuildResumeUrl(resume, base)
                        }
                        _ => base.to_string(),
                }
        }

        async fn Open(&self) -> Result<Socket> {
                let url = self.getConnectUrl();
                let mut request = url.as_str().into_client_request()?;
                if let Some(host) = self._settings.getHostHeader() {
                        request.headers_mut()
                                .insert("host", HeaderValue::from_str(host)?);
                }
                let connect = tokio_tungstenite::connect_async(request);
                let (socket, _) =
                        tokio::time::timeout(CONNECT_TIMEOUT, connect)
                                .await
                                .context("connection timed out")??;
                Ok(socket)
        }
}

// ============================================================================
// Helper Functions
// ============================================================================

// Read frames until the HELLO frame and return the heartbeat interval.
async fn ReadHello(socket: &mut Socket) -> Result<Duration> {
        let read = async {
                while let Some(message) = socket.next().await {
                        let Message::Text(text) = message? else { continue };
                        let frame: Incoming =
                                serde_json::from_str(text.as_str())?;
                        if frame.op != protocol::OP_HELLO { continue; }
                        let hello: Hello = ParseData(&frame.d)
                                .context("HELLO has no interval")?;
                        return Ok(Duration::from_millis(
                                hello.heartbeat_interval.max(1_000),
                        ));
                }
                bail!("socket closed before HELLO")
        };
        tokio::time::timeout(HELLO_TIMEOUT, read)
                .await
                .context("HELLO timed out")?
}

// Add the query of the base URL (version and encoding) to the resume URL.
fn BuildResumeUrl(resume: &str, base: &str) -> String {
        let query = base.split_once('?').map(|(_, q)| q).unwrap_or_default();
        if query.is_empty() || resume.contains('?') {
                return resume.to_string();
        }
        format!("{}/?{query}", resume.trim_end_matches('/'))
}

// Exponential delay: 2, 4, 8, ... seconds, with a 60 second limit.
fn Backoff(failures: u32) -> Duration {
        Duration::from_secs(2u64.saturating_pow(failures.min(6)))
                .min(MAX_BACKOFF)
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn ResumeUrlKeepsQuery() {
                let url = BuildResumeUrl(
                        "wss://resume.discord.gg",
                        "wss://x/?v=10&encoding=json",
                );
                assert_eq!(url, "wss://resume.discord.gg/?v=10&encoding=json");
        }

        #[test]
        fn BackoffHasLimit() {
                assert_eq!(Backoff(1), Duration::from_secs(2));
                assert_eq!(Backoff(20), MAX_BACKOFF);
        }
}
