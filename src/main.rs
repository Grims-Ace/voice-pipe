// ============================================================================
// ============================================================================
// Start voice-pipe: load the config, start logging, and run the bridge until
// the operating system asks the process to stop.
// ============================================================================
// ============================================================================

// The style guide uses PascalCase for functions and _camelCase for private
// fields. Rust warns about these names, so turn those warnings off.
#![allow(non_snake_case)]

mod audio;
mod bridge;
mod commands;
mod config;
mod gateway;
mod platform;
mod rest;
mod retry;
mod voice;

use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use tokio::sync::watch;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::config::Config;

// ============================================================================
// Constants
// ============================================================================

const DEFAULT_CONFIG_PATH: &str = "config.toml";
// LiveKit and WebRTC make very large futures. A large stack prevents a stack
// overflow when the runtime runs them.
const THREAD_STACK_SIZE: usize = 32 * 1024 * 1024;
// Time limit for the bots to leave voice before the process stops.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);

// ============================================================================
// Start Up
// ============================================================================

fn main() -> Result<()> {
        let handle = thread::Builder::new()
                .name("voice-pipe".into())
                .stack_size(THREAD_STACK_SIZE)
                .spawn(Run)?;
        handle.join().map_err(|_| anyhow!("main thread panicked"))?
}

// Load the config, start logging, and run the bridge until shutdown.
fn Run() -> Result<()> {
        let path = FindConfigPath()?;
        // Start logging before the config is validated, so that the
        // validation warnings are visible.
        let mut config = Config::Read(&path)?;
        StartLogging(config.getGeneral().getLogLevel());
        config.Prepare()?;
        info!(path = %path.display(), "config loaded");
        // Two TLS libraries are in the build, so rustls cannot select a
        // crypto provider without help.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_stack_size(THREAD_STACK_SIZE / 2)
                .build()?;
        runtime.block_on(RunUntilShutdown(config));
        Ok(())
}

// Use the first argument as the config path, or config.toml.
fn FindConfigPath() -> Result<PathBuf> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        ParseConfigPath(&args)
}

fn ParseConfigPath(args: &[String]) -> Result<PathBuf> {
        let path = match args {
                [flag, path, ..] if flag == "--config" => path.as_str(),
                [flag] if flag == "--config" => {
                        return Err(anyhow!("--config needs a path"));
                }
                [path, ..] => path.as_str(),
                [] => DEFAULT_CONFIG_PATH,
        };
        Ok(PathBuf::from(path))
}

// Use RUST_LOG if it is set, else the level from the config. Songbird logs
// an error for each packet that it cannot decode, so turn that log off.
fn StartLogging(level: &str) {
        let filter = EnvFilter::try_from_default_env()
                .or_else(|_| EnvFilter::try_new(level))
                .unwrap_or_else(|_| EnvFilter::new("info"))
                .add_directive(
                        "songbird::driver::tasks::udp_rx=off"
                                .parse()
                                .expect("valid directive"),
                );
        tracing_subscriber::fmt().with_env_filter(filter).init();
}

// ============================================================================
// Shutdown
// ============================================================================

// Run the bridge. On a stop signal, tell it to leave voice and wait for it
// for a limited time.
async fn RunUntilShutdown(config: Config) {
        let (trigger, signal) = watch::channel(false);
        let mut bridge = tokio::spawn(bridge::Supervise(config, signal));
        tokio::select! {
                _ = &mut bridge => {
                        error!("bridge stopped without a stop signal");
                        return;
                }
                _ = WaitForStopSignal() => {}
        }
        info!("stop signal received, bots leave voice");
        let _ = trigger.send(true);
        match tokio::time::timeout(SHUTDOWN_GRACE, bridge).await {
                Ok(_) => info!("bridge stopped"),
                Err(_) => warn!("bridge did not stop in time"),
        }
}

#[cfg(unix)]
async fn WaitForStopSignal() {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate =
                signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = terminate.recv() => {}
        }
}

#[cfg(not(unix))]
async fn WaitForStopSignal() {
        let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
        use super::*;

        fn Args(items: &[&str]) -> Vec<String> {
                items.iter().map(|s| s.to_string()).collect()
        }

        #[test]
        fn ConfigPathComesFromArguments() {
                let path = ParseConfigPath(&Args(&["--config", "a.toml"]));
                assert_eq!(path.unwrap(), PathBuf::from("a.toml"));
                let path = ParseConfigPath(&Args(&["b.toml"]));
                assert_eq!(path.unwrap(), PathBuf::from("b.toml"));
                let path = ParseConfigPath(&[]);
                assert_eq!(path.unwrap(), PathBuf::from(DEFAULT_CONFIG_PATH));
        }

        #[test]
        fn ConfigFlagNeedsAPath() {
                assert!(ParseConfigPath(&Args(&["--config"])).is_err());
        }
}
