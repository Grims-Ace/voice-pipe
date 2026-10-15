// ============================================================================
// ============================================================================
// Run one link: a Discord voice channel and a Fluxer voice channel that the
// bridge joins together. Each link runs in its own task.
// ============================================================================
// ============================================================================

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tracing::info;

use super::hub::Hub;
use super::side::{Side, SideEvent};
use super::status::LinkStatus;
use crate::audio::{AudioFrame, FRAME_TIME};
use crate::platform::Platform;
use crate::voice::SpeakerFrame;

// ============================================================================
// Constants
// ============================================================================

// Time between checks for lost connections.
const CHECK_INTERVAL: Duration = Duration::from_secs(2);
// Time limit to wait for connections that are still opening when a link
// stops, so that no bot stays behind in a channel.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(40);
// Frames that can wait between a listener and the link task.
const HEARD_QUEUE: usize = 256;
const EVENT_QUEUE: usize = 32;

// ============================================================================
// Types
// ============================================================================

// The two channel IDs that identify a link.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LinkKey {
        _discordChannelId: String,
        _fluxerChannelId: String,
}

// The people (bots not included) in the two channels of a link.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Occupancy {
        _discordUsers: HashSet<String>,
        _fluxerUsers: HashSet<String>,
}

// The coordinator's handle to a running link task.
pub struct LinkHandle {
        _occupancy: watch::Sender<Occupancy>,
        _stop: watch::Sender<bool>,
        _status: Arc<Mutex<LinkStatus>>,
        _task: JoinHandle<()>,
}

// The reason that the link task woke up. Size differences do not matter,
// because each value lives for one loop step only.
#[allow(clippy::large_enum_variant)]
enum Wake {
        Stop,
        Occupancy,
        Event(SideEvent),
        Heard(Platform, SpeakerFrame),
        Mix,
        Check,
}

// The state that the link task owns.
struct Link {
        _key: LinkKey,
        _name: String,
        _discord: Side,
        _fluxer: Side,
        _occupancy: watch::Receiver<Occupancy>,
        _stop: watch::Receiver<bool>,
        _events: mpsc::Receiver<SideEvent>,
        _heardOnDiscord: mpsc::Receiver<SpeakerFrame>,
        _heardOnFluxer: mpsc::Receiver<SpeakerFrame>,
        _status: Arc<Mutex<LinkStatus>>,
        _isAudioActive: bool,
        _framesToDiscord: u64,
        _framesToFluxer: u64,
}

// ============================================================================
// Link Key and Occupancy
// ============================================================================

impl LinkKey {
        pub fn getChannelId(&self, platform: Platform) -> &str {
                match platform {
                        Platform::Discord => &self._discordChannelId,
                        Platform::Fluxer => &self._fluxerChannelId,
                }
        }

        pub fn New(discord_channel_id: &str, fluxer_channel_id: &str) -> Self {
                Self {
                        _discordChannelId: discord_channel_id.to_string(),
                        _fluxerChannelId: fluxer_channel_id.to_string(),
                }
        }
}

impl Occupancy {
        pub fn getUsers(&self, platform: Platform) -> &HashSet<String> {
                match platform {
                        Platform::Discord => &self._discordUsers,
                        Platform::Fluxer => &self._fluxerUsers,
                }
        }

        pub fn isEmpty(&self) -> bool {
                self._discordUsers.is_empty() && self._fluxerUsers.is_empty()
        }

        // Audio flows only when people are in both channels.
        pub fn hasBothSides(&self) -> bool {
                !self._discordUsers.is_empty() && !self._fluxerUsers.is_empty()
        }

        pub fn New(
                discord_users: HashSet<String>,
                fluxer_users: HashSet<String>,
        ) -> Self {
                Self {
                        _discordUsers: discord_users,
                        _fluxerUsers: fluxer_users,
                }
        }
}

// ============================================================================
// Link Handle
// ============================================================================

impl LinkHandle {
        pub fn getStatus(&self) -> LinkStatus {
                self._status.lock().unwrap().clone()
        }
        pub fn isFinished(&self) -> bool { self._task.is_finished() }

        // Start the task of a link.
        pub fn Start(
                hub: Arc<Hub>,
                key: LinkKey,
                name: &str,
                occupancy: Occupancy,
        ) -> Self {
                let (occupancy_tx, occupancy_rx) = watch::channel(occupancy);
                let (stop_tx, stop_rx) = watch::channel(false);
                let status = Arc::new(Mutex::new(LinkStatus::Starting(
                        key.clone(),
                        name,
                )));
                let link = Link::New(
                        hub,
                        key,
                        name,
                        occupancy_rx,
                        stop_rx,
                        status.clone(),
                );
                Self {
                        _occupancy: occupancy_tx,
                        _stop: stop_tx,
                        _status: status,
                        _task: tokio::spawn(link.Run()),
                }
        }

        // Give the link the new list of people. Wake it only on a change.
        pub fn UpdateOccupancy(&self, occupancy: Occupancy) {
                self._occupancy.send_if_modified(|current| {
                        if *current == occupancy { return false; }
                        *current = occupancy;
                        true
                });
        }

        pub fn Stop(&self) {
                let _ = self._stop.send(true);
        }

        // Wait until the link task has closed all of its bots.
        pub async fn Wait(self) {
                let _ = self._task.await;
        }
}

// ============================================================================
// Link Task
// ============================================================================

