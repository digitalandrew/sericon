use crate::{
    ipc::{self, Request},
    journal::Page,
    session::Status,
    tui::{self, Frame, Tone},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use regex::bytes::{Regex, RegexBuilder};
use std::{
    io::{self, BufRead, BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};
use unicode_width::UnicodeWidthChar;

trait Storage: Read + Write + Seek + Send {}
impl<T: Read + Write + Seek + Send> Storage for T {}

// Strip terminal commands across serial-read boundaries. Search is a text view,
// not a terminal emulator: CR is omitted, LF retained, and tabs expanded.
#[derive(Default)]
pub(crate) struct TextFilter {
    state: u8,
}
impl TextFilter {
    pub(crate) fn write(&mut self, data: &[u8], out: &mut impl Write) -> io::Result<()> {
        for &b in data {
            match self.state {
                0 => match b {
                    27 => self.state = 1,
                    b'\n' => out.write_all(b"\n")?,
                    b'\t' => out.write_all(b"    ")?,
                    32..=126 | 128..=255 => out.write_all(&[b])?,
                    _ => (),
                },
                1 => {
                    self.state = match b {
                        b'[' => 2,
                        b']' | b'P' | b'X' | b'^' | b'_' => 3,
                        0x20..=0x2f => 5,
                        _ => 0,
                    }
                }
                2 => {
                    if (0x40..=0x7e).contains(&b) {
                        self.state = 0;
                    }
                }
                3 => match b {
                    7 => self.state = 0,
                    27 => self.state = 4,
                    _ => (),
                },
                4 => {
                    self.state = if b == b'\\' || b == 7 {
                        0
                    } else if b == 27 {
                        4
                    } else {
                        3
                    }
                }
                5 => {
                    if (0x30..=0x7e).contains(&b) {
                        self.state = 0;
                    }
                }
                _ => unreachable!(),
            }
        }
        Ok(())
    }
}

struct History {
    data: BufReader<Box<dyn Storage>>,
    len: u64,
    gap: bool,
}
impl History {
    fn load(id: &str, cancel: &AtomicBool) -> Result<Self> {
        let status: Status = serde_json::from_value(ipc::call(id, &Request::Status)?)?;
        // Logged sessions can be arbitrarily long. Use an anonymous, private
        // temporary file rather than retaining the entire transcript in RAM.
        // No-log sessions never write search history to disk.
        let data: Box<dyn Storage> = if status.log_directory.is_some() {
            Box::new(tempfile::tempfile().context("create temporary search snapshot")?)
        } else {
            Box::new(Cursor::new(Vec::new()))
        };
        let mut out = BufWriter::new(data);
        let mut cursor = 0;
        let mut gap = false;
        let mut rx_filter = TextFilter::default();
        while cursor < status.latest_cursor {
            check_cancel(cancel)?;
            let page: Page = serde_json::from_value(ipc::call(
                id,
                &Request::Read {
                    after: cursor,
                    limit: 128,
                    wait_ms: 0,
                },
            )?)?;
            if page.history_gap {
                gap = true;
                rx_filter = TextFilter::default();
                out.write_all(b"\n[history gap]\n")?;
            }
            for event in &page.events {
                if event.seq > status.latest_cursor {
                    break;
                }
                if let Some(data) = &event.data_base64 {
                    let bytes = STANDARD.decode(data)?;
                    match event.kind.as_str() {
                        "rx" => rx_filter.write(&bytes, &mut out)?,
                        "tx" => {
                            write!(out, "\n[{} TX] ", event.actor)?;
                            TextFilter::default().write(&bytes, &mut out)?;
                            out.write_all(b"\n")?;
                        }
                        _ => (),
                    }
                }
            }
            if page.next_cursor <= cursor {
                bail!("session history stopped advancing");
            }
            cursor = page.next_cursor;
        }
        out.flush()?;
        let len = out.stream_position()?;
        let data = out.into_inner().map_err(|e| e.into_error())?;
        Ok(Self {
            data: BufReader::new(data),
            len,
            gap,
        })
    }

    fn find(&mut self, needle: &[u8], from: u64, cancel: &AtomicBool) -> Result<Option<u64>> {
        // KMP keeps scan memory bounded and handles terms split across reads.
        let mut prefix = vec![0; needle.len()];
        let mut matched = 0;
        for i in 1..needle.len() {
            while matched > 0 && needle[i] != needle[matched] {
                matched = prefix[matched - 1];
            }
            if needle[i] == needle[matched] {
                matched += 1;
            }
            prefix[i] = matched;
        }
        self.data.seek(SeekFrom::Start(from))?;
        let mut offset = from;
        matched = 0;
        let mut buffer = [0u8; 16384];
        loop {
            check_cancel(cancel)?;
            let n = self.data.read(&mut buffer)?;
            if n == 0 {
                return Ok(None);
            }
            for &byte in &buffer[..n] {
                while matched > 0 && byte != needle[matched] {
                    matched = prefix[matched - 1];
                }
                if byte == needle[matched] {
                    matched += 1;
                }
                offset += 1;
                if matched == needle.len() {
                    return Ok(Some(offset - needle.len() as u64));
                }
            }
        }
    }

    fn excerpt(&mut self, position: Range<u64>, number: usize, wrapped: bool) -> Result<Hit> {
        let start = position.start.saturating_sub(8192);
        // Even a regex matching a megabyte-long line gets a bounded excerpt.
        let end = (position.start + 16384).min(self.len);
        self.data.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![0; (end - start) as usize];
        self.data.read_exact(&mut bytes)?;
        let split = (position.start - start) as usize;
        let split_end = (position.end.min(end) - start) as usize;
        let before = String::from_utf8_lossy(&bytes[..split]);
        let matched = if position.is_empty() {
            "▏".into()
        } else {
            String::from_utf8_lossy(&bytes[split..split_end])
        };
        let after = String::from_utf8_lossy(&bytes[split_end..]);
        let active = before.len()..before.len() + matched.len();
        Ok(Hit {
            text: format!("{before}{matched}{after}"),
            active,
            number,
            wrapped,
            empty: position.is_empty(),
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Literal,
    Regex,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Literal => "literal",
            Self::Regex => "regex",
        }
    }
    fn other(self) -> Self {
        match self {
            Self::Literal => Self::Regex,
            Self::Regex => Self::Literal,
        }
    }
    fn message(self) -> String {
        format!(
            "Type a {} search {}, then Enter.",
            self.name(),
            if self == Self::Regex {
                "pattern"
            } else {
                "string"
            }
        )
    }
}

enum Matcher {
    Literal(String),
    Regex(RegexScan),
}
impl Matcher {
    fn compile(query: String, mode: Mode) -> Result<Self> {
        if mode == Mode::Literal {
            return Ok(Self::Literal(query));
        }
        let regex = RegexBuilder::new(&query)
            .size_limit(2 * 1024 * 1024)
            .dfa_size_limit(1024 * 1024)
            .nest_limit(64)
            .build()
            .map_err(|e| {
                anyhow::anyhow!(
                    "Invalid regex: {}",
                    e.to_string().lines().last().unwrap_or("invalid pattern")
                )
            })?;
        Ok(Self::Regex(RegexScan {
            regex,
            line: Vec::new(),
            start: 0,
            next: 0,
            loaded: false,
        }))
    }
    fn find(
        &mut self,
        history: &mut History,
        from: u64,
        cancel: &AtomicBool,
    ) -> Result<Option<Range<u64>>> {
        match self {
            Self::Literal(text) => Ok(history
                .find(text.as_bytes(), from, cancel)?
                .map(|start| start..start + text.len() as u64)),
            Self::Regex(scan) => scan.find(history, from, cancel),
        }
    }
    fn advance(&self, position: &Range<u64>) -> u64 {
        match self {
            Self::Literal(_) => position.start + 1, // Preserve overlapping literal matches.
            Self::Regex(_) => position.end.max(position.start + 1),
        }
    }
}

const MAX_REGEX_LINE: usize = 8 * 1024 * 1024;
struct RegexScan {
    regex: Regex,
    line: Vec<u8>,
    start: u64,
    next: u64,
    loaded: bool,
}
impl RegexScan {
    fn find(
        &mut self,
        history: &mut History,
        from: u64,
        cancel: &AtomicBool,
    ) -> Result<Option<Range<u64>>> {
        check_cancel(cancel)?;
        if from == 0 {
            self.loaded = false;
            self.next = 0;
        }
        // Keep the complete line so ^ and word boundaries retain their context
        // when advancing through several matches on the same line.
        if self.loaded
            && let Some(found) = self.find_in_line(from)
        {
            return Ok(Some(found));
        }
        history.data.seek(SeekFrom::Start(self.next))?;
        while self.next < history.len {
            check_cancel(cancel)?;
            self.start = self.next;
            self.line.clear();
            loop {
                check_cancel(cancel)?;
                let buffer = history.data.fill_buf()?;
                if buffer.is_empty() {
                    break;
                }
                let n = buffer
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(buffer.len(), |i| i + 1);
                if self.line.len() + n > MAX_REGEX_LINE {
                    bail!(
                        "Regex search encountered a line over 8 MiB; use literal search for this data"
                    );
                }
                let end = buffer[n - 1] == b'\n';
                self.line.extend_from_slice(&buffer[..n]);
                history.data.consume(n);
                self.next += n as u64;
                if end {
                    break;
                }
            }
            if self.next == self.start {
                bail!("search snapshot ended unexpectedly");
            }
            if self.line.last() == Some(&b'\n') {
                self.line.pop();
            }
            self.loaded = true;
            if let Some(found) = self.find_in_line(from) {
                return Ok(Some(found));
            }
        }
        Ok(None)
    }
    fn find_in_line(&self, from: u64) -> Option<Range<u64>> {
        let offset = usize::try_from(from.saturating_sub(self.start)).ok()?;
        if offset > self.line.len() {
            return None;
        }
        let mut offset = offset;
        loop {
            let m = self.regex.find_at(&self.line, offset)?;
            // bytes::Regex can report empty matches inside a UTF-8 codepoint.
            // Keep the visible zero-width marker on character boundaries.
            if m.is_empty() && self.line.get(m.start()).is_some_and(|b| b & 0xc0 == 0x80) {
                offset = m.end() + 1;
                continue;
            }
            return Some(self.start + m.start() as u64..self.start + m.end() as u64);
        }
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        bail!("search cancelled");
    }
    Ok(())
}

struct Hit {
    text: String,
    active: Range<usize>,
    number: usize,
    wrapped: bool,
    empty: bool,
}
struct Found {
    hit: Option<Hit>,
    gap: bool,
}
struct Task {
    cancel: Arc<AtomicBool>,
    next: mpsc::Sender<()>,
    results: mpsc::Receiver<Result<Found>>,
}
impl Task {
    fn start(id: String, mut matcher: Matcher) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (next, commands) = mpsc::channel();
        let (results, receive) = mpsc::channel();
        thread::spawn(move || {
            let mut run = || -> Result<()> {
                let mut history = History::load(&id, &worker_cancel)?;
                let mut from = 0;
                let mut number = 0;
                loop {
                    check_cancel(&worker_cancel)?;
                    let mut position = matcher.find(&mut history, from, &worker_cancel)?;
                    let wrapped = position.is_none() && from != 0;
                    if wrapped {
                        position = matcher.find(&mut history, 0, &worker_cancel)?;
                        number = 0;
                    }
                    let hit = if let Some(position) = position {
                        number += 1;
                        from = matcher.advance(&position);
                        Some(history.excerpt(position, number, wrapped)?)
                    } else {
                        None
                    };
                    if results
                        .send(Ok(Found {
                            hit,
                            gap: history.gap,
                        }))
                        .is_err()
                        || commands.recv().is_err()
                    {
                        break;
                    }
                }
                Ok(())
            };
            if let Err(e) = run() {
                let _ = results.send(Err(e));
            }
        });
        Self {
            cancel,
            next,
            results: receive,
        }
    }
}
impl Drop for Task {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub enum Action {
    Stay,
    Menu,
    Back,
    Stop,
    Detach,
}
pub struct View {
    id: String,
    query: Vec<u8>,
    mode: Mode,
    editing: bool,
    prefix: bool,
    keys: crate::picker::Keys,
    options: Option<usize>,
    busy: bool,
    task: Option<Task>,
    found: Option<Found>,
    message: String,
    dimensions: (usize, usize),
    showing_help: bool,
    help_scroll: usize,
}
impl View {
    pub fn open(id: &str) -> Result<Self> {
        let mut view = Self {
            id: id.into(),
            query: Vec::new(),
            mode: Mode::Literal,
            editing: true,
            prefix: false,
            keys: crate::picker::Keys::default(),
            options: None,
            busy: false,
            task: None,
            found: None,
            message: Mode::Literal.message(),
            dimensions: (0, 0),
            showing_help: false,
            help_scroll: 0,
        };
        io::stdout().write_all(b"\x1b[?25l")?;
        view.render()?;
        Ok(view)
    }
    pub fn key(&mut self, mut byte: u8) -> Result<Action> {
        if !self.prefix && byte != 29 {
            let Some(key) = self.keys.key(
                byte,
                self.editing && self.options.is_none() && !self.showing_help,
            ) else {
                return Ok(Action::Stay);
            };
            byte = key;
            if let Some(mut selected) = self.options.filter(|_| !self.showing_help) {
                let items = self.option_items();
                if crate::picker::move_selection(&mut selected, byte, items.len()) {
                    self.options = Some(selected);
                } else if byte == b'q' || byte == 3 {
                    self.options = None;
                } else if matches!(byte, b'\r' | b'\n') {
                    self.options = None;
                    match selected {
                        0 => (),
                        1 => return self.key(b'\r'),
                        2 => self.edit_query(),
                        3 | 4 => {
                            let mode = if selected == 3 {
                                Mode::Literal
                            } else {
                                Mode::Regex
                            };
                            if self.mode != mode {
                                self.toggle_mode();
                            }
                        }
                        5 => self.showing_help = true,
                        _ => return Ok(Action::Back),
                    }
                }
                self.render()?;
                return Ok(Action::Stay);
            }
            if byte == 3 || (byte == b'o' && !self.editing && !self.showing_help) {
                self.options = Some(0);
                self.render()?;
                return Ok(Action::Stay);
            }
        }
        if self.prefix {
            self.prefix = false;
            match byte {
                b'm' => return Ok(Action::Menu),
                b'b' => return Ok(Action::Back),
                b'q' => return Ok(Action::Stop),
                b'd' => return Ok(Action::Detach),
                b'f' => self.edit_query(),
                b'?' => {
                    self.keys = crate::picker::Keys::default();
                    self.showing_help = !self.showing_help;
                }
                _ => (),
            }
        } else if byte == 29 {
            self.prefix = true;
        } else if self.showing_help {
            if matches!(byte, b'q' | b'\r' | b'\n') {
                self.showing_help = false;
            } else if byte == b'j' {
                self.help_scroll = self.help_scroll.saturating_add(1);
            } else if byte == b'k' {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            } else if byte == b' ' {
                self.help_scroll = self.help_scroll.saturating_add(10);
            }
        } else if self.editing {
            match byte {
                b'\r' | b'\n' if !self.query.is_empty() => {
                    match String::from_utf8(self.query.clone()) {
                        Ok(query) => match Matcher::compile(query, self.mode) {
                            Ok(matcher) => {
                                self.task = Some(Task::start(self.id.clone(), matcher));
                                self.editing = false;
                                self.busy = true;
                                self.message = "Searching retained history...".into();
                            }
                            Err(e) => {
                                self.message = format!("{e}; edit the pattern or Tab for literal.")
                            }
                        },
                        Err(_) => {
                            self.message =
                                "Search text must be UTF-8; Ctrl-U clears the prompt.".into()
                        }
                    }
                }
                8 | 127 => {
                    if let Some(last) = self.query.pop()
                        && last & 0xc0 == 0x80
                    {
                        while self.query.last().is_some_and(|b| b & 0xc0 == 0x80) {
                            self.query.pop();
                        }
                        self.query.pop();
                    }
                }
                21 => self.query.clear(),
                9 => self.toggle_mode(),
                32..=126 | 128..=255 if self.query.len() < 1024 => self.query.push(byte),
                _ => (),
            }
        } else if byte == b'q' {
            return Ok(Action::Back);
        } else if byte == b'f' {
            self.edit_query();
        } else if byte == b'r' || byte == 9 {
            self.toggle_mode();
        } else if matches!(byte, b'\r' | b'\n')
            && !self.busy
            && self.found.as_ref().is_some_and(|f| f.hit.is_some())
        {
            if let Some(task) = &self.task {
                let _ = task.next.send(());
            }
            self.busy = true;
            self.message = "Searching for the next match...".into();
        }
        self.render()?;
        Ok(Action::Stay)
    }
    fn edit_query(&mut self) {
        self.task = None;
        self.query.clear();
        self.editing = true;
        self.keys = crate::picker::Keys::default();
        self.busy = false;
        self.found = None;
        self.showing_help = false;
        self.message = self.mode.message();
    }
    pub fn wheel(&mut self, up: bool) -> Result<()> {
        if self.options.is_some() || self.showing_help {
            self.key(if up { b'k' } else { b'j' })?;
        }
        Ok(())
    }
    fn toggle_mode(&mut self) {
        let query = std::mem::take(&mut self.query);
        self.mode = self.mode.other();
        self.edit_query();
        self.query = query;
    }
    pub fn tick(&mut self) -> Result<()> {
        if self.keys.expired() {
            if self.showing_help {
                self.showing_help = false;
            } else {
                self.options = if self.options.is_some() {
                    None
                } else {
                    Some(0)
                };
            }
            self.render()?;
        }
        let result = self.task.as_ref().and_then(|t| t.results.try_recv().ok());
        if let Some(result) = result {
            self.busy = false;
            match result {
                Ok(found) => {
                    let gap = if found.gap {
                        "Earlier history unavailable | "
                    } else {
                        ""
                    };
                    self.message = match &found.hit {
                        Some(hit) => format!(
                            "{gap}Match {}{}{}",
                            hit.number,
                            if hit.wrapped { " (wrapped)" } else { "" },
                            if hit.empty { " (zero-width)" } else { "" },
                        ),
                        None => format!("{gap}No matches; f to search again, q returns to live."),
                    };
                    self.found = Some(found);
                }
                Err(e) => {
                    self.message = format!("Search failed: {e:#}; q returns to live.");
                }
            }
            self.render()?;
        } else if dimensions() != self.dimensions {
            self.render()?;
        }
        Ok(())
    }
    pub(crate) fn render(&mut self) -> Result<()> {
        self.dimensions = dimensions();
        let query = String::from_utf8_lossy(&self.query);
        let mut frame = Frame::new(
            if self.showing_help {
                "Search help"
            } else {
                "Search"
            },
            "Retained RX + TX · local search · live capture continues",
        );
        if self.showing_help {
            let lines = tui::wrap(crate::help::SEARCH, frame.columns);
            self.help_scroll = self.help_scroll.min(lines.len().saturating_sub(frame.rows));
            for (i, line) in lines
                .iter()
                .skip(self.help_scroll)
                .take(frame.rows)
                .enumerate()
            {
                frame.body(
                    i,
                    line,
                    if line.ends_with(':') {
                        Tone::Accent
                    } else {
                        Tone::Normal
                    },
                );
            }
            frame.footer(
                "Your query and selected match are preserved.",
                Tone::Muted,
                if frame.columns < 60 {
                    "q close · j/k scroll · ^]b live"
                } else {
                    "Enter/q: close help · j/k: scroll · Ctrl-] b: live"
                },
            );
        } else {
            let label = format!("[{}] /", self.mode.name());
            frame.body(0, &prompt(&query, frame.columns, self.mode), Tone::Accent);
            if let Some(hit) = self.found.as_ref().and_then(|f| f.hit.as_ref()) {
                let rows = frame.rows.saturating_sub(2);
                for (i, row) in context_rows(
                    hit,
                    (self.mode == Mode::Literal).then_some(query.as_ref()),
                    frame.columns,
                    rows,
                    frame.theme.active_match(),
                )
                .into_iter()
                .enumerate()
                {
                    frame.rich(frame.top + 2 + i, &row);
                }
            } else {
                let (heading, hint) = if self.editing {
                    (
                        "Search retained session history",
                        "Type a query above. Tab switches between literal and regex.",
                    )
                } else if self.busy {
                    (
                        "Searching…",
                        "Capture and other clients continue while the history is scanned.",
                    )
                } else if self.found.is_some() {
                    (
                        "No matches found.",
                        "Press f to edit a fresh query, or q to return to the live terminal.",
                    )
                } else {
                    (
                        "Search could not finish.",
                        "Press f to try another query, or q to return to the live terminal.",
                    )
                };
                frame.body(2, heading, Tone::Normal);
                for (i, line) in tui::wrap(hint, frame.columns).iter().enumerate() {
                    frame.body(4 + i, line, Tone::Muted);
                }
                if self.mode == Mode::Regex && self.editing {
                    frame.body(
                        7,
                        "Try: error|warning   (?i)failed   code=\\d{3}",
                        Tone::Muted,
                    );
                }
                if self.message.starts_with("Invalid") || self.message.starts_with("Search failed")
                {
                    for (i, line) in tui::wrap(&self.message, frame.columns).iter().enumerate() {
                        frame.body(9 + i, line, Tone::Error);
                    }
                }
            }
            let keys = if self.editing {
                format!(
                    "Enter: search · Esc: options · Tab: {} · Ctrl-U: clear",
                    self.mode.other().name()
                )
            } else {
                format!(
                    "Enter: next · Esc: options · f: new query · r: {} · q: live",
                    self.mode.other().name()
                )
            };
            let tone = if self.message.starts_with("Invalid") || self.message.contains("failed") {
                Tone::Error
            } else if self
                .found
                .as_ref()
                .is_some_and(|f| f.hit.is_none() || f.gap)
            {
                Tone::Warning
            } else {
                Tone::Muted
            };
            let keys = if frame.columns < 60 {
                if self.editing {
                    "Enter find · Esc options · Tab mode"
                } else {
                    "Enter next · Esc options · q live"
                }
            } else {
                &keys
            };
            frame.footer(&self.message, tone, keys);
            if self.editing {
                frame.input(0, &label, &query);
            }
        }
        let mut out = io::stdout().lock();
        let mut text = frame.finish();
        if let Some(selected) = self.options.filter(|_| !self.showing_help) {
            text.push_str(&crate::picker::frame(
                "Search options",
                &self.option_items(),
                selected,
                "Your query and match are preserved",
                self.dimensions,
            ));
        }
        out.write_all(text.as_bytes())?;
        out.flush()?;
        Ok(())
    }
    fn option_items(&self) -> Vec<crate::picker::Item> {
        use crate::picker::Item;
        vec![
            Item::new(
                "Return to search",
                "Keep the current query and selected match",
            ),
            Item::new(
                if self.editing { "Search" } else { "Next match" },
                "Find the next occurrence in retained RX and TX",
            ),
            Item::new("New search", "Enter a new query"),
            Item::new(
                "Literal text",
                "Match the text exactly; punctuation has no special meaning",
            ),
            Item::new(
                "Regular expression",
                "Use regex patterns such as error|warning or (?i)failed",
            ),
            Item::new("Search help", "Read examples and available controls"),
            Item::new(
                "Back to live terminal",
                "Capture continues while you search",
            ),
        ]
    }
}
impl Drop for View {
    fn drop(&mut self) {
        self.task = None;
        let mut out = io::stdout().lock();
        let _ = out.write_all(b"\x1b[0m\x1b[?25h");
        let _ = out.flush();
    }
}
pub(crate) fn dimensions() -> (usize, usize) {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut size) } == 0
        && size.ws_col > 0
        && size.ws_row > 0
    {
        (
            usize::from(size.ws_col).saturating_sub(1).max(1),
            usize::from(size.ws_row).max(4),
        )
    } else {
        (79, 24)
    }
}
pub(crate) fn clipped(text: &str, width: usize) -> String {
    let mut used = 0;
    text.chars()
        .filter(|c| !c.is_control())
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= width
        })
        .collect()
}
fn prompt(query: &str, width: usize, mode: Mode) -> String {
    let prefix = format!("[{}] /", mode.name());
    let tail = tui::tail(query, width.saturating_sub(prefix.len() + 1));
    tui::clip(&format!("{prefix}{tail}"), width)
}
fn context_rows(
    hit: &Hit,
    query: Option<&str>,
    width: usize,
    height: usize,
    active_style: &str,
) -> Vec<String> {
    let matches: Vec<_> = query
        .map(|query| {
            hit.text
                .match_indices(query)
                .map(|(i, s)| i..i + s.len())
                .collect()
        })
        .unwrap_or_default();
    let mut rows = vec![String::new()];
    let mut col = 0;
    let mut selected = 0;
    let mut style = "";
    let mut matching = 0;
    for (i, c) in hit.text.char_indices() {
        if c == '\n' || col + c.width().unwrap_or(0) > width {
            rows.last_mut().unwrap().push_str("\x1b[0m");
            rows.push(String::new());
            col = 0;
            style = "";
        }
        if i == hit.active.start {
            selected = rows.len() - 1;
        }
        if c.is_control() {
            continue;
        }
        while matches.get(matching).is_some_and(|m| m.end <= i) {
            matching += 1;
        }
        let wanted = if hit.active.contains(&i) {
            active_style
        } else if matches.get(matching).is_some_and(|m| m.contains(&i)) {
            "\x1b[7m"
        } else {
            "\x1b[0m"
        };
        let row = rows.last_mut().unwrap();
        if style != wanted {
            row.push_str(wanted);
            style = wanted;
        }
        row.push(c);
        col += c.width().unwrap_or(0);
    }
    rows.last_mut().unwrap().push_str("\x1b[0m");
    let start = selected
        .saturating_sub(height / 2)
        .min(rows.len().saturating_sub(height));
    rows.into_iter().skip(start).take(height).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_split_terminal_sequences_without_splitting_words() {
        let mut filter = TextFilter::default();
        let mut out = Vec::new();
        for part in [
            b"bo\x1b[3".as_slice(),
            b"2mot\x1b[0m\r\n\x1b]52;c;bad",
            b"\x1b\\safe\0",
        ] {
            filter.write(part, &mut out).unwrap();
        }
        assert_eq!(out, b"boot\nsafe");
    }
    #[test]
    fn streaming_search_handles_boundaries_overlaps_and_cancel() {
        let mut bytes = vec![b'x'; 16383];
        bytes.extend_from_slice(b"banana");
        let mut history = History {
            len: bytes.len() as u64,
            data: BufReader::new(Box::new(Cursor::new(bytes))),
            gap: false,
        };
        let cancel = AtomicBool::new(false);
        assert_eq!(history.find(b"banana", 0, &cancel).unwrap(), Some(16383));
        assert_eq!(history.find(b"ana", 0, &cancel).unwrap(), Some(16384));
        assert_eq!(history.find(b"ana", 16385, &cancel).unwrap(), Some(16386));
        assert_eq!(history.find(b"missing", 0, &cancel).unwrap(), None);
        cancel.store(true, Ordering::Relaxed);
        assert!(history.find(b"x", 0, &cancel).is_err());
    }
    #[test]
    fn wraps_long_unicode_lines_and_keeps_active_match_visible() {
        let before = "界".repeat(500);
        let hit = Hit {
            text: format!("{before}needle{}", "z".repeat(500)),
            active: before.len()..before.len() + 6,
            number: 1,
            wrapped: false,
            empty: false,
        };
        let rows = context_rows(&hit, Some("needle"), 20, 6, "\x1b[1;30;43m");
        assert!(rows.join("\n").contains("\x1b[1;30;43mneedle"));
        assert_eq!(rows.len(), 6);
        assert_eq!(clipped("界界\x1b", 3), "界");
        assert_eq!(
            prompt("a very long search term", 24, Mode::Literal),
            "[literal] /…search term"
        );
    }

    fn history(bytes: &[u8]) -> History {
        History {
            len: bytes.len() as u64,
            data: BufReader::new(Box::new(Cursor::new(bytes.to_vec()))),
            gap: false,
        }
    }

    #[test]
    fn regex_matches_variable_lengths_across_buffers_and_keeps_anchor_context() {
        let mut bytes = vec![b'x'; 16382];
        bytes.extend_from_slice(b" ERROR 17 err 3\nerrorerror\nerror 222\n");
        let cancel = AtomicBool::new(false);
        let mut h = history(&bytes);
        let mut m = Matcher::compile(r"(?i)(?:error|err) [0-9]+".into(), Mode::Regex).unwrap();
        let mut from = 0;
        let mut found = Vec::new();
        while let Some(position) = m.find(&mut h, from, &cancel).unwrap() {
            found.push(bytes[position.start as usize..position.end as usize].to_vec());
            from = m.advance(&position);
            let hit = h.excerpt(position, found.len(), false).unwrap();
            assert!(
                hit.text
                    .contains(std::str::from_utf8(found.last().unwrap()).unwrap())
            );
        }
        assert_eq!(
            found,
            [
                b"ERROR 17".to_vec(),
                b"err 3".to_vec(),
                b"error 222".to_vec()
            ]
        );
        let mut m = Matcher::compile("^error".into(), Mode::Regex).unwrap();
        let first = m.find(&mut h, 0, &cancel).unwrap().unwrap();
        let next = m.find(&mut h, first.end, &cancel).unwrap().unwrap();
        assert_eq!(
            &bytes[next.start as usize..next.end as usize + 4],
            b"error 222"
        );
        let mut m = Matcher::compile(r"\berror\b".into(), Mode::Regex).unwrap();
        assert_eq!(
            m.find(&mut h, first.start + 5, &cancel).unwrap(),
            Some(next)
        );
    }

    #[test]
    fn regex_empty_matches_advance_wrap_and_highlight_unicode_boundaries() {
        let cancel = AtomicBool::new(false);
        let mut h = history("é\nx\n".as_bytes());
        let mut m = Matcher::compile("^|$".into(), Mode::Regex).unwrap();
        let mut from = 0;
        let mut positions = Vec::new();
        while let Some(position) = m.find(&mut h, from, &cancel).unwrap() {
            assert!(position.is_empty());
            from = m.advance(&position);
            positions.push(position.start);
            let hit = h.excerpt(position, positions.len(), false).unwrap();
            assert!(
                context_rows(&hit, None, 40, 8, "\x1b[1;30;43m")
                    .join("")
                    .contains("\x1b[1;30;43m▏")
            );
            assert!(positions.len() < 10);
        }
        assert_eq!(positions, vec![0, 2, 3, 4]);
        assert_eq!(m.find(&mut h, 0, &cancel).unwrap(), Some(0..0));
        let mut m = Matcher::compile("(?:)".into(), Mode::Regex).unwrap();
        assert_eq!(m.find(&mut h, 1, &cancel).unwrap(), Some(2..2));
    }

    #[test]
    fn regex_reports_invalid_patterns_and_resource_limits_without_truncating_lines() {
        assert!(Matcher::compile("[".into(), Mode::Regex).is_err());
        assert!(Matcher::compile(r"(foo)\1".into(), Mode::Regex).is_err());
        assert!(Matcher::compile(r"a{100000000}".into(), Mode::Regex).is_err());
        assert!(Matcher::compile("[".into(), Mode::Literal).is_ok());
        let mut h = history(&vec![b'x'; MAX_REGEX_LINE + 1]);
        let mut m = Matcher::compile("x".into(), Mode::Regex).unwrap();
        assert!(
            m.find(&mut h, 0, &AtomicBool::new(false))
                .unwrap_err()
                .to_string()
                .contains("over 8 MiB")
        );
    }
}
