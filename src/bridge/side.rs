// ============================================================================
// ============================================================================
// Manage the bridge bots in one voice channel of a link. Each person on the
// other platform gets a mirror here: a proxy that looks like them, or the main
// bot as "multiple users" when no proxy is free.
// ============================================================================
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use super::hub::{Hub, PlatformHub};
use super::link::LinkKey;
use super::overflow::OverflowMixer;
use super::proxy::{ProxyAccount, ProxySession};
use super::status::SideStatus;
use crate::audio::AudioFrame;
use crate::platform::Platform;
use crate::voice::{SpeakerFrame, VoiceConnection, VoiceTarget};

// ============================================================================
// Constants
// ============================================================================

// Time limit for the main bot to join voice.
const MAIN_OPEN_TIMEOUT: Duration = Duration::from_secs(30);
// Wait time after the main bot fails to join before it tries again.
const MAIN_RETRY_DELAY: Duration = Duration::from_secs(30);

// ============================================================================
// Types
// ============================================================================

// The result of a connection that a side started in the background. There
// is one event for each connection, so the size of the variants does not
// matter.
#[allow(clippy::large_enum_variant)]
pub enum SideEvent {
        ProxyOpened(Platform, String, Result<ProxySession, ProxyAccount>),
        MainOpened(Platform, Option<VoiceConnection>),
}

// The bot that receives the audio of the people in this channel.
#[derive(Clone, PartialEq, Eq)]
enum Listener {
        Main,
        Proxy(String),
}

// The bridge bots in the channel of one platform.
pub struct Side {
        _hub: Arc<Hub>,
        _platform: Platform,
        _key: LinkKey,
        _target: VoiceTarget,
        _proxies: HashMap<String, ProxySession>,
        _opening: HashSet<String>,
        _overflowUsers: HashSet<String>,
        _main: Option<VoiceConnection>,
        _isMainOpening: bool,
        _hasMainSlot: bool,
        _mainRetryAt: Option<Instant>,
        _listener: Option<Listener>,
        _overflow: OverflowMixer,
        _events: mpsc::Sender<SideEvent>,
}

impl SideEvent {
        pub fn getPlatform(&self) -> Platform {
                match self {
                        SideEvent::ProxyOpened(platform, _, _) => *platform,
                        SideEvent::MainOpened(platform, _) => *platform,
                }
        }
}

impl Side {
        // ====================================================================
        // Accessors
        // ====================================================================

        // Return true while a proxy or the main bot is still connecting.
        pub fn hasPendingOpens(&self) -> bool {
                !self._opening.is_empty() || self._isMainOpening
        }

        pub fn getStatus(&self) -> SideStatus {
                let mut names: Vec<String> = self
                        ._proxies
                        .values()
                        .map(|s| s.getNickname().to_string())
                        .collect();
                names.sort();
                SideStatus::New(
                        self._platform,
                        names,
                        self._opening.len(),
                        self._overflowUsers.len(),
                        self._main.is_some(),
                )
        }

        fn getPlatformHub(&self) -> &PlatformHub {
                self._hub.getPlatform(self._platform)
        }

        fn getListenerVoice(&self) -> Option<&VoiceConnection> {
                match self._listener.as_ref()? {
                        Listener::Main => self._main.as_ref(),
                        Listener::Proxy(user) => {
                                self._proxies.get(user).map(|s| s.getVoice())
                        }
                }
        }

        // ====================================================================
        // Constructor
        // ====================================================================