impl Link {
        fn New(
                hub: Arc<Hub>,
                key: LinkKey,
                name: &str,
                occupancy: watch::Receiver<Occupancy>,
                stop: watch::Receiver<bool>,
                status: Arc<Mutex<LinkStatus>>,
        ) -> Self {
                let (events_tx, events_rx) = mpsc::channel(EVENT_QUEUE);
                let (discord_tx, discord_rx) = mpsc::channel(HEARD_QUEUE);
                let (fluxer_tx, fluxer_rx) = mpsc::channel(HEARD_QUEUE);
                Self {
                        _discord: Side::New(
                                hub.clone(),
                                Platform::Discord,
                                key.clone(),
                                discord_tx,
                                events_tx.clone(),
                        ),
                        _fluxer: Side::New(
                                hub,
                                Platform::Fluxer,
                                key.clone(),
                                fluxer_tx,
                                events_tx,
                        ),
                        _key: key,
                        _name: name.to_string(),
                        _occupancy: occupancy,
                        _stop: stop,
                        _events: events_rx,
                        _heardOnDiscord: discord_rx,
                        _heardOnFluxer: fluxer_rx,
                        _status: status,
                        _isAudioActive: false,
                        _framesToDiscord: 0,
                        _framesToFluxer: 0,
                }
        }

        // Keep the mirrors equal to the people in the channels, and relay
        // audio, until the coordinator stops the link.
        async fn Run(mut self) {
                info!(link = %self._name, "link started");
                self.Sync();
                let mut mix_tick = tokio::time::interval(FRAME_TIME);
                mix_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
                let mut check_tick = tokio::time::interval(CHECK_INTERVAL);
                loop {
                        let wake = tokio::select! {
                                _ = self._stop.wait_for(|s| *s) => Wake::Stop,
                                Ok(()) = self._occupancy.changed() => {
                                        Wake::Occupancy
                                }
                                Some(event) = self._events.recv() => {
                                        Wake::Event(event)
                                }
                                Some(heard) = self._heardOnDiscord.recv() => {
                                        Wake::Heard(Platform::Discord, heard)
                                }
                                Some(heard) = self._heardOnFluxer.recv() => {
                                        Wake::Heard(Platform::Fluxer, heard)
                                }
                                _ = mix_tick.tick() => Wake::Mix,
                                _ = check_tick.tick() => Wake::Check,
                        };
                        if !self.HandleWake(wake) { break; }
                }
                let name = self._name.clone();
                self.Close().await;
                info!(link = %name, "link stopped");
        }

        // Do the work for one wake up. Return false when the link must stop.
        fn HandleWake(&mut self, wake: Wake) -> bool {
                match wake {
                        Wake::Stop => return false,
                        Wake::Occupancy => self.Sync(),
                        Wake::Event(event) => {
                                self.HandleEvent(event);
                                self.Sync();
                        }
                        Wake::Heard(platform, (speaker, frame)) => {
                                self.Relay(platform, &speaker, frame)
                        }
                        Wake::Mix => {
                                self._discord.SendOverflow();
                                self._fluxer.SendOverflow();
                        }
                        Wake::Check => self.Check(),
                }
                true
        }

        // Play audio heard on one platform through the mirrors on the other.
        fn Relay(
                &mut self,
                heard_on: Platform,
                speaker: &str,
                frame: AudioFrame,
        ) {
                match heard_on {
                        Platform::Discord => {
                                if self._fluxer.Relay(speaker, frame) {
                                        self._framesToFluxer += 1;
                                }
                        }
                        Platform::Fluxer => {
                                if self._discord.Relay(speaker, frame) {
                                        self._framesToDiscord += 1;
                                }
                        }
                }
        }

        // Make both sides match the people in the opposite channel, and turn
        // listening on only when both channels have people.
        fn Sync(&mut self) {
                let occupancy = self._occupancy.borrow().clone();
                self._discord.Sync(occupancy.getUsers(Platform::Fluxer));
                self._fluxer.Sync(occupancy.getUsers(Platform::Discord));
                let is_active = occupancy.hasBothSides();
                if is_active != self._isAudioActive {
                        info!(
                                link = %self._name,
                                is_active,
                                "audio relay changed",
                        );
                        self._isAudioActive = is_active;
                }
                self._discord.UpdateListener(is_active);
                self._fluxer.UpdateListener(is_active);
                self.PublishStatus(&occupancy);
        }

        // Replace mirrors that lost their connection, and update the status.
        fn Check(&mut self) {
                self._discord.PruneLost();
                self._fluxer.PruneLost();
                self.Sync();
        }

        fn HandleEvent(&mut self, event: SideEvent) {
                match event.getPlatform() {
                        Platform::Discord => self._discord.HandleEvent(event),
                        Platform::Fluxer => self._fluxer.HandleEvent(event),
                }
        }

        fn PublishStatus(&self, occupancy: &Occupancy) {
                let status = LinkStatus::New(
                        self._key.clone(),
                        &self._name,
                        occupancy.getUsers(Platform::Discord).len(),
                        occupancy.getUsers(Platform::Fluxer).len(),
                        self._isAudioActive,
                        self._discord.getStatus(),
                        self._fluxer.getStatus(),
                        self._framesToDiscord,
                        self._framesToFluxer,
                );
                *self._status.lock().unwrap() = status;
        }

        // Wait for connections that are still opening, then close all bots.
        async fn Close(mut self) {
                self.WaitForPendingOpens().await;
                tokio::join!(self._discord.Close(), self._fluxer.Close());
        }

        // Store the results of connections that are still opening, so that
        // Close can disconnect them. Stop at the drain time limit.
        async fn WaitForPendingOpens(&mut self) {
                let deadline = Instant::now() + DRAIN_TIMEOUT;
                while self.hasPendingOpens() {
                        let next = self._events.recv();
                        let waited = tokio::time::timeout_at(deadline, next);
                        let Ok(Some(event)) = waited.await else { return };
                        self.HandleEvent(event);
                }
        }

        fn hasPendingOpens(&self) -> bool {
                self._discord.hasPendingOpens()
                        || self._fluxer.hasPendingOpens()
        }
}
