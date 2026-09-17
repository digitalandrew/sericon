//! UART terminal state is isolated from Sericon's chrome and local messages.
use crate::{
    session::Status,
    tui::{self, Theme, Tone},
};
use std::io::{self, Write};

pub(crate) struct Screen {
    mouse: bool,
}
impl Screen {
    pub fn enter(mouse: bool) -> io::Result<Self> {
        let screen = Self { mouse };
        io::stdout().write_all(b"\x1b[?1049h\x1b[0m\x1b[2J\x1b[H")?;
        if mouse {
            io::stdout().write_all(b"\x1b[?1000h\x1b[?1006h")?;
        }
        Ok(screen)
    }
}
impl Drop for Screen {
    fn drop(&mut self) {
        let mut out = io::stdout().lock();
        if self.mouse {
            let _ = out.write_all(b"\x1b[?1000l\x1b[?1006l");
        }
        let _ = out.write_all(b"\x1b[0m\x1b[?25h\x1b[?1049l");
        let _ = out.flush();
    }
}

pub(crate) struct Live {
    parser: vt100::Parser,
    status: Status,
    message: String,
    tone: Tone,
    size: (usize, usize),
    previous: Vec<String>,
    cursor: Option<(u16, u16, bool)>,
    pub paused: bool,
    force: bool,
}
impl Live {
    pub fn new(status: Status, scrollback_lines: usize) -> Self {
        let size = crate::search::dimensions();
        let (_, rows) = layout(size.1);
        Self {
            parser: vt100::Parser::new(rows as u16, size.0 as u16, scrollback_lines),
            status,
            message: "Live · wheel / PgUp scroll · Ctrl-] f searches retained history".into(),
            tone: Tone::Muted,
            size,
            previous: Vec::new(),
            cursor: None,
            paused: false,
            force: true,
        }
    }
    pub fn receive(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }
    pub fn scrolled(&self) -> bool {
        self.parser.screen().scrollback() > 0
    }
    pub fn scroll(&mut self, up: bool, page: bool) {
        let amount = if page {
            layout(self.size.1).1.saturating_sub(1).max(1)
        } else {
            3
        };
        let offset = self.parser.screen().scrollback();
        self.parser.screen_mut().set_scrollback(if up {
            offset.saturating_add(amount)
        } else {
            offset.saturating_sub(amount)
        });
        self.force = true;
    }
    pub fn bottom(&mut self) {
        if self.scrolled() {
            self.parser.screen_mut().set_scrollback(0);
            self.force = true;
        }
    }
    pub fn contains(&self, x: usize, y: usize) -> bool {
        let (top, rows) = layout(self.size.1);
        x <= self.size.0 && y > top && y <= top + rows
    }
    pub fn update_status(&mut self, status: Status) {
        self.status = status;
    }
    pub fn notice(&mut self, message: &str, tone: Tone) {
        self.message = message.into();
        self.tone = tone;
    }
    pub fn resume(&mut self) {
        self.paused = false;
        self.force = true;
    }
    pub fn render(&mut self) -> io::Result<()> {
        if self.paused {
            return Ok(());
        }
        let bytes = self.frame(crate::search::dimensions());
        if !bytes.is_empty() {
            let mut out = io::stdout().lock();
            out.write_all(&bytes)?;
            out.flush()?;
        }
        Ok(())
    }
    pub fn redraw(&mut self) -> io::Result<()> {
        self.force = true;
        let paused = self.paused;
        self.paused = false;
        let result = self.render();
        self.paused = paused;
        result
    }
    fn frame(&mut self, size: (usize, usize)) -> Vec<u8> {
        let (columns, height) = size;
        let (top, rows) = layout(height);
        if size != self.size {
            self.parser
                .screen_mut()
                .set_size(rows as u16, columns as u16);
            self.size = size;
            self.force = true;
        }
        let theme = Theme::new();
        let mut lines = vec![String::new(); height];
        let mut put = |row: usize, text: &str, tone: Tone| {
            lines[row] = theme.paint(&tui::clip(text, columns), tone);
        };
        if height >= 16 {
            put(
                0,
                &format!("  ◇ sericon  v{}  /  Live", env!("CARGO_PKG_VERSION")),
                Tone::Title,
            );
            put(
                1,
                &format!(
                    "  {} · {} baud · {} · input: {}",
                    self.status.port,
                    self.status.baud,
                    self.status.detection,
                    self.status.writer.as_ref().map_or("shared", |w| &w.actor)
                ),
                Tone::Normal,
            );
            put(2, &format!("  Session  {}", self.status.id), Tone::Muted);
            let log = self
                .status
                .log_directory
                .as_ref()
                .map_or("off · memory history only".into(), |p| {
                    p.display().to_string()
                });
            put(
                3,
                &format!("  Log      {}", tui::tail(&log, columns.saturating_sub(11))),
                Tone::Muted,
            );
            put(4, &"─".repeat(columns), Tone::Muted);
        } else {
            put(
                0,
                &format!(
                    "◇ sericon / Live · {} · {}",
                    self.status.baud, self.status.detection
                ),
                Tone::Title,
            );
            if top > 1 {
                put(1, &"─".repeat(columns), Tone::Muted);
            }
        }
        if height >= 8 {
            put(height - 3, &"─".repeat(columns), Tone::Muted);
        }
        if self.scrolled() {
            let offset = self.parser.screen().scrollback();
            let message = if columns >= 65 {
                format!("Scrollback · {offset} lines back · capturing · End / Ctrl-] b live")
            } else {
                format!("Scrollback · {offset} back · End live")
            };
            put(height - 2, &message, Tone::Accent);
        } else {
            put(height - 2, &self.message, self.tone);
        }
        put(
            height - 1,
            if columns >= 65 {
                "Ctrl-] then  m menu · f search · x formulas · ? help · d detach · q stop"
            } else {
                "Ctrl-] m menu · f find · x formulas"
            },
            Tone::Muted,
        );
        for row in 0..rows {
            lines[top + row] = serial_row(self.parser.screen(), row as u16, columns as u16);
        }
        let screen = self.parser.screen();
        let (row, column) = screen.cursor_position();
        let cursor = (row, column, screen.hide_cursor() || self.scrolled());
        let mut output = String::new();
        if self.force {
            output.push_str("\x1b[0m\x1b[H\x1b[2J");
        }
        for (i, text) in lines.iter().enumerate() {
            if self.force || self.previous.get(i) != Some(text) {
                output.push_str(&format!("\x1b[{};1H\x1b[0m\x1b[2K{text}\x1b[0m", i + 1));
            }
        }
        if !output.is_empty() || self.cursor != Some(cursor) {
            output.push_str(&format!(
                "\x1b[{};{}H{}",
                top + usize::from(row) + 1,
                usize::from(column).min(columns - 1) + 1,
                if cursor.2 { "\x1b[?25l" } else { "\x1b[?25h" }
            ));
        }
        self.previous = lines;
        self.cursor = Some(cursor);
        self.force = false;
        output.into_bytes()
    }
}

