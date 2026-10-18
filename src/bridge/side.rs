// ============================================================================
// ============================================================================
// Manage the bridge bots in one voice channel of a link. Each person on the
// other platform gets a mirror here. When there are enough proxies, each
// person gets a proxy that looks like them. When there are not, one group bot
// (the main bot, or a proxy when the main bot is busy) speaks for all of them,
// and the single proxies leave so that other links can use them.
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
use super::proxy::{ProxyAccount, ProxyLook, ProxySession};
use super::status::SideStatus;
use crate::audio::AudioFrame;
use crate::gateway::VoiceFlags;
use crate::platform::Platform;
use crate::voice::{SpeakerFrame, VoiceConnection, VoiceTarget};

// ============================================================================
// Constants
// ============================================================================

// Time limit for the main bot to join voice.
const MAIN_OPEN_TIMEOUT: Duration = Duration::from_secs(30);
// Wait time after the main bot fails to join before it tries again.
const MAIN_RETRY_DELAY: Duration = Duration::from_secs(30);
// Time that enough proxies must be free before a group goes back to single
// proxies, so that bots do not leave and join again and again.
const SPLIT_DELAY: Duration = Duration::from_secs(10);

// ============================================================================
// Types
// ============================================================================

// The result of a connection that a side started in the background. There
// is one event for each connection, so the size of the variants does not
// matter.
#[allow(clippy::large_enum_variant)]
pub enum SideEvent {
        ProxyOpened(Platform, ProxyLook, Result<ProxySession, ProxyAccount>),
        MainOpened(Platform, Option<VoiceConnection>),
}

// The bot that speaks for all users of the other platform.
#[allow(clippy::large_enum_variant)]
enum GroupBot {
        Main(VoiceConnection),
        Proxy(ProxySession),
}

// The bot that receives the audio of the people in this channel.
#[derive(Clone, PartialEq, Eq)]
enum Listener {
        Group,
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
        _userFlags: HashMap<String, VoiceFlags>,
        _group: Option<GroupBot>,
        _groupUsers: HashSet<String>,
        _isMainOpening: bool,
        _isGroupProxyOpening: bool,
        _hasMainSlot: bool,
        _mainRetryAt: Option<Instant>,
        _isSplitting: bool,
        _splitReadySince: Option<Instant>,
        _listener: Option<Listener>,
        _mixer: OverflowMixer,
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

impl GroupBot {
        fn getVoice(&self) -> &VoiceConnection {
                match self {
                        GroupBot::Main(voice) => voice,
                        GroupBot::Proxy(session) => session.getVoice(),
                }
        }
}

impl Side {
        // ====================================================================
        // Accessors and Constructor
        // ====================================================================

        // Return true while a proxy or the main bot is still connecting.
        pub fn hasPendingOpens(&self) -> bool {
                !self._opening.is_empty() || self.isGroupOpening()
        }

        pub fn getStatus(&self) -> SideStatus {
                let mut names: Vec<String> = self
                        ._proxies
                        .values()
                        .map(|s| s.getNickname())
                        .collect();
                names.sort();
                let group_opening = usize::from(self.isGroupOpening());
                SideStatus::New(
                        self._platform,
                        names,
                        self._opening.len() + group_opening,
                        self._groupUsers.len(),
                        self._group.is_some(),
                )
        }

        fn isGroupOpening(&self) -> bool {
                self._isMainOpening || self._isGroupProxyOpening
        }

        // Return true while a group bot is in voice or connects.
        fn isGroupActive(&self) -> bool {
                self._group.is_some() || self.isGroupOpening()
        }

        // Return true if the user has a single proxy that is open or opens.
        fn hasProxy(&self, user: &str) -> bool {
                self._proxies.contains_key(user) || self._opening.contains(user)
        }

        // A group proxy stays in voice for the last user of a split, so
        // that user needs no new proxy.
        fn getSplitReserve(&self) -> usize {
                usize::from(matches!(self._group, Some(GroupBot::Proxy(_))))
        }