        // `heard` gets the audio that the listener of this side receives.
        pub fn New(
                hub: Arc<Hub>,
                platform: Platform,
                key: LinkKey,
                heard: mpsc::Sender<SpeakerFrame>,
                events: mpsc::Sender<SideEvent>,
        ) -> Self {
                let side = hub.getPlatform(platform);
                let target = VoiceTarget::New(
                        platform,
                        side.getGuildId(),
                        key.getChannelId(platform),
                        side.getMainBotId(),
                        side.getBotIds(),
                        heard,
                        hub.getTuning(),
                        side.getLivekitUrl(),
                );
                let jitter = hub.getConfig().getGeneral().getJitterFrames();
                Self {
                        _hub: hub,
                        _platform: platform,
                        _key: key,
                        _target: target,
                        _proxies: HashMap::new(),
                        _opening: HashSet::new(),
                        _overflowUsers: HashSet::new(),
                        _main: None,
                        _isMainOpening: false,
                        _hasMainSlot: false,
                        _mainRetryAt: None,
                        _listener: None,
                        _overflow: OverflowMixer::New(jitter),
                        _events: events,
                }
        }

        // ====================================================================
        // Mirrors
        // ====================================================================

        // Make the mirrors in this channel match `users`, the people in the
        // linked channel on the other platform.
        pub fn Sync(&mut self, users: &HashSet<String>) {
                self.RemoveDeparted(users);
                self.OpenProxies(users);
                self.UpdateOverflow(users);
        }

        // Store the result of a background connection.
        pub fn HandleEvent(&mut self, event: SideEvent) {
                match event {
                        SideEvent::ProxyOpened(_, user, Ok(session)) => {
                                self._opening.remove(&user);
                                self._proxies.insert(user, session);
                        }
                        SideEvent::ProxyOpened(_, user, Err(account)) => {
                                self._opening.remove(&user);
                                self.getPlatformHub().RestProxy(account);
                        }
                        SideEvent::MainOpened(_, Some(voice)) => {
                                self._isMainOpening = false;
                                self._main = Some(voice);
                        }
                        SideEvent::MainOpened(_, None) => {
                                self._isMainOpening = false;
                                self._mainRetryAt =
                                        Some(Instant::now() + MAIN_RETRY_DELAY);
                                self.ReleaseMain();
                        }
                }
        }

        // Remove mirrors whose voice connection was lost. The next Sync
        // opens new mirrors for those users.
        pub fn PruneLost(&mut self) {
                let lost: Vec<String> = self
                        ._proxies
                        .iter()
                        .filter(|(_, s)| !s.getVoice().isConnected())
                        .map(|(user, _)| user.clone())
                        .collect();
                for user in lost {
                        warn!(
                                platform = %self._platform,
                                %user,
                                "proxy lost its voice connection",
                        );
                        if let Some(session) = self._proxies.remove(&user) {
                                self.ReleaseProxy(session);
                        }
                }
                if self._main.as_ref().is_some_and(|v| !v.isConnected()) {
                        warn!(
                                platform = %self._platform,
                                "main bot lost its voice connection",
                        );
                        self._mainRetryAt =
                                Some(Instant::now() + MAIN_RETRY_DELAY);
                        self.ReleaseMain();
                }
        }

        // Disconnect all bots of this side and give back the proxies and the
        // main bot.
        pub async fn Close(mut self) {
                self.StopListening();
                let mut tasks: Vec<JoinHandle<()>> = Vec::new();
                let sessions: Vec<ProxySession> =
                        self._proxies.drain().map(|(_, s)| s).collect();
                for session in sessions {
                        tasks.push(self.SpawnProxyClose(session));
                }
                self._overflowUsers.clear();
                if let Some(task) = self.ReleaseMain() { tasks.push(task); }
                join_all(tasks).await;
        }

        fn RemoveDeparted(&mut self, users: &HashSet<String>) {
                let departed: Vec<String> = self
                        ._proxies
                        .keys()
                        .filter(|u| !users.contains(*u))
                        .cloned()
                        .collect();
                for user in departed {
                        info!(
                                platform = %self._platform,
                                %user,
                                "user left, proxy leaves",
                        );
                        if let Some(session) = self._proxies.remove(&user) {
                                self.ReleaseProxy(session);
                        }
                }
        }