fn layout(height: usize) -> (usize, usize) {
    let top = if height >= 16 {
        5
    } else if height >= 8 {
        2
    } else {
        1
    };
    let footer = if height >= 8 { 3 } else { 2 };
    (top, height.saturating_sub(top + footer).max(1))
}

// Only cell contents and SGR attributes reach the host terminal. Target cursor
// motion, erase/reset, OSC and alternate-screen commands stay inside the pane.
fn serial_row(screen: &vt100::Screen, row: u16, columns: u16) -> String {
    let mut output = String::new();
    let mut previous = String::new();
    for column in 0..columns {
        // History retains the width at which each row was captured. A wider
        // window pads those old rows instead of indexing beyond their cells.
        let Some(cell) = screen.cell(row, column) else {
            output.push(' ');
            continue;
        };
        if cell.is_wide_continuation() {
            continue;
        }
        let mut style = String::from("\x1b[0");
        for (enabled, code) in [
            (cell.bold(), 1),
            (cell.dim(), 2),
            (cell.italic(), 3),
            (cell.underline(), 4),
            (cell.inverse(), 7),
        ] {
            if enabled {
                style.push_str(&format!(";{code}"));
            }
        }
        for (colour, code) in [(cell.fgcolor(), 38), (cell.bgcolor(), 48)] {
            match colour {
                vt100::Color::Default => (),
                vt100::Color::Idx(n) => style.push_str(&format!(";{code};5;{n}")),
                vt100::Color::Rgb(r, g, b) => style.push_str(&format!(";{code};2;{r};{g};{b}")),
            }
        }
        style.push('m');
        if style != previous {
            output.push_str(&style);
            previous = style;
        }
        let text = cell.contents();
        output.push_str(if text.is_empty() { " " } else { text });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn live() -> Live {
        Live::new(
            serde_json::from_value(json!({
                "id":"fixture", "port":"/dev/fixture", "device":{"port":"/dev/fixture"},
                "started":"fixture", "launch_dir":"/tmp", "baud":115200, "detection":"waiting",
                "connected":true, "latest_cursor":0
            }))
            .unwrap(),
            10_000,
        )
    }
    fn row(parser: &vt100::Parser, row: u16) -> String {
        parser
            .screen()
            .rows(0, 79)
            .nth(row.into())
            .unwrap()
            .trim_end()
            .to_owned()
    }
    #[test]
    fn notices_between_rx_fragments_preserve_uart_text_style_and_cursor() {
        let mut live = live();
        let mut host = vt100::Parser::new(24, 80, 0);
        host.process(&live.frame((79, 24)));
        live.receive(b"2291 I nvs_read_\x1b[3");
        host.process(&live.frame((79, 24)));
        live.notice("115200 baud: baud locked from readable text", Tone::Muted);
        host.process(&live.frame((79, 24)));
        live.receive(b"2mblob\x1b[0m caf\xc3");
        host.process(&live.frame((79, 24)));
        live.notice("agent TX: help", Tone::Accent);
        host.process(&live.frame((79, 24)));
        live.receive(b"\xa9\r\n");
        host.process(&live.frame((79, 24)));
        assert!(row(&host, 0).contains("sericon"));
        assert_eq!(row(&host, 5), "2291 I nvs_read_blob café");
        assert_eq!(
            host.screen().cell(5, 16).unwrap().fgcolor(),
            vt100::Color::Idx(2)
        );
        assert!(row(&host, 23).contains("Ctrl-]"));
        assert!(!live.parser.screen().contents().contains("agent TX"));
        assert_eq!(host.screen().cursor_position(), (6, 0));
    }
    #[test]
    fn target_cursor_erase_reset_and_scrolling_cannot_overwrite_chrome() {
        let mut live = live();
        let mut host = vt100::Parser::new(24, 80, 0);
        for i in 0..80 {
            live.receive(format!("line {i}\r\n").as_bytes());
            host.process(&live.frame((79, 24)));
            assert!(row(&host, 0).contains("sericon"));
        }
        for control in [
            b"\x1b[2J\x1b[H".as_slice(),
            b"\x1b[?1049h\x1b[2J\x1b[H",
            b"\x1b[?1049l\x1bc",
        ] {
            live.receive(control);
            live.receive(b"DEVICE HOME");
            host.process(&live.frame((79, 24)));
            assert!(row(&host, 0).contains("sericon"));
            assert_eq!(row(&host, 5), "DEVICE HOME");
            assert!(row(&host, 23).contains("Ctrl-]"));
        }
        assert!(
            !String::from_utf8(live.frame((79, 24)))
                .unwrap()
                .contains("1049")
        );
    }
    #[test]
    fn returning_from_menus_and_resizing_repaints_complete_live_view() {
        let mut live = live();
        let mut host = vt100::Parser::new(24, 80, 0);
        live.receive(b"kept partial");
        host.process(&live.frame((79, 24)));
        host.process(b"\x1b[2J\x1b[HMENU");
        live.resume();
        host.process(&live.frame((79, 24)));
        assert!(row(&host, 0).contains("sericon"));
        assert_eq!(row(&host, 5), "kept partial");
        let mut small = vt100::Parser::new(12, 40, 0);
        small.process(&live.frame((39, 12)));
        assert!(row(&small, 0).contains("sericon"));
        assert_eq!(row(&small, 2), "kept partial");
        assert!(row(&small, 11).contains("Ctrl-]"));
    }
    #[test]
    fn scrollback_stays_anchored_during_rx_and_resizes_without_losing_styles() {
        let mut live = live();
        live.frame((79, 24));
        for i in 0..60 {
            live.receive(format!("\x1b[31mold-{i:03}\x1b[0m\r\n").as_bytes());
        }
        live.scroll(true, true);
        let contents = live.parser.screen().contents();
        let offset = live.parser.screen().scrollback();
        for _ in 0..5 {
            live.receive(b"new output\r\n");
        }
        assert_eq!(live.parser.screen().contents(), contents);
        assert_eq!(live.parser.screen().scrollback(), offset + 5);
        let mut host = vt100::Parser::new(24, 120, 0);
        host.process(&live.frame((119, 24)));
        assert!(host.screen().contents().contains("Scrollback"));
        assert!(host.screen().hide_cursor());
        assert_eq!(
            host.screen().cell(5, 0).unwrap().fgcolor(),
            vt100::Color::Idx(1)
        );
        // A shorter window must not cut off the live tail. Resizing also must
        // not interrupt an in-flight UTF-8 character or colour sequence.
        live.receive(b"tail caf\xc3");
        live.frame((39, 12));
        live.bottom();
        live.receive(b"\xa9\x1b[3");
        live.frame((119, 24));
        live.receive(b"2m green\x1b[0m\r\n");
        assert!(live.parser.screen().contents().contains("tail café green"));
        live.scroll(true, true);
        assert!(live.parser.screen().contents().contains("old-"));
        live.bottom();
        live.receive(b"live again\r\n");
        assert!(!live.scrolled());
        assert!(live.parser.screen().contents().contains("live again"));
    }
    #[test]
    fn scrollback_limit_clamps_and_live_follow_resumes_at_bottom() {
        let mut live = live();
        live.parser = vt100::Parser::new(4, 20, 8);
        for i in 0..40 {
            live.receive(format!("line-{i}\r\n").as_bytes());
        }
        for _ in 0..10 {
            live.scroll(true, false);
        }
        assert_eq!(live.parser.screen().scrollback(), 8);
        assert!(!live.parser.screen().contents().contains("line-0\n"));
        for _ in 0..10 {
            live.scroll(false, false);
        }
        assert!(!live.scrolled());
        live.receive(b"fresh line\r\n");
        assert!(live.parser.screen().contents().contains("fresh line"));
    }
}
