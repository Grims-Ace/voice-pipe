// ============================================================================
// ============================================================================
// Keep the fixed channel links of the config, and add or remove them while
// the bridge runs. Each change is saved in the [[links]] list of the config
// file, so that it stays after a restart.
// ============================================================================
// ============================================================================

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, bail};

use crate::config::{Config, LinkSettings};
use crate::platform::Platform;

// The header of one entry of the [[links]] list.
const LINKS_HEADER: &str = "[[links]]";

// ============================================================================
// Types
// ============================================================================

// The fixed links that the bridge uses now, and the config file that keeps
// them.
pub struct FixedLinks {
        _links: Mutex<Vec<LinkSettings>>,
        _configPath: PathBuf,
}

// ============================================================================
// Fixed Links
// ============================================================================

impl FixedLinks {
        pub fn getLinks(&self) -> Vec<LinkSettings> {
                self._links.lock().unwrap().clone()
        }

        // Return the fixed link that uses a channel of a platform, if any.
        pub fn FindLink(
                &self,
                platform: Platform,
                channel_id: &str,
        ) -> Option<LinkSettings> {
                let links = self._links.lock().unwrap();
                let mut found = links.iter();
                found.find(|l| l.hasChannel(platform, channel_id)).cloned()
        }

        pub fn New(links: &[LinkSettings], config_path: &Path) -> Self {
                Self {
                        _links: Mutex::new(links.to_vec()),
                        _configPath: config_path.to_path_buf(),
                }
        }

        // Save a new link in the config file, then use it at once. Refuse a
        // link with a channel that is already in a fixed link, because a
        // channel can be in only one link.
        pub fn Add(&self, link: LinkSettings) -> Result<()> {
                let mut links = self._links.lock().unwrap();
                let is_used = links.iter().any(|l| SharesChannel(l, &link));
                if is_used { bail!("a channel of the link is already linked"); }
                let text = ReadConfigText(&self._configPath)?;
                let changed = AppendLinkText(&text, &link);
                WriteConfigText(&self._configPath, &changed)?;
                links.push(link);
                Ok(())
        }

        // Remove a link from the config file, then stop using it.
        pub fn Remove(&self, link: &LinkSettings) -> Result<()> {
                let mut links = self._links.lock().unwrap();
                let text = ReadConfigText(&self._configPath)?;
                let Some(changed) = RemoveLinkText(&text, link) else {
                        bail!(
                                "the link is not a [[links]] entry in the \
                                 config file, remove it by hand"
                        );
                };
                WriteConfigText(&self._configPath, &changed)?;
                links.retain(|l| l != link);
                Ok(())
        }
}

// ============================================================================
// Config File Edits
// ============================================================================

// Return true if two links use the same Discord or Fluxer channel.
fn SharesChannel(a: &LinkSettings, b: &LinkSettings) -> bool {
        [Platform::Discord, Platform::Fluxer]
                .iter()
                .any(|p| a.hasChannel(*p, b.getChannelId(*p)))
}

fn ReadConfigText(path: &Path) -> Result<String> {
        fs::read_to_string(path)
                .with_context(|| format!("cannot read {}", path.display()))
}

// Add a [[links]] entry at the end of the config text. All other lines,
// comments included, stay the same.
fn AppendLinkText(text: &str, link: &LinkSettings) -> String {
        let mut out = text.to_string();
        if !out.is_empty() && !out.ends_with('\n') { out.push('\n'); }
        let discord = QuoteValue(link.getChannelId(Platform::Discord));
        let fluxer = QuoteValue(link.getChannelId(Platform::Fluxer));
        out.push_str(&format!(
                "\n{LINKS_HEADER}\ndiscord_channel_id = {discord}\n\
                 fluxer_channel_id = {fluxer}\n"
        ));
        out
}

// Write a value as a TOML string, with quotes and escapes.
fn QuoteValue(value: &str) -> String {
        toml::Value::String(value.to_string()).to_string()
}

// Remove the [[links]] entry of a link from the config text. All other
// lines stay the same. Return None if no entry has that link.
fn RemoveLinkText(text: &str, link: &LinkSettings) -> Option<String> {
        let lines: Vec<&str> = text.lines().collect();
        let (start, end) = FindLinkEntry(&lines, link)?;
        // Also remove one empty line before the entry, so that the file
        // does not collect empty lines.
        let start = match start.checked_sub(1) {
                Some(above) if lines[above].trim().is_empty() => above,
                _ => start,
        };
        let mut kept: Vec<&str> = lines[..start].to_vec();
        kept.extend_from_slice(&lines[end..]);
        let mut out = kept.join("\n");
        if text.ends_with('\n') && !out.is_empty() { out.push('\n'); }
        Some(out)
}

// Find the lines of the [[links]] entry of a link: from its header to its
// last key line. Comments after the last key belong to the next part of
// the file, so they stay.
fn FindLinkEntry(
        lines: &[&str],
        link: &LinkSettings,
) -> Option<(usize, usize)> {
        let headers = lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.trim() == LINKS_HEADER)
                .map(|(i, _)| i);
        for start in headers {
                let end = FindEntryEnd(lines, start);
                let body = lines[start + 1..end].join("\n");
                let Ok(entry) = toml::from_str::<LinkSettings>(&body) else {
                        continue;
                };
                if IsSameLink(&entry, link) { return Some((start, end)); }
        }
        None
}

