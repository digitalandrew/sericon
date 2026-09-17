use super::tree::{MAX_SCAN_DIRECTORIES, Tree};
use crate::{
    ipc::{self, Request},
    picker::{self, Item, Keys},
    search::Action,
    tui::{Frame, Tone},
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{
    io::{self, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

struct TreeLoad {
    run: String,
    path: String,
    cursor: usize,
    entries: Vec<Value>,
}

fn size_label(value: &str) -> String {
    let Ok(n) = value.parse::<u64>() else {
        return "unknown".into();
    };
    for (unit, scale) in [("GiB", 1u64 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)] {
        if n >= scale {
            return format!("{:.1} {unit}", n as f64 / scale as f64);
        }
    }
    format!("{n} bytes")
}

#[derive(Clone)]
struct Download {
    remote: String,
    default: PathBuf,
    from_browser: bool,
}

#[derive(Clone)]
enum Screen {
    Ready,
    Browse,
    TreeOptions,
    Path(bool),
    DownloadLocation(Download),
    Destination(Download),
    Helper,
    HelperMenu,
    HelperChoices(usize),
    HelperLocation(Value),
    HelperDirectory(Value),
    HelperReview {
        payload: Value,
        directory: String,
    },
    UploadSource,
    UploadTarget(String),
    UploadReview {
        source: String,
        target: String,
        executable: bool,
    },
    CollectDestination,
    Inspector,
    Sections,
    Job(String),
}
impl Screen {
    fn prompt(&self) -> bool {
        matches!(
            self,
            Self::Path(_)
                | Self::Destination(_)
                | Self::Helper
                | Self::HelperDirectory(_)
                | Self::UploadSource
                | Self::UploadTarget(_)
                | Self::CollectDestination
        )
    }
}
pub(crate) struct View {
    session: String,
    client: String,
    screen: Screen,
    directory: String,
    helper: String,
    helper_directory: String,
    helpers: Vec<Value>,
    entries: Vec<Value>,
    tree: Option<Tree>,
    tree_load: Option<TreeLoad>,
    tree_scan: bool,
    tree_paused: bool,
    tree_scan_count: usize,
    tree_notice: String,
    tree_fetched: Instant,
    selected: usize,
    input: Vec<u8>,
    run: Option<String>,
    page: Value,
    results: Vec<Value>,
    cursor: usize,
    message: String,
    prefix: bool,
    keys: Keys,
    choice: usize,
    options: bool,
    pub closed: bool,
    fetched: Instant,
    last: String,
    section: usize,
    scroll: usize,
}
impl View {
    pub fn open(session: &str, client: &str) -> Self {
        Self {
            session: session.into(),
            client: client.into(),
            screen: Screen::Ready,
            directory: "/".into(),
            helper: String::new(),
            helper_directory: "/var/tmp".into(),
            helpers: vec![],
            entries: vec![],
            tree: None,
            tree_load: None,
            tree_scan: false,
            tree_paused: false,
            tree_scan_count: 0,
            tree_notice: String::new(),
            tree_fetched: Instant::now(),
            selected: 0,
            input: vec![],
            run: None,
            page: Value::Null,
            results: vec![],
            cursor: 0,
            message: String::new(),
            prefix: false,
            keys: Keys::default(),
            choice: 0,
            options: false,
            closed: false,
            fetched: Instant::now(),
            last: String::new(),
            section: 0,
            scroll: 0,
        }
    }
    fn start(&mut self, op: &str, path: &str, output: Option<PathBuf>, name: &str) -> Result<()> {
        let params = match op {
            "probe" | "inspect" | "collect-overview" => json!({"helper":self.helper}),
            "list" => json!({"path":path,"helper":self.helper}),
            _ => json!({"path":path,"name":name,"helper":self.helper}),
        };
        self.start_params(op, params, output)
    }
    fn start_params(&mut self, op: &str, params: Value, output: Option<PathBuf>) -> Result<()> {
        anyhow::ensure!(
            self.tree_load.is_none(),
            "Wait for the current directory read to finish"
        );
        self.tree_scan = false;
        let run = super::start(&self.session, op, params, output, &self.client, 3_600_000)?;
        self.run = Some(run["id"].as_str().context("invalid job response")?.into());
        self.page = json!({"run":run});
        self.cursor = 0;
        self.results.clear();
        self.screen = Screen::Job(op.into());
        self.message.clear();
        self.fetched = Instant::now() - Duration::from_secs(1);
        Ok(())
    }
    fn list(&mut self, path: String) -> Result<()> {
        if !self.helper.is_empty() {
            anyhow::ensure!(
                self.tree_load.is_none(),
                "Wait for the current directory read to finish"
            );
            self.tree = Some(Tree::new(&path));
            self.directory = self.tree.as_ref().unwrap().root.clone();
            self.tree_scan = false;
            self.tree_paused = false;
            self.tree_notice.clear();
            self.screen = Screen::Browse;
            self.selected = 0;
            self.choice = 0;
            self.message.clear();
            return Ok(());
        }
        self.start("list", &path, None, "")?;
        self.directory = path;
        Ok(())
    }
    fn back(&mut self) -> Action {
        self.keys = Keys::default();
        self.message.clear();
        if self.options {
            self.options = false;
            return Action::Stay;
        }
        self.choice = 0;
        self.screen = match self.screen.clone() {
            Screen::Ready => return Action::Back,
            Screen::TreeOptions => {
                self.choice = self.selected;
                Screen::Browse
            }
            Screen::Browse => {
                self.tree_scan = false;
                self.tree_paused = true;
                Screen::Ready
            }
            Screen::Helper | Screen::HelperChoices(_) => Screen::HelperMenu,
            Screen::HelperLocation(payload) => {
                self.choice = self
                    .helpers
                    .iter()
                    .position(|p| p["arch"] == payload["arch"])
                    .unwrap_or(0);
                Screen::HelperChoices(self.choice)
            }
            Screen::HelperDirectory(payload) | Screen::HelperReview { payload, .. } => {
                self.input.clear();
                Screen::HelperLocation(payload)
            }
            Screen::UploadTarget(source) => {
                self.input = source.into_bytes();
                Screen::UploadSource
            }
            Screen::UploadReview { source, target, .. } => {
                self.input = target.into_bytes();
                Screen::UploadTarget(source)
            }
            Screen::Destination(download) => {
                self.input.clear();
                Screen::DownloadLocation(download)
            }
            Screen::DownloadLocation(download) => {
                if download.from_browser {
                    self.choice = self.selected;
                    Screen::Browse
                } else {
                    self.input = download.remote.into_bytes();
                    Screen::Path(false)
                }
            }
            Screen::Inspector => Screen::Sections,
            _ => Screen::Ready,
        };
        Action::Stay
    }
    fn menu_items(&self) -> Option<(&'static str, Vec<(u8, Item)>)> {
        let item = |key, label: &str, detail: &str| (key, Item::new(label, detail));
        let backend = if self.helper.is_empty() {
            "Existing shell tools".into()
        } else {
            format!("Helper: {}", self.helper)
        };
        let result = match &self.screen {
            Screen::Ready => (
                "Embedded Linux / Files",
                vec![
                    item(
                        b'\r',
                        if self.tree.is_some() {
                            "Return to file tree"
                        } else {
                            "Connect and browse"
                        },
                        &format!("Start at an empty Linux shell prompt. {backend}"),
                    ),
                    item(
                        b'o',
                        "Set up helper",
                        "Install, upgrade, or select an existing helper",
                    ),
                    item(
                        b'd',
                        "Download a file",
                        "Enter a DUT path and choose a host directory",
                    ),
                    item(
                        b'u',
                        "Upload a file",
                        "Choose a host file and a new DUT path; requires a helper",
                    ),
                    item(
                        b'i',
                        "Inspect device",
                        "View identity, memory, CPU, storage and flash layout",
                    ),
                    item(
                        b's',
                        "Collect device overview",
                        "Save device information and a hash manifest on the host",
                    ),
                    item(
                        b'r',
                        "Recent file job",
                        "Check progress, results, or a completed installation",
                    ),
                    item(
                        b'g',
                        "Open directory path",
                        "Enter a DUT directory directly",
                    ),
                    item(
                        b'Q',
                        "Back to live terminal",
                        "Capture and background jobs continue",
                    ),
                ],
            ),
            Screen::DownloadLocation(download) => (
                "Download location",
                vec![
                    (
                        b'\r',
                        Item::new(
                            "Use ./downloads (default)",
                            format!("Host: {}", download.default.display()),
                        )
                        .enter_action("download"),
                    ),
                    item(
                        b'e',
                        "Enter another directory…",
                        "Choose a folder on this host for the download",
                    ),
                    item(
                        3,
                        if download.from_browser {
                            "Back to file browser"
                        } else {
                            "Back to DUT file path"
                        },
                        "Return without starting a download",
                    ),
                ],
            ),
            Screen::HelperMenu => (
                "Helper setup",
                vec![
                    item(
                        b'a',
                        "Install / upgrade helper",
                        "Choose the DUT architecture and where to install it",
                    ),
                    item(b'h', "Select existing helper", &backend),
                    item(
                        b'z',
                        "Use existing shell tools",
                        "Download without an installed helper when DUT utilities support it",
                    ),
                    item(
                        3,
                        "Back to Files",
                        "Return without changing the selected backend",
                    ),
                ],
            ),
            Screen::HelperChoices(_) => {
                let mut items: Vec<_> = self
                    .helpers
                    .iter()
                    .map(|p| {
                        (
                            b'\r',
                            Item::new(
                                format!(
                                    "{} · {} bytes",
                                    p["arch"].as_str().unwrap_or("?"),
                                    p["bytes"]
                                ),
                                p["abi"].as_str().unwrap_or(""),
                            ),
                        )
                    })
                    .collect();
                items.push(item(
                    3,
                    "Back to helper setup",
                    if items.is_empty() {
                        "No helpers bundled in this session; use a build with prepared payloads"
                    } else {
                        "Choose the DUT architecture and ABI, not the host architecture"
                    },
                ));
                ("Helper architecture", items)
            }
            Screen::HelperLocation(_) => (
                "Helper location",
                vec![
                    item(
                        b'\r',
                        &format!("Use {}", self.helper_directory),
                        "Existing DUT folder; must allow writing and execution.",
                    ),
                    item(
                        b'e',
                        "Enter another directory…",
                        "Type the full path of a folder on the DUT, for example /tmp",
                    ),
                    item(3, "Back to architecture", "Choose a different helper build"),
                ],
            ),
            Screen::HelperReview { payload, directory } => (
                "Install helper",
                vec![
                    item(
                        b'\r',
                        "Install helper",
                        &format!(
                            "{} → {directory}; verifies, runs, then selects the helper. Start at an empty shell prompt.",
                            payload["arch"].as_str().unwrap_or("")
                        ),
                    ),
                    item(3, "Change install location", directory),
                    item(
                        b'B',
                        "Back to Files",
                        "Leave setup without installing anything",
                    ),
                ],
            ),
            Screen::UploadReview {
                source,
                target,
                executable,
            } => (
                "Upload file",
                vec![
                    item(
                        b'\r',
                        "Upload file",
                        &format!("{source} → {target}; existing destinations are never replaced"),
                    ),
                    item(
                        b'e',
                        if *executable {
                            "Executable permission: on"
                        } else {
                            "Executable permission: off"
                        },
                        "Enter toggles owner execution permission; the uploaded file is never run",
                    ),
                    item(3, "Change destination", target),
                    item(
                        b'B',
                        "Back to Files",
                        "Leave setup without uploading anything",
                    ),
                ],
            ),
            Screen::TreeOptions => (
                "Tree options",
                vec![
                    item(
                        b'T',
                        "Return to tree",
                        "Keep the current selection and expanded folders",
                    ),
                    item(
                        b'S',
                        if self.tree_scan {
                            "Stop scanning"
                        } else {
                            "Scan filesystem"
                        },
                        &format!(
                            "{} below {}; skip /proc, /sys, /dev and symlink targets",
                            if self.tree_scan {
                                "Finish the current directory, then stop"
                            } else {
                                "Cache directory listings"
                            },
                            self.directory
                        ),
                    ),
                    item(
                        b'R',
                        "Refresh selected folder",
                        "Discard its cached children and read the directory again",
                    ),
                    item(
                        b'g',
                        "Open directory path",
                        "Start a new tree at a DUT directory",
                    ),
                    item(
                        b'B',
                        "Back to Files",
                        "Download, upload, helper setup and device inspection",
                    ),
                    item(
                        b'Q',
                        "Back to live terminal",
                        "Current directory read finishes; no further scanning",
                    ),
                ],
            ),
            Screen::Browse if self.tree.is_some() => {
                let mut items: Vec<_> = self
                    .tree
                    .as_ref()
                    .unwrap()
                    .rows_at_width(crate::search::dimensions().0)
                    .into_iter()
                    .map(|row| (b'\r', row.item))
                    .collect();
                items.push(item(
                    b'O',
                    "Tree options",
                    "Scan filesystem, refresh a folder, or open a directory path",
                ));
                items.push(item(
                    b'B',
                    "Back to Files",
                    "File transfers, helper setup and inspection",
                ));
                items.push(item(b'Q', "Back to live terminal", "Keep capture running"));
                ("Files tree", items)
            }
            Screen::Browse => {
                let mut items: Vec<_> = self
                    .entries
                    .iter()
                    .map(|e| {
                        (
                            b'\r',
                            Item::new(
                                format!(
                                    "{}{}",
                                    e["name"].as_str().unwrap_or("?"),
                                    if e["kind"] == "directory" { "/" } else { "" }
                                ),
                                e["path"].as_str().unwrap_or(""),
                            )
                            .enter_action(
                                if e["kind"] == "directory" {
                                    "open"
                                } else if e["kind"] == "file" {
                                    "download"
                                } else {
                                    "choose"
                                },
                            ),
                        )
                    })
                    .collect();
                items.push((
                    8,
                    Item::new("Parent directory", &self.directory).enter_action("open"),
                ));
                items.push(item(
                    b'B',
                    "Files menu",
                    "Download, upload, helper setup, inspection and recent jobs",
                ));
                items.push(item(b'Q', "Back to live terminal", "Keep capture running"));
                ("Files browser", items)
            }
            Screen::Sections => {
                let mut items: Vec<_> = self
                    .results
                    .iter()
                    .filter(|r| r["section"].is_string())
                    .map(|r| {
                        (
                            b'\r',
                            Item::new(
                                r["section"].as_str().unwrap(),
                                format!(
                                    "{} · Enter to view; Esc returns here",
                                    r["status"].as_str().unwrap_or("")
                                ),
                            ),
                        )
                    })
                    .collect();
                items.push(item(
                    b'B',
                    "Back to Files",
                    "Return to file and device actions",
                ));
                ("Device inspector", items)
            }
            Screen::Job(_) if self.options => (
                "File job options",
                vec![
                    item(
                        b'P',
                        "View progress and results",
                        "Return to the current job",
                    ),
                    item(
                        b'c',
                        "Cancel job",
                        "Stop further work; partial output remains available",
                    ),
                    item(
                        b'v',
                        "View collected overview",
                        "Open device sections from a completed collection",
                    ),
                    item(b'B', "Files menu", "The job continues in the background"),
                    item(
                        b'Q',
                        "Back to live terminal",
                        "The job continues in the background",
                    ),
                ],
            ),
            Screen::Inspector if self.options => (
                "Inspector options",
                vec![
                    item(3, "Return to section", "Continue reading this section"),
                    item(
                        b'b',
                        "Choose another section",
                        "Identity, CPU, memory, uptime, mounts, storage and flash layout",
                    ),
                    item(b'B', "Files menu", "Return to file and device actions"),
                ],
            ),
            _ => return None,
        };
        Some(result)
    }
    fn tree_path(&self) -> Option<String> {
        let index = if matches!(self.screen, Screen::Browse) {
            self.choice
        } else {
            self.selected
        };
        self.tree
            .as_ref()?
            .rows()
            .get(index)
            .map(|r| r.path.clone())
    }
    fn tree_restore(&mut self, path: Option<&str>, extra: usize) {
        if let Some(tree) = &self.tree {
            let rows = tree.rows();
            self.selected = path
                .and_then(|p| rows.iter().position(|r| r.path == p))
                .unwrap_or(rows.len() + extra);
            if matches!(self.screen, Screen::Browse) {
                self.choice = self.selected;
            }
        }
    }
    fn tree_tick(&mut self) -> Result<()> {
        if self.tree_load.is_some() && self.tree_fetched.elapsed() >= Duration::from_millis(250) {
            let mut load = self.tree_load.take().unwrap();
            let anchor = self.tree_path();
            let index = if matches!(self.screen, Screen::Browse) {
                self.choice
            } else {
                self.selected
            };
            let extra = index.saturating_sub(self.tree.as_ref().map_or(0, |t| t.rows().len()));
            match ipc::call(
                &self.session,
                &Request::FormulaRead {
                    run: load.run.clone(),
                    after: load.cursor,
                    limit: 128,
                },
            ) {
                Ok(page) => {
                    load.cursor = page["next_cursor"].as_u64().unwrap_or(0) as usize;
                    load.entries.extend(
                        page["results"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter(|e| e["kind"].is_string())
                            .cloned(),
                    );
                    if page["run"]["state"] == "running" || page["has_more"] == true {
                        self.tree_load = Some(load);
                    } else if let Some(tree) = &mut self.tree {
                        if page["run"]["state"] == "completed" {
                            if let Err(error) = tree.insert(&load.path, &load.entries) {
                                tree.fail(&load.path, error.to_string());
                                self.tree_scan = false;
                                self.tree_paused = true;
                                self.tree_notice = error.to_string();
                            }
                        } else {
                            tree.fail(
                                &load.path,
                                page["run"]["error"]
                                    .as_str()
                                    .unwrap_or("Directory read did not complete")
                                    .into(),
                            );
                            let status = ipc::call(&self.session, &Request::Status)?;
                            if page["run"]["writer_retained"] == true
                                || status["writer"].is_object()
                                || status["connected"] != true
                            {
                                self.tree_scan = false;
                                self.tree_paused = true;
                                self.tree_notice =
                                    "Scan stopped; check connection and input ownership".into();
                            }
                        }
                        self.tree_restore(anchor.as_deref(), extra);
                        if !self.tree_scan {
                            match self.tree_notice.as_str() {
                                "Refreshing folder" => self.tree_notice.clear(),
                                "Stopping after current directory" => {
                                    self.tree_notice = "Scan stopped".into()
                                }
                                _ => (),
                            }
                        }
                    }
                }
                Err(error) => {
                    if let Some(tree) = &mut self.tree {
                        tree.fail(&load.path, error.to_string());
                    }
                    self.tree_scan = false;
                    self.tree_paused = true;
                    self.tree_notice = format!("Directory read stopped: {error}");
                }
            }
            self.tree_fetched = Instant::now();
        }
        if self.tree_load.is_none() && !self.tree_paused && matches!(self.screen, Screen::Browse) {
            if self.tree_scan && self.tree_scan_count >= MAX_SCAN_DIRECTORIES {
                self.tree_scan = false;
                self.tree_paused = true;
                self.tree_notice = format!(
                    "Scan paused at {MAX_SCAN_DIRECTORIES} folders; choose Scan filesystem to continue"
                );
                return Ok(());
            }
            let next = self
                .tree
                .as_ref()
                .and_then(|t| t.next_unloaded(self.tree_scan));
            if let Some(path) = next {
                let run = super::start(
                    &self.session,
                    "list",
                    json!({"path":path,"helper":self.helper}),
                    None,
                    &self.client,
                    120_000,
                )?;
                self.tree_load = Some(TreeLoad {
                    run: run["id"].as_str().context("invalid directory job")?.into(),
                    path,
                    cursor: 0,
                    entries: vec![],
                });
                if self.tree_scan {
                    self.tree_scan_count += 1;
                }
                self.tree_fetched = Instant::now() - Duration::from_secs(1);
            } else if self.tree_scan {
                self.tree_scan = false;
                self.tree_notice = "Scan complete within tree limits".into();
            }
        }
        Ok(())
    }
    fn tree_key(&mut self, b: u8) -> Result<bool> {
        if !matches!(self.screen, Screen::Browse) || self.tree.is_none() {
            return Ok(false);
        }
        if matches!(b, b'o' | b'O' | b'\t') {
            self.selected = self.choice;
            self.screen = Screen::TreeOptions;
            return Ok(true);
        }
        if b == b'c' {
            self.tree_scan = false;
            self.tree_paused = true;
            self.tree_notice = if self.tree_load.is_some() {
                "Stopping after current directory"
            } else {
                "Scan stopped"
            }
            .into();
            return Ok(true);
        }
        if !matches!(b, b'\r' | b'\n' | 2 | 6 | 8 | 127 | b'h' | b'l') {
            return Ok(false);
        }
        let Some(path) = self.tree_path() else {
            return Ok(false);
        };
        let tree = self.tree.as_mut().unwrap();
        let node = tree.node(&path).unwrap().clone();
        if matches!(b, 2 | 8 | 127 | b'h') {
            if node.kind == "directory" && tree.is_expanded(&path) {
                tree.collapse(&path);
            } else if let Some(parent) = node.parent {
                self.tree_restore(Some(&parent), 0);
            }
        } else if node.kind == "directory" {
            if tree.is_expanded(&path) {
                if matches!(b, b'\r' | b'\n') {
                    tree.collapse(&path);
                } else if let Some(child) = node.children.and_then(|c| c.first().cloned()) {
                    self.tree_restore(Some(&child), 0);
                }
            } else {
                tree.expand(&path);
                self.tree_paused = false;
                self.message.clear();
            }
        } else if node.kind == "file" && matches!(b, b'\r' | b'\n') {
            self.tree_scan = false;
            self.tree_paused = true;
            anyhow::ensure!(
                self.tree_load.is_none(),
                "Finishing the current directory read; press Enter again to download"
            );
            self.selected = self.choice;
            self.destination(path)?;
        } else if matches!(b, b'\r' | b'\n') {
            self.message = "Symlinks and special files are shown but not opened".into();
        }
        Ok(true)
    }
    pub fn tick(&mut self) -> Result<()> {
        if self.keys.expired() {
            self.closed = matches!(self.back(), Action::Back);
        }
        if matches!(self.screen, Screen::Job(_))
            && self.fetched.elapsed() >= Duration::from_millis(250)
        {
            if let Some(run) = &self.run {
                match ipc::call(
                    &self.session,
                    &Request::FormulaRead {
                        run: run.clone(),
                        after: self.cursor,
                        limit: 128,
                    },
                ) {
                    Ok(page) => {
                        self.cursor = page["next_cursor"].as_u64().unwrap_or(0) as usize;
                        self.results
                            .extend(page["results"].as_array().cloned().unwrap_or_default());
                        self.page = page;
                        if self.page["run"]["state"] == "completed"
                            && self.page["has_more"] == false
                        {
                            if matches!(&self.screen,Screen::Job(op) if op == "list") {
                                self.entries = self
                                    .results
                                    .iter()
                                    .filter(|v| v["kind"].is_string())
                                    .cloned()
                                    .collect();
                                self.selected = 0;
                                self.screen = Screen::Browse;
                                self.message = format!("{} entries", self.entries.len());
                            } else if matches!(&self.screen,Screen::Job(op) if op == "inspect") {
                                self.screen = Screen::Sections;
                                self.choice = 0;
                                self.section = 0;
                                self.scroll = 0;
                            } else if self.page["run"]["formula"] == "linux-helper-install" {
                                if let Some(helper) = self.results.iter().rev().find_map(|r| {
                                    (r["verified_before_execution"] == true)
                                        .then(|| r["helper"].as_str().filter(|p| !p.is_empty()))
                                        .flatten()
                                }) {
                                    self.helper = helper.into();
                                    self.tree = None;
                                    self.screen = Screen::Ready;
                                    self.message = "Helper installed and selected".into();
                                } else {
                                    self.message =
                                        "No verified helper returned; selection unchanged".into();
                                }
                            } else if matches!(&self.screen,Screen::Job(op) if op == "probe")
                                && let Err(e) = self.list(self.directory.clone())
                            {
                                self.message = format!("{e:#}");
                            }
                        }
                    }
                    Err(e) => self.message = format!("{e:#}"),
                }
            }
            self.fetched = Instant::now();
        }
        if let Err(error) = self.tree_tick() {
            self.tree_scan = false;
            self.tree_paused = true;
            if let Some(tree) = &mut self.tree
                && let Some(path) = tree.next_unloaded(false)
            {
                tree.fail(&path, error.to_string());
            }
            self.message = error.to_string();
        }
        self.render()
    }
    pub fn redraw(&mut self) -> Result<()> {
        self.last.clear();
        self.render()
    }
    pub fn wheel(&mut self, up: bool) -> Result<()> {
        if self.menu_items().is_some() || matches!(self.screen, Screen::Inspector) {
            self.key(if up { b'k' } else { b'j' })?;
        }
        Ok(())
    }
    pub fn key(&mut self, b: u8) -> Result<Action> {
        let previous = std::mem::discriminant(&self.screen);
        let result = self.handle(b);
        if previous != std::mem::discriminant(&self.screen) {
            self.choice = match self.screen {
                Screen::HelperChoices(selected) => selected,
                Screen::Browse => self.selected,
                _ => 0,
            };
            self.options = false;
        }
        let action = match result {
            Ok(a) => a,
            Err(e) => {
                self.message = format!("{e:#}");
                Action::Stay
            }
        };
        self.tick()?;
        Ok(action)
    }
    fn handle(&mut self, mut b: u8) -> Result<Action> {
        if self.prefix {
            self.prefix = false;
            return Ok(match b {
                b'm' => Action::Menu,
                b'b' => Action::Back,
                b'q' => Action::Stop,
                b'd' => Action::Detach,
                _ => Action::Stay,
            });
        }
        if b == 29 {
            self.prefix = true;
            return Ok(Action::Stay);
        }
        let Some(key) = self.keys.key(b, self.screen.prompt()) else {
            return Ok(Action::Stay);
        };
        b = key;
        if b == 3 || (b == b'q' && !self.screen.prompt()) {
            return Ok(self.back());
        }
        if self.tree_key(b)? {
            return Ok(Action::Stay);
        }
        if let Some((_, items)) = self.menu_items() {
            if picker::move_selection(&mut self.choice, b, items.len()) {
                return Ok(Action::Stay);
            }
            if matches!(b, b'\r' | b'\n') {
                let command = items.get(self.choice).map(|i| i.0).unwrap_or(b'b');
                match self.screen {
                    Screen::HelperChoices(_) if self.choice < self.helpers.len() => {
                        self.screen = Screen::HelperChoices(self.choice)
                    }
                    Screen::Browse if self.tree.is_none() && self.choice < self.entries.len() => {
                        self.selected = self.choice
                    }
                    Screen::Sections
                        if self.choice
                            < self
                                .results
                                .iter()
                                .filter(|r| r["section"].is_string())
                                .count() =>
                    {
                        self.section = self.choice;
                        self.scroll = 0;
                        self.screen = Screen::Inspector;
                        return Ok(Action::Stay);
                    }
                    _ => (),
                }
                b = command;
                if b == b'O' {
                    self.selected = self.choice;
                    self.screen = Screen::TreeOptions;
                    return Ok(Action::Stay);
                }
                if matches!(b, b'T' | b'S' | b'R') && matches!(self.screen, Screen::TreeOptions) {
                    if b == b'S' {
                        self.tree_scan = !self.tree_scan;
                        self.tree_paused = !self.tree_scan;
                        self.tree_scan_count = 0;
                        self.tree_notice = if self.tree_scan {
                            "Scanning filesystem"
                        } else {
                            "Stopping after current directory"
                        }
                        .into();
                    }
                    if b == b'R' {
                        anyhow::ensure!(
                            self.tree_load.is_none(),
                            "Wait for the current directory read to finish"
                        );
                        self.tree_scan = false;
                        self.tree_paused = false;
                        if let Some(path) = self
                            .tree_path()
                            .or_else(|| self.tree.as_ref().map(|t| t.root.clone()))
                        {
                            let tree = self.tree.as_mut().unwrap();
                            let node = tree.node(&path).unwrap();
                            let folder = if node.kind == "directory" {
                                path
                            } else {
                                node.parent.clone().unwrap()
                            };
                            tree.refresh(&folder);
                            self.tree_restore(Some(&folder), 0);
                            self.tree_notice = "Refreshing folder".into();
                        }
                    }
                    self.screen = Screen::Browse;
                    self.choice = self.selected;
                    self.message.clear();
                    return Ok(Action::Stay);
                }
                if b == 3 {
                    return Ok(self.back());
                }
                self.options = false;
                if b == b'o' {
                    self.screen = Screen::HelperMenu;
                    return Ok(Action::Stay);
                }
                if b == b'z' {
                    anyhow::ensure!(
                        self.tree_load.is_none(),
                        "Wait for the current directory read to finish"
                    );
                    self.helper.clear();
                    self.tree = None;
                    self.tree_scan = false;
                    self.screen = Screen::Ready;
                    self.message = "Using existing shell tools".into();
                    return Ok(Action::Stay);
                }
                if b == b'Q' {
                    return Ok(Action::Back);
                }
                if b == b'B' {
                    self.tree_scan = false;
                    self.tree_paused = true;
                    self.screen = Screen::Ready;
                    return Ok(Action::Stay);
                }
                if b == b'P' {
                    return Ok(Action::Stay);
                }
            }
        } else if !self.screen.prompt() && (b == b'\t' || b == b'o') {
            self.options = true;
            self.choice = 0;
            return Ok(Action::Stay);
        }
        if self.screen.prompt() {
            match b {
                21 => self.input.clear(),
                8 | 127 => {
                    self.input.pop();
                    while std::str::from_utf8(&self.input).is_err() {
                        self.input.pop();
                    }
                }
                3 => return Ok(self.back()),
                b'\r' | b'\n' => {
                    let value = std::str::from_utf8(&self.input)?.to_owned();
                    match self.screen.clone() {
                        Screen::HelperDirectory(payload) => {
                            anyhow::ensure!(
                                value.starts_with('/')
                                    && value.len() <= 512
                                    && !value.chars().any(char::is_control),
                                "Use an absolute DUT directory, at most 512 bytes"
                            );
                            self.helper_directory = value.clone();
                            self.screen = Screen::HelperReview {
                                payload,
                                directory: value,
                            };
                            self.input.clear();
                            self.message.clear();
                        }
                        Screen::UploadSource => {
                            let source =
                                std::path::absolute(&value)?.to_string_lossy().into_owned();
                            let name = std::path::Path::new(&source)
                                .file_name()
                                .context("source needs a filename")?
                                .to_string_lossy();
                            self.input =
                                format!("{}/{}", self.directory.trim_end_matches('/'), name)
                                    .into_bytes();
                            self.screen = Screen::UploadTarget(source);
                        }
                        Screen::UploadTarget(source) => {
                            self.screen = Screen::UploadReview {
                                source,
                                target: value,
                                executable: false,
                            };
                            self.input.clear();
                        }
                        Screen::CollectDestination => self.start(
                            "collect-overview",
                            "",
                            Some(std::path::absolute(value)?),
                            "",
                        )?,
                        Screen::Helper => {
                            anyhow::ensure!(
                                self.tree_load.is_none(),
                                "Wait for the current directory read to finish"
                            );
                            self.tree = None;
                            self.tree_scan = false;
                            self.helper = value;
                            self.screen = Screen::Ready;
                            self.message = "Backend selected; Enter connects".into();
                        }
                        Screen::Path(true) => self.list(value)?,
                        Screen::Path(false) => self.destination(value)?,
                        Screen::Destination(download) => {
                            anyhow::ensure!(
                                !value.trim().is_empty() && !value.chars().any(char::is_control),
                                "Enter a host folder, for example ./captures"
                            );
                            self.download(&download.remote, std::path::absolute(value)?)?;
                        }
                        _ => (),
                    }
                }
                32..=255 if self.input.len() < 1024 => self.input.push(b),
                _ => (),
            }
            return Ok(Action::Stay);
        }
        if matches!(self.screen, Screen::Job(_))
            && self.page["run"]["state"] == "running"
            && matches!(b, b'd' | b'g' | b'r' | b'h' | b'a' | b'u' | b'i' | b's')
        {
            self.message = "Wait for this job or cancel it with c".into();
            return Ok(Action::Stay);
        }
        if let Screen::DownloadLocation(download) = self.screen.clone() {
            match b {
                b'\r' | b'\n' => self.download(&download.remote, download.default)?,
                b'e' => {
                    self.screen = Screen::Destination(download);
                    self.input.clear();
                }
                b'b' | 3 => return Ok(self.back()),
                _ => (),
            }
            return Ok(Action::Stay);
        }
        if let Screen::HelperChoices(selected) = self.screen {
            match b {
                b'j' => {
                    self.screen = Screen::HelperChoices(
                        (selected + 1).min(self.helpers.len().saturating_sub(1)),
                    )
                }
                b'k' => self.screen = Screen::HelperChoices(selected.saturating_sub(1)),
                b'\r' | b'\n' => {
                    if let Some(payload) = self.helpers.get(selected) {
                        self.screen = Screen::HelperLocation(payload.clone());
                    }
                }
                b'b' | 3 => return Ok(self.back()),
                b'q' => return Ok(Action::Back),
                _ => (),
            }
            return Ok(Action::Stay);
        }
        if let Screen::HelperLocation(payload) = self.screen.clone() {
            match b {
                b'\r' | b'\n' => {
                    self.screen = Screen::HelperReview {
                        payload,
                        directory: self.helper_directory.clone(),
                    };
                }
                b'e' => {
                    self.screen = Screen::HelperDirectory(payload);
                    self.input.clear();
                }
                b'b' | 3 => return Ok(self.back()),
                _ => (),
            }
            return Ok(Action::Stay);
        }
        if let Screen::HelperReview { payload, directory } = self.screen.clone() {
            match b {
                b'\r' | b'\n' => self.start_params(
                    "helper-install",
                    json!({"arch":payload["arch"],"path":directory}),
                    None,
                )?,
                b'b' | 3 => return Ok(self.back()),
                b'q' => return Ok(Action::Back),
                _ => (),
            }
            return Ok(Action::Stay);
        }
        if let Screen::UploadReview {
            source,
            target,
            executable,
        } = self.screen.clone()
        {
            match b {
                b'e' => self.screen = Screen::UploadReview { source, target, executable: !executable },
                b'\r' | b'\n' => self.start_params("upload", json!({"source":source,"path":target,"helper":self.helper,"executable":executable}), None)?,
                b'q' => return Ok(Action::Back),
                3 | b'b' => return Ok(self.back()),
                _ => (),
            }
            return Ok(Action::Stay);
        }
        if matches!(self.screen, Screen::Inspector) {
            let count = self
                .results
                .iter()
                .filter(|r| r["section"].is_string())
                .count();
            match b {
                b'n' | b'\t' => {
                    self.section = (self.section + 1) % count.max(1);
                    self.scroll = 0;
                    return Ok(Action::Stay);
                }
                b'p' => {
                    self.section = (self.section + count.saturating_sub(1)) % count.max(1);
                    self.scroll = 0;
                    return Ok(Action::Stay);
                }
                b'j' => {
                    self.scroll = self.scroll.saturating_add(1);
                    return Ok(Action::Stay);
                }
                b'k' => {
                    self.scroll = self.scroll.saturating_sub(1);
                    return Ok(Action::Stay);
                }
                b'b' | b'\r' | b'\n' => {
                    self.screen = Screen::Sections;
                    return Ok(Action::Stay);
                }
                _ => (),
            }
        }
        match b {
            b'u' | b'i' | b's' if self.helper.is_empty() => {
                self.screen = Screen::HelperMenu;
                self.message = "This action needs a helper; choose how to set it up".into();
            }
            b'a' => {
                let inventory = ipc::call(&self.session, &Request::HelperInventory)
                    .context("Restart older sessions to list helpers")?;
                self.helpers = inventory
                    .as_array()
                    .context("invalid helper inventory")?
                    .clone();
                self.screen = Screen::HelperChoices(0);
                self.message.clear();
            }
            b'u' => {
                self.screen = Screen::UploadSource;
                self.input.clear();
                self.message.clear();
            }
            b'i' => self.start("inspect", "", None, "")?,
            b's' => {
                self.screen = Screen::CollectDestination;
                self.input = std::env::current_dir()?
                    .join("collections")
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec();
                self.message.clear();
            }
            b'v' if self.results.iter().any(|r| r["section"].is_string()) => {
                self.screen = Screen::Sections;
                self.section = 0;
                self.scroll = 0;
            }
            b'h' => {
                if self.helper.is_empty()
                    && let Ok(runs) = ipc::call(&self.session, &Request::FormulaRuns)
                    && let Some(run) = runs.as_array().and_then(|v| {
                        v.iter().rev().find(|r| {
                            r["formula"] == "linux-helper-install" && r["state"] == "completed"
                        })
                    })
                    && let Some(id) = run["id"].as_str()
                    && let Ok(page) = ipc::call(
                        &self.session,
                        &Request::FormulaRead {
                            run: id.into(),
                            after: 0,
                            limit: 128,
                        },
                    )
                    && let Some(helper) = page["results"].as_array().and_then(|v| {
                        v.iter().rev().find_map(|r| {
                            r["verified_before_execution"]
                                .as_bool()
                                .filter(|v| *v)
                                .and_then(|_| r["helper"].as_str())
                        })
                    })
                {
                    self.helper = helper.into();
                }
                self.screen = Screen::Helper;
                self.input = self.helper.as_bytes().to_vec();
                self.message.clear();
            }
            b'q' => return Ok(Action::Back),
            b'c' => {
                if let Some(run) = &self.run {
                    ipc::call(&self.session, &Request::FormulaCancel { run: run.clone() })?;
                    self.message = "Cancellation requested; incomplete output is retained".into();
                }
            }
            b'd' => {
                self.screen = Screen::Path(false);
                self.input = b"/".to_vec();
                self.message.clear();
            }
            b'g' => {
                self.screen = Screen::Path(true);
                self.input = self.directory.as_bytes().to_vec();
                self.message.clear();
            }
            b'r' => {
                let runs = ipc::call(&self.session, &Request::FormulaRuns)?;
                if let Some(run) = runs.as_array().and_then(|v| {
                    v.iter().rev().find(|r| {
                        r["formula"]
                            .as_str()
                            .is_some_and(|n| n.starts_with("linux-"))
                    })
                }) {
                    self.run = Some(run["id"].as_str().unwrap_or("").into());
                    self.screen = Screen::Job("recent".into());
                    self.cursor = 0;
                    self.results.clear();
                    self.page = json!({"run":run});
                    self.fetched = Instant::now() - Duration::from_secs(1);
                } else {
                    self.message = "No recent Linux Files jobs".into();
                }
            }
            b'j' => self.selected = (self.selected + 1).min(self.entries.len().saturating_sub(1)),
            b'k' => self.selected = self.selected.saturating_sub(1),
            8 | 127 if matches!(self.screen, Screen::Browse) => {
                let parent = self
                    .directory
                    .trim_end_matches('/')
                    .rsplit_once('/')
                    .map_or("/", |(p, _)| if p.is_empty() { "/" } else { p })
                    .to_owned();
                self.list(parent)?;
            }
            b'\r' | b'\n' => match self.screen.clone() {
                Screen::Ready if self.tree.is_some() => {
                    self.screen = Screen::Browse;
                    self.choice = self.selected;
                    self.message.clear();
                }
                Screen::Ready => self.start("probe", "", None, "")?,
                Screen::Browse => {
                    if let Some(e) = self.entries.get(self.selected) {
                        let path = e["path"].as_str().unwrap_or("").to_owned();
                        if e["kind"] == "directory" {
                            self.list(path)?;
                        } else if e["kind"] == "file" {
                            self.destination(path)?;
                        } else {
                            self.message =
                                "Only directories and regular files are supported".into();
                        }
                    }
                }
                Screen::Job(_) => {
                    self.options = true;
                    self.choice = 0;
                }
                _ => (),
            },
            _ => (),
        }
        Ok(Action::Stay)
    }
    fn destination(&mut self, path: String) -> Result<()> {
        self.screen = Screen::DownloadLocation(Download {
            remote: path,
            default: std::env::current_dir()?.join("downloads"),
            from_browser: matches!(self.screen, Screen::Browse),
        });
        self.input.clear();
        self.message.clear();
        Ok(())
    }
    fn download(&mut self, remote: &str, output: PathBuf) -> Result<()> {
        let name: String = remote
            .rsplit('/')
            .next()
            .unwrap_or("download.bin")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || "._-".contains(c) {
                    c
                } else {
                    '_'
                }
            })
            .take(120)
            .collect();
        self.start("download", remote, Some(output), &name)
    }
    fn render(&mut self) -> Result<()> {
        if let Some((title, items)) = self.menu_items() {
            let items: Vec<_> = items.into_iter().map(|(_, item)| item).collect();
            self.choice = self.choice.min(items.len().saturating_sub(1));
            let remote_note;
            let note = if !self.message.is_empty() {
                &self.message
            } else if matches!(self.screen, Screen::Browse) && self.tree.is_some() {
                remote_note = if let Some(load) = &self.tree_load {
                    if self.tree_paused {
                        format!("Stopping after current directory · {}", load.path)
                    } else {
                        format!(
                            "{} {} · c stop",
                            if self.tree_scan {
                                "Scanning"
                            } else {
                                "Loading"
                            },
                            load.path
                        )
                    }
                } else if !self.tree_notice.is_empty() {
                    self.tree_notice.clone()
                } else {
                    "←/→ folders · o options".into()
                };
                &remote_note
            } else if let Screen::DownloadLocation(download) = &self.screen {
                remote_note = format!("DUT: {}", download.remote);
                &remote_note
            } else if matches!(self.screen, Screen::HelperLocation(_)) {
                "Choose a folder on the DUT"
            } else {
                "UART capture continues"
            };
            let summary;
            let layout = if matches!(self.screen, Screen::Browse)
                && let Some(tree) = &self.tree
            {
                let (nodes, folders, errors) = tree.counts();
                summary =
                    format!("{nodes} entries · {folders} folders loaded · {errors} unavailable");
                (120, summary.as_str())
            } else {
                (64, "")
            };
            let text = format!(
                "\x1b[H\x1b[2J{}",
                picker::frame_layout(
                    title,
                    &items,
                    self.choice,
                    note,
                    crate::search::dimensions(),
                    layout
                )
            );
            if text != self.last {
                io::stdout().write_all(text.as_bytes())?;
                io::stdout().flush()?;
                self.last = text;
            }
            return Ok(());
        }
        let mut f = Frame::new(
            match self.screen {
                Screen::HelperDirectory(_) => "Helper location",
                Screen::Destination(_) => "Download location",
                _ => "Embedded Linux / Files",
            },
            match self.screen {
                Screen::HelperDirectory(_) => {
                    "Choose an existing folder on the device connected over UART"
                }
                Screen::Destination(_) => "Save the DUT file in a unique folder on this host",
                _ => "Files and device inspection through the shared UART session",
            },
        );
        let keys = match &self.screen {
            Screen::Ready
            | Screen::Browse
            | Screen::TreeOptions
            | Screen::HelperMenu
            | Screen::Sections
            | Screen::HelperChoices(_)
            | Screen::DownloadLocation(_)
            | Screen::HelperLocation(_)
            | Screen::HelperReview { .. }
            | Screen::UploadReview { .. } => unreachable!("option menu rendered above"),
            Screen::Path(dir) => {
                f.body(
                    0,
                    if *dir {
                        "Browse a DUT directory"
                    } else {
                        "Download a DUT file"
                    },
                    Tone::Normal,
                );
                f.input(2, "DUT path: ", &String::from_utf8_lossy(&self.input));
                "Enter continue · Ctrl-U clear · Esc back"
            }
            Screen::Destination(download) => {
                f.body(0, "Enter a download folder on this host", Tone::Accent);
                f.body(1, &format!("DUT: {}", download.remote), Tone::Normal);
                f.body(2, "Type a path, e.g. ./captures", Tone::Muted);
                f.input_box(3, "Path: ", &String::from_utf8_lossy(&self.input));
                if f.columns < 60 {
                    "Enter download · ^U clear · Esc back"
                } else {
                    "Enter download · Ctrl-U clear · Esc back"
                }
            }
            Screen::Helper => {
                f.body(0, "Use an existing Sericon helper on the DUT", Tone::Normal);
                f.body(
                    1,
                    "Empty uses existing shell tools. This does not upload software.",
                    Tone::Muted,
                );
                f.input(3, "Helper path: ", &String::from_utf8_lossy(&self.input));
                "Enter select · Ctrl-U clear · Esc back"
            }
            Screen::HelperDirectory(_) => {
                f.body(0, "Enter an existing folder on the DUT", Tone::Accent);
                f.body(1, "Must allow writing and execution.", Tone::Normal);
                f.body(2, "Type a path, then Enter. e.g. /tmp", Tone::Muted);
                f.input_box(3, "Path: ", &String::from_utf8_lossy(&self.input));
                if f.columns < 60 {
                    "Enter review · ^U clear · Esc back"
                } else {
                    "Enter review · Ctrl-U clear · Esc back"
                }
            }
            Screen::UploadSource => {
                f.body(0, "Upload a host file to the DUT", Tone::Accent);
                f.body(1, "Choose a regular file, at most 64 MiB.", Tone::Muted);
                f.input(3, "Host file: ", &String::from_utf8_lossy(&self.input));
                "Enter continue · Ctrl-U clear · Esc back"
            }
            Screen::UploadTarget(source) => {
                f.body(0, &format!("Host: {source}"), Tone::Normal);
                f.body(
                    1,
                    "Destination must be a new file in an existing DUT directory.",
                    Tone::Muted,
                );
                f.input(3, "DUT file: ", &String::from_utf8_lossy(&self.input));
                "Enter review · Ctrl-U clear · Esc back"
            }
            Screen::CollectDestination => {
                f.body(0, "Collect a fresh device overview", Tone::Accent);
                f.body(
                    1,
                    "Save inspector results and a hash manifest in a unique folder.",
                    Tone::Muted,
                );
                f.input(3, "Host directory: ", &String::from_utf8_lossy(&self.input));
                "Enter collect · Ctrl-U clear · Esc back"
            }
            Screen::Inspector => {
                let sections: Vec<_> = self
                    .results
                    .iter()
                    .filter(|r| r["section"].is_string())
                    .collect();
                if let Some(record) = sections.get(self.section) {
                    f.body(
                        0,
                        &format!(
                            "Device inspector · {} ({}/{}) · {}",
                            record["section"].as_str().unwrap_or(""),
                            self.section + 1,
                            sections.len(),
                            record["status"].as_str().unwrap_or("")
                        ),
                        Tone::Accent,
                    );
                    let mut lines = Vec::new();
                    if let Some(rows) = record["values"].as_array() {
                        for row in rows {
                            let values: Vec<_> = row
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|v| v.as_str().unwrap_or(""))
                                .collect();
                            if values.len() == 5 {
                                lines.push(values[0].into());
                                if values[1] == "ok" {
                                    lines.extend(crate::tui::wrap(
                                        &format!(
                                            "  {} free / {} total",
                                            size_label(values[3]),
                                            size_label(values[2])
                                        ),
                                        f.columns,
                                    ));
                                    lines.push(format!("  Writable: {}", values[4]));
                                } else {
                                    lines.push("  Unavailable".into());
                                }
                            } else {
                                lines.extend(crate::tui::wrap(&values.join(": "), f.columns));
                            }
                        }
                    } else {
                        for line in record["preview"].as_str().unwrap_or("").lines() {
                            lines.extend(crate::tui::wrap(line, f.columns));
                        }
                    }
                    if record["status"] == "unavailable" {
                        lines.push("This section is unavailable on the DUT.".into());
                    }
                    if record["preview_truncated"] == true {
                        lines.push(
                            "Preview shortened; collection saves the bounded raw section.".into(),
                        );
                    }
                    let visible = f.rows.saturating_sub(2).max(1);
                    self.scroll = self.scroll.min(lines.len().saturating_sub(visible));
                    for (i, line) in lines.iter().skip(self.scroll).take(visible).enumerate() {
                        f.body(i + 2, line, Tone::Normal);
                    }
                }
                if f.columns < 60 {
                    "↑/↓ scroll · Enter/Esc sections"
                } else {
                    "↑/↓ scroll · Enter/Esc sections · Tab options · Ctrl-] b live"
                }
            }
            Screen::Job(_) => {
                let r = &self.page["run"];
                let p = &r["progress"];
                f.body(
                    0,
                    &format!(
                        "{} · {}",
                        r["id"].as_str().unwrap_or(""),
                        r["state"].as_str().unwrap_or("")
                    ),
                    Tone::Accent,
                );
                f.body(
                    1,
                    if p["stage"] == "completed" {
                        "Download verified"
                    } else {
                        p["stage"].as_str().unwrap_or("Starting…")
                    },
                    Tone::Normal,
                );
                if let Some(total) = p["total"].as_u64() {
                    f.body(
                        2,
                        &format!("{} / {total} bytes", p["bytes"].as_u64().unwrap_or(0)),
                        Tone::Normal,
                    );
                }
                if let Some(speed) = p["bytes_per_second"].as_f64() {
                    f.body(
                        3,
                        &format!(
                            "{speed:.0} bytes/s · ETA {:.0}s · retry {}",
                            p["eta_seconds"].as_f64().unwrap_or(0.0),
                            p["retry"].as_u64().unwrap_or(0)
                        ),
                        Tone::Muted,
                    );
                }
                let mut row = 4;
                if let Some(dest) = self.results.iter().rev().find_map(|r| {
                    (r["verified"] == true && r["operation"] == "upload")
                        .then(|| r["destination"].as_str())
                        .flatten()
                }) {
                    for line in crate::tui::wrap(&format!("Uploaded: {dest}"), f.columns) {
                        f.body(row, &line, Tone::Success);
                        row += 1;
                    }
                }
                if let Some(error) = r["error"].as_str() {
                    for line in crate::tui::wrap(error, f.columns) {
                        f.body(row, &line, Tone::Error);
                        row += 1;
                    }
                }
                if let Some(a) = r["artifacts"].as_array() {
                    for a in a {
                        for line in crate::tui::wrap(
                            &format!(
                                "{} {}",
                                if a["complete"] == true {
                                    "Saved:"
                                } else {
                                    "Partial:"
                                },
                                a["path"].as_str().unwrap_or("")
                            ),
                            f.columns,
                        ) {
                            f.body(row, &line, Tone::Normal);
                            row += 1;
                        }
                    }
                }
                if r["writer_retained"] == true {
                    f.body(
                        row,
                        "Input retained; Ctrl-] m → Take input ownership to recover",
                        Tone::Warning,
                    );
                }
                if r["state"] == "running" {
                    "Enter options · Esc Files · c cancel"
                } else if f.columns < 60 {
                    "Enter options · Esc Files"
                } else {
                    "Enter options · Esc Files · c cancel · Ctrl-] b live"
                }
            }
        };
        f.footer(
            &self.message,
            if self.message.is_empty() {
                Tone::Muted
            } else {
                Tone::Warning
            },
            keys,
        );
        let text = f.finish();
        if text != self.last {
            let mut out = io::stdout().lock();
            out.write_all(text.as_bytes())?;
            out.flush()?;
            self.last = text;
        }
        Ok(())
    }
}
