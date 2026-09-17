use super::{Definition, get, list};
use crate::{
    ipc::{self, Request},
    picker::{self, Item, Keys},
    search::Action,
    tui::{self, Frame, Tone},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone)]
enum Screen {
    Formulas,
    Configure(String),
    Field(String, String),
    Parameters(String),
    Runs,
    Results(String),
    Detail(String, Value),
    Source(String),
}
pub struct View {
    session: String,
    client: String,
    config: Option<PathBuf>,
    definitions: Vec<Definition>,
    runs: Vec<Value>,
    screen: Screen,
    selected: usize,
    offset: usize,
    page: Value,
    input: Vec<u8>,
    message: String,
    showing_help: bool,
    help_scroll: usize,
    message_error: bool,
    prefix: bool,
    keys: Keys,
    options: Value,
    source_parent: Option<String>,
    pub closed: bool,
    fetched: Instant,
    last_render: String,
}
impl View {
    pub fn open(session: &str, client: &str, config: Option<&Path>) -> Result<Self> {
        let definitions = list(config)?;
        let view = Self {
            session: session.into(),
            client: client.into(),
            config: config.map(Path::to_path_buf),
            definitions,
            runs: Vec::new(),
            screen: Screen::Formulas,
            selected: 0,
            offset: 0,
            page: Value::Null,
            input: Vec::new(),
            message: String::new(),
            showing_help: false,
            help_scroll: 0,
            message_error: false,
            prefix: false,
            keys: Keys::default(),
            options: json!({"params":{}}),
            source_parent: None,
            closed: false,
            fetched: Instant::now() - Duration::from_secs(1),
            last_render: String::new(),
        };
        io::stdout().write_all(b"\x1b[?25l")?;
        Ok(view)
    }
    pub fn tick(&mut self) -> Result<()> {
        if self.keys.expired() {
            self.closed = matches!(self.back(), Action::Back);
        }
        if self.fetched.elapsed() >= Duration::from_millis(300) {
            let result = match &self.screen {
                Screen::Runs => ipc::call(&self.session, &Request::FormulaRuns)
                    .map(|v| self.runs = v.as_array().cloned().unwrap_or_default()),
                Screen::Results(run) => ipc::call(
                    &self.session,
                    &Request::FormulaRead {
                        run: run.clone(),
                        after: self.offset,
                        limit: 32,
                    },
                )
                .map(|v| self.page = v),
                _ => Ok(()),
            };
            if let Err(e) = result {
                self.message = format!("{e:#}");
                self.message_error = true;
            }
            self.fetched = Instant::now();
        }
        self.render()
    }
    pub fn redraw(&mut self) -> Result<()> {
        self.last_render.clear();
        self.tick()
    }
    pub fn wheel(&mut self, up: bool) -> Result<()> {
        if self.menu_items().is_some()
            || self.showing_help
            || matches!(self.screen, Screen::Detail(..) | Screen::Source(_))
        {
            self.key(if up { b'k' } else { b'j' })?;
        }
        Ok(())
    }
    fn back(&mut self) -> Action {
        self.keys = Keys::default();
        if self.showing_help {
            self.showing_help = false;
            return Action::Stay;
        }
        self.message.clear();
        self.message_error = false;
        if !matches!(self.screen, Screen::Field(..) | Screen::Parameters(_)) {
            self.selected = 0;
        }
        self.screen = match self.screen.clone() {
            Screen::Formulas => return Action::Back,
            Screen::Parameters(name) | Screen::Field(name, _) => Screen::Configure(name),
            Screen::Configure(_) | Screen::Runs => Screen::Formulas,
            Screen::Results(_) => Screen::Runs,
            Screen::Detail(run, _) => Screen::Results(run),
            Screen::Source(_) => self
                .source_parent
                .take()
                .map_or(Screen::Formulas, Screen::Configure),
        };
        self.fetched = Instant::now() - Duration::from_secs(1);
        Action::Stay
    }
    fn config_fields(&self, name: &str) -> Vec<String> {
        let mut fields: Vec<_> = self
            .definitions
            .iter()
            .find(|d| d.name == name)
            .into_iter()
            .flat_map(|d| d.parameters.keys().map(|key| format!("params.{key}")))
            .collect();
        fields.extend(["timeout_ms".into(), "output_dir".into()]);
        fields
    }
    fn field_kind(&self, name: &str, key: &str) -> &str {
        if key == "timeout_ms" {
            return "integer";
        }
        if key == "output_dir" {
            return "string";
        }
        self.definitions
            .iter()
            .find(|d| d.name == name)
            .and_then(|d| d.parameters.get(key.strip_prefix("params.").unwrap_or(key)))
            .map_or("string", |p| p.kind.as_str())
    }
    fn menu_items(&self) -> Option<(String, Vec<(u8, Item)>)> {
        let item = |key, label: &str, detail: &str| (key, Item::new(label, detail));
        let (title, items) = match &self.screen {
            Screen::Formulas => {
                let mut items: Vec<_> = self
                    .definitions
                    .iter()
                    .map(|d| (b'\r', Item::new(&d.name, &d.description)))
                    .collect();
                items.extend([
                    item(
                        b'r',
                        "Recent runs",
                        "Inspect progress, results, artifacts, or cancel a job",
                    ),
                    item(
                        b'?',
                        "Formula help",
                        "Controls, parameters and scripting reference",
                    ),
                    item(
                        b'q',
                        "Back to live terminal",
                        "Background jobs and capture continue",
                    ),
                ]);
                ("Formulas".into(), items)
            }
            Screen::Configure(name) => {
                let definition = self.definitions.iter().find(|d| &d.name == name)?;
                let mut items = vec![item(
                    b'\r',
                    "Run formula",
                    if definition.interactive {
                        "This formula can send UART commands. Enter starts a background run."
                    } else {
                        "Analyse retained history. Enter starts a background run."
                    },
                )];
                for key in self.config_fields(name) {
                    let (label, value, description) =
                        if let Some(param) = key.strip_prefix("params.") {
                            let p = &definition.parameters[param];
                            (
                                param,
                                &self.options["params"][param],
                                p.description.as_str(),
                            )
                        } else {
                            (
                                key.as_str(),
                                &self.options[&key],
                                if key == "timeout_ms" {
                                    "Maximum run time in milliseconds; default 300000"
                                } else {
                                    "Host directory for artifacts; empty uses the session default"
                                },
                            )
                        };
                    let value = if value.is_null() {
                        "(default / unset)".into()
                    } else if let Some(v) = value.as_str() {
                        v.to_owned()
                    } else {
                        value.to_string()
                    };
                    items.push((b'e', Item::new(format!("{label}: {value}"), description)));
                }
                items.extend([
                    item(
                        b'J',
                        "Edit advanced JSON",
                        "Edit the complete options object, then Enter to run",
                    ),
                    item(b's', "View source", "Read the formula before running it"),
                    item(b'q', "Back to formulas", "Keep capture running"),
                ]);
                (format!("Run {name}"), items)
            }
            Screen::Runs => {
                let mut items: Vec<_> = self
                    .runs
                    .iter()
                    .map(|r| {
                        (
                            b'\r',
                            Item::new(
                                format!(
                                    "{} · {}",
                                    r["formula"].as_str().unwrap_or("?"),
                                    r["state"].as_str().unwrap_or("?")
                                ),
                                format!(
                                    "Run {} · {} findings",
                                    r["id"].as_str().unwrap_or(""),
                                    r["result_count"]
                                ),
                            ),
                        )
                    })
                    .collect();
                items.push(item(
                    b'f',
                    "Choose a formula",
                    "No formula runs yet. Start one from the formula list.",
                ));
                items.push(item(b'q', "Back to formulas", "Return to the formula list"));
                ("Recent runs".into(), items)
            }
            Screen::Results(_) => {
                let mut items: Vec<_> = self.page["results"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|r| {
                        (
                            b'\r',
                            Item::new(
                                r["value"]
                                    .as_str()
                                    .map(str::to_owned)
                                    .unwrap_or_else(|| r.to_string()),
                                "Open finding context and retained evidence",
                            ),
                        )
                    })
                    .collect();
                let detail = if items.is_empty() {
                    match self.page["run"]["state"].as_str() {
                        Some("completed") => "No findings in this run.",
                        Some("failed") => "Formula failed.",
                        Some("cancelled") => "Formula cancelled.",
                        _ => "Running formula…",
                    }
                } else {
                    "View status, warnings and output paths"
                };
                items.extend([
                    item(b'v', "Run details", detail),
                    item(
                        b'c',
                        "Cancel run",
                        "Revoke further formula writes; retained output remains",
                    ),
                    item(b'n', "Next page", "Read the next page of findings"),
                    item(b'p', "Previous page", "Read the previous page of findings"),
                    item(b'r', "Recent runs", "Choose another run"),
                    item(b'f', "Choose a formula", "Configure a new background run"),
                    item(b'q', "Back", "Return to recent runs"),
                ]);
                (
                    format!(
                        "Formulas / {}",
                        self.page["run"]["formula"].as_str().unwrap_or("Results")
                    ),
                    items,
                )
            }
            _ => return None,
        };
        Some((title, items))
    }
    pub fn key(&mut self, byte: u8) -> Result<Action> {
        let action = self.handle(byte);
        let action = match action {
            Ok(a) => a,
            Err(e) => {
                self.message = format!("{e:#}");
                self.message_error = true;
                Action::Stay
            }
        };
        self.tick()?;
        Ok(action)
    }
    fn handle(&mut self, mut byte: u8) -> Result<Action> {
        if self.prefix {
            self.prefix = false;
            self.keys = Keys::default();
            return Ok(match byte {
                b'm' => Action::Menu,
                b'b' => Action::Back,
                b'q' => Action::Stop,
                b'd' => Action::Detach,
                b't' => {
                    ipc::call(
                        &self.session,
                        &Request::Claim {
                            actor: "human".into(),
                            client_id: self.client.clone(),
                            takeover: true,
                        },
                    )?;
                    self.message = "Input taken over; formula writes revoked".into();
                    Action::Stay
                }
                b'?' => {
                    self.showing_help = !self.showing_help;
                    Action::Stay
                }
                _ => Action::Stay,
            });
        }
        if byte == 29 {
            self.prefix = true;
            return Ok(Action::Stay);
        }
        let prompt =
            matches!(self.screen, Screen::Parameters(_) | Screen::Field(..)) && !self.showing_help;
        let Some(key) = self.keys.key(byte, prompt) else {
            return Ok(Action::Stay);
        };
        byte = key;
        if byte == 3 || (byte == b'q' && !prompt && !self.showing_help) {
            return Ok(self.back());
        }
        if self.showing_help {
            match byte {
                b'q' | b'\r' | b'\n' | b'?' => self.showing_help = false,
                b'j' => self.help_scroll = self.help_scroll.saturating_add(1),
                b'k' => self.help_scroll = self.help_scroll.saturating_sub(1),
                _ => (),
            }
            return Ok(Action::Stay);
        }
        if let Screen::Field(name, key) = self.screen.clone() {
            match byte {
                b'\r' | b'\n' => {
                    let text = std::str::from_utf8(&self.input)?;
                    let kind = self.field_kind(&name, &key);
                    let value = if text.is_empty() {
                        Value::Null
                    } else if kind == "string" {
                        json!(text)
                    } else {
                        serde_json::from_str(text)
                            .context("Enter a valid JSON value for this field")?
                    };
                    anyhow::ensure!(
                        value.is_null()
                            || match kind {
                                "integer" => value.as_i64().is_some(),
                                "number" => value.is_number(),
                                "boolean" => value.is_boolean(),
                                _ => value.is_string(),
                            },
                        "Expected {kind}; edit the value or Esc to go back"
                    );
                    let dest = if let Some(param) = key.strip_prefix("params.") {
                        &mut self.options["params"][param]
                    } else {
                        &mut self.options[&key]
                    };
                    *dest = value;
                    // Missing optional values should retain the formula's defaults.
                    if dest.is_null() {
                        if let Some(param) = key.strip_prefix("params.") {
                            self.options["params"]
                                .as_object_mut()
                                .unwrap()
                                .remove(param);
                        } else {
                            self.options.as_object_mut().unwrap().remove(&key);
                        }
                    }
                    self.screen = Screen::Configure(name);
                    self.message.clear();
                    self.message_error = false;
                }
                21 => self.input.clear(),
                8 | 127 => {
                    self.input.pop();
                    while std::str::from_utf8(&self.input).is_err() {
                        self.input.pop();
                    }
                }
                32..=255 if self.input.len() < 8192 => self.input.push(byte),
                _ => (),
            }
            return Ok(Action::Stay);
        }
        if let Some((_, items)) = self.menu_items() {
            if picker::move_selection(&mut self.selected, byte, items.len()) {
                return Ok(Action::Stay);
            }
            if matches!(byte, b'\r' | b'\n') {
                let command = items.get(self.selected).map(|i| i.0).unwrap_or(b'q');
                if let Screen::Configure(name) = self.screen.clone() {
                    if let Some(key) = self
                        .config_fields(&name)
                        .get(self.selected.saturating_sub(1))
                        .filter(|_| self.selected > 0 && command == b'e')
                        .cloned()
                    {
                        let value = if let Some(param) = key.strip_prefix("params.") {
                            &self.options["params"][param]
                        } else {
                            &self.options[&key]
                        };
                        if self.field_kind(&name, &key) == "boolean" {
                            let value = json!(!value.as_bool().unwrap_or(false));
                            self.options["params"][key.strip_prefix("params.").unwrap()] = value;
                        } else {
                            self.input = if value.is_null() {
                                vec![]
                            } else if let Some(text) = value.as_str() {
                                text.as_bytes().to_vec()
                            } else {
                                serde_json::to_vec(value)?
                            };
                            self.screen = Screen::Field(name, key);
                        }
                        return Ok(Action::Stay);
                    }
                    if command == b'\r' || command == b'J' {
                        self.input = serde_json::to_vec(&self.options)?;
                        self.screen = Screen::Parameters(name.clone());
                        if command == b'J' {
                            return Ok(Action::Stay);
                        }
                        let result = self.handle(b'\r');
                        if result.is_err() {
                            self.screen = Screen::Configure(name);
                        }
                        return result;
                    }
                    if command == b's' {
                        let definition = get(self.config.as_deref(), &name)?;
                        self.source_parent = Some(name);
                        self.screen = Screen::Source(definition.source.unwrap_or_default());
                        self.selected = 0;
                        return Ok(Action::Stay);
                    }
                }
                byte = command;
                if byte == b'q' {
                    return Ok(self.back());
                }
            }
        }
        if let Screen::Parameters(name) = &self.screen {
            match byte {
                b'\r' | b'\n' => {
                    let options: Value = serde_json::from_slice(&self.input)?;
                    let object = options
                        .as_object()
                        .context("options must be a JSON object")?;
                    for key in object.keys() {
                        if !["params", "timeout_ms", "output_dir"].contains(&key.as_str()) {
                            anyhow::bail!("unknown option: {key}");
                        }
                    }
                    let timeout_ms = options
                        .get("timeout_ms")
                        .map_or(Ok(300000), |v| v.as_u64().context("invalid timeout_ms"))?;
                    let output_dir = options
                        .get("output_dir")
                        .filter(|v| !v.is_null())
                        .map(|v| {
                            v.as_str()
                                .context("output_dir must be a path string")
                                .and_then(|p| Ok(std::path::absolute(p)?))
                        })
                        .transpose()?;
                    let run = ipc::call(
                        &self.session,
                        &Request::FormulaStart {
                            formula: get(self.config.as_deref(), name)?,
                            params: options.get("params").cloned().unwrap_or(json!({})),
                            initiator: "human".into(),
                            timeout_ms,
                            output_dir,
                        },
                    )?;
                    self.screen =
                        Screen::Results(run["id"].as_str().context("missing run ID")?.into());
                    self.offset = 0;
                    self.selected = 0;
                    self.page = Value::Null;
                    self.message.clear();
                    self.message_error = false;
                }
                127 | 8 => {
                    self.input.pop();
                    while std::str::from_utf8(&self.input).is_err() {
                        self.input.pop();
                    }
                }
                21 => self.input.clear(),
                32..=255 if self.input.len() < 8192 => {
                    self.input.push(byte);
                }
                _ => (),
            }
            self.fetched = Instant::now() - Duration::from_secs(1);
            return Ok(Action::Stay);
        }
        match byte {
            b'v' => {
                if let Screen::Results(id) = &self.screen {
                    self.screen = Screen::Detail(id.clone(), json!({"run": self.page["run"]}));
                    self.selected = 0;
                }
            }
            b'q' => match &self.screen {
                Screen::Detail(run, _) => {
                    self.screen = Screen::Results(run.clone());
                }
                Screen::Source(_) => {
                    self.screen = Screen::Formulas;
                    self.selected = 0;
                }
                _ => return Ok(Action::Back),
            },
            b'f' => {
                self.definitions = list(self.config.as_deref())?;
                self.screen = Screen::Formulas;
                self.selected = 0;
                self.message.clear();
                self.message_error = false;
            }
            b'r' => {
                self.screen = Screen::Runs;
                self.selected = 0;
                self.message.clear();
                self.message_error = false;
            }
            b'j' => self.selected = self.selected.saturating_add(1),
            b'k' => self.selected = self.selected.saturating_sub(1),
            b's' => {
                if let Screen::Formulas = &self.screen
                    && let Some(d) = self.definitions.get(self.selected)
                {
                    self.screen = Screen::Source(d.source.clone().unwrap_or_default());
                    self.selected = 0;
                }
            }
            b'n' => {
                if matches!(self.screen, Screen::Results(_)) && self.page["has_more"] == true {
                    self.offset = self.page["next_cursor"].as_u64().unwrap_or(0) as usize;
                    self.selected = 0;
                }
            }
            b'p' => {
                if matches!(self.screen, Screen::Results(_)) {
                    self.offset = self.offset.saturating_sub(32);
                    self.selected = 0;
                }
            }
            b'c' => {
                let id = match &self.screen {
                    Screen::Results(id) | Screen::Detail(id, _) => Some(id.clone()),
                    Screen::Runs => self
                        .runs
                        .get(self.selected)
                        .and_then(|r| r["id"].as_str())
                        .map(str::to_owned),
                    _ => None,
                };
                if let Some(run) = id {
                    ipc::call(&self.session, &Request::FormulaCancel { run })?;
                    self.message = "Cancellation requested; previously sent bytes remain".into();
                }
            }
            b'\r' | b'\n' => match &self.screen {
                Screen::Formulas => {
                    if let Some(d) = self.definitions.get(self.selected) {
                        let mut params = serde_json::Map::new();
                        for (name, p) in &d.parameters {
                            if let Some(v) = &p.default {
                                params.insert(name.clone(), v.clone());
                            }
                        }
                        self.input = serde_json::to_vec(&json!({"params":params}))?;
                        self.options = json!({"params":params});
                        self.screen = Screen::Configure(d.name.clone());
                        self.selected = 0;
                        self.message.clear();
                    }
                }
                Screen::Runs => {
                    if let Some(id) = self.runs.get(self.selected).and_then(|r| r["id"].as_str()) {
                        self.screen = Screen::Results(id.into());
                        self.offset = 0;
                        self.selected = 0;
                        self.page = Value::Null;
                    }
                }
                Screen::Results(id) => {
                    if let Some(row) = self.page["results"]
                        .as_array()
                        .and_then(|a| a.get(self.selected))
                    {
                        self.screen = Screen::Detail(id.clone(), row.clone());
                        self.selected = 0;
                    }
                }
                _ => (),
            },
            b'?' => self.showing_help = true,
            _ => (),
        }
        if !matches!(byte, b'j' | b'k') {
            self.fetched = Instant::now() - Duration::from_secs(1);
        }
        Ok(Action::Stay)
    }
    fn render(&mut self) -> Result<()> {
        if self.showing_help {
            let mut frame = Frame::new("Formulas help", "Your selection and options are preserved");
            let lines = tui::wrap(crate::help::FORMULAS, frame.columns);
            self.help_scroll = self.help_scroll.min(lines.len().saturating_sub(frame.rows));
            for (i, line) in lines
                .iter()
                .skip(self.help_scroll)
                .take(frame.rows)
                .enumerate()
            {
                frame.body(i, line, Tone::Normal);
            }
            frame.footer(
                "Capture and background jobs continue.",
                Tone::Muted,
                "q: close help · j/k: scroll",
            );
            return self.draw(frame.finish());
        }
        if let Some((title, items)) = self.menu_items() {
            self.selected = self.selected.min(items.len().saturating_sub(1));
            let items: Vec<_> = items.into_iter().map(|(_, item)| item).collect();
            let note = if !self.message.is_empty() {
                self.message.clone()
            } else if matches!(self.screen, Screen::Results(_)) {
                run_message(&self.page["run"])
            } else if matches!(self.screen, Screen::Runs) && self.runs.is_empty() {
                "No formula runs yet.".into()
            } else {
                "UART capture continues".into()
            };
            return self.draw(format!(
                "\x1b[H\x1b[2J{}",
                picker::frame(
                    &title,
                    &items,
                    self.selected,
                    &note,
                    crate::search::dimensions()
                )
            ));
        }
        let mut title = "Formulas".to_string();
        let mut subtitle = format!(
            "Session {} · capture continues while you work",
            self.session
        );
        match &self.screen {
            Screen::Field(name, _) => title = format!("Formulas / {name}"),
            Screen::Parameters(name) => title = format!("Formulas / Run {name}"),
            Screen::Runs => title = "Formulas / Recent runs".into(),
            Screen::Results(_) => {
                title = format!(
                    "Formulas / {}",
                    self.page["run"]["formula"].as_str().unwrap_or("Results")
                );
                subtitle = format!(
                    "{} · {} findings · run {}",
                    self.page["run"]["state"].as_str().unwrap_or("loading"),
                    self.page["run"]["result_count"].as_u64().unwrap_or(0),
                    self.page["run"]["id"].as_str().unwrap_or("…")
                );
            }
            Screen::Detail(_, row) => {
                title = if row.get("run").is_some() {
                    "Formulas / Run details"
                } else {
                    "Formulas / Finding context"
                }
                .into()
            }
            Screen::Source(_) => title = "Formulas / Source".into(),
            _ => (),
        }
        let mut frame = Frame::new(&title, &subtitle);
        let mut message = self.message.clone();
        let mut tone = Tone::Muted;
        let keys;
        let mut input = None;
        match &self.screen {
            Screen::Configure(_) | Screen::Formulas | Screen::Runs | Screen::Results(_) => {
                unreachable!("configuration menu rendered above")
            }
            Screen::Field(name, key) => {
                keys = "Enter save · Ctrl-U clear · Esc back";
                frame.body(0, key.strip_prefix("params.").unwrap_or(key), Tone::Accent);
                frame.body(
                    1,
                    &format!("{} · empty uses the default", self.field_kind(name, key)),
                    Tone::Muted,
                );
                input = Some(String::from_utf8_lossy(&self.input).into_owned());
            }
            Screen::Parameters(name) => {
                keys = "Enter: run · Ctrl-U: clear · Esc: back";
                frame.body(0, "Options JSON", Tone::Accent);
                if let Some(d) = self.definitions.iter().find(|d| &d.name == name) {
                    let description = if d.interactive {
                        "UART input · can send commands."
                    } else {
                        "Analysis only · retained history."
                    };
                    frame.body(
                        4,
                        description,
                        if d.interactive {
                            Tone::Warning
                        } else {
                            Tone::Muted
                        },
                    );
                    frame.body(5, "Optional: timeout_ms, output_dir", Tone::Muted);
                    for (i, (n, p)) in d.parameters.iter().enumerate() {
                        frame.body(
                            7 + i,
                            &format!(
                                "{n} · {}{} · {}",
                                p.kind,
                                if p.required { " (required)" } else { "" },
                                p.description
                            ),
                            Tone::Normal,
                        );
                    }
                }
                input = Some(String::from_utf8_lossy(&self.input).into_owned());
            }
            Screen::Detail(_, value) => {
                keys = "Esc/q: back · j/k/↑/↓: scroll · Ctrl-] b: live";
                let text = detail(value);
                let lines = tui::wrap(&text, frame.columns);
                self.selected = self.selected.min(lines.len().saturating_sub(frame.rows));
                for (i, line) in lines
                    .iter()
                    .skip(self.selected)
                    .take(frame.rows)
                    .enumerate()
                {
                    frame.body(i, line, Tone::Normal);
                }
                message = format!(
                    "Lines {}–{} of {}",
                    self.selected + 1,
                    (self.selected + frame.rows).min(lines.len()),
                    lines.len()
                );
            }
            Screen::Source(source) => {
                keys = "Esc/q: back · j/k/↑/↓: scroll · Ctrl-] b: live";
                let lines = tui::wrap(source, frame.columns);
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
                        if line.trim_start().starts_with("//") {
                            Tone::Muted
                        } else {
                            Tone::Normal
                        },
                    );
                }
                message = format!(
                    "Embedded Rhai · lines {}–{} of {}",
                    self.selected + 1,
                    (self.selected + frame.rows).min(lines.len()),
                    lines.len()
                );
            }
        }
        if self.message_error {
            tone = Tone::Error;
        }
        let keys = if frame.columns < 60 {
            match self.screen {
                Screen::Parameters(_) => "Enter run · Ctrl-U clear · Esc back",
                Screen::Field(..) => "Enter save · Ctrl-U clear · Esc back",
                _ => "Esc/q back · ↑/↓ scroll · ^]b live",
            }
        } else {
            keys
        };
        frame.footer(&message, tone, keys);
        if let Some(input) = input {
            frame.input(2.min(frame.rows - 1), "› ", &input);
        }
        self.draw(frame.finish())
    }
    fn draw(&mut self, screen: String) -> Result<()> {
        if screen != self.last_render {
            let mut out = io::stdout().lock();
            out.write_all(screen.as_bytes())?;
            out.flush()?;
            self.last_render = screen;
        }
        Ok(())
    }
}
fn run_message(run: &Value) -> String {
    if let Some(error) = run["error"].as_str() {
        return format!("Error: {error}");
    }
    if let Some(warnings) = run["warnings"].as_array()
        && !warnings.is_empty()
    {
        return format!(
            "Warning: {} · v: details",
            warnings[0].as_str().unwrap_or("See run details")
        );
    }
    let artifacts = run["artifacts"].as_array().map_or(0, Vec::len);
    if artifacts > 0 {
        return format!("{artifacts} output file(s) · v: paths and details");
    }
    if run["state"] == "completed" {
        return format!(
            "Completed · {} findings · history through event {}",
            run["result_count"], run["history_end"]
        );
    }
    let p = &run["progress"];
    if let Some(message) = p["message"].as_str() {
        if let (Some(done), Some(total)) = (p["done"].as_u64(), p["total"].as_u64())
            && total > 0
        {
            return format!(
                "{message} · {:.0}% ({done}/{total})",
                (done as f64 / total as f64 * 100.0).min(100.0)
            );
        }
        return message.into();
    }
    "Capture continues · leaving this view keeps the job running.".into()
}
fn detail(value: &Value) -> String {
    if let Some(run) = value.get("run") {
        let mut lines = Vec::new();
        for (key, label) in [
            ("formula", "Formula"),
            ("state", "Status"),
            ("id", "Run"),
            ("session", "Session"),
            ("initiator", "Started by"),
            ("started", "Started"),
            ("finished", "Finished"),
            ("result_count", "Findings"),
            ("history_end", "History through"),
            ("writer_retained", "Input retained"),
            ("revision", "Source revision"),
            ("archive", "Archive"),
            ("error", "Error"),
        ] {
            if !run[key].is_null() {
                lines.push(format!(
                    "{label}: {}",
                    run[key]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| run[key].to_string())
                ));
            }
        }
        for key in ["warnings", "artifacts", "progress"] {
            if !run[key].is_null() && run[key] != json!([]) {
                lines.push(format!(
                    "\n{key}:\n{}",
                    serde_json::to_string_pretty(&run[key]).unwrap_or_default()
                ));
            }
        }
        return lines.join("\n");
    }
    if let Some(text) = value["value"].as_str() {
        let mut lines = vec![
            text.to_owned(),
            String::new(),
            format!("Kind: {}", value["kind"].as_str().unwrap_or("finding")),
            format!("Occurrences: {}", value["count"]),
            String::new(),
            "Evidence context".into(),
        ];
        if let Some(locations) = value["locations"].as_array() {
            for loc in locations {
                lines.push(format!(
                    "\n{} · event {} · {}",
                    loc["direction"].as_str().unwrap_or("?"),
                    loc["cursor"],
                    loc["time"].as_str().unwrap_or("")
                ));
                lines.push(loc["context"].as_str().unwrap_or("").into());
            }
        }
        if value["locations_truncated"] == true {
            lines.push("\nOnly the first eight evidence locations are shown.".into());
        }
        return lines.join("\n");
    }
    serde_json::to_string_pretty(value).unwrap_or_default()
}

impl Drop for View {
    fn drop(&mut self) {
        let mut out = io::stdout().lock();
        let _ = out.write_all(b"\x1b[0m\x1b[?25h");
        let _ = out.flush();
    }
}
