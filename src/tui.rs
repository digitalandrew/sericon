//! Shared presentation for local terminal views. UART bytes never pass through it.
use std::io::{self, IsTerminal, Write};
use unicode_width::UnicodeWidthChar;

#[derive(Clone, Copy)]
pub(crate) enum Tone {
    Normal,
    Title,
    Accent,
    Muted,
    Success,
    Warning,
    Error,
    Selected,
    Blue,
    Cyan,
    Magenta,
}

#[derive(Clone, Copy)]
pub(crate) struct Theme {
    colour: bool,
}
impl Theme {
    pub fn new() -> Self {
        Self {
            colour: io::stdout().is_terminal()
                && std::env::var("TERM").is_ok_and(|v| v != "dumb")
                && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
        }
    }
    pub fn style(self, tone: Tone) -> &'static str {
        if !self.colour {
            return match tone {
                Tone::Title => "\x1b[1m",
                Tone::Selected => "\x1b[7m",
                _ => "",
            };
        }
        match tone {
            Tone::Normal => "",
            Tone::Title => "\x1b[1;33m",
            Tone::Accent => "\x1b[33m",
            Tone::Muted => "\x1b[90m",
            Tone::Success => "\x1b[32m",
            Tone::Warning => "\x1b[33m",
            Tone::Error => "\x1b[31m",
            Tone::Selected => "\x1b[1;7m",
            Tone::Blue => "\x1b[94m",
            Tone::Cyan => "\x1b[36m",
            Tone::Magenta => "\x1b[35m",
        }
    }
    pub fn paint(self, text: &str, tone: Tone) -> String {
        format!("{}{}\x1b[0m", self.style(tone), clean(text))
    }
    pub fn active_match(self) -> &'static str {
        if self.colour {
            "\x1b[1;30;43m"
        } else {
            "\x1b[1;7m"
        }
    }
}

// Strip complete escape sequences, not just ESC itself, from untrusted metadata.
pub(crate) fn clean(text: &str) -> String {
    let mut bytes = Vec::new();
    let _ = crate::search::TextFilter::default().write(text.as_bytes(), &mut bytes);
    String::from_utf8_lossy(&bytes)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}
pub(crate) fn width(text: &str) -> usize {
    text.chars().map(|c| c.width().unwrap_or(0)).sum()
}
pub(crate) fn clip(text: &str, columns: usize) -> String {
    let text = clean(text);
    if width(&text) <= columns {
        return text;
    }
    if columns == 0 {
        return String::new();
    }
    format!("{}…", crate::search::clipped(&text, columns - 1))
}
pub(crate) fn tail(text: &str, columns: usize) -> String {
    let text = clean(text);
    if width(&text) <= columns {
        return text;
    }
    let reversed: String = text.chars().rev().collect();
    let end: String = crate::search::clipped(&reversed, columns.saturating_sub(1))
        .chars()
        .rev()
        .collect();
    clip(&format!("…{end}"), columns)
}
pub(crate) fn wrap(text: &str, columns: usize) -> Vec<String> {
    let mut result = Vec::new();
    for line in text.lines() {
        let mut row = String::new();
        let mut used = 0;
        for c in clean(line).chars() {
            let n = c.width().unwrap_or(0);
            if used + n > columns.max(1) && !row.is_empty() {
                result.push(std::mem::take(&mut row));
                used = 0;
            }
            row.push(c);
            used += n;
        }
        result.push(row);
    }
    result
}