        // Start a proxy for each user that has none, while proxies are free.
        fn OpenProxies(&mut self, users: &HashSet<String>) {
                for user in users {
                        if self._proxies.contains_key(user)
                                || self._opening.contains(user)
                        {
                                continue;
                        }
                        let Some(account) =
                                self.getPlatformHub().AcquireProxy()
                        else {
                                return;
                        };
                        self.OpenProxy(user.clone(), account);
                }
        }

        fn OpenProxy(&mut self, user: String, account: ProxyAccount) {
                self._opening.insert(user.clone());
                let (hub, platform, events) = (
                        self._hub.clone(),
                        self._platform,
                        self._events.clone(),
                );
                let target = self._target.ForBot(account.getBotId());
                tokio::spawn(async move {
                        let result = ProxySession::Open(
                                &hub, account, &target, platform, &user,
                        )
                        .await;
                        let event =
                                SideEvent::ProxyOpened(platform, user, result);
                        if let Err(failed) = events.send(event).await {
                                DiscardEvent(&hub, failed.0).await;
                        }
                });
        }

        fn ReleaseProxy(&mut self, session: ProxySession) {
                self.SpawnProxyClose(session);
        }

        fn SpawnProxyClose(&self, session: ProxySession) -> JoinHandle<()> {
                let (hub, platform) = (self._hub.clone(), self._platform);
                tokio::spawn(async move {
                        let account = session.Close().await;
                        hub.getPlatform(platform).ReturnProxy(account);
                })
        }

        // ====================================================================
        // Main Bot (Overflow)
        // ====================================================================

        // Users without a proxy go to the main bot. Bring the main bot in
        // when it has users, and take it out when it has none.
        fn UpdateOverflow(&mut self, users: &HashSet<String>) {
                self._overflowUsers = users
                        .iter()
                        .filter(|u| {
                                !self._proxies.contains_key(*u)
                                        && !self._opening.contains(*u)
                        })
                        .cloned()
                        .collect();
                if self._overflowUsers.is_empty() {
                        self.ReleaseMain();
                        return;
                }
                self.OpenMainIfFree();
        }

        fn OpenMainIfFree(&mut self) {
                if self._main.is_some() || self._isMainOpening { return; }
                if self._mainRetryAt.is_some_and(|at| Instant::now() < at) {
                        return;
                }
                if !self._hasMainSlot {
                        self._hasMainSlot =
                                self.getPlatformHub().ClaimMain(&self._key);
                }
                if !self._hasMainSlot { return; }
                self._isMainOpening = true;
                let (hub, platform, events) = (
                        self._hub.clone(),
                        self._platform,
                        self._events.clone(),
                );
                let target = self._target.clone();
                tokio::spawn(async move {
                        let voice = OpenMain(&hub, platform, &target).await;
                        let event = SideEvent::MainOpened(platform, voice);
                        if let Err(failed) = events.send(event).await {
                                DiscardEvent(&hub, failed.0).await;
                        }
                });
        }

        // Take the main bot out of voice and free it for other links. The
        // slot stays reserved until the bot has left, so that a new join on
        // the same gateway cannot cross the leave.
        fn ReleaseMain(&mut self) -> Option<JoinHandle<()>> {
                if self._listener == Some(Listener::Main) {
                        self._listener = None;
                }
                self._overflow.Clear();
                let voice = self._main.take();
                if !self._hasMainSlot || self._isMainOpening { return None; }
                self._hasMainSlot = false;
                let (hub, platform, key) =
                        (self._hub.clone(), self._platform, self._key.clone());
                Some(tokio::spawn(async move {
                        if let Some(voice) = voice {
                                CloseMain(&hub, platform, voice).await;
                        }
                        hub.getPlatform(platform).ReleaseMain(&key);
                }))
        }

        // ====================================================================
        // Audio
        // ====================================================================

        // Play the audio of a speaker from the other platform through its
        // mirror. Return true if a mirror took the frame.
        pub fn Relay(&mut self, speaker: &str, frame: AudioFrame) -> bool {
                if let Some(session) = self._proxies.get(speaker) {
                        session.getVoice().SendFrame(frame);
                        return true;
                }
                if self._main.is_none()
                        || !self._overflowUsers.contains(speaker)
                {
                        return false;
                }
                self._overflow.Push(speaker, frame);
                true
        }

