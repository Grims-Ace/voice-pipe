// ============================================================================
// ============================================================================
// Record the state of each link and write the report for the status command.
// ============================================================================
// ============================================================================

use std::fmt::Write as _;

use super::hub::Hub;
use super::link::LinkKey;
use crate::audio::FRAME_TIME;
use crate::platform::{PLATFORMS, Platform};

// ============================================================================
// Types
// ============================================================================

// The mirrors in the channel of one platform.
#[derive(Clone, Debug)]
pub struct SideStatus {
        _platform: Platform,
        _proxyNames: Vec<String>,
        _openingCount: usize,
        _groupCount: usize,
        _hasGroupBot: bool,
}

// The state of one link at the last update.
#[derive(Clone, Debug)]
pub struct LinkStatus {
        _key: LinkKey,
        _name: String,
        _discordUsers: usize,
        _fluxerUsers: usize,
        _isAudioActive: bool,
        _discordSide: SideStatus,
        _fluxerSide: SideStatus,
        _framesToDiscord: u64,
        _framesToFluxer: u64,
}

// ============================================================================
// Side Status
// ============================================================================

impl SideStatus {
        pub fn New(
                platform: Platform,
                proxy_names: Vec<String>,
                opening_count: usize,
                group_count: usize,
                has_group_bot: bool,
        ) -> Self {
                Self {
                        _platform: platform,
                        _proxyNames: proxy_names,
                        _openingCount: opening_count,
                        _groupCount: group_count,
                        _hasGroupBot: has_group_bot,
                }
        }

        // Write one line, for example "In Discord: Alice, Multiple Users (2)".
        fn Format(&self, overflow_name: &str) -> String {
                let mut parts = self._proxyNames.clone();
                if self._openingCount > 0 {
                        parts.push(format!("{} joining", self._openingCount));
                }
                if self._groupCount > 0 && self._hasGroupBot {
                        parts.push(format!(
                                "{overflow_name} ({})",
                                self._groupCount
                        ));
                }
                if self._groupCount > 0 && !self._hasGroupBot {
                        parts.push(format!(
                                "{} without a mirror",
                                self._groupCount
                        ));
                }
                if parts.is_empty() { parts.push("nobody".into()); }
                format!(
                        "In {}: {}",
                        self._platform.getTitle(),
                        parts.join(", ")
                )
        }
}

// ============================================================================
// Link Status
// ============================================================================

impl LinkStatus {
        pub fn getKey(&self) -> &LinkKey { &self._key }
        pub fn getName(&self) -> &str { &self._name }

        #[allow(clippy::too_many_arguments)]
        pub fn New(
                key: LinkKey,
                name: &str,
                discord_users: usize,
                fluxer_users: usize,
                is_audio_active: bool,
                discord_side: SideStatus,
                fluxer_side: SideStatus,
                frames_to_discord: u64,
                frames_to_fluxer: u64,
        ) -> Self {
                Self {
                        _key: key,
                        _name: name.to_string(),
                        _discordUsers: discord_users,
                        _fluxerUsers: fluxer_users,
                        _isAudioActive: is_audio_active,
                        _discordSide: discord_side,
                        _fluxerSide: fluxer_side,
                        _framesToDiscord: frames_to_discord,
                        _framesToFluxer: frames_to_fluxer,
                }
        }

        // An empty status for a link that did not report yet.
        pub fn Starting(key: LinkKey, name: &str) -> Self {
                let discord = SideStatus::New(
                        Platform::Discord,
                        Vec::new(),
                        0,
                        0,
                        false,
                );
                let fluxer = SideStatus::New(
                        Platform::Fluxer,
                        Vec::new(),
                        0,
                        0,
                        false,
                );
                Self::New(key, name, 0, 0, false, discord, fluxer, 0, 0)
        }

        fn Format(&self, overflow_name: &str) -> String {
                let relay = if self._isAudioActive {
                        "audio relay on"
                } else {
                        "presence only"
                };
                format!(
                        "**{}** · {relay}\n\
                         Discord users: {} · Fluxer users: {}\n{}\n{}\n\
                         Relayed speech: {} to Discord, {} to Fluxer",
                        self._name,
                        self._discordUsers,
                        self._fluxerUsers,
                        self._discordSide.Format(overflow_name),
                        self._fluxerSide.Format(overflow_name),
                        FormatSeconds(self._framesToDiscord),
                        FormatSeconds(self._framesToFluxer),
                )
        }
}

// ============================================================================
// Report
// ============================================================================

// Write the full report for the status command.
pub fn FormatReport(hub: &Hub, links: &[LinkStatus]) -> String {
        let overflow_name = hub.getConfig().getGeneral().getOverflowName();
        let state =
                if hub.isEnabled() { "on" } else { "off (users are ignored)" };
        let mut text = format!("**voice-pipe status**\nBridge: **{state}**\n");
        for platform in PLATFORMS {
                let _ = writeln!(
                        text,
                        "{}",
                        FormatPlatform(hub, platform, links)
                );
        }
        if links.is_empty() {
                text.push_str("\nNo voice channels are linked now.");
                return text;
        }
        let _ = writeln!(text, "Active links: {}", links.len());
        for link in links {
                let _ = write!(text, "\n{}\n", link.Format(overflow_name));
        }
        text
}

// Write the proxy count and the main bot state of one platform.
fn FormatPlatform(
        hub: &Hub,
        platform: Platform,
        links: &[LinkStatus],
) -> String {
        let side = hub.getPlatform(platform);
        let (free, held, total) = side.getProxyCounts();
        let held = match held {
                0 => String::new(),
                n => format!(" ({n} wait after a failure)"),
        };
        let main = match side.getMainLink() {
                Some(key) => {
                        let name = links
                                .iter()
                                .find(|l| *l.getKey() == key)
                                .map_or("a link", |l| l.getName());
                        format!("group bot in {name}")
                }
                None => "not in voice".to_string(),
        };
        format!(
                "{} proxies free: {free} of {total}{held} · main bot: \
                 {main}",
                platform.getTitle()
        )
}

// Convert a frame count to seconds of speech.
fn FormatSeconds(frames: u64) -> String {
        format!("{:.1} s", frames as f64 * FRAME_TIME.as_secs_f64())
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn SideLineShowsOverflow() {
                let side = SideStatus::New(
                        Platform::Fluxer,
                        vec!["Alice".into()],
                        0,
                        2,
                        true,
                );
                assert_eq!(
                        side.Format("Multiple Users"),
                        "In Fluxer: Alice, Multiple Users (2)"
                );
        }

        #[test]
        fn EmptySideSaysNobody() {
                let side = SideStatus::New(
                        Platform::Discord,
                        Vec::new(),
                        0,
                        0,
                        false,
                );
                assert_eq!(side.Format("x"), "In Discord: nobody");
        }

        #[test]
        fn FramesBecomeSeconds() {
                assert_eq!(FormatSeconds(150), "3.0 s");
        }
}
