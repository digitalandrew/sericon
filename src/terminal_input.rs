//! Decode only local scrolling input. All other keyboard bytes stay exact.
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq)]
pub(crate) enum Event {
    Bytes(Vec<u8>),
    Wheel { up: bool, x: usize, y: usize },
    Page { up: bool, bytes: Vec<u8> },
    End(Vec<u8>),
}

#[derive(Default)]
pub(crate) struct Decoder {
    pending: Vec<u8>,
    since: Option<Instant>,
    discard_mouse: bool,
}
impl Decoder {
    fn mouse(&self) -> bool {
        self.pending.starts_with(b"\x1b[<") || self.pending.starts_with(b"\x1b[M")
    }
    pub fn expire(&mut self) -> Vec<Event> {
        // A recognised mouse report never becomes UART text, even when split
        // over slow reads. Bare Escape/partial ordinary keys remain usable.
        if !self.mouse()
            && !self.discard_mouse
            && self
                .since
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(250))
        {
            self.since = None;
            return vec![Event::Bytes(std::mem::take(&mut self.pending))];
        }
        vec![]
    }
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        for &b in bytes {
            self.byte(b, &mut events);
        }
        events
    }
    fn byte(&mut self, b: u8, events: &mut Vec<Event>) {
        if self.discard_mouse {
            if b < 32 {
                self.discard_mouse = false;
                self.byte(b, events);
            } else if matches!(b, b'M' | b'm') {
                self.discard_mouse = false;
            }
            return;
        }
        if self.pending.is_empty() {
            if b == 27 {
                self.pending.push(b);
                self.since = Some(Instant::now());
            } else if let Some(Event::Bytes(v)) = events.last_mut() {
                v.push(b);
            } else {
                events.push(Event::Bytes(vec![b]));
            }
            return;
        }
        if b < 32 && self.pending.len() > 1 {
            let mouse = self.mouse();
            let pending = std::mem::take(&mut self.pending);
            self.since = None;
            if !mouse {
                events.push(Event::Bytes(pending));
            }
            self.byte(b, events);
            return;
        }
        self.pending.push(b);
        if self.pending == b"\x1b[" || self.pending == b"\x1bO" || self.pending == b"\x1b[M" {
            return;
        }
        if self.pending.starts_with(b"\x1b[M") {
            if self.pending.len() == 6 {
                let report = std::mem::take(&mut self.pending);
                self.since = None;
                if report[3..].iter().all(|b| *b >= 32) {
                    Self::wheel(
                        report[3] as usize - 32,
                        report[4] as usize - 32,
                        report[5] as usize - 32,
                        events,
                    );
                }
            }
            return;
        }
        if self.pending.starts_with(b"\x1b[<") {
            if matches!(b, b'M' | b'm') {
                let report = std::mem::take(&mut self.pending);
                self.since = None;
                if b == b'M' {
                    let values: Option<Vec<usize>> =
                        std::str::from_utf8(&report[3..report.len() - 1])
                            .ok()
                            .and_then(|s| s.split(';').map(|n| n.parse().ok()).collect());
                    if let Some(v) = values
                        && v.len() == 3
                    {
                        Self::wheel(v[0], v[1], v[2], events);
                    }
                }
            } else if self.pending.len() > 64 || (b != b'<' && !b.is_ascii_digit() && b != b';') {
                self.pending.clear();
                self.since = None;
                self.discard_mouse = true;
            }
            return;
        }
        let complete = if self.pending.starts_with(b"\x1b[") {
            (0x40..=0x7e).contains(&b)
        } else if self.pending.starts_with(b"\x1bO") {
            self.pending.len() >= 3
        } else {
            true
        };
        if complete || self.pending.len() > 64 {
            let bytes = std::mem::take(&mut self.pending);
            self.since = None;
            events.push(match bytes.as_slice() {
                b"\x1b[5~" => Event::Page { up: true, bytes },
                b"\x1b[6~" => Event::Page { up: false, bytes },
                b"\x1b[F" | b"\x1bOF" | b"\x1b[4~" | b"\x1b[8~" => Event::End(bytes),
                _ => Event::Bytes(bytes),
            });
        }
    }
    fn wheel(button: usize, x: usize, y: usize, events: &mut Vec<Event>) {
        // Strip Shift/Meta/Control, but reject motion and horizontal buttons.
        if x > 0 && y > 0 && matches!(button & !28, 64 | 65) {
            events.push(Event::Wheel {
                up: button & 1 == 0,
                x,
                y,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_mouse_and_clicks_never_become_keyboard_bytes() {
        let mut d = Decoder::default();
        assert!(d.push(b"\x1b[<6").is_empty());
        d.since = Some(Instant::now() - Duration::from_secs(1));
        assert!(d.expire().is_empty());
        assert_eq!(
            d.push(b"4;12;8M"),
            vec![Event::Wheel {
                up: true,
                x: 12,
                y: 8
            }]
        );
        assert!(
            d.push(b"\x1b[<0;12;8M\x1b[<0;12;8m\x1b[<66;12;8M")
                .is_empty()
        );
        assert_eq!(d.push(b"\x1b[M"), vec![]);
        assert_eq!(
            d.push(&[97, 44, 40]),
            vec![Event::Wheel {
                up: false,
                x: 12,
                y: 8
            }]
        );
        assert!(
            d.push(format!("\x1b[<{}M", "1".repeat(200)).as_bytes())
                .is_empty()
        );
        assert_eq!(d.push(b"ok\r"), vec![Event::Bytes(b"ok\r".to_vec())]);
    }
    #[test]
    fn keyboard_sequences_and_binary_bytes_remain_exact() {
        let mut d = Decoder::default();
        assert_eq!(
            d.push(b"\x1b[A\x1b[3~\x00\xff"),
            vec![
                Event::Bytes(b"\x1b[A".to_vec()),
                Event::Bytes(b"\x1b[3~\x00\xff".to_vec())
            ]
        );
        assert_eq!(
            d.push(b"\x1b[5~\x1b[F"),
            vec![
                Event::Page {
                    up: true,
                    bytes: b"\x1b[5~".to_vec()
                },
                Event::End(b"\x1b[F".to_vec())
            ]
        );
        assert!(d.push(b"\x1b").is_empty());
        d.since = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(d.expire(), vec![Event::Bytes(vec![27])]);
        assert!(d.push(b"\x1b[<64;12").is_empty());
        assert_eq!(d.push(b"\x1dm"), vec![Event::Bytes(b"\x1dm".to_vec())]);
    }
}
