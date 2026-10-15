// ============================================================================
// ============================================================================
// Watch the voice channels on both platforms, start a link when people enter
// a linked channel, stop it when both channels are empty, and answer the chat
// commands.
// ============================================================================
// ============================================================================

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use futures_util::future::join_all;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use super::hub::Hub;
use super::link::{LinkHandle, LinkKey, Occupancy};
use super::matching::{self, ChannelPair};
use super::status;
use crate::commands::{self, Command};
use crate::gateway::{ChatMessage, VoiceChannel};
use crate::platform::Platform;

// Time between checks when no gateway event comes.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);
// Time that a link must be unwanted before it stops.
const STOP_DELAY: Duration = Duration::from_secs(5);

// The owner of all link tasks.
pub struct Coordinator {
        _hub: Arc<Hub>,
        _links: HashMap<LinkKey, LinkHandle>,
        _stopping: Vec<(LinkKey, LinkHandle)>,
        _unwantedSince: HashMap<LinkKey, Instant>,
        _warnedNames: HashSet<(Platform, String)>,
        _messages: mpsc::Receiver<ChatMessage>,
}

impl Coordinator {
        pub fn New(hub: Arc<Hub>) -> Self {
                let gateway = hub.getPlatform(Platform::Discord).getGateway();
                let messages = gateway
                        .TakeMessages()
                        .unwrap_or_else(|| mpsc::channel(1).1);
                Self {
                        _hub: hub,
                        _links: HashMap::new(),
                        _stopping: Vec::new(),
                        _unwantedSince: HashMap::new(),
                        _warnedNames: HashSet::new(),
                        _messages: messages,
                }
        }

        // Run until a stop request, or until a main gateway closes.
        pub async fn Run(
                mut self,
                mut stop: watch::Receiver<bool>,
        ) -> Result<()> {
                let hub = self._hub.clone();
                let discord = hub.getPlatform(Platform::Discord).getGateway();
                let fluxer = hub.getPlatform(Platform::Fluxer).getGateway();
                let mut tick = tokio::time::interval(RECONCILE_INTERVAL);
                info!("bridge is watching voice channels");
                let result = loop {
                        if discord.isClosed() || fluxer.isClosed() {
                                break Err(anyhow!(
                                        "a main bot gateway closed"
                                ));
                        }
                        tokio::select! {
                                _ = stop.wait_for(|stop| *stop) => break Ok(()),
                                _ = discord.Changed() => {}
                                _ = fluxer.Changed() => {}
                                _ = tick.tick() => {}
                                Some(message) = self._messages.recv() => {
                                        self.HandleMessage(message)
                                }
                        }
                        self.Reconcile();
                };
                self.StopAll().await;
                result
        }

        // ====================================================================
        // Links
        // ====================================================================

        // Start, update, and stop links to match the people in the channels.
        fn Reconcile(&mut self) {
                self._stopping.retain(|(_, handle)| !handle.isFinished());
                let pairs = self.FindPairs();
                let found: HashSet<LinkKey> =
                        pairs.iter().map(|p| p.getKey().clone()).collect();
                let mut wanted: HashSet<LinkKey> = HashSet::new();
                let is_enabled = self._hub.isEnabled();
                for pair in pairs.iter().filter(|_| is_enabled) {
                        let occupancy = self.FindOccupancy(pair.getKey());
                        if occupancy.isEmpty() {
                                // Let the mirrors leave now. The link task
                                // stops after the stop delay.
                                self.UpdateLink(pair.getKey(), occupancy);
                                continue;
                        }
                        wanted.insert(pair.getKey().clone());
                        self.EnsureLink(pair, occupancy);
                }
                self.StopUnwanted(&wanted, &found);
        }

        // Find the channel pairs, and warn once about channel names that
        // are used two or more times.
        fn FindPairs(&mut self) -> Vec<ChannelPair> {
                let discord = self.FindVoiceChannels(Platform::Discord);
                let fluxer = self.FindVoiceChannels(Platform::Fluxer);
                self.WarnDuplicates(Platform::Discord, &discord);
                self.WarnDuplicates(Platform::Fluxer, &fluxer);
                let config = self._hub.getConfig();
                matching::FindPairs(
                        config.getLinks(),
                        config.getGeneral().isNameMatchEnabled(),
                        &discord,
                        &fluxer,
                )
        }

        fn FindVoiceChannels(&self, platform: Platform) -> Vec<VoiceChannel> {
                let side = self._hub.getPlatform(platform);
                side.getGateway().getVoiceChannels(side.getGuildId())
        }

        fn WarnDuplicates(&mut self, platform: Platform, channels: &[VoiceChannel]) {
                for name in matching::FindDuplicateNames(channels) {
                        if !self._warnedNames.insert((platform, name.clone())) {
                                continue;
                        }
                        warn!(
                                %platform,
                                channel = %name,
                                "two or more voice channels have this name, \
                                 the bridge uses the first one in the list",
                        );
                }
        }

        fn FindOccupancy(&self, key: &LinkKey) -> Occupancy {
                let discord = self._hub.getPlatform(Platform::Discord);
                let fluxer = self._hub.getPlatform(Platform::Fluxer);
                Occupancy::New(
                        discord.getHumans(key.getChannelId(Platform::Discord)),
                        fluxer.getHumans(key.getChannelId(Platform::Fluxer)),
                )
        }

