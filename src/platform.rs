// ============================================================================
// ============================================================================
// Identify the two chat platforms that the bridge connects.
// ============================================================================
// ============================================================================

use std::fmt;

// One of the two chat platforms that the bridge connects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
        Discord,
        Fluxer,
}

// All platforms, in the order that logs and reports show them.
pub const PLATFORMS: [Platform; 2] = [Platform::Discord, Platform::Fluxer];

impl Platform {
        // Return the platform on the other side of the bridge.
        pub fn getOpposite(self) -> Platform {
                match self {
                        Platform::Discord => Platform::Fluxer,
                        Platform::Fluxer => Platform::Discord,
                }
        }

        // Return the lowercase name for logs and config keys.
        pub fn getLabel(self) -> &'static str {
                match self {
                        Platform::Discord => "discord",
                        Platform::Fluxer => "fluxer",
                }
        }

        // Return the name for messages that people read.
        pub fn getTitle(self) -> &'static str {
                match self {
                        Platform::Discord => "Discord",
                        Platform::Fluxer => "Fluxer",
                }
        }
}

impl fmt::Display for Platform {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.getLabel())
        }
}
