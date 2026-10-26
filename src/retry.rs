// ============================================================================
// ============================================================================
// Give the wait time before the next connection attempt. The first attempts
// come quickly, so that a short network problem is over in seconds. Then
// one attempt each minute, and after 5 minutes one attempt each 10 minutes.
// ============================================================================
// ============================================================================

use std::time::Duration;

// Longest wait while the outage is short.
const FAST_RETRY_MAX: Duration = Duration::from_secs(60);
// Outage time after which the attempts become slow.
const SLOW_AFTER: Duration = Duration::from_secs(5 * 60);
// Wait between attempts after a long outage.
const SLOW_RETRY: Duration = Duration::from_secs(10 * 60);

// Return the wait after `failures` failed attempts in a row, when the
// outage started `outage` ago. The fast waits are 2, 4, 8, 16, 32, then
// 60 seconds.
pub fn RetryDelay(failures: u32, outage: Duration) -> Duration {
        if outage >= SLOW_AFTER { return SLOW_RETRY; }
        Duration::from_secs(2u64.saturating_pow(failures.min(6)))
                .min(FAST_RETRY_MAX)
}

#[cfg(test)]
mod tests {
        use super::*;

        #[test]
        fn FirstAttemptsAreFast() {
                let short = Duration::from_secs(10);
                assert_eq!(RetryDelay(1, short), Duration::from_secs(2));
                assert_eq!(RetryDelay(3, short), Duration::from_secs(8));
        }

        #[test]
        fn ShortOutageWaitsOneMinute() {
                let outage = Duration::from_secs(4 * 60);
                assert_eq!(RetryDelay(20, outage), FAST_RETRY_MAX);
        }

        #[test]
        fn LongOutageWaitsTenMinutes() {
                assert_eq!(RetryDelay(1, SLOW_AFTER), SLOW_RETRY);
                assert_eq!(RetryDelay(50, SLOW_AFTER * 3), SLOW_RETRY);
        }
}
