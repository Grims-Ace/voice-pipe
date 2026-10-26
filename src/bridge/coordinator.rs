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

use futures_util::future::join_all;
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

use super::chat::ChatCommands;
use super::hub::Hub;
use super::link::{LinkHandle, LinkKey, Occupancy};
use super::matching::{self, ChannelPair};
use super::status::LinkStatus;
use crate::gateway::{ChatMessage, VoiceChannel};
use crate::platform::{PLATFORMS, Platform};

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
        _chat: ChatCommands,
        _discordChat: mpsc::Receiver<ChatMessage>,
        _fluxerChat: mpsc::Receiver<ChatMessage>,
}

impl Coordinator {
        pub fn New(hub: Arc<Hub>) -> Self {
                Self {
                        _discordChat: TakeMessages(&hub, Platform::Discord),
                        _fluxerChat: TakeMessages(&hub, Platform::Fluxer),
                        _chat: ChatCommands::New(hub.clone()),
                        _hub: hub,
                        _links: HashMap::new(),
                        _stopping: Vec::new(),
                        _unwantedSince: HashMap::new(),
                        _warnedNames: HashSet::new(),
                }
        }

        // Run until a stop request. A main gateway that loses its connection
        // connects again by itself, so the links keep running.
        pub async fn Run(mut self, mut stop: watch::Receiver<bool>) {
                let hub = self._hub.clone();
                let discord = hub.getPlatform(Platform::Discord).getGateway();
                let fluxer = hub.getPlatform(Platform::Fluxer).getGateway();
                let mut tick = tokio::time::interval(RECONCILE_INTERVAL);
                info!("bridge is watching voice channels");
                loop {
                        let heard = tokio::select! {
                                _ = stop.wait_for(|stop| *stop) => break,
                                _ = discord.Changed() => None,
                                _ = fluxer.Changed() => None,
                                _ = tick.tick() => None,
                                Some(m) = self._discordChat.recv() => {
                                        Some((Platform::Discord, m))
                                }
                                Some(m) = self._fluxerChat.recv() => {
                                        Some((Platform::Fluxer, m))
                                }
                        };
                        if let Some((platform, message)) = heard {
                                self.HandleMessage(platform, &message);
                        }
                        self._chat.CheckTimeout();
                        self.Reconcile();
                }
                self.StopAll().await;
        }

        // ====================================================================
        // Links
        // ====================================================================

        // Start, update, and stop links to match the people in the channels,
        // and show the number of people on the main bots.
        fn Reconcile(&mut self) {
                self._stopping.retain(|(_, handle)| !handle.isFinished());
                let pairs = self.FindPairs();
                let found: HashSet<LinkKey> =
                        pairs.iter().map(|p| p.getKey().clone()).collect();
                let mut wanted: HashSet<LinkKey> = HashSet::new();
                let mut people = 0;
                let is_enabled = self._hub.isEnabled();
                for pair in pairs.iter().filter(|_| is_enabled) {
                        let occupancy = self.FindOccupancy(pair.getKey());
                        if occupancy.isEmpty() {
                                // Let the mirrors leave now. The link task
                                // stops after the stop delay.
                                self.UpdateLink(pair.getKey(), occupancy);
                                continue;
                        }
                        people += occupancy.CountPeople();
                        wanted.insert(pair.getKey().clone());
                        self.EnsureLink(pair, occupancy);
                }
                self.StopUnwanted(&wanted, &found);
                self._hub.ShowMainStatus(people);
        }

        // Find the channel pairs, and warn once about channel names that
        // are used two or more times.
        fn FindPairs(&mut self) -> Vec<ChannelPair> {
                let discord = self.FindVoiceChannels(Platform::Discord);
                let fluxer = self.FindVoiceChannels(Platform::Fluxer);
                self.WarnDuplicates(Platform::Discord, &discord);
                self.WarnDuplicates(Platform::Fluxer, &fluxer);
                let afk = self.FindAfkChannelIds();
                let skipped: Vec<&str> =
                        afk.iter().map(String::as_str).collect();
                let config = self._hub.getConfig();
                let fixed = self._hub.getFixedLinks().getLinks();
                matching::FindPairs(
                        &fixed,
                        config.getGeneral().isNameMatchEnabled(),
                        &discord,
                        &fluxer,
                        &skipped,
                )
        }

        // Return the AFK channels of both guilds, or none when the config
        // does not ignore them.
        fn FindAfkChannelIds(&self) -> Vec<String> {
                if !self._hub.getConfig().getGeneral().isAfkIgnored() {
                        return Vec::new();
                }
                PLATFORMS
                        .iter()
                        .filter_map(|platform| {
                                let side = self._hub.getPlatform(*platform);
                                let gateway = side.getGateway();
                                gateway.getAfkChannelId(side.getGuildId())
                        })
                        .collect()
        }

        fn FindVoiceChannels(&self, platform: Platform) -> Vec<VoiceChannel> {
                let side = self._hub.getPlatform(platform);
                side.getGateway().getVoiceChannels(side.getGuildId())
        }

        fn WarnDuplicates(
                &mut self,
                platform: Platform,
                channels: &[VoiceChannel],
        ) {
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
        fn StopUnwanted(
                &mut self,
                wanted: &HashSet<LinkKey>,
                found: &HashSet<LinkKey>,
        ) {
                let now = Instant::now();
                let is_enabled = self._hub.isEnabled();
                self._unwantedSince.retain(|key, _| !wanted.contains(key));
                let unwanted: Vec<LinkKey> = self
                        ._links
                        .keys()
                        .filter(|k| !wanted.contains(*k))
                        .cloned()
                        .collect();
                for key in unwanted {
                        let since = *self
                                ._unwantedSince
                                .entry(key.clone())
                                .or_insert(now);
                        if is_enabled && now - since < STOP_DELAY { continue; }
                        let reason = match (is_enabled, found.contains(&key)) {
                                (false, _) => "bridge is off",
                                (true, false) => {
                                        "a channel of the link is gone"
                                }
                                (true, true) => "both channels are empty",
                        };
                        self.StopLink(key, reason);
                }
        }

        fn StopLink(&mut self, key: LinkKey, reason: &str) {
                self._unwantedSince.remove(&key);
                let Some(handle) = self._links.remove(&key) else { return };
                let name = handle.getStatus().getName().to_string();
                info!(link = %name, reason, "link stops");
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

        // Give a chat message to the command handler, with the state of the
        // running links for the status command.
        fn HandleMessage(&mut self, platform: Platform, message: &ChatMessage) {
                let links = self.getLinkStatuses();
                self._chat.Handle(platform, message, &links);
        }

        // Return the state of each running link, in name order.
        fn getLinkStatuses(&self) -> Vec<LinkStatus> {
                let mut links: Vec<LinkStatus> =
                        self._links.values().map(|h| h.getStatus()).collect();
                links.sort_by(|a, b| a.getName().cmp(b.getName()));
                links
        }
}

// Take the chat message queue of the main bot of a platform. A queue that
// another coordinator already took gives an empty queue.
fn TakeMessages(hub: &Hub, platform: Platform) -> mpsc::Receiver<ChatMessage> {
        let gateway = hub.getPlatform(platform).getGateway();
        gateway.TakeMessages().unwrap_or_else(|| mpsc::channel(1).1)
}