        fn UpdateLink(&self, key: &LinkKey, occupancy: Occupancy) {
                if let Some(handle) = self._links.get(key) {
                        handle.UpdateOccupancy(occupancy);
                }
        }

        // Update a running link, or start it. Do not start a link while its
        // old task still closes, so that two tasks never share channels.
        fn EnsureLink(&mut self, pair: &ChannelPair, occupancy: Occupancy) {
                if let Some(handle) = self._links.get(pair.getKey()) {
                        handle.UpdateOccupancy(occupancy);
                        return;
                }
                if self._stopping.iter().any(|(key, _)| key == pair.getKey()) {
                        return;
                }
                let handle = LinkHandle::Start(
                        self._hub.clone(),
                        pair.getKey().clone(),
                        pair.getName(),
                        occupancy,
                );
                self._links.insert(pair.getKey().clone(), handle);
        }

        // Stop the links that were not wanted for the full stop delay. A
        // short gap (a reconnect, or a channel event) must not stop a link,
        // because a restart makes all of its bots leave and join again.
        // When the bridge is off, stop at once.
        fn StopUnwanted(&mut self, wanted: &HashSet<LinkKey>, found: &HashSet<LinkKey>) {
                let now = Instant::now();
                let is_enabled = self._hub.isEnabled();
                self._unwantedSince.retain(|key, _| !wanted.contains(key));
                let unwanted: Vec<LinkKey> =
                        self._links.keys().filter(|k| !wanted.contains(*k)).cloned().collect();
                for key in unwanted {
                        let since = *self._unwantedSince.entry(key.clone()).or_insert(now);
                        if is_enabled && now - since < STOP_DELAY { continue; }
                        let reason = match (is_enabled, found.contains(&key)) {
                                (false, _) => "bridge is off",
                                (true, false) => "a channel of the link is gone",
                                (true, true) => "both channels are empty",
                        };
                        self.StopLink(key, reason);
                }
        }

        fn StopLink(&mut self, key: LinkKey, reason: &str) {
                self._unwantedSince.remove(&key);
                let Some(handle) = self._links.remove(&key) else { return };
                info!(link = %handle.getStatus().getName(), reason, "link stops");
                handle.Stop();
                self._stopping.push((key, handle));
        }

        // Stop all links and wait until their bots have left.
        async fn StopAll(&mut self) {
                let keys: Vec<LinkKey> = self._links.keys().cloned().collect();
                for key in keys { self.StopLink(key, "bridge shuts down"); }
                let handles = self._stopping.drain(..).map(|(_, h)| h.Wait());
                join_all(handles).await;
        }

        // ====================================================================
        // Commands
        // ====================================================================

        // Answer a command from the configured Discord guild.
        fn HandleMessage(&mut self, message: ChatMessage) {
                let discord = self._hub.getPlatform(Platform::Discord);
                if message.isAuthorBot()
                        || message.getGuildId() != discord.getGuildId()
                {
                        return;
                }
                let prefix = self
                        ._hub
                        .getConfig()
                        .getGeneral()
                        .getCommandPrefix()
                        .to_string();
                let Some(command) =
                        commands::ParseCommand(&prefix, message.getContent())
                else {
                        return;
                };
                info!(
                        user = %message.getAuthorId(),
                        ?command,
                        "command received",
                );
                let reply = match command {
                        Command::Help => commands::BuildHelp(&prefix),
                        Command::Toggle => self.Toggle(&message),
                        Command::Status => self.BuildStatus(),
                        Command::Unknown(name) => {
                                commands::BuildUnknownReply(&prefix, &name)
                        }
                };
                self.SendReply(&message, reply);
        }

        // Turn the bridge off or on. The next Reconcile stops or starts the
        // links.
        fn Toggle(&self, message: &ChatMessage) -> String {
                if !self.canToggle(message) {
                        return "You need a command role for this.".into();
                }
                let is_enabled = !self._hub.isEnabled();
                self._hub.SetEnabled(is_enabled);
                if is_enabled {
                        "Bridge is **on**. The bots mirror users again.".into()
                } else {
                        "Bridge is **off**. The bots leave voice.".into()
                }
        }

        // Everyone can toggle when no command roles are set.
        fn canToggle(&self, message: &ChatMessage) -> bool {
                let allowed = self
                        ._hub
                        .getConfig()
                        .getPlatform(Platform::Discord)
                        .getCommandRoleIds();
                allowed.is_empty()
                        || message
                                .getRoleIds()
                                .iter()
                                .any(|r| allowed.contains(r))
        }

        fn BuildStatus(&self) -> String {
                let mut links: Vec<_> =
                        self._links.values().map(|h| h.getStatus()).collect();
                links.sort_by(|a, b| a.getName().cmp(b.getName()));
                status::FormatReport(&self._hub, &links)
        }

        // Send the reply in the background, so that a slow API call does not
        // stop the coordinator.
        fn SendReply(&self, message: &ChatMessage, reply: String) {
                let hub = self._hub.clone();
                let (channel, id) = (
                        message.getChannelId().to_string(),
                        message.getId().to_string(),
                );
                tokio::spawn(async move {
                        let rest = hub.getPlatform(Platform::Discord).getRest();
                        if let Err(e) =
                                rest.SendReply(&channel, &id, &reply).await
                        {
                                warn!(error = %e, "cannot send command reply");
                        }
                });
        }
}
