use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::warn;

/// Sliding-window token-per-minute rate governor.
///
/// Tracks estimated input tokens over the last 60 seconds and delays requests
/// that would push the rate over the configured maximum. This prevents bursting
/// into Anthropic's TPM limit when multiple sub-agents fire concurrently.
///
/// Set `max_tpm = 0` to disable throttling entirely.
pub struct TokenRateGovernor {
    window: Mutex<VecDeque<(Instant, u64)>>,
    pub max_tpm: u64,
}

impl TokenRateGovernor {
    pub fn new(max_tpm: u64) -> Self {
        Self {
            window: Mutex::new(VecDeque::new()),
            max_tpm,
        }
    }

    /// Returns how many tokens are currently in the sliding window (last 60s).
    #[allow(dead_code)]
    pub async fn current_tpm(&self) -> u64 {
        let mut window = self.window.lock().await;
        let cutoff = Instant::now() - Duration::from_secs(60);
        while let Some((t, _)) = window.front() {
            if *t < cutoff { window.pop_front(); } else { break; }
        }
        window.iter().map(|(_, n)| *n).sum()
    }

    /// Wait until the window has capacity for `token_count` more tokens, then record them.
    /// If `max_tpm == 0`, records immediately without any delay.
    pub async fn wait_and_record(&self, token_count: u64) {
        if self.max_tpm == 0 {
            return;
        }

        loop {
            let mut window = self.window.lock().await;
            let now = Instant::now();
            let cutoff = now - Duration::from_secs(60);

            // Evict expired entries
            while let Some((t, _)) = window.front() {
                if *t < cutoff { window.pop_front(); } else { break; }
            }

            let current: u64 = window.iter().map(|(_, n)| *n).sum();

            if current + token_count <= self.max_tpm {
                window.push_back((now, token_count));
                return;
            }

            // Find the earliest moment when enough tokens will have expired
            let needed_to_free = (current + token_count).saturating_sub(self.max_tpm);
            let mut freed = 0u64;
            let mut wait_until = now;
            for (t, n) in window.iter() {
                freed += n;
                // Add 100ms buffer past expiry to avoid spinning on a tight boundary
                wait_until = *t + Duration::from_secs(60) + Duration::from_millis(100);
                if freed >= needed_to_free {
                    break;
                }
            }

            let wait_duration = wait_until.saturating_duration_since(now);
            drop(window);

            warn!(
                wait_ms = wait_duration.as_millis(),
                current_tpm = current,
                request_tokens = token_count,
                max_tpm = self.max_tpm,
                "Rate governor throttling request to stay under TPM limit"
            );
            tokio::time::sleep(wait_duration).await;
        }
    }
}
