use std::{time::{Duration}};


/// Aggregates rolling ping statistics and history for rendering the ASCII sparkline output.
pub(crate) struct State {

    // Holds the most recent ping results for graphing.
    pub(crate) samples: Vec<Option<Duration>>,

    // Number of pings that have been recorded overall.
    pub(crate) index: usize,

    // The number of echo requests were sent.
    pub(crate) sent: u64,

    // The number of responses that were successfully received.
    pub(crate) received: u64,

    // Tracks the fastest response times.
    pub(crate) min: Option<Duration>,

    // Tracks the slowest response times.
    pub(crate) max: Option<Duration>,

    // Accumulates total RTT (round trip time).
    pub(crate) sum: Duration,

    // The most recent RTT (round trip time).
    pub(crate) last: Option<Duration>,

    // Accumulates total jitter differences.
    pub(crate) jitter_sum: f64,
}

impl State {

    /// Creates a `State` with capacity to retain `hist` recent ping results.
    pub(crate) fn new(hist: usize) -> Self {
        Self {
            samples: vec![None; hist],
            index: 0,
            sent: 0,
            received: 0,
            min: None,
            max: None,
            sum: Duration::from_secs(0),
            last: None,
            jitter_sum: 0.0,
        }
    }

    /// Records a new ping round-trip time, updating rolling statistics and history.
    pub(crate) fn push(&mut self, rtt: Option<Duration>) {
        self.sent += 1;
        let len = self.samples.len();
        let slot = self.index % len;
        self.samples[slot] = rtt;
        self.index += 1;

        if let Some(d) = rtt {
            self.received += 1;
            self.min = Some(self.min.map_or(d, |m| m.min(d)));
            self.max = Some(self.max.map_or(d, |m| m.max(d)));
            self.sum += d;
            if let Some(prev) = self.last {
                let diff = (d.as_secs_f64() - prev.as_secs_f64()).abs();
                self.jitter_sum += diff;
            }
            self.last = Some(d);
        }
    }

    /// Returns the average (mean) RTT across all received pings, or `None` if none succeeded.
    pub(crate) fn avg(&self) -> Option<Duration> {
        if self.received == 0 {
            None
        } else {
            Some(self.sum / self.received as u32)
        }
    }

    /// Returns packet loss as a percentage of sent minus received pings.
    pub(crate) fn loss_percentage(&self) -> f64 {
        if self.sent == 0 {
            0.0
        } else {
            100.0 * (self.sent - self.received) as f64 / self.sent as f64
        }
    }

    /// Returns the mean jitter (RTT variance) in milliseconds, or `None` if fewer than two packets.
    pub(crate) fn jitter(&self) -> Option<f64> {
        if self.received <= 1 {
            None
        } else {
            Some(self.jitter_sum / (self.received -1) as f64 * 1000.0) // as milliseconds
        }
    }
}
