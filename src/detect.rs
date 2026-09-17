use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub enum Decision {
    None,
    Lock(f64),
    Next,
    Inconclusive,
}
pub struct Detector {
    pub active: bool,
    pub scanning: bool,
    pub index: usize,
    sample: Vec<u8>,
    since: Option<Instant>,
    window: Duration,
    min_bytes: usize,
}
impl Detector {
    pub fn new(active: bool, ms: u64, min_bytes: usize) -> Self {
        Self {
            active,
            scanning: false,
            index: 0,
            sample: Vec::new(),
            since: None,
            window: Duration::from_millis(ms),
            min_bytes,
        }
    }
    pub fn observe(&mut self, bytes: &[u8], now: Instant) {
        if !self.active {
            return;
        }
        self.since.get_or_insert(now);
        self.sample.extend(
            bytes
                .iter()
                .take(8192usize.saturating_sub(self.sample.len())),
        );
    }
    pub fn decide(&self, now: Instant) -> Decision {
        if !self.active {
            return Decision::None;
        }
        if self.sample.len() >= self.min_bytes {
            let half = self.sample.len() / 2;
            let a = score(&self.sample[..half]);
            let b = score(&self.sample[half..]);
            if a >= 0.95 && b >= 0.95 {
                return Decision::Lock(a.min(b));
            }
        }
        if self
            .since
            .is_some_and(|t| now.duration_since(t) >= self.window)
        {
            if self.scanning || (self.sample.len() >= 8 && score(&self.sample) < 0.8) {
                return Decision::Next;
            }
            if !self.sample.is_empty() {
                return Decision::Inconclusive;
            }
        }
        Decision::None
    }
    pub fn next(&mut self, now: Instant) {
        self.index += 1;
        self.scanning = true;
        self.sample.clear();
        self.since = Some(now);
    }
    pub fn pause(&mut self) {
        self.active = false;
    }
    pub fn rescan(&mut self, now: Instant) {
        self.active = true;
        self.scanning = true;
        self.index = 0;
        self.sample.clear();
        self.since = Some(now);
    }
}
pub fn score(data: &[u8]) -> f64 {
    if data.is_empty() {
        return 0.0;
    }
    let mut symbols = std::collections::HashSet::new();
    let mut letters = 0;
    let good = data
        .iter()
        .filter(|&&b| {
            if b.is_ascii_alphanumeric() {
                letters += 1;
                symbols.insert(b);
            }
            b.is_ascii_graphic() || matches!(b, b' ' | b'\r' | b'\n' | b'\t' | 8 | 27)
        })
        .count();
    if letters < 3 || symbols.len() < 3 {
        return 0.0;
    }
    good as f64 / data.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_stays_put_and_garbage_scans() {
        let mut d = Detector::new(true, 100, 32);
        let now = Instant::now();
        assert_eq!(d.decide(now + Duration::from_secs(60)), Decision::None);
        d.observe(&[0xff; 40], now);
        assert_eq!(d.decide(now + Duration::from_millis(101)), Decision::Next);
        d.next(now);
        d.observe(
            b"Linux version 6.1.0 starting\r\nWelcome to the console!\r\n",
            now,
        );
        assert!(matches!(d.decide(now), Decision::Lock(_)));
    }
    #[test]
    fn sparse_text_and_repetitive_noise_do_not_lock() {
        let mut d = Detector::new(true, 100, 32);
        let now = Instant::now();
        d.observe(b"OK", now);
        assert_eq!(d.decide(now), Decision::None);
        assert_eq!(score(&[b'U'; 100]), 0.0);
        assert!(score(b"\x1b[32mLinux console ready\x1b[0m\r\n") > 0.95);
    }
}
