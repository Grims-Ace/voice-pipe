// ============================================================================
// ============================================================================
// Answer the chat commands on both platforms: help, status, and toggle, and
// the link, confirm, and unlink commands that change the fixed channel
// links.
// ============================================================================
// ============================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{info, warn};

use super::hub::Hub;
use super::status::{self, LinkStatus};
use crate::commands::{self, Command};
use crate::config::LinkSettings;
use crate::gateway::{ChatMessage, VoiceChannel};
use crate::platform::Platform;

// ============================================================================
// Constants
// ============================================================================

// Time that a user must wait between two commands, so that command spam
// cannot push the main bot into API rate limits.
const COMMAND_COOLDOWN: Duration = Duration::from_secs(3);
// Time that a link request waits for the confirm command.
const LINK_TIMEOUT: Duration = Duration::from_secs(300);
// The reply to a command that needs a command role.
const NO_ROLE: &str = "You need a command role for this.";

// ============================================================================
// Types
// ============================================================================

// A link request: the voice channel where a user sent the link command,
// and the message to reply to when the request times out.
struct PendingLink {
        _platform: Platform,
        _channel: VoiceChannel,
        _messageId: String,
        _since: Instant,
}

// The command handler. It keeps the cooldown of each user and the one link
// request that waits for confirmation.
pub struct ChatCommands {
        _hub: Arc<Hub>,
        _lastCommands: HashMap<String, Instant>,
        _pendingLink: Option<PendingLink>,
}

// ============================================================================
// Command Handler
// ============================================================================

impl ChatCommands {
        fn getPrefix(&self) -> String {
                let general = self._hub.getConfig().getGeneral();
                general.getCommandPrefix().to_string()
        }

        pub fn New(hub: Arc<Hub>) -> Self {
                Self {
                        _hub: hub,
                        _lastCommands: HashMap::new(),
                        _pendingLink: None,
                }
        }

        // Answer a command from the configured guild of a platform. `links`
        // is the state of each running link, for the status command.
        pub fn Handle(
                &mut self,
                platform: Platform,
                message: &ChatMessage,
                links: &[LinkStatus],
        ) {
                let guild_id = self._hub.getPlatform(platform).getGuildId();
                if message.isAuthorBot() || message.getGuildId() != guild_id {
                        return;
                }
                let prefix = self.getPrefix();
                let content = message.getContent();
                let Some(command) = commands::ParseCommand(&prefix, content)
                else {
                        return;
                };
                let user = format!("{platform}:{}", message.getAuthorId());
                if !self.TryStartCommand(&user) { return; }
                info!(
                        %platform,
                        user = %message.getAuthorId(),
                        ?command,
                        "command received",
                );
                let reply = self.Answer(command, platform, message, links);
                let (channel, id) = (message.getChannelId(), message.getId());
                self.SendReply(platform, channel, id, reply);
        }

        // Do the work of a command and return the reply.
        fn Answer(
                &mut self,
                command: Command,
                platform: Platform,
                message: &ChatMessage,
                links: &[LinkStatus],
        ) -> String {
                let prefix = self.getPrefix();
                let hub = &self._hub;
                match command {
                        Command::Help => commands::FormatHelp(&prefix),
                        Command::Status => status::FormatReport(hub, links),
                        Command::Toggle => self.Toggle(message),
                        Command::Link => self.Link(platform, message),
                        Command::Confirm => self.Confirm(platform, message),
                        Command::Unlink => self.Unlink(platform, message),
                        Command::Unknown(name) => {
                                commands::FormatUnknownReply(&prefix, &name)
                        }
                }
        }

        // Drop a link request that waited too long, and tell the user.
        pub fn CheckTimeout(&mut self) {
                let is_due = self
                        ._pendingLink
                        .as_ref()
                        .is_some_and(|p| p._since.elapsed() >= LINK_TIMEOUT);
                if !is_due { return; }
                let Some(pending) = self._pendingLink.take() else { return };
                let name = pending._channel.getName();
                info!(channel = %name, "link request timed out");
                let reply =
                        format!("The link request for **{name}** timed out.");
                let channel = pending._channel.getId();
                let id = &pending._messageId;
                self.SendReply(pending._platform, channel, id, reply);
        }