// Return the index after the last key line of the entry that starts at
// `start`. The entry ends at the next table header or at the end of the
// text.
fn FindEntryEnd(lines: &[&str], start: usize) -> usize {
        let next_header = lines[start + 1..]
                .iter()
                .position(|line| line.trim_start().starts_with('['))
                .map_or(lines.len(), |i| start + 1 + i);
        let last_key = lines[start + 1..next_header]
                .iter()
                .rposition(|line| IsKeyLine(line))
                .map_or(start, |i| start + 1 + i);
        last_key + 1
}

// A key line has a value and is not a comment.
fn IsKeyLine(line: &str) -> bool {
        let line = line.trim_start();
        !line.starts_with('#') && line.contains('=')
}

fn IsSameLink(a: &LinkSettings, b: &LinkSettings) -> bool {
        [Platform::Discord, Platform::Fluxer]
                .iter()
                .all(|p| a.hasChannel(*p, b.getChannelId(*p)))
}

// Check that the new text is still a valid config, then replace the file.
// Write a temporary file first and rename it, so that a crash cannot leave
// a broken config. Keep the permissions of the old file, because the
// config holds the bot tokens.
fn WriteConfigText(path: &Path, text: &str) -> Result<()> {
        toml::from_str::<Config>(text)
                .context("the changed config would not be valid")?;
        let permissions = fs::metadata(path)
                .with_context(|| format!("cannot read {}", path.display()))?
                .permissions();
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, text).with_context(|| {
                format!("cannot write {}", temporary.display())
        })?;
        fs::set_permissions(&temporary, permissions)?;
        fs::rename(&temporary, path)
                .with_context(|| format!("cannot replace {}", path.display()))
}

#[cfg(test)]
mod tests {
        use super::*;

        const CONFIG: &str = "# Bridge config\n\
                [discord]\nguild_id = \"1\"\nmain_token = \"t\"\n\n\
                [fluxer]\nguild_id = \"2\"\nmain_token = \"t\"\n\n\
                [[links]]\n# Lobby\ndiscord_channel_id = \"10\"\n\
                fluxer_channel_id = \"20\"\n\n\
                # Second link\n\
                [[links]]\ndiscord_channel_id = \"11\"\n\
                fluxer_channel_id = \"21\"\n";

        fn ParseLinks(text: &str) -> Vec<LinkSettings> {
                let config: Config = toml::from_str(text).unwrap();
                config.getLinks().to_vec()
        }

        #[test]
        fn AppendAddsOneEntry() {
                let link = LinkSettings::New("12", "22");
                let text = AppendLinkText(CONFIG, &link);
                assert!(text.starts_with(CONFIG));
                let links = ParseLinks(&text);
                assert_eq!(links.len(), 3);
                assert_eq!(links[2], link);
        }

        #[test]
        fn RemoveKeepsOtherLinesAndComments() {
                let link = LinkSettings::New("10", "20");
                let text = RemoveLinkText(CONFIG, &link).unwrap();
                assert_eq!(ParseLinks(&text), [LinkSettings::New("11", "21")]);
                assert!(text.contains("# Bridge config"));
                assert!(text.contains("# Second link"));
                assert!(!text.contains("# Lobby"));
        }

        #[test]
        fn RemoveTheLastEntry() {
                let link = LinkSettings::New("11", "21");
                let text = RemoveLinkText(CONFIG, &link).unwrap();
                assert_eq!(ParseLinks(&text), [LinkSettings::New("10", "20")]);
                assert!(text.ends_with('\n'));
        }

        #[test]
        fn UnknownLinkIsNotRemoved() {
                let link = LinkSettings::New("99", "98");
                assert_eq!(RemoveLinkText(CONFIG, &link), None);
        }

        #[test]
        fn LinksThatShareAChannelConflict() {
                let a = LinkSettings::New("10", "20");
                assert!(SharesChannel(&a, &LinkSettings::New("10", "30")));
                assert!(SharesChannel(&a, &LinkSettings::New("40", "20")));
                assert!(!SharesChannel(&a, &LinkSettings::New("40", "30")));
        }

        #[test]
        fn FileChangesKeepThePermissions() {
                let id = std::process::id();
                let path = std::env::temp_dir()
                        .join(format!("voice-pipe-{id}-links.toml"));
                fs::write(&path, CONFIG).unwrap();
                let links = FixedLinks::New(&ParseLinks(CONFIG), &path);
                links.Add(LinkSettings::New("12", "22")).unwrap();
                assert!(links.Add(LinkSettings::New("12", "23")).is_err());
                links.Remove(&LinkSettings::New("10", "20")).unwrap();
                let saved = ParseLinks(&fs::read_to_string(&path).unwrap());
                assert_eq!(saved, links.getLinks());
                assert_eq!(saved.len(), 2);
                let _ = fs::remove_file(&path);
        }
}