        // Send one mixed overflow frame through the main bot.
        pub fn PumpOverflow(&mut self) {
                if self._overflow.isEmpty() { return; }
                let Some(frame) = self._overflow.Pull() else { return };
                if let Some(main) = &self._main { main.SendFrame(frame); }
        }

        // Make one bot listen while audio is active, and none while it is
        // not. Choose a new listener when the old one leaves.
        pub fn UpdateListener(&mut self, is_active: bool) {
                if !is_active {
                        self.StopListening();
                        return;
                }
                if self.getListenerVoice().is_some_and(|v| v.isConnected()) {
                        return;
                }
                self.StopListening();
                self._listener = self.ChooseListener();
                let Some(voice) = self.getListenerVoice() else { return };
                voice.SetListening(true);
                info!(platform = %self._platform, "listener chosen");
        }

        fn StopListening(&mut self) {
                if let Some(voice) = self.getListenerVoice() {
                        voice.SetListening(false);
                }
                self._listener = None;
        }

        fn ChooseListener(&self) -> Option<Listener> {
                if self._main.as_ref().is_some_and(|v| v.isConnected()) {
                        return Some(Listener::Main);
                }
                self._proxies
                        .iter()
                        .find(|(_, s)| s.getVoice().isConnected())
                        .map(|(user, _)| Listener::Proxy(user.clone()))
        }
}

// ============================================================================
// Helper Functions
// ============================================================================

// Name the main bot for overflow and join it to the channel.
async fn OpenMain(
        hub: &Hub,
        platform: Platform,
        target: &VoiceTarget,
) -> Option<VoiceConnection> {
        let side = hub.getPlatform(platform);
        let name = hub.getConfig().getGeneral().getOverflowName();
        if let Err(e) = side
                .getRest()
                .SetNickname(side.getGuildId(), side.getMainBotId(), Some(name))
                .await
        {
                warn!(%platform, error = %e, "cannot set main bot nickname");
        }
        let join = VoiceConnection::Join(side.getGateway(), target);
        match tokio::time::timeout(MAIN_OPEN_TIMEOUT, join).await {
                Ok(Ok(voice)) => {
                        info!(%platform, "main bot joined voice for overflow");
                        Some(voice)
                }
                Ok(Err(e)) => {
                        warn!(
                                %platform,
                                error = %format!("{e:#}"),
                                "main bot could not join voice",
                        );
                        None
                }
                Err(_) => {
                        warn!(%platform, "main bot join timed out");
                        side.getGateway().LeaveVoice(side.getGuildId());
                        None
                }
        }
}

// Take the main bot out of voice and remove its overflow nickname.
async fn CloseMain(hub: &Hub, platform: Platform, voice: VoiceConnection) {
        let side = hub.getPlatform(platform);
        voice.Close().await;
        side.getGateway().LeaveVoice(side.getGuildId());
        if let Err(e) = side
                .getRest()
                .SetNickname(side.getGuildId(), side.getMainBotId(), None)
                .await
        {
                warn!(%platform, error = %e, "cannot remove main bot nickname");
        }
        info!(%platform, "main bot left voice");
}

// Clean up a connection that finished after its link stopped.
async fn DiscardEvent(hub: &Hub, event: SideEvent) {
        match event {
                SideEvent::ProxyOpened(platform, _, Ok(session)) => {
                        let account = session.Close().await;
                        hub.getPlatform(platform).ReturnProxy(account);
                }
                SideEvent::ProxyOpened(platform, _, Err(account)) => {
                        hub.getPlatform(platform).RestProxy(account)
                }
                // The link already gave back the main slot, so only close the
                // media. A gateway leave could remove the bot from a new link.
                SideEvent::MainOpened(_, Some(voice)) => voice.Close().await,
                SideEvent::MainOpened(_, None) => {}
        }
}
