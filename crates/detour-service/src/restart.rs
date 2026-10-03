use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Allows up to `max_restarts` engine restarts within a sliding `window`.
#[derive(Debug, Clone)]
pub struct RestartPolicy {
    max_restarts: u32,
    window: Duration,
    exits: VecDeque<Instant>,
}

impl RestartPolicy {
    pub fn new(max_restarts: u32, window: Duration) -> Self {
        Self {
            max_restarts,
            window,
            exits: VecDeque::new(),
        }
    }

    /// Records an engine exit at `now` and says whether to restart it.
    pub fn allow_restart(&mut self, now: Instant) -> bool {
        while self
            .exits
            .front()
            .is_some_and(|&t| now.duration_since(t) >= self.window)
        {
            self.exits.pop_front();
        }
        self.exits.push_back(now);
        self.exits.len() <= self.max_restarts as usize
    }

    pub fn recent_exits(&self) -> usize {
        self.exits.len()
    }

    pub fn window(&self) -> Duration {
        self.window
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn gives_up_after_limit_within_window() {
        let t = Instant::now();
        let mut p = RestartPolicy::new(2, MIN);
        assert!(p.allow_restart(t));
        assert!(p.allow_restart(t + Duration::from_secs(1)));
        assert!(!p.allow_restart(t + Duration::from_secs(2)));
    }

    #[test]
    fn old_exits_fall_out_of_window() {
        let t = Instant::now();
        let mut p = RestartPolicy::new(1, MIN);
        assert!(p.allow_restart(t));
        assert!(p.allow_restart(t + MIN));
        assert!(p.allow_restart(t + MIN * 2));
        assert_eq!(p.recent_exits(), 1);
    }
}