        // Record a command of a user. Return false while the user is in the
        // cooldown, so that the command is ignored.
        fn TryStartCommand(&mut self, user: &str) -> bool {
                let now = Instant::now();
                self._lastCommands
                        .retain(|_, at| now - *at < COMMAND_COOLDOWN);
                if self._lastCommands.contains_key(user) { return false; }
                self._lastCommands.insert(user.to_string(), now);
                true
        }

        // ====================================================================
        // Commands
        // ====================================================================

        // Turn the bridge off or on. The next Reconcile stops or starts the
        // links.
        fn Toggle(&self, message: &ChatMessage) -> String {
                if !self.hasCommandRole(message) { return NO_ROLE.into(); }
                let is_enabled = !self._hub.isEnabled();
                self._hub.SetEnabled(is_enabled);
                if is_enabled {
                        "Bridge is **on**. The bots mirror users again.".into()
                } else {
                        "Bridge is **off**. The bots leave voice.".into()
                }
        }

        // Start a link request for the voice channel of this text chat. The
        // confirm command on the other platform completes it. A newer
        // request replaces an older one.
        fn Link(
                &mut self,
                platform: Platform,
                message: &ChatMessage,
        ) -> String {
                let prefix = self.getPrefix();
                if !self.hasCommandRole(message) { return NO_ROLE.into(); }
                let Some(channel) =
                        self.FindVoiceChannel(platform, message.getChannelId())
                else {
                        return FormatNotVoice(&prefix);
                };
                if let Some(reply) = self.FindLinkedReply(platform, &channel) {
                        return reply;
                }
                info!(%platform, channel = %channel.getName(), "link request");
                self._pendingLink = Some(PendingLink {
                        _platform: platform,
                        _channel: channel,
                        _messageId: message.getId().to_string(),
                        _since: Instant::now(),
                });
                format!("Listening to `{prefix}confirm`.")
        }

        // Link the voice channel of this text chat with the channel of the
        // link request, and save the link in the config.
        fn Confirm(
                &mut self,
                platform: Platform,
                message: &ChatMessage,
        ) -> String {
                let prefix = self.getPrefix();
                let Some(pending) = &self._pendingLink else {
                        return format!(
                                "No link request waits. Send `{prefix}link` \
                                 in a voice channel first."
                        );
                };
                if pending._platform == platform {
                        let other = platform.getOpposite().getTitle();
                        return format!(
                                "Send `{prefix}confirm` in a voice channel \
                                 on {other}."
                        );
                }
                if !self.hasCommandRole(message) { return NO_ROLE.into(); }
                let Some(channel) =
                        self.FindVoiceChannel(platform, message.getChannelId())
                else {
                        return FormatNotVoice(&prefix);
                };
                if let Some(reply) = self.FindLinkedReply(platform, &channel) {
                        return reply;
                }
                let link = BuildLink(pending, platform, &channel);
                if let Err(e) = self._hub.getFixedLinks().Add(link.clone()) {
                        warn!(error = %format!("{e:#}"), "cannot save link");
                        return "Cannot save the link. See the bridge log."
                                .into();
                }
                self._pendingLink = None;
                let reply = self.FormatLink("Linked", "with", &link);
                info!(link = %reply, "channels linked");
                reply
        }

        // Remove the fixed link of the voice channel of this text chat, from
        // the bridge and from the config.
        fn Unlink(&self, platform: Platform, message: &ChatMessage) -> String {
                let prefix = self.getPrefix();
                if !self.hasCommandRole(message) { return NO_ROLE.into(); }
                let Some(channel) =
                        self.FindVoiceChannel(platform, message.getChannelId())
                else {
                        return FormatNotVoice(&prefix);
                };
                let links = self._hub.getFixedLinks();
                let found = links.FindLink(platform, channel.getId());
                let Some(link) = found else {
                        return self.FormatNotFixed(platform, &channel);
                };
                if let Err(e) = links.Remove(&link) {
                        warn!(error = %format!("{e:#}"), "cannot remove link");
                        return "Cannot remove the link. See the bridge log."
                                .into();
                }
                let reply = self.FormatLink("Unlinked", "from", &link);
                info!(link = %reply, "channels unlinked");
                reply
        }

        // ====================================================================
        // Helper Functions
        // ====================================================================

