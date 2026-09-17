//! Shared local option menus: the same navigation and popup used by Ctrl-] m.
use crate::tui::{self, Theme, Tone};
use std::time::{Duration, Instant};

pub(crate) struct Item {
    pub label: String,
    pub detail: String,
    enter_action: &'static str,
    tone: Tone,
}
impl Item {
    pub fn new(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            detail: detail.into(),
            enter_action: "choose",
            tone: Tone::Normal,
        }
    }
    pub fn enter_action(mut self, action: &'static str) -> Self {
        self.enter_action = action;
        self
    }
    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }
}

#[derive(Default)]
pub(crate) struct Keys {
    escape: u8,
    since: Option<Instant>,
}
impl Keys {
    pub fn expired(&mut self) -> bool {
        if self.escape == 1
            && self
                .since
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(150))
        {
            self.escape = 0;
            self.since = None;
            return true;
        }
        false
    }
    // Decode complete sequences; never let arrow/function-key bytes enter prompts.
    pub fn key(&mut self, byte: u8, prompt: bool) -> Option<u8> {
        if self.escape != 0 {
            match (self.escape, byte) {
                (1, b'[' | b'O') => self.escape = 2,
                (2, b'A' | b'B' | b'C' | b'D' | b'H' | b'F') => {
                    self.escape = 0;
                    if !prompt {
                        return Some(match byte {
                            b'A' => b'k',
                            b'B' => b'j',
                            b'C' => 6,
                            b'D' => 2,
                            b'H' => 1,
                            _ => 5,
                        });
                    }
                }
                (2, 0x20..=0x3f) => (),
                _ => self.escape = 0,
            }
            return None;
        }
        if byte == 27 {
            self.escape = 1;
            self.since = Some(Instant::now());
            None
        } else {
            Some(byte)
        }
    }
}

pub(crate) fn move_selection(selected: &mut usize, byte: u8, count: usize) -> bool {
    *selected = (*selected).min(count.saturating_sub(1));
    match byte {
        b'j' => *selected = (*selected + 1).min(count.saturating_sub(1)),
        b'k' => *selected = selected.saturating_sub(1),
        1 => *selected = 0,
        5 => *selected = count.saturating_sub(1),
        _ => return false,
    }
    true
}

pub(crate) fn frame(
    title: &str,
    items: &[Item],
    selected: usize,
    note: &str,
    size: (usize, usize),
) -> String {
    frame_layout(title, items, selected, note, size, (64, ""))
}

pub(crate) fn frame_layout(
    title: &str,
    items: &[Item],
    selected: usize,
    note: &str,
    size: (usize, usize),
    layout: (usize, &str),
) -> String {
    let (columns, rows) = size;
    let theme = Theme::new();
    let width = columns.saturating_sub(4).clamp(4, layout.0).min(columns);
    let visible = rows.saturating_sub(10).clamp(1, items.len().max(1));
    let height = (visible + 8).min(rows);
    let left = columns.saturating_sub(width) / 2 + 1;
    let top = rows.saturating_sub(height) / 2 + 1;
    let inner = width.saturating_sub(2);
    let mut text = String::from("\x1b[0m\x1b[?25l");
    let mut line = |index: usize, content: &str, tone: Tone| {
        if index >= height {
            return;
        }
        let content = tui::clip(content, inner);
        let padded = format!(
            "{content}{}",
            " ".repeat(inner.saturating_sub(tui::width(&content)))
        );
        text.push_str(&format!(
            "\x1b[{};{left}H{}{}{}\x1b[0m",
            top + index,
            theme.paint("│", Tone::Muted),
            theme.paint(&padded, tone),
            theme.paint("│", Tone::Muted)
        ));
    };
    line(0, "", Tone::Normal);
    let heading = format!(" ◇ sericon  /  {title}");
    line(
        1,
        &if tui::width(&heading) > inner {
            format!(" ◇ {title}")
        } else {
            heading
        },
        Tone::Title,
    );
    line(2, &format!(" {}", layout.1), Tone::Muted);
    let selected = selected.min(items.len().saturating_sub(1));
    let start = selected.saturating_sub(visible - 1);
    for (index, item) in items.iter().enumerate().skip(start).take(visible) {
        line(
            index - start + 3,
            &format!(
                " {} {}",
                if index == selected { "›" } else { " " },
                item.label
            ),
            if index == selected {
                Tone::Selected
            } else {
                item.tone
            },
        );
    }
    let detail = items
        .get(selected)
        .map_or("No options available", |item| item.detail.as_str());
    line(visible + 3, "", Tone::Muted);
    line(visible + 4, "", Tone::Muted);
    for (i, part) in words(detail, inner.saturating_sub(2))
        .iter()
        .take(2)
        .enumerate()
    {
        line(visible + 3 + i, &format!(" {part}"), Tone::Muted);
    }
    let action = items
        .get(selected)
        .map_or("choose", |item| item.enter_action);
    let hints = [
        format!(" ↑/↓ select · Enter {action} · Esc/q back"),
        format!(" ↑/↓ · Enter {action} · Esc/q back"),
        format!(" ↑/↓ · Enter {action} · q back"),
        format!(" Enter {action} · q back"),
    ];
    let hint = hints
        .iter()
        .find(|hint| tui::width(hint) <= inner)
        .map_or(" ↑/↓ Enter · q back", String::as_str);
    line(visible + 5, hint, Tone::Accent);
    line(visible + 6, &format!(" {note}"), Tone::Muted);
    line(height.saturating_sub(1), "", Tone::Normal);
    for (row, first, last) in [(top, '╭', '╮'), (top + height.saturating_sub(1), '╰', '╯')]
    {
        text.push_str(&format!(
            "\x1b[{row};{left}H{}",
            theme.paint(&format!("{first}{}{last}", "─".repeat(inner)), Tone::Muted)
        ));
    }
    text
}

fn words(text: &str, columns: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in tui::clean(text).split_whitespace() {
        if !line.is_empty() && tui::width(&line) + 1 + tui::width(word) > columns {
            lines.push(std::mem::take(&mut line));
        }
        if tui::width(word) > columns {
            let mut parts = tui::wrap(word, columns);
            line = parts.pop().unwrap_or_default();
            lines.extend(parts);
        } else {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_and_prompts_consume_fragmented_sequences() {
        let mut keys = Keys::default();
        let decoded: Vec<_> = b"\x1b[A\x1bOB\x1b[15~\x1b[H\x1b[F"
            .iter()
            .filter_map(|b| keys.key(*b, false))
            .collect();
        assert_eq!(decoded, b"kj\x01\x05");
        for b in b"\x1b[B\x1bOA\x1b[3~" {
            assert_eq!(keys.key(*b, true), None);
        }
        assert_eq!(keys.key(b'j', true), Some(b'j'));
        keys.key(27, false);
        assert!(!keys.expired());
        keys.since = Some(Instant::now() - Duration::from_millis(200));
        assert!(keys.expired());
        assert!(!keys.expired());
    }
}