pub(crate) struct Frame {
    pub theme: Theme,
    pub columns: usize,
    pub rows: usize,
    pub top: usize,
    pub left: usize,
    height: usize,
    text: String,
    input_cursor: Option<(usize, usize)>,
}
impl Frame {
    pub fn new(title: &str, subtitle: &str) -> Self {
        let (width, height) = crate::search::dimensions();
        let left = if width >= 60 { 3 } else { 1 };
        let top = if height >= 16 { 6 } else { 3 };
        let mut frame = Self {
            theme: Theme::new(),
            columns: width.saturating_sub(left * 2).max(1),
            rows: height.saturating_sub(top + 3).max(1),
            top,
            left,
            height,
            text: "\x1b[?25l\x1b[0m\x1b[H\x1b[2J".into(),
            input_cursor: None,
        };
        frame.line(
            if height >= 16 { 2 } else { 1 },
            &format!("◇ sericon  /  {title}"),
            Tone::Title,
        );
        if height >= 16 {
            frame.line(3, subtitle, Tone::Muted);
            frame.rule(4);
        }
        frame
    }
    pub fn line(&mut self, row: usize, text: &str, tone: Tone) {
        let text = self.theme.paint(&clip(text, self.columns), tone);
        self.rich(row, &text);
    }
    // Call only with internally styled text already bounded to columns.
    pub fn rich(&mut self, row: usize, text: &str) {
        if row >= 1 && row <= self.height {
            self.text
                .push_str(&format!("\x1b[{row};{}H{text}\x1b[0m", self.left + 1));
        }
    }
    pub fn body(&mut self, index: usize, text: &str, tone: Tone) {
        if index < self.rows {
            self.line(self.top + index, text, tone);
        }
    }
    pub fn rule(&mut self, row: usize) {
        self.line(row, &"─".repeat(self.columns), Tone::Muted);
    }
    pub fn footer(&mut self, message: &str, tone: Tone, keys: &str) {
        if self.height >= 10 {
            self.rule(self.height - 3);
        }
        self.line(self.height - 2, message, tone);
        self.line(self.height - 1, keys, Tone::Muted);
    }
    pub fn input(&mut self, index: usize, label: &str, value: &str) {
        let prefix = clip(label, self.columns.saturating_sub(2));
        let value = tail(value, self.columns.saturating_sub(width(&prefix) + 1));
        self.body(index, &format!("{prefix}{value}"), Tone::Accent);
        let row = (self.top + index).min(self.height);
        self.input_cursor = Some((row, self.left + width(&prefix) + width(&value) + 1));
    }
    pub fn input_box(&mut self, index: usize, label: &str, value: &str) {
        if self.columns < 8 || index + 2 >= self.rows {
            self.input(index.min(self.rows.saturating_sub(1)), label, value);
            return;
        }
        let prefix = clip(label, self.columns - 5);
        let value = tail(value, self.columns - width(&prefix) - 5);
        let content = format!("{prefix}{value}");
        let padding = " ".repeat(self.columns - width(&content) - 4);
        let rule = "─".repeat(self.columns - 2);
        self.body(index, &format!("╭{rule}╮"), Tone::Accent);
        self.body(index + 1, &format!("│ {content}{padding} │"), Tone::Normal);
        self.body(index + 2, &format!("╰{rule}╯"), Tone::Accent);
        self.input_cursor = Some((self.top + index + 1, self.left + width(&content) + 3));
    }
    pub fn finish(mut self) -> String {
        if let Some((row, column)) = self.input_cursor {
            self.text
                .push_str(&format!("\x1b[{row};{column}H\x1b[?25h"));
        }
        self.text
    }
}

