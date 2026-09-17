//! Local action picker. Navigation never reaches the serial input path.
use crate::picker::{self, Item, Keys};
use std::io::{self, Write};

const ITEMS: &[(u8, &str, &str)] = &[
    (b'b', "Live terminal", "Return to UART output and input"),
    (
        b'f',
        "Search history",
        "Find text or regex matches in retained traffic",
    ),
    (
        b'x',
        "Formulas",
        "Run scripts and inspect background results",
    ),
    (b'?', "Help", "Browse terminal controls and examples"),
    (
        b'l',
        "Embedded Linux / Files",
        "Browse and download files through a Linux shell",
    ),
    (b'a', "Yield input", "Release your input reservation"),
    (
        b't',
        "Take input ownership",
        "Take over input; revoke formula writes",
    ),
    (b'o', "Hold input", "Keep ownership across commands"),
    (
        b'r',
        "Rescan baud rate",
        "Restart detection; release input first",
    ),
    (b'd', "Detach", "Leave the terminal; keep capture running"),
    (
        b'q',
        "Quit / stop session",
        "Stop capture and close the UART",
    ),
];

pub(crate) enum Action {
    Stay,
    Close,
    Command(u8),
}

pub(crate) struct Menu {
    selected: usize,
    hold: bool,
    prefix: bool,
    keys: Keys,
    size: (usize, usize),
    last: String,
}
impl Menu {
    pub fn new(hold: bool) -> Self {
        Self {
            selected: 0,
            hold,
            prefix: false,
            keys: Keys::default(),
            size: crate::search::dimensions(),
            last: String::new(),
        }
    }
    // Wait briefly for CSI/SS3 bytes before treating a lone Escape as dismissal.
    pub fn escape_expired(&mut self) -> bool {
        self.keys.expired()
    }
    pub fn resized(&self) -> bool {
        self.size != crate::search::dimensions()
    }
    pub fn key(&mut self, byte: u8) -> Action {
        if self.prefix {
            self.prefix = false;
            return match byte {
                b'm' | b'b' => Action::Close,
                b'l' | b'f' | b'x' | b'?' | b'a' | b't' | b'o' | b'r' | b'd' | b'q' => {
                    Action::Command(byte)
                }
                _ => Action::Stay,
            };
        }
        let Some(byte) = self.keys.key(byte, false) else {
            return Action::Stay;
        };
        if picker::move_selection(&mut self.selected, byte, ITEMS.len()) {
            return Action::Stay;
        }
        match byte {
            29 => self.prefix = true,
            b'q' | 3 => return Action::Close,
            b'\r' | b'\n' => return Action::Command(ITEMS[self.selected].0),
            b'j' => self.selected = (self.selected + 1).min(ITEMS.len() - 1),
            b'k' => self.selected = self.selected.saturating_sub(1),
            _ => (),
        }
        Action::Stay
    }
    pub fn tick(&mut self) -> io::Result<()> {
        // The underlying view was repainted on resize, even if centering leaves
        // this popup's coordinates unchanged (for example, one extra column).
        if self.resized() {
            self.last.clear();
        }
        self.size = crate::search::dimensions();
        let text = self.frame(self.size);
        if text != self.last {
            let mut out = io::stdout().lock();
            out.write_all(text.as_bytes())?;
            out.flush()?;
            self.last = text;
        }
        Ok(())
    }
    fn frame(&self, size: (usize, usize)) -> String {
        let items: Vec<_> = ITEMS
            .iter()
            .map(|(key, label, detail)| {
                if *key == b'o' && self.hold {
                    Item::new("Hold input: on", "Release ownership and turn holding off")
                } else if *key == b'o' {
                    Item::new("Hold input: off", format!("{detail} · Ctrl-] o"))
                } else {
                    Item::new(*label, format!("{detail} · Ctrl-] {}", *key as char))
                }
            })
            .collect();
        picker::frame(
            "Menu",
            &items,
            self.selected,
            "UART capture continues",
            size,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragmented_arrows_and_unknown_sequences_stay_local() {
        let mut menu = Menu::new(false);
        for byte in b"\x1b[B\x1bOB\x1b[A\x1b[15~" {
            assert!(matches!(menu.key(*byte), Action::Stay));
        }
        assert!(matches!(menu.key(b'\r'), Action::Command(b'f')));
        assert!(matches!(menu.key(b'q'), Action::Close));
        menu.key(27);
        assert!(!menu.escape_expired());
    }

    #[test]
    fn selection_stays_visible_in_small_monochrome_grids() {
        for (columns, rows) in [(92, 28), (40, 12), (24, 8)] {
            let mut menu = Menu::new(true);
            menu.selected = ITEMS.len() - 1;
            let mut host = vt100::Parser::new(rows, columns, 0);
            host.process(menu.frame((columns as usize, rows as usize)).as_bytes());
            let screen = host.screen();
            assert!(screen.contents().contains("Quit / stop"));
            assert!((0..rows).any(|row| (0..columns).any(|col| {
                let cell = screen.cell(row, col).unwrap();
                cell.contents() == "›" && cell.inverse()
            })));
            assert!(screen.hide_cursor());
        }
    }
}
