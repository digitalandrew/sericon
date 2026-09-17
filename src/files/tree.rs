//! Client-side directory cache. Only explicit directory listings touch the DUT.
use crate::{picker::Item, tui::Tone};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_NODES: usize = 16_384;
pub const MAX_SCAN_DIRECTORIES: usize = 1_024;
const MAX_DEPTH: usize = 32;

#[derive(Clone)]
pub struct Node {
    pub path: String,
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    pub children: Option<Vec<String>>,
    pub error: Option<String>,
    depth: usize,
}
pub struct Row {
    pub path: String,
    pub item: Item,
}
pub struct Tree {
    pub root: String,
    nodes: BTreeMap<String, Node>,
    expanded: BTreeSet<String>,
}
impl Tree {
    pub fn new(root: &str) -> Self {
        let root = root.trim_end_matches('/');
        let root = if root.is_empty() { "/" } else { root }.to_owned();
        let node = Node {
            path: root.clone(),
            name: root.clone(),
            kind: "directory".into(),
            parent: None,
            children: None,
            error: None,
            depth: 0,
        };
        Self {
            root: root.clone(),
            nodes: BTreeMap::from([(root.clone(), node)]),
            expanded: BTreeSet::from([root]),
        }
    }
    pub fn node(&self, path: &str) -> Option<&Node> {
        self.nodes.get(path)
    }
    pub fn is_expanded(&self, path: &str) -> bool {
        self.expanded.contains(path)
    }
    pub fn expand(&mut self, path: &str) {
        if self.nodes.get(path).is_some_and(|n| n.kind == "directory") {
            self.expanded.insert(path.into());
        }
    }
    pub fn collapse(&mut self, path: &str) {
        self.expanded.remove(path);
    }
    pub fn fail(&mut self, path: &str, error: String) {
        if let Some(node) = self.nodes.get_mut(path) {
            node.error = Some(error);
        }
    }
    pub fn refresh(&mut self, path: &str) {
        let mut pending = self
            .nodes
            .get(path)
            .and_then(|n| n.children.clone())
            .unwrap_or_default();
        while let Some(child) = pending.pop() {
            if let Some(node) = self.nodes.remove(&child) {
                pending.extend(node.children.unwrap_or_default());
            }
            self.expanded.remove(&child);
        }
        if let Some(node) = self.nodes.get_mut(path) {
            node.children = None;
            node.error = None;
        }
        self.expand(path);
    }
    pub fn insert(&mut self, path: &str, entries: &[Value]) -> Result<()> {
        let depth = self.nodes.get(path).map_or(0, |n| n.depth + 1);
        ensure!(depth <= MAX_DEPTH, "Tree depth limit ({MAX_DEPTH}) reached");
        ensure!(entries.len() <= 512, "Directory exceeds 512 entries");
        ensure!(
            self.nodes.len() + entries.len() <= MAX_NODES,
            "Tree cache limit ({MAX_NODES} entries) reached; open a smaller root"
        );
        let mut children = Vec::new();
        let mut seen = BTreeSet::new();
        for entry in entries {
            let p = entry["path"].as_str().unwrap_or("");
            let name = entry["name"].as_str().unwrap_or("");
            ensure!(
                !name.is_empty()
                    && !matches!(name, "." | "..")
                    && !name.contains('/')
                    && p == format!("{}/{name}", path.trim_end_matches('/'))
                    && seen.insert(p),
                "Invalid child path in directory listing"
            );
            children.push(Node {
                path: p.into(),
                name: name.into(),
                kind: entry["kind"].as_str().unwrap_or("other").into(),
                parent: Some(path.into()),
                children: None,
                error: None,
                depth,
            });
        }
        children.sort_by(|a, b| {
            (a.kind != "directory", &a.name).cmp(&(b.kind != "directory", &b.name))
        });
        let expanded = self.is_expanded(path);
        self.refresh(path);
        if !expanded {
            self.collapse(path);
        }
        let paths = children.iter().map(|n| n.path.clone()).collect();
        for node in children {
            self.nodes.insert(node.path.clone(), node);
        }
        if let Some(parent) = self.nodes.get_mut(path) {
            parent.children = Some(paths);
            parent.error = None;
        }
        Ok(())
    }
    pub fn counts(&self) -> (usize, usize, usize) {
        (
            self.nodes.len(),
            self.nodes.values().filter(|n| n.children.is_some()).count(),
            self.nodes.values().filter(|n| n.error.is_some()).count(),
        )
    }
    pub fn next_unloaded(&self, scan: bool) -> Option<String> {
        if scan {
            self.nodes
                .values()
                .find(|n| Self::loadable(n) && !virtual_path(&n.path))
                .map(|n| n.path.clone())
        } else {
            self.rows().iter().find_map(|row| {
                let n = self.nodes.get(&row.path)?;
                (Self::loadable(n) && self.is_expanded(&n.path)).then(|| n.path.clone())
            })
        }
    }
    fn loadable(node: &Node) -> bool {
        node.kind == "directory"
            && node.children.is_none()
            && node.error.is_none()
            && node.depth < MAX_DEPTH
    }
    pub fn rows(&self) -> Vec<Row> {
        self.rows_at_width(usize::MAX)
    }
    pub fn rows_at_width(&self, columns: usize) -> Vec<Row> {
        let mut rows = Vec::new();
        self.append(&self.root, "", "", columns.saturating_sub(30), &mut rows);
        rows
    }
    fn append(
        &self,
        path: &str,
        prefix: &str,
        branch: &str,
        max_indent: usize,
        rows: &mut Vec<Row>,
    ) {
        let Some(node) = self.node(path) else {
            return;
        };
        let open = self.is_expanded(path);
        let (icon, tone, kind) = appearance(node, open);
        let marker = if node.kind == "directory" {
            if open { "▾" } else { "▸" }
        } else {
            " "
        };
        let suffix = if node.kind == "directory" && node.path != "/" {
            "/"
        } else {
            ""
        };
        let state = if let Some(error) = &node.error {
            format!(" · unavailable: {error}")
        } else if node.kind == "directory" {
            if node.depth >= MAX_DEPTH {
                " · depth limit".into()
            } else if let Some(children) = &node.children {
                format!(" · {} entries", children.len())
            } else if virtual_path(path) {
                " · virtual filesystem; skipped by scan".into()
            } else {
                " · not loaded".into()
            }
        } else {
            String::new()
        };
        let shown_prefix = if prefix.chars().count() > max_indent {
            let end: String = prefix
                .chars()
                .rev()
                .take(max_indent.saturating_sub(2))
                .collect();
            format!("… {}", end.chars().rev().collect::<String>())
        } else {
            prefix.into()
        };
        rows.push(Row {
            path: path.into(),
            item: Item::new(
                format!(
                    "{shown_prefix}{branch}{marker} {icon} {}{suffix}{}",
                    node.name,
                    if node.error.is_some() { " !" } else { "" }
                ),
                format!("{kind}{state} · {}", node.path),
            )
            .tone(if node.error.is_some() {
                Tone::Warning
            } else {
                tone
            })
            .enter_action(if node.kind == "directory" {
                if open { "collapse" } else { "expand" }
            } else if node.kind == "file" {
                "download"
            } else {
                "info"
            }),
        });
        if open && let Some(children) = &node.children {
            let indent = if branch.is_empty() {
                String::new()
            } else {
                format!("{prefix}{}", if branch == "└─ " { "   " } else { "│  " })
            };
            for (i, child) in children.iter().enumerate() {
                self.append(
                    child,
                    &indent,
                    if i + 1 == children.len() {
                        "└─ "
                    } else {
                        "├─ "
                    },
                    max_indent,
                    rows,
                );
            }
        }
    }
}
fn virtual_path(path: &str) -> bool {
    ["/proc", "/sys", "/dev"]
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
}
fn appearance(node: &Node, open: bool) -> (&'static str, Tone, &'static str) {
    match node.kind.as_str() {
        "directory" => (if open { "📂" } else { "📁" }, Tone::Accent, "Directory"),
        "link" => ("↗", Tone::Cyan, "Symlink (not followed)"),
        "file" => {
            let name = node.name.to_ascii_lowercase();
            let ext = name.rsplit_once('.').map_or("", |(_, ext)| ext);
            match ext {
                "sh" | "py" | "pl" | "lua" | "js" | "rhai" | "rs" | "c" | "h" | "go" => {
                    ("λ", Tone::Success, "Code / script")
                }
                "conf" | "cfg" | "ini" | "json" | "yaml" | "yml" | "toml" | "xml" | "env" => {
                    ("≡", Tone::Cyan, "Configuration / data")
                }
                "zip" | "tar" | "gz" | "tgz" | "xz" | "bz2" | "zst" | "lz4" => {
                    ("▣", Tone::Magenta, "Archive")
                }
                "bin" | "elf" | "so" | "img" | "fw" | "squashfs" | "ubifs" | "jffs2" => {
                    ("◆", Tone::Blue, "Binary / firmware")
                }
                "png" | "jpg" | "jpeg" | "gif" | "svg" | "bmp" | "webp" => {
                    ("▧", Tone::Magenta, "Image")
                }
                "txt" | "log" | "md" | "rst" => ("≡", Tone::Normal, "Text"),
                _ if node.path.starts_with("/bin/")
                    || node.path.starts_with("/sbin/")
                    || node.path.starts_with("/usr/bin/")
                    || node.path.starts_with("/usr/sbin/") =>
                {
                    ("⚙", Tone::Success, "Program (by location)")
                }
                _ => ("📄", Tone::Normal, "File"),
            }
        }
        _ => ("◇", Tone::Muted, "Special file (not downloadable)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn entry(path: &str, kind: &str) -> Value {
        json!({"path":path,"name":path.rsplit('/').next().unwrap(),"kind":kind})
    }
    #[test]
    fn lazy_tree_keeps_cached_children_and_scan_does_not_expand_folders() {
        let mut tree = Tree::new("/");
        tree.insert("/", &[entry("/etc", "directory"), entry("/alias", "link")])
            .unwrap();
        assert!(tree.next_unloaded(false).is_none());
        assert_eq!(tree.next_unloaded(true).as_deref(), Some("/etc"));
        tree.insert("/etc", &[entry("/etc/config.json", "file")])
            .unwrap();
        assert!(!tree.is_expanded("/etc"));
        assert_eq!(tree.rows().len(), 3);
        tree.expand("/etc");
        assert_eq!(tree.rows()[2].path, "/etc/config.json");
        assert!(tree.rows()[2].item.label.contains("│  └─"));
        tree.collapse("/etc");
        tree.expand("/etc");
        assert!(tree.next_unloaded(false).is_none());
        tree.refresh("/etc");
        assert!(tree.node("/etc/config.json").is_none());
        assert_eq!(tree.next_unloaded(false).as_deref(), Some("/etc"));
        assert!(
            tree.insert("/etc", &[entry("/outside", "directory")])
                .is_err()
        );
        assert!(tree.node("/outside").is_none());
    }
    #[test]
    fn scans_skip_virtual_filesystems_links_errors_and_enforce_bounds() {
        let mut tree = Tree::new("/");
        tree.insert(
            "/",
            &[
                entry("/dev", "directory"),
                entry("/proc", "directory"),
                entry("/sys", "directory"),
                entry("/etc", "directory"),
                entry("/loop", "link"),
            ],
        )
        .unwrap();
        tree.fail("/etc", "Permission denied".into());
        assert!(tree.next_unloaded(true).is_none());
        tree.expand("/proc");
        assert_eq!(tree.next_unloaded(false).as_deref(), Some("/proc"));
        assert_eq!(tree.counts().2, 1);
        assert!(
            tree.insert(
                "/proc",
                &(0..513)
                    .map(|i| entry(&format!("/proc/{i}"), "file"))
                    .collect::<Vec<_>>()
            )
            .is_err()
        );
        let mut deep = Tree::new("/");
        let mut path = "/".to_string();
        for _ in 0..MAX_DEPTH {
            let child = format!("{}/d", path.trim_end_matches('/'));
            deep.insert(&path, &[entry(&child, "directory")]).unwrap();
            deep.expand(&child);
            path = child;
        }
        assert!(deep.next_unloaded(true).is_none());
        assert!(
            deep.rows()
                .last()
                .unwrap()
                .item
                .detail
                .contains("depth limit")
        );
        let row = deep.rows_at_width(40).pop().unwrap();
        assert!(row.item.label.starts_with('…'));
        assert!(crate::tui::width(&row.item.label) < 30);
        let mut wide = Tree::new("/");
        wide.insert(
            "/",
            &(0..33)
                .map(|i| entry(&format!("/d{i}"), "directory"))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for i in 0..31 {
            wide.insert(
                &format!("/d{i}"),
                &(0..512)
                    .map(|j| entry(&format!("/d{i}/{j}"), "file"))
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        }
        assert!(
            wide.insert(
                "/d31",
                &(0..512)
                    .map(|j| entry(&format!("/d31/{j}"), "file"))
                    .collect::<Vec<_>>()
            )
            .is_err()
        );
        assert!(wide.counts().0 <= MAX_NODES);
    }
    #[test]
    fn tree_rendering_keeps_unicode_bounds_and_strips_metadata_commands() {
        let mut tree = Tree::new("/");
        tree.insert(
            "/",
            &[
                entry("/folder", "directory"),
                entry("/evil\x1b[2J.bin", "file"),
                entry("/界.json", "file"),
            ],
        )
        .unwrap();
        let items: Vec<_> = tree.rows().into_iter().map(|r| r.item).collect();
        for (cols, rows) in [(92, 28), (40, 12), (24, 8)] {
            let text = crate::picker::frame_layout(
                "Files tree",
                &items,
                2,
                "←/→ folders · o options",
                (cols, rows),
                (120, "3 entries"),
            );
            assert!(!text.contains("\x1b[2J"));
            let mut term = vt100::Parser::new(rows as u16, cols as u16, 0);
            term.process(text.as_bytes());
            assert!(term.screen().contents().contains("evil.bin"));
            assert!(term.screen().hide_cursor());
        }
    }
}