        fn getPlatformHub(&self) -> &PlatformHub {
                self._hub.getPlatform(self._platform)
        }

        fn getListenerVoice(&self) -> Option<&VoiceConnection> {
                match self._listener.as_ref()? {
                        Listener::Group => {
                                self._group.as_ref().map(|g| g.getVoice())
                        }
                        Listener::Proxy(user) => {
                                self._proxies.get(user).map(|s| s.getVoice())
                        }
                }
        }

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
                        _userFlags: HashMap::new(),
                        _group: None,
                        _groupUsers: HashSet::new(),
                        _isMainOpening: false,
                        _isGroupProxyOpening: false,
                        _hasMainSlot: false,
                        _mainRetryAt: None,
                        _isSplitting: false,
                        _splitReadySince: None,
                        _listener: None,
                        _mixer: OverflowMixer::New(jitter),
                        _events: events,
                }
        }

        // ====================================================================
        // Mirrors
        // ====================================================================

        // Make the mirrors in this channel match `people`, the people in the
        // linked channel on the other platform. Keep their mute
        // state for ApplyVoiceFlags.
        pub fn Sync(&mut self, people: &HashMap<String, VoiceFlags>) {
                self._userFlags = people.clone();
                let users: HashSet<String> = people.keys().cloned().collect();
                self.SyncUsers(&users);
        }

        // Show the mute state of each person on their single proxy. A group
        // bot speaks for many people, so it never shows a mute.
        pub fn ApplyVoiceFlags(&mut self) {
                let channel = self._key.getChannelId(self._platform);
                for (user, session) in self._proxies.iter_mut() {
                        let flags = self._userFlags.get(user).copied();
                        let flags = flags.unwrap_or_default();
                        session.ShowVoiceFlags(channel, flags);
                }
        }

        fn SyncUsers(&mut self, users: &HashSet<String>) {
                self.RemoveDeparted(users);
                if users.is_empty() {
                        self.ReleaseGroup();
                } else {
                        if !self.isGroupActive() { self.SyncSingles(users); }
                        if self.isGroupActive() { self.SyncGroup(users); }
                }
                self.UpdateGroupUsers(users);
        }

        // Store the result of a background connection. The next Sync closes
        // a single proxy that the group bot made unnecessary.
        pub fn HandleEvent(&mut self, event: SideEvent) {
                match event {
                        SideEvent::ProxyOpened(_, look, Ok(session)) => {
                                self.StoreProxy(look, session)
                        }
                        SideEvent::ProxyOpened(_, look, Err(account)) => {
                                self.ForgetOpening(&look);
                                self.getPlatformHub().HoldProxy(account);
                        }
                        SideEvent::MainOpened(_, Some(voice)) => {
                                self._isMainOpening = false;
                                self._group = Some(GroupBot::Main(voice));
                        }
                        SideEvent::MainOpened(_, None) => {
                                self._isMainOpening = false;
                                self._mainRetryAt =
                                        Some(Instant::now() + MAIN_RETRY_DELAY);
                                self.ReleaseMain(None);
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
                        self.RemoveProxy(&user);
                }
                let Some(group) = &self._group else { return };
                if group.getVoice().isConnected() { return; }
                warn!(
                        platform = %self._platform,
                        "group bot lost its voice connection",
                );
                if matches!(group, GroupBot::Main(_)) {
                        self._mainRetryAt =
                                Some(Instant::now() + MAIN_RETRY_DELAY);
                }
                self.ReleaseGroup();
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
                self._groupUsers.clear();
                if let Some(task) = self.ReleaseGroup() { tasks.push(task); }
                join_all(tasks).await;
        }

        // Record the users that the group bot speaks for. Without a group
        // bot in voice, these are the users that have no mirror.
        fn UpdateGroupUsers(&mut self, users: &HashSet<String>) {
                let has_group = self._group.is_some();
                self._groupUsers = users
                        .iter()
                        .filter(|u| {
                                !self._proxies.contains_key(*u)
                                        && (has_group
                                                || !self._opening.contains(*u))
                        })
                        .cloned()
                        .collect();
        }

        // ====================================================================
        // Single Proxies
        // ====================================================================

        // Give each user a proxy. Start a group bot when proxies run out. Do
        // not open proxies that the group bot would make leave at once.
        fn SyncSingles(&mut self, users: &HashSet<String>) {
                let lacking =
                        users.iter().filter(|u| !self.hasProxy(u)).count();
                let free = self.getPlatformHub().CountFreeProxies();
                if lacking <= free && self.OpenProxies(users, 0) { return; }
                self.StartGroup();
        }

        // Open a single proxy for each user that has none, but leave
        // `reserve` users for the group proxy. Return false when no proxy
        // was free for a user.
        fn OpenProxies(
                &mut self,
                users: &HashSet<String>,
                reserve: usize,
        ) -> bool {
                let mut lacking: Vec<String> = users
                        .iter()
                        .filter(|u| !self.hasProxy(u))
                        .cloned()
                        .collect();
                lacking.sort();
                for user in lacking.into_iter().skip(reserve) {
                        let Some(account) =
                                self.getPlatformHub().AcquireProxy()
                        else {
                                return false;
                        };
                        self.OpenProxy(ProxyLook::User(user), account);
                }
                true
        }

        // Start to connect a proxy in the background.
        fn OpenProxy(&mut self, look: ProxyLook, account: ProxyAccount) {
                match &look {
                        ProxyLook::User(user) => {
                                self._opening.insert(user.clone());
                        }
                        ProxyLook::Group => self._isGroupProxyOpening = true,
                }
                let (hub, platform, events) = (
                        self._hub.clone(),
                        self._platform,
                        self._events.clone(),
                );
                let target = self._target.ForBot(account.getBotId());
                tokio::spawn(async move {
                        let result = ProxySession::Open(
                                &hub, account, &target, platform, &look,
                        )
                        .await;
                        let event =
                                SideEvent::ProxyOpened(platform, look, result);
                        if let Err(failed) = events.send(event).await {
                                DiscardEvent(&hub, failed.0).await;
                        }
                });
        }

        // Keep a proxy that finished its connection.
        fn StoreProxy(&mut self, look: ProxyLook, session: ProxySession) {
                self.ForgetOpening(&look);
                match look {
                        ProxyLook::User(user) => {
                                self._proxies.insert(user, session);
                        }
                        ProxyLook::Group => {
                                self._group = Some(GroupBot::Proxy(session));
                        }
                }
        }

        fn ForgetOpening(&mut self, look: &ProxyLook) {
                match look {
                        ProxyLook::User(user) => {
                                self._opening.remove(user);
                        }
                        ProxyLook::Group => self._isGroupProxyOpening = false,
                }
        }

        // Close the proxies of users that left the linked channel.
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
                        self.RemoveProxy(&user);
                }
        }

        // Close all single proxies, because the group bot speaks for their
        // users now.
        fn CloseSingles(&mut self) {
                if self._proxies.is_empty() { return; }
                info!(
                        platform = %self._platform,
                        count = self._proxies.len(),
                        "group bot is in voice, single proxies leave",
                );
                let users: Vec<String> =
                        self._proxies.keys().cloned().collect();
                for user in users { self.RemoveProxy(&user); }
        }

        // Close the single proxy of a user and give it back to the pool.
        fn RemoveProxy(&mut self, user: &str) {
                self.StopListeningTo(&Listener::Proxy(user.to_string()));
                let Some(session) = self._proxies.remove(user) else { return };
                self.SpawnProxyClose(session);
        }

        // Close a proxy in the background and give its account to the pool.
        fn SpawnProxyClose(&self, session: ProxySession) -> JoinHandle<()> {
                let (hub, platform) = (self._hub.clone(), self._platform);
                tokio::spawn(async move {
                        let account = session.Close().await;
                        hub.getPlatform(platform).ReleaseProxy(account);
                })
        }

        // Change the look of a proxy that is in voice, in the background.
        fn SpawnLookChange(&self, session: &ProxySession, look: ProxyLook) {
                let (hub, platform) = (self._hub.clone(), self._platform);
                let profile = session.getProfile();
                tokio::spawn(async move {
                        profile.Apply(&hub, platform, &look).await;
                });
        }

        // ====================================================================
        // Group Bot
        // ====================================================================

        // Start a bot that speaks for all users: the main bot if it is
        // free, else a proxy that is in this channel, else a free proxy.
        fn StartGroup(&mut self) {
                if self.TryOpenMain() || self.TryPromoteProxy() { return; }
                if let Some(account) = self.getPlatformHub().AcquireProxy() {
                        self.OpenProxy(ProxyLook::Group, account);
                }
        }

        // Keep one bot for all users. When it is in voice, close the single
        // proxies, or go back to single proxies when enough are free.
        fn SyncGroup(&mut self, users: &HashSet<String>) {
                if self._group.is_none() { return; }
                if self._isSplitting {
                        self.ContinueSplit(users);
                        return;
                }
                self.CloseSingles();
                if self.CheckSplitReady(users) { self.StartSplit(users); }
        }

        // Start the main bot as the group bot. Return false if the main bot
        // is busy in another link or must wait after a failure.
        fn TryOpenMain(&mut self) -> bool {
                if self._mainRetryAt.is_some_and(|at| Instant::now() < at) {
                        return false;
                }
                if !self._hasMainSlot {
                        self._hasMainSlot =
                                self.getPlatformHub().ClaimMain(&self._key);
                }
                if !self._hasMainSlot { return false; }
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
                true
        }

        // Make a single proxy of this channel the group bot. It is already
        // in voice, so it only changes its name and avatar.
        fn TryPromoteProxy(&mut self) -> bool {
                let Some(user) = self
                        ._proxies
                        .iter()
                        .find(|(_, s)| s.getVoice().isConnected())
                        .map(|(user, _)| user.clone())
                else {
                        return false;
                };
                self.StopListeningTo(&Listener::Proxy(user.clone()));
                let Some(mut session) = self._proxies.remove(&user) else {
                        return false;
                };
                // A group is many people, so the group bot never shows the
                // mute of the person that this proxy showed before.
                let channel = self._key.getChannelId(self._platform);
                session.ShowVoiceFlags(channel, VoiceFlags::default());
                info!(
                        platform = %self._platform,
                        %user,
                        "main bot is busy, a proxy speaks for the group",
                );
                self.SpawnLookChange(&session, ProxyLook::Group);
                self._group = Some(GroupBot::Proxy(session));
                true
        }

        // Take the group bot out of voice. Return the task that closes it.
        fn ReleaseGroup(&mut self) -> Option<JoinHandle<()>> {
                self.StopListeningTo(&Listener::Group);
                self._mixer.Clear();
                self._isSplitting = false;
                self._splitReadySince = None;
                match self._group.take() {
                        Some(GroupBot::Proxy(session)) => {
                                Some(self.SpawnProxyClose(session))
                        }
                        Some(GroupBot::Main(voice)) => {
                                self.ReleaseMain(Some(voice))
                        }
                        None => self.ReleaseMain(None),
                }
        }

        // Take the main bot out of voice and free it for other links. The
        // slot stays reserved until the bot has left, so that a new join on
        // the same gateway cannot cross the leave.
        fn ReleaseMain(
                &mut self,
                voice: Option<VoiceConnection>,
        ) -> Option<JoinHandle<()>> {
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
        // Split (Group Back to Single Proxies)
        // ====================================================================

        // Return true when enough proxies were free for the full split
        // delay.
        fn CheckSplitReady(&mut self, users: &HashSet<String>) -> bool {
                let needed = users.len().saturating_sub(self.getSplitReserve());
                let free = self.getPlatformHub().CountFreeProxies();
                if free < needed {
                        self._splitReadySince = None;
                        return false;
                }
                let since =
                        *self._splitReadySince.get_or_insert_with(Instant::now);
                since.elapsed() >= SPLIT_DELAY
        }

        fn StartSplit(&mut self, users: &HashSet<String>) {
                info!(
                        platform = %self._platform,
                        "proxies are free, group goes back to single proxies",
                );
                self._isSplitting = true;
                self._splitReadySince = None;
                self.ContinueSplit(users);
        }

        // Open single proxies for the users of the group. The group bot
        // speaks for each user until the proxy of that user is in voice.
        fn ContinueSplit(&mut self, users: &HashSet<String>) {
                let reserve = self.getSplitReserve();
                if !self.OpenProxies(users, reserve) {
                        self.StopSplit();
                        return;
                }
                let waiting = users
                        .iter()
                        .filter(|u| !self._proxies.contains_key(*u))
                        .count();
                if !self._opening.is_empty() || waiting > reserve { return; }
                self.FinishSplit(users);
        }

        // Stop a split when other links took the free proxies.
        fn StopSplit(&mut self) {
                info!(
                        platform = %self._platform,
                        "not enough proxies, the group bot stays",
                );
                self._isSplitting = false;
                self.CloseSingles();
        }

        // Let the main bot leave, or give the group proxy to the last user
        // that has no single proxy.
        fn FinishSplit(&mut self, users: &HashSet<String>) {
                self._isSplitting = false;
                let keeper = users
                        .iter()
                        .find(|u| !self._proxies.contains_key(*u))
                        .cloned();
                info!(platform = %self._platform, "group split finished");
                if !matches!(self._group, Some(GroupBot::Proxy(_))) {
                        self.ReleaseGroup();
                        return;
                }
                self.StopListeningTo(&Listener::Group);
                self._mixer.Clear();
                let Some(GroupBot::Proxy(session)) = self._group.take() else {
                        return;
                };
                let Some(user) = keeper else {
                        self.SpawnProxyClose(session);
                        return;
                };
                self.SpawnLookChange(&session, ProxyLook::User(user.clone()));
                self._proxies.insert(user, session);
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
                if self._group.is_none() || !self._groupUsers.contains(speaker)
                {
                        return false;
                }
                self._mixer.Push(speaker, frame);
                true
        }

        // Send one mixed frame of the group users through the group bot.
        pub fn SendGroupAudio(&mut self) {
                if self._mixer.isEmpty() { return; }
                let Some(frame) = self._mixer.Pull() else { return };
                if let Some(group) = &self._group {
                        group.getVoice().SendFrame(frame);
                }
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

        // Stop the listener if it is `listener`, because its bot leaves or
        // changes its role.
        fn StopListeningTo(&mut self, listener: &Listener) {
                if self._listener.as_ref() == Some(listener) {
                        self.StopListening();
                }
        }

        fn ChooseListener(&self) -> Option<Listener> {
                let group = self._group.as_ref();
                if group.is_some_and(|g| g.getVoice().isConnected()) {
                        return Some(Listener::Group);
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

// Give the main bot the group name and join it to the channel.
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
                        info!(%platform, "main bot joined voice for a group");
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

// Take the main bot out of voice and remove its group nickname.
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
                        hub.getPlatform(platform).ReleaseProxy(account);
                }
                SideEvent::ProxyOpened(platform, _, Err(account)) => {
                        hub.getPlatform(platform).HoldProxy(account)
                }
                // The link already gave back the main slot, so only close the
                // media. A gateway leave could remove the bot from a new link.
                SideEvent::MainOpened(_, Some(voice)) => voice.Close().await,
                SideEvent::MainOpened(_, None) => {}
        }
}
