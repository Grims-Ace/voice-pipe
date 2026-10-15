// ============================================================================
// ============================================================================
// Decide which Discord voice channel goes with which Fluxer voice channel:
// first the fixed links from the config, then channels with the same name.
// ============================================================================
// ============================================================================

use std::collections::{HashMap, HashSet};

use super::link::LinkKey;
use crate::config::LinkSettings;
use crate::gateway::VoiceChannel;
use crate::platform::Platform;

// A pair of voice channels that the bridge can link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelPair {
        _key: LinkKey,
        _name: String,
}

impl ChannelPair {
        pub fn getKey(&self) -> &LinkKey { &self._key }
        pub fn getName(&self) -> &str { &self._name }

        fn New(discord: &VoiceChannel, fluxer: &VoiceChannel) -> Self {
                Self {
                        _key: LinkKey::New(discord.getId(), fluxer.getId()),
                        _name: discord.getName().to_string(),
                }
        }
}

// Find all channel pairs. Each channel is in a maximum of one pair.
pub fn FindPairs(
        links: &[LinkSettings],
        is_name_match_enabled: bool,
        discord: &[VoiceChannel],
        fluxer: &[VoiceChannel],
) -> Vec<ChannelPair> {
        let mut used: HashSet<&str> = HashSet::new();
        let mut pairs = FindFixedPairs(links, discord, fluxer, &mut used);
        if is_name_match_enabled {
                pairs.extend(FindNamePairs(discord, fluxer, &mut used));
        }
        pairs
}

// Pair the channels that the config links by ID.
fn FindFixedPairs<'a>(
        links: &[LinkSettings],
        discord: &'a [VoiceChannel],
        fluxer: &'a [VoiceChannel],
        used: &mut HashSet<&'a str>,
) -> Vec<ChannelPair> {
        let mut pairs = Vec::new();
        for link in links {
                let d = discord.iter().find(|c| {
                        c.getId() == link.getChannelId(Platform::Discord)
                });
                let f = fluxer.iter().find(|c| {
                        c.getId() == link.getChannelId(Platform::Fluxer)
                });
                let (Some(d), Some(f)) = (d, f) else { continue };
                if used.contains(d.getId()) || used.contains(f.getId()) {
                        continue;
                }
                used.extend([d.getId(), f.getId()]);
                pairs.push(ChannelPair::New(d, f));
        }
        pairs
}

// Pair the remaining channels that have the same name (outer spaces are
// ignored, but case is sensitive).
fn FindNamePairs<'a>(
        discord: &'a [VoiceChannel],
        fluxer: &'a [VoiceChannel],
        used: &mut HashSet<&'a str>,
) -> Vec<ChannelPair> {
        let mut fluxer_by_name: HashMap<&str, &VoiceChannel> = HashMap::new();
        for channel in fluxer.iter().filter(|c| !used.contains(c.getId())) {
                fluxer_by_name
                        .entry(NormalizeName(channel.getName()))
                        .or_insert(channel);
        }
        let free: Vec<&VoiceChannel> =
                discord.iter().filter(|c| !used.contains(c.getId())).collect();
        let mut pairs = Vec::new();
        for d in free {
                let Some(f) =
                        fluxer_by_name.remove(NormalizeName(d.getName()))
                else {
                        continue;
                };
                used.extend([d.getId(), f.getId()]);
                pairs.push(ChannelPair::New(d, f));
        }
        pairs
}

// Return the names that two or more voice channels share. Name matching
// uses the first of these channels in the channel list.
pub fn FindDuplicateNames(channels: &[VoiceChannel]) -> Vec<String> {
        let mut counts: HashMap<&str, (usize, &str)> = HashMap::new();
        for channel in channels {
                let name = NormalizeName(channel.getName());
                counts.entry(name).or_insert((0, channel.getName())).0 += 1;
        }
        let mut names: Vec<String> = counts
                .into_values()
                .filter(|(count, _)| *count > 1)
                .map(|(_, name)| name.to_string())
                .collect();
        names.sort();
        names
}

fn NormalizeName(name: &str) -> &str { name.trim() }

#[cfg(test)]
mod tests {
        use super::*;

        fn Channels(items: &[(&str, &str)]) -> Vec<VoiceChannel> {
                let indexed = items.iter().enumerate();
                indexed.map(|(i, (id, name))| VoiceChannel::New(id, name, i as i64)).collect()
        }

        fn Link(discord: &str, fluxer: &str) -> LinkSettings {
                let text = format!(
                        "discord_channel_id = \"{discord}\"\n\
                         fluxer_channel_id = \"{fluxer}\""
                );
                toml::from_str(&text).unwrap()
        }

        #[test]
        fn SameNamesArePaired() {
                let d = Channels(&[("1", "General"), ("2", "Games")]);
                let f = Channels(&[("a", " General "), ("b", "Music")]);
                let pairs = FindPairs(&[], true, &d, &f);
                assert_eq!(pairs.len(), 1);
                assert_eq!(pairs[0].getKey(), &LinkKey::New("1", "a"));
        }

        #[test]
        fn FixedLinksComeFirst() {
                let d = Channels(&[("1", "General"), ("2", "Other")]);
                let f = Channels(&[("a", "General"), ("b", "Lobby")]);
                let pairs = FindPairs(&[Link("2", "a")], true, &d, &f);
                assert_eq!(pairs, vec![ChannelPair::New(&d[1], &f[0])]);
        }

        #[test]
        fn CaseMismatchDoesNotPair() {
                let d = Channels(&[("1", "General")]);
                let f = Channels(&[("a", "general")]);
                let pairs = FindPairs(&[], true, &d, &f);
                assert!(pairs.is_empty());
        }

        #[test]
        fn FirstOfTwoSameNamesWins() {
                let d = Channels(&[("1", "General"), ("2", " General ")]);
                let f = Channels(&[("a", "General")]);
                let pairs = FindPairs(&[], true, &d, &f);
                assert_eq!(pairs.len(), 1);
                assert_eq!(pairs[0].getKey(), &LinkKey::New("1", "a"));
                assert_eq!(FindDuplicateNames(&d), ["General".to_string()]);
                assert!(FindDuplicateNames(&f).is_empty());
        }

        #[test]
        fn NameMatchCanBeOff() {
                let d = Channels(&[("1", "General")]);
                let f = Channels(&[("a", "General")]);
                assert!(FindPairs(&[], false, &d, &f).is_empty());
        }
}
