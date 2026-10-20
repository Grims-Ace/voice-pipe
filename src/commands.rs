// ============================================================================
// ============================================================================
// Read the chat commands of the main bots and write the help text.
// ============================================================================
// ============================================================================

// A command that a user on Discord or Fluxer can send.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
        Help,
        Toggle,
        Status,
        Link,
        Confirm,
        Unlink,
        Unknown(String),
}

// Read a command from a message. Return None if the message does not start
// with the prefix. A prefix with no command shows the help.
pub fn ParseCommand(prefix: &str, content: &str) -> Option<Command> {
        let rest = content.trim_start().strip_prefix(prefix)?;
        let name =
                rest.split_whitespace().next().unwrap_or("help").to_lowercase();
        let command = match name.as_str() {
                "help" => Command::Help,
                "toggle" => Command::Toggle,
                "status" => Command::Status,
                "link" => Command::Link,
                "confirm" => Command::Confirm,
                "unlink" => Command::Unlink,
                _ => Command::Unknown(name),
        };
        Some(command)
}

// Write the list of commands for the help command.
pub fn FormatHelp(prefix: &str) -> String {
        format!(
                "**voice-pipe commands**\n\
                 `{prefix}help` · Show this list.\n\
                 `{prefix}toggle` · Turn the bridge off or on. When it is \
                 off, the bots leave voice and ignore users.\n\
                 `{prefix}status` · Show the bridge state, the proxy bots, \
                 and the voice relays.\n\
                 `{prefix}link` · Start to link this voice channel. Then \
                 send `{prefix}confirm` in a voice channel on the other \
                 platform within 5 minutes.\n\
                 `{prefix}confirm` · Link this voice channel with the one \
                 that waits for confirmation.\n\
                 `{prefix}unlink` · Remove the fixed link of this voice \
                 channel.\n\
                 Send link, confirm, and unlink in the text chat of a \
                 voice channel."
        )
}

// Write the reply for a command that does not exist.
pub fn FormatUnknownReply(prefix: &str, name: &str) -> String {
        format!("Unknown command `{name}`. Send `{prefix}help` for a list.")
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn CommandsNeedThePrefix() {
                assert_eq!(
                        ParseCommand("vp!", "vp!status"),
                        Some(Command::Status)
                );
                assert_eq!(
                        ParseCommand("vp!", "vp! TOGGLE now"),
                        Some(Command::Toggle)
                );
                assert_eq!(ParseCommand("vp!", "hello vp!help"), None);
        }

        #[test]
        fn LinkCommandsAreKnown() {
                assert_eq!(ParseCommand("vp!", "vp!link"), Some(Command::Link));
                assert_eq!(
                        ParseCommand("vp!", "vp!Confirm"),
                        Some(Command::Confirm)
                );
                assert_eq!(
                        ParseCommand("vp!", "vp!unlink"),
                        Some(Command::Unlink)
                );
        }

        #[test]
        fn BarePrefixShowsHelp() {
                assert_eq!(ParseCommand("vp!", "vp!"), Some(Command::Help));
        }

        #[test]
        fn OtherWordsAreUnknown() {
                assert_eq!(
                        ParseCommand("!", "!dance"),
                        Some(Command::Unknown("dance".into()))
                );
        }
}
