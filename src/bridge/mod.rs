// ============================================================================
// ============================================================================
// Run the bridge: log in all bots, run the coordinator, and start again with
// a delay when a main bot connection fails.
// ============================================================================
// ============================================================================

mod chat;
mod coordinator;
mod fixed_links;
mod hub;
mod link;
mod matching;
mod overflow;
mod proxy;
mod saved;
mod side;
mod status;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::watch;
use tracing::{error, warn};

use crate::config::Config;
use coordinator::Coordinator;
use fixed_links::FixedLinks;
use hub::Hub;

const MAX_RESTART_DELAY: Duration = Duration::from_secs(120);
// A run that lasts this long counts as healthy, so the restart delay starts
// again from the shortest value.
const HEALTHY_RUN_TIME: Duration = Duration::from_secs(300);

// Run the bridge until `stop` becomes true. Start again after an error. The
// fixed links live outside the restarts, so that links that the commands
// changed stay the same after a restart.
pub async fn Supervise(config: Config, mut stop: watch::Receiver<bool>) {
        let links = FixedLinks::New(config.getLinks(), config.getPath());
        let (config, links) = (Arc::new(config), Arc::new(links));
        let mut failures: u32 = 0;
        loop {
                let started = Instant::now();
                let run = RunOnce(config.clone(), links.clone(), stop.clone());
                let result = run.await;
                if started.elapsed() >= HEALTHY_RUN_TIME { failures = 0; }
                match result {
                        Ok(()) => return,
                        Err(e) => {
                                error!(
                                        error = %format!("{e:#}"),
                                        "bridge stopped with an error",
                                )
                        }
                }
                failures += 1;
                let wait = RestartDelay(failures);
                warn!(wait_secs = wait.as_secs(), "bridge starts again");
                tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = stop.wait_for(|stop| *stop) => return,
                }
        }
}

// Log in, run the coordinator, and log out.
async fn RunOnce(
        config: Arc<Config>,
        links: Arc<FixedLinks>,
        stop: watch::Receiver<bool>,
) -> Result<()> {
        let hub = Hub::Start(config, links).await?;
        let result = Coordinator::New(hub.clone()).Run(stop).await;
        hub.Close().await;
        result
}

// Exponential delay: 4, 8, 16, ... seconds, with a 2 minute limit.
fn RestartDelay(failures: u32) -> Duration {
        Duration::from_secs(2u64.saturating_pow(failures.min(7) + 1))
                .min(MAX_RESTART_DELAY)
}