pub(crate) fn notice(message: &str, tone: Tone) {
    let theme = Theme::new();
    let (columns, _) = crate::search::dimensions();
    let mut text = String::from("\r\n");
    for line in wrap(message, columns.saturating_sub(4).max(1)) {
        text.push_str(&format!("  {}\r\n", theme.paint(&line, tone)));
    }
    let mut out = io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

pub(crate) struct Help {
    selected: usize,
    prefix: bool,
    escape: u8,
    last: String,
}
impl Help {
    pub fn open() -> io::Result<Self> {
        io::stdout().write_all(b"\x1b[?25l")?;
        Ok(Self {
            selected: 0,
            prefix: false,
            escape: 0,
            last: String::new(),
        })
    }
    pub fn key(&mut self, byte: u8) -> crate::search::Action {
        use crate::search::Action;
        if self.prefix {
            self.prefix = false;
            return match byte {
                b'm' => Action::Menu,
                b'q' => Action::Stop,
                b'd' => Action::Detach,
                b'b' | b'?' => Action::Back,
                _ => Action::Stay,
            };
        }
        if byte == 29 {
            self.prefix = true;
        } else if self.escape != 0 {
            if self.escape == 1 && byte == b'[' {
                self.escape = 2;
            } else {
                if self.escape == 2 && byte == b'A' {
                    self.selected = self.selected.saturating_sub(1);
                }
                if self.escape == 2 && byte == b'B' {
                    self.selected = self.selected.saturating_add(1);
                }
                if (0x40..=0x7e).contains(&byte) {
                    self.escape = 0;
                }
            }
        } else {
            match byte {
                27 => self.escape = 1,
                b'q' | b'\r' | b'\n' => return Action::Back,
                b'j' => self.selected = self.selected.saturating_add(1),
                b'k' => self.selected = self.selected.saturating_sub(1),
                b' ' => self.selected = self.selected.saturating_add(10),
                _ => (),
            }
        }
        Action::Stay
    }
    pub fn tick(&mut self) -> io::Result<()> {
        let mut frame = Frame::new(
            "Help",
            "Local controls · UART capture continues in the background",
        );
        let lines = wrap(&crate::help::terminal(), frame.columns);
        self.selected = self.selected.min(lines.len().saturating_sub(frame.rows));
        for (i, line) in lines
            .iter()
            .skip(self.selected)
            .take(frame.rows)
            .enumerate()
        {
            frame.body(
                i,
                line,
                if !line.starts_with(' ') && line.ends_with(':') {
                    Tone::Accent
                } else {
                    Tone::Normal
                },
            );
        }
        frame.footer(
            &format!(
                "Lines {}–{} of {}",
                self.selected + 1,
                (self.selected + frame.rows).min(lines.len()),
                lines.len()
            ),
            Tone::Muted,
            if frame.columns < 60 {
                "q close · ↑/↓ scroll · Space page"
            } else {
                "Enter/q: close help · j/k/↑/↓: scroll · Space: page"
            },
        );
        let text = frame.finish();
        if text != self.last {
            let mut out = io::stdout().lock();
            out.write_all(text.as_bytes())?;
            out.flush()?;
            self.last = text;
        }
        Ok(())
    }
    pub fn redraw(&mut self) -> io::Result<()> {
        self.last.clear();
        self.tick()
    }
}
impl Drop for Help {
    fn drop(&mut self) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(b"\x1b[0m\x1b[?25h");
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_cannot_inject_terminal_commands_and_unicode_input_fits() {
        assert_eq!(clean("ok\x1b[2J\x1b]52;c;secret\x07done"), "okdone");
        for n in 0..60 {
            let value = "界é".repeat(30);
            assert!(width(&clip(&value, n)) <= n);
            assert!(width(&tail(&value, n)) <= n);
        }
        assert_eq!(Theme { colour: false }.active_match(), "\x1b[1;7m");
    }
    #[test]
    fn input_cursor_stays_in_field_after_footer_and_resize() {
        for (columns, rows) in [(92, 28), (40, 12), (20, 8)] {
            for boxed in [false, true] {
                let mut frame = Frame {
                    theme: Theme { colour: false },
                    columns: columns - 2,
                    rows: rows - 6,
                    top: 3,
                    left: 1,
                    height: rows,
                    text: String::new(),
                    input_cursor: None,
                };
                let value = format!("/{}\x1b[2J\x1b]52;c;secret\x07", "界é".repeat(40));
                if boxed {
                    frame.input_box(1, "Path: ", &value);
                } else {
                    frame.input(1, "Path: ", &value);
                }
                frame.footer("", Tone::Muted, "Esc back");
                let rendered = frame.finish();
                assert!(!rendered.contains("secret"));
                let mut terminal = vt100::Parser::new(rows as u16, columns as u16, 0);
                terminal.process(rendered.as_bytes());
                let screen = terminal.screen();
                let (row, column) = screen.cursor_position();
                assert_eq!(row, if boxed && rows >= 12 { 4 } else { 3 });
                assert!(column <= (columns - 2) as u16);
                assert!(!screen.hide_cursor());
                assert!(screen.contents().contains("Path: …"));
                assert!(screen.contents().contains("Esc back"));
                if boxed && rows >= 12 {
                    assert!(column < (columns - 2) as u16);
                    assert!(screen.contents().contains('╭'));
                    assert_eq!(
                        screen.cell(row, (columns - 2) as u16).unwrap().contents(),
                        "│"
                    );
                }
            }
        }
    }
}
