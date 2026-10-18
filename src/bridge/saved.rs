// ============================================================================
// ============================================================================
// Read and write the state that must stay the same after a restart: the
// on/off switch of the bridge.
// ============================================================================
// ============================================================================

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tracing::warn;

// The values in the state file.
#[derive(Deserialize, Serialize)]
struct SavedState {
        #[serde(rename = "enabled")]
        _isEnabled: bool,
}

// Return the saved switch state. Return None when no state was saved, or
// when the file is not valid.
pub fn ReadEnabled(path: &Path) -> Option<bool> {
        let text = fs::read_to_string(path).ok()?;
        match toml::from_str::<SavedState>(&text) {
                Ok(state) => Some(state._isEnabled),
                Err(e) => {
                        let path = path.display();
                        warn!(%path, error = %e, "state file is not valid");
                        None
                }
        }
}

// Save the switch state. Errors only cause a warning, because the bridge
// works without the file.
pub fn WriteEnabled(path: &Path, is_enabled: bool) {
        if let Err(e) = WriteState(path, is_enabled) {
                let path = path.display();
                warn!(%path, error = %e, "cannot save the switch state");
        }
}

// Write a temporary file first and rename it, so that a crash during the
// write cannot leave a broken state file.
fn WriteState(path: &Path, is_enabled: bool) -> Result<()> {
        let state = SavedState { _isEnabled: is_enabled };
        let text = toml::to_string(&state)?;
        let temporary = path.with_extension("tmp");
        fs::write(&temporary, text)?;
        fs::rename(&temporary, path)?;
        Ok(())
}

#[cfg(test)]
mod tests {
        use super::*;

        fn TestPath(name: &str) -> std::path::PathBuf {
                let id = std::process::id();
                std::env::temp_dir().join(format!("voice-pipe-{id}-{name}"))
        }

        #[test]
        fn SavedStateReadsBack() {
                let path = TestPath("state.toml");
                WriteEnabled(&path, false);
                assert_eq!(ReadEnabled(&path), Some(false));
                WriteEnabled(&path, true);
                assert_eq!(ReadEnabled(&path), Some(true));
                let _ = fs::remove_file(&path);
        }

        #[test]
        fn MissingOrBrokenFileGivesNone() {
                let path = TestPath("broken.toml");
                assert_eq!(ReadEnabled(&path), None);
                fs::write(&path, "enabled = maybe").unwrap();
                assert_eq!(ReadEnabled(&path), None);
                let _ = fs::remove_file(&path);
        }
}