        fn getVoiceChannels(&self, platform: Platform) -> Vec<VoiceChannel> {
                let side = self._hub.getPlatform(platform);
                side.getGateway().getVoiceChannels(side.getGuildId())
        }

        // Return true if the author can use the commands that change the
        // bridge (toggle, link, confirm, and unlink). The config decides:
        // a role from the command roles of either platform, or everyone
        // when no command roles are set.
        fn hasCommandRole(&self, message: &ChatMessage) -> bool {
                let config = self._hub.getConfig();
                config.isCommandAllowed(message.getRoleIds())
        }

        // Return the voice channel of a text chat. Each voice channel has a
        // text chat with the same ID.
        fn FindVoiceChannel(
                &self,
                platform: Platform,
                channel_id: &str,
        ) -> Option<VoiceChannel> {
                let channels = self.getVoiceChannels(platform);
                channels.into_iter().find(|c| c.getId() == channel_id)
        }

        // Return the reply for a channel that is already in a fixed link, or
        // None if it is free.
        fn FindLinkedReply(
                &self,
                platform: Platform,
                channel: &VoiceChannel,
        ) -> Option<String> {
                let links = self._hub.getFixedLinks();
                links.FindLink(platform, channel.getId())?;
                let prefix = self.getPrefix();
                Some(format!(
                        "**{}** is already linked. Send `{prefix}unlink` \
                         there first.",
                        channel.getName()
                ))
        }

        // Return the name of the channel of a link on a platform, or its ID
        // if the channel is not known.
        fn FindChannelName(
                &self,
                platform: Platform,
                link: &LinkSettings,
        ) -> String {
                let channel_id = link.getChannelId(platform);
                match self.FindVoiceChannel(platform, channel_id) {
                        Some(channel) => channel.getName().to_string(),
                        None => channel_id.to_string(),
                }
        }

        // Write a reply such as "Linked **A** (Discord) with **B** (Fluxer).".
        fn FormatLink(
                &self,
                verb: &str,
                word: &str,
                link: &LinkSettings,
        ) -> String {
                let discord = self.FindChannelName(Platform::Discord, link);
                let fluxer = self.FindChannelName(Platform::Fluxer, link);
                format!(
                        "{verb} **{discord}** (Discord) {word} **{fluxer}** \
                         (Fluxer)."
                )
        }

        // Explain why a channel has no fixed link. A channel with the same
        // name on the other platform is linked by name, which the unlink
        // command cannot change.
        fn FormatNotFixed(
                &self,
                platform: Platform,
                channel: &VoiceChannel,
        ) -> String {
                let general = self._hub.getConfig().getGeneral();
                let name = channel.getName().trim();
                let others = self.getVoiceChannels(platform.getOpposite());
                let has_twin =
                        others.iter().any(|c| c.getName().trim() == name);
                if general.isNameMatchEnabled() && has_twin {
                        return format!(
                                "**{name}** is linked by name, not by a fixed \
                                 link. Rename one of the channels, or set \
                                 `match_by_name = false`."
                        );
                }
                format!("**{name}** has no fixed link.")
        }

        // Send the reply in the background, so that a slow API call does not
        // stop the coordinator.
        fn SendReply(
                &self,
                platform: Platform,
                channel_id: &str,
                message_id: &str,
                reply: String,
        ) {
                let hub = self._hub.clone();
                let (channel, id) =
                        (channel_id.to_string(), message_id.to_string());
                tokio::spawn(async move {
                        let rest = hub.getPlatform(platform).getRest();
                        let sent = rest.SendReply(&channel, &id, &reply).await;
                        if let Err(e) = sent {
                                warn!(%platform, error = %e, "cannot reply");
                        }
                });
        }
}

// Make the link of a request and the channel of the confirm command.
fn BuildLink(
        pending: &PendingLink,
        platform: Platform,
        channel: &VoiceChannel,
) -> LinkSettings {
        let requested = pending._channel.getId();
        let confirmed = channel.getId();
        match platform {
                Platform::Discord => LinkSettings::New(confirmed, requested),
                Platform::Fluxer => LinkSettings::New(requested, confirmed),
        }
}

fn FormatNotVoice(prefix: &str) -> String {
        format!(
                "Send `{prefix}link`, `{prefix}confirm`, and `{prefix}unlink` \
                 in the text chat of a voice channel."
        )
}
