//! The Settings screen — Ctrl+P. ONE page, no drilling.
//!
//! Every row is a real setting. The layout is exactly what you see:
//! a main heading (APPEARANCE, PERMISSIONS, …) with its subsetting options
//! directly below it, all sections on one scrollable page. There are no
//! sub-screens to click through any more — the old
//! "open Settings → click the group → click the subgroup" flow is gone.
//!
//! Leaf paths are the dotted keys of the `romacode_shared::tui_settings`
//! catalog, so each row reads and writes the same value the brain persists:
//! ← → steps the value and the new value goes to the brain as a `tui.<path>`
//! preference. Rows that open a flow (Models, Providers, Tools, Config file)
//! hand off to the same-named screen inside the identical Settings panel.
//!
//! ```text
//! APPEARANCE ───────────────────────────────
//!   ○ Theme                              dark
//!   ○ Color mode                        system
//! AI & SERVER ──────────────────────────────
//!   ● Models / Providers / Tools / Config file
//! PERMISSIONS ──────────────────────────────
//!   ○ Manual             ask before shell commands
//!   ● Auto accept                       ✓ active   ← only the active row is marked
//! …every section follows on the same page…
//! ```
//!
//! Typing filters leaves (label or section); the `/` slash menu in chat
//! surfaces the same flattened list.

// ── data ───────────────────────────────────────────────────────────────────

/// Who owns a setting — drives the badge in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// Lives in the brain's `config.json` (server side).
    Brain,
    /// Lives in the screen's `tui.*` tree (client side).
    Screen,
}

/// What a leaf does when activated.
#[derive(Clone, Debug)]
pub enum Leaf {
    /// A live setting: ← → step it through the catalog values.
    Setting { path: &'static str },
    /// A live setting pinned to one value (Manual / Auto accept).
    Preset { path: &'static str, value: &'static str },
    /// Hand off to one of the existing screen flows.
    Dest(Dest),
}

/// Where a `Dest` leaf routes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dest {
    /// Open the Select Model screen (saved + fetched models).
    SelectModel,
    /// Open the Providers screen (add API keys, known + custom).
    OpenProviders,
    /// Open the integrated Tools builder.
    OpenTools,
    /// Toast the config file path.
    OpenConfigFile,
    /// Toast the effective config.
    PrintEffectiveConfig,
    /// Reset all settings to defaults.
    ResetSettings,
}

/// One node in the tree.
#[derive(Clone, Debug)]
pub struct Node {
    pub label: &'static str,
    pub kind: NodeKind,
}

#[derive(Clone, Debug)]
pub enum NodeKind {
    Leaf { leaf: Leaf, owner: Owner, hint: &'static str },
    Group { children: &'static [Node] },
}

// ── the static sections ────────────────────────────────────────────────────

/// Root-level sections, in display order. APPEARANCE comes first (it is the
/// section users reach for most); the renderer prints every section heading
/// with its rows directly below on the same page.
pub const ROOT: &[Node] = &[
    Node { label: "APPEARANCE", kind: NodeKind::Group { children: APPEARANCE } },
    Node { label: "AI & SERVER", kind: NodeKind::Group { children: AI_SERVER } },
    Node { label: "PERMISSIONS", kind: NodeKind::Group { children: PERMISSIONS } },
    Node { label: "SESSION", kind: NodeKind::Group { children: SESSION } },
    Node { label: "TABS & DIFFS", kind: NodeKind::Group { children: TABS_DIFFS } },
    Node { label: "INPUT", kind: NodeKind::Group { children: INPUT } },
    Node { label: "ALERTS", kind: NodeKind::Group { children: ALERTS } },
    Node { label: "ADVANCED", kind: NodeKind::Group { children: ADVANCED } },
];

// Manual asks before a shell command — the one action that leaves the project.
// Auto accept never asks. Reads, edits and web fetches run in both modes.
const PERMISSIONS: &[Node] = &[
    leaf("Manual", Owner::Brain, "ask before shell commands", Leaf::Preset {
        path: "session.permissions",
        value: "prompt",
    }),
    leaf("Auto accept", Owner::Brain, "run every tool without asking", Leaf::Preset {
        path: "session.permissions",
        value: "autoaccept",
    }),
];

const APPEARANCE: &[Node] = &[
    leaf("Theme", Owner::Screen, "color palette", Leaf::Setting { path: "theme.name" }),
    leaf("Color mode", Owner::Screen, "system / dark / light", Leaf::Setting { path: "theme.mode" }),
    leaf("Animations", Owner::Screen, "interface motion", Leaf::Setting { path: "animations" }),
    leaf("Release highlights", Owner::Screen, "show what's new", Leaf::Setting { path: "highlights" }),
    leaf("Window title", Owner::Screen, "terminal title", Leaf::Setting { path: "terminal.title" }),
];

const SESSION: &[Node] = &[
    leaf("Sidebar", Owner::Screen, "side panel", Leaf::Setting { path: "session.sidebar" }),
    leaf("Scrollbar", Owner::Screen, "show in transcript", Leaf::Setting { path: "session.scrollbar" }),
    leaf("Scroll on send", Owner::Screen, "jump to latest", Leaf::Setting { path: "session.submit_scroll" }),
    leaf("Thinking", Owner::Screen, "reasoning blocks", Leaf::Setting { path: "session.thinking" }),
    leaf("Markdown", Owner::Screen, "source / rendered", Leaf::Setting { path: "session.markdown" }),
    leaf("Tool grouping", Owner::Screen, "merge tool runs", Leaf::Setting { path: "session.grouping" }),
    leaf("Verbosity", Owner::Screen, "low / medium / high", Leaf::Setting { path: "session.verbosity" }),
    leaf("Transcript images", Owner::Screen, "preview attachments", Leaf::Setting { path: "session.image_preview" }),
    leaf("TPS", Owner::Screen, "tokens per second", Leaf::Setting { path: "session.tps" }),
    leaf("New session location", Owner::Screen, "directory", Leaf::Setting { path: "session.new_location" }),
];

const TABS_DIFFS: &[Node] = &[
    leaf("Tabs mode", Owner::Screen, "off / on / auto", Leaf::Setting { path: "tabs.mode" }),
    leaf("Tabs scope", Owner::Screen, "directory / global", Leaf::Setting { path: "tabs.scope" }),
    leaf("Tabs layout", Owner::Screen, "horizontal / vertical", Leaf::Setting { path: "tabs.layout" }),
    leaf("Tabs indicators", Owner::Screen, "status / numbers", Leaf::Setting { path: "tabs.indicators" }),
    leaf("Diff layout", Owner::Brain, "auto / split / unified", Leaf::Setting { path: "diffs.view" }),
    leaf("Diff wrapping", Owner::Brain, "none / word", Leaf::Setting { path: "diffs.wrap" }),
    leaf("Diff file tree", Owner::Brain, "show changed files", Leaf::Setting { path: "diffs.tree" }),
    leaf("Diff single patch", Owner::Brain, "one file at a time", Leaf::Setting { path: "diffs.single" }),
];

const INPUT: &[Node] = &[
    leaf("Scroll speed", Owner::Screen, "lines per tick", Leaf::Setting { path: "scroll.speed" }),
    leaf("Scroll acceleration", Owner::Screen, "faster on repeat", Leaf::Setting { path: "scroll.acceleration" }),
    leaf("Mouse", Owner::Screen, "capture clicks + wheel", Leaf::Setting { path: "mouse" }),
    leaf("Editor context", Owner::Screen, "send active file", Leaf::Setting { path: "prompt.editor" }),
    leaf("Large pastes", Owner::Screen, "compact / full", Leaf::Setting { path: "prompt.paste" }),
    leaf("Image previews", Owner::Screen, "above the prompt", Leaf::Setting { path: "prompt.image_preview" }),
    leaf("Leader timeout", Owner::Screen, "key sequence wait", Leaf::Setting { path: "leader.timeout" }),
    leaf("Copy behavior", Owner::Screen, "manual / select", Leaf::Setting { path: "terminal.copy" }),
];

const ALERTS: &[Node] = &[
    leaf("Notifications", Owner::Screen, "desktop alerts", Leaf::Setting { path: "attention.notifications" }),
    leaf("Sounds", Owner::Screen, "alert sounds", Leaf::Setting { path: "attention.sound" }),
    leaf("Volume", Owner::Screen, "0 - 100%", Leaf::Setting { path: "attention.volume" }),
];

const AI_SERVER: &[Node] = &[
    leaf("Models", Owner::Brain, "saved + active model", Leaf::Dest(Dest::SelectModel)),
    leaf("Providers", Owner::Brain, "API keys · known + custom", Leaf::Dest(Dest::OpenProviders)),
    leaf("Tools", Owner::Brain, "42 builtin (incl. dsd trace) + documents + MCP", Leaf::Dest(Dest::OpenTools)),
    leaf("Documents", Owner::Brain, "Word · Excel · PPT · PDF · charts · Figma", Leaf::Dest(Dest::OpenTools)),
    leaf("Config file", Owner::Brain, "where settings live", Leaf::Dest(Dest::OpenConfigFile)),
];

const ADVANCED: &[Node] = &[
    leaf("Developer tools",  Owner::Screen, "debug overlay",       Leaf::Setting { path: "debug.devtools" }),
    leaf("Effective config", Owner::Brain,  "current values",       Leaf::Dest(Dest::PrintEffectiveConfig)),
    leaf("Reset settings",   Owner::Brain,  "restore every default", Leaf::Dest(Dest::ResetSettings)),
];

// shorthand constructors
const fn leaf(label: &'static str, owner: Owner, hint: &'static str, leaf_kind: Leaf) -> Node {
    Node { label, kind: NodeKind::Leaf { leaf: leaf_kind, owner, hint } }
}

// ── the flat page model ────────────────────────────────────────────────────

/// The full flat leaf list: `(section heading, node)` in display order.
/// One row per setting across every section — the whole Settings page.
pub fn leaves() -> Vec<(&'static str, &'static Node)> {
    let mut out = Vec::new();
    for g in ROOT {
        if let NodeKind::Group { children } = &g.kind {
            for n in children.iter() {
                out.push((g.label, n));
            }
        }
    }
    out
}

/// One visible row of the flat Settings screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VRow {
    /// A section heading (APPEARANCE, …) — drawn bold orange, not selectable.
    Heading(&'static str),
    /// A selectable leaf. The value is the position in the *visible* leaf
    /// list (which is what `Tree::sel` indexes).
    Leaf(usize),
}

/// Runtime state for the flat Settings page.
///
/// `sel` is the cursor over the *visible* leaves; `top` is the first visible
/// visual row (headings + leaves) so the cursor never scrolls off-screen.
/// Typing filters leaves by label or section.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub sel: usize,
    pub top: usize,
    /// Filter query (typed directly on the page).
    pub query: String,
}

impl Tree {
    pub fn root() -> Self { Self::default() }

    /// The visible leaves: `(position-in-full-list, node, section)`, filtered
    /// by the query (empty query = everything).
    pub fn visible_leaves(&self) -> Vec<(usize, &'static Node, &'static str)> {
        let q = self.query.to_lowercase();
        leaves()
            .into_iter()
            .enumerate()
            .filter(|(_, (sec, n))| {
                q.is_empty()
                    || n.label.to_lowercase().contains(&q)
                    || sec.to_lowercase().contains(&q)
            })
            .map(|(i, (sec, n))| (i, n, sec))
            .collect()
    }

    /// The visual rows of the page: for every section that has visible leaves,
    /// the heading followed by its leaf rows. With an empty query this is the
    /// whole page — every heading with its subsettings right below it.
    pub fn rows(&self) -> Vec<VRow> {
        let vis = self.visible_leaves();
        let mut out = Vec::with_capacity(vis.len() + ROOT.len());
        let mut last: Option<&'static str> = None;
        for (pos, (_, _, sec)) in vis.iter().enumerate() {
            if last != Some(sec) {
                out.push(VRow::Heading(sec));
                last = Some(sec);
            }
            out.push(VRow::Leaf(pos));
        }
        out
    }

    /// The leaf under the cursor (if any).
    pub fn selected(&self) -> Option<&'static Leaf> {
        let vis = self.visible_leaves();
        let n = vis.get(self.sel.min(vis.len().saturating_sub(1)))?.1;
        match &n.kind {
            NodeKind::Leaf { leaf, .. } => Some(leaf),
            NodeKind::Group { .. } => None,
        }
    }

    pub fn up(&mut self) {
        self.sel = self.sel.saturating_sub(1);
    }
    pub fn down(&mut self) {
        let n = self.visible_leaves().len();
        if n > 0 {
            self.sel = (self.sel + 1).min(n - 1);
        }
    }

    /// The visual row index (inside [`Tree::rows`]) the cursor sits on.
    fn selected_visual_row(&self) -> usize {
        for (i, r) in self.rows().iter().enumerate() {
            if let VRow::Leaf(pos) = r {
                if *pos == self.sel {
                    return i;
                }
            }
        }
        0
    }

    /// Clamp the viewport so the selected row is always visible. `avail` is
    /// the number of visual rows the renderer can show. Call every draw.
    pub fn clamp_viewport(&mut self, avail: usize) {
        if avail == 0 {
            return;
        }
        let vrow = self.selected_visual_row();
        if vrow < self.top {
            self.top = vrow;
        } else if vrow >= self.top + avail {
            self.top = vrow + 1 - avail;
        }
    }
}

// ── slash menu ─────────────────────────────────────────────────────────────

/// One flattened leaf with its full path (section / leaf label).
#[derive(Clone, Debug)]
pub struct FlatLeaf {
    pub path: String,
    pub label: &'static str,
    pub leaf: &'static Leaf,
    pub owner: Owner,
}

/// Flatten the whole page into leaves with their full dotted paths.
pub fn flatten() -> Vec<FlatLeaf> {
    fn walk(nodes: &'static [Node], prefix: &mut Vec<&'static str>, out: &mut Vec<FlatLeaf>) {
        for n in nodes {
            match &n.kind {
                NodeKind::Leaf { leaf, owner, .. } => {
                    prefix.push(n.label);
                    out.push(FlatLeaf {
                        path: prefix.join(" / "),
                        label: n.label,
                        leaf,
                        owner: *owner,
                    });
                    prefix.pop();
                }
                NodeKind::Group { children } => {
                    prefix.push(n.label);
                    walk(children, prefix, out);
                    prefix.pop();
                }
            }
        }
    }
    let mut out = Vec::new();
    let mut prefix = Vec::new();
    walk(ROOT, &mut prefix, &mut out);
    out
}

/// The `/` slash-menu state in chat.
#[derive(Clone, Debug, Default)]
pub struct SlashMenu {
    pub query: String,
    pub sel: usize,
    pub top: usize,
    pub items: Vec<FlatLeaf>,
}

impl SlashMenu {
    pub fn open() -> Self {
        Self { items: flatten(), ..Default::default() }
    }
    pub fn refresh(&mut self) {
        let q = self.query.to_lowercase();
        self.items = flatten()
            .into_iter()
            .filter(|f| q.is_empty() || f.path.to_lowercase().contains(&q) || f.label.to_lowercase().contains(&q))
            .collect();
        self.sel = self.sel.min(self.items.len().saturating_sub(1));
        self.top = 0;
    }
    pub fn up(&mut self) { self.sel = self.sel.saturating_sub(1); self.top = 0; }
    pub fn down(&mut self) { self.sel = (self.sel + 1).min(self.items.len().saturating_sub(1)); }
    pub fn selected(&self) -> Option<&FlatLeaf> {
        self.items.get(self.sel.min(self.items.len().saturating_sub(1)))
    }
    /// Clamp the slash-menu viewport (same idea as Tree::clamp_viewport).
    pub fn clamp_viewport(&mut self, avail: usize) {
        if avail == 0 || self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + avail {
            self.top = self.sel.saturating_sub(avail.saturating_sub(1));
        }
    }
}

// ── mouse hit-testing ──────────────────────────────────────────────────────────

/// One mouse-hit-testable row. Plain u16 quartet, same shape as
/// `settings::HitRow`, so the App's hit logic stays uniform.
#[derive(Clone, Copy, Debug)]
pub struct TreeHit {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub kind: TreeHitKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeHitKind {
    /// A leaf row; the value is the position in the *visible* leaf list
    /// (what `Tree::sel` indexes).
    TreeRow(usize),
    SlashRow(usize),
    /// The panel background: swallows the click, activates nothing.
    Panel,
}

impl TreeHit {
    pub fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.x.saturating_add(self.w) && y >= self.y && y < self.y.saturating_add(self.h)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use romacode_shared::catalog;

    /// The one-page structure: eight sections, APPEARANCE first, and every
    /// section's leaves sit directly under its heading.
    #[test]
    fn eight_sections_with_appearance_first() {
        let labels: Vec<&str> = ROOT.iter().map(|n| n.label).collect();
        assert_eq!(labels.len(), 8);
        assert_eq!(
            labels,
            ["APPEARANCE", "AI & SERVER", "PERMISSIONS", "SESSION", "TABS & DIFFS", "INPUT", "ALERTS", "ADVANCED"],
        );
        // AI & SERVER is deliberately the second section (index 1).
        assert_eq!(labels[1], "AI & SERVER");
        // every root node is a plain section of leaves (no nested submenus —
        // the drill-down is gone)
        for n in ROOT {
            match &n.kind {
                NodeKind::Group { children } => {
                    assert!(!children.is_empty(), "{} must not be empty", n.label);
                    for c in children.iter() {
                        assert!(
                            matches!(c.kind, NodeKind::Leaf { .. }),
                            "{} must not nest further groups (no drilling)",
                            n.label
                        );
                    }
                }
                _ => panic!("root must only contain section groups"),
            }
        }
    }

    #[test]
    fn the_page_is_headings_with_leaves_below() {
        let t = Tree::root();
        let rows = t.rows();
        // first visual row is the APPEARANCE heading, the next one is Theme
        assert_eq!(rows[0], VRow::Heading("APPEARANCE"));
        assert_eq!(rows[1], VRow::Leaf(0));
        // every section appears as a heading, in order
        let headings: Vec<&str> = rows
            .iter()
            .filter_map(|r| match r {
                VRow::Heading(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(
            headings,
            ["APPEARANCE", "AI & SERVER", "PERMISSIONS", "SESSION", "TABS & DIFFS", "INPUT", "ALERTS", "ADVANCED"],
        );
        // leaf count matches the flat leaf list
        assert_eq!(rows.len() - headings.len(), leaves().len());
    }

    #[test]
    fn permissions_are_manual_and_auto_accept() {
        let (_, node) = leaves()
            .into_iter()
            .find(|(sec, n)| *sec == "PERMISSIONS" && n.label == "Manual")
            .expect("Manual must exist");
        match &node.kind {
            NodeKind::Leaf { leaf: Leaf::Preset { path, value }, .. } => {
                assert_eq!(*path, "session.permissions");
                assert_eq!(*value, "prompt");
            }
            _ => panic!("Manual must be a Preset leaf"),
        }
        let (_, node) = leaves()
            .into_iter()
            .find(|(sec, n)| *sec == "PERMISSIONS" && n.label == "Auto accept")
            .expect("Auto accept must exist");
        match &node.kind {
            NodeKind::Leaf { leaf: Leaf::Preset { path, value }, .. } => {
                assert_eq!(*path, "session.permissions");
                assert_eq!(*value, "autoaccept");
            }
            _ => panic!("Auto accept must be a Preset leaf"),
        }
    }

    #[test]
    fn every_setting_leaf_maps_to_a_catalog_row() {
        use romacode_shared::tui_settings::{def, parse_wire};
        for f in flatten() {
            match f.leaf {
                Leaf::Setting { path } => {
                    assert!(def(path).is_some(), "{path} is not in the settings catalog");
                }
                Leaf::Preset { path, value } => {
                    let d = def(path).expect("preset path must be in the catalog");
                    assert!(parse_wire(d, value).is_ok(), "{path} rejects {value}");
                }
                Leaf::Dest(_) => {}
            }
        }
    }

    /// The AI & SERVER section is exactly the four flow rows, one Enter away
    /// from the same page — no "Providers > Known Providers > OpenAI" chain.
    #[test]
    fn ai_and_server_rows_are_the_four_flows() {
        let t = Tree::root();
        let rows: Vec<&'static str> = t
            .visible_leaves()
            .into_iter()
            .filter(|(_, _, sec)| *sec == "AI & SERVER")
            .map(|(_, n, _)| n.label)
            .collect();
        assert_eq!(rows, ["Models", "Providers", "Tools", "Documents", "Config file"]);
        for (_, n, sec) in t.visible_leaves() {
            if sec != "AI & SERVER" {
                continue;
            }
            match &n.kind {
                NodeKind::Leaf { leaf: Leaf::Dest(d), .. } => {
                    assert!(
                        matches!(d, Dest::SelectModel | Dest::OpenProviders | Dest::OpenTools | Dest::OpenConfigFile),
                        "{:?} must route to a flow screen",
                        n.label
                    );
                }
                _ => panic!("AI & SERVER rows must be Dest leaves"),
            }
        }
    }

    /// The Providers row opens the full provider picker: every catalog
    /// provider is reachable from the ONE Settings page.
    #[test]
    fn providers_dest_reaches_the_whole_catalog() {
        // The bundled fallback catalog carries the core providers; the live
        // network catalog is richer. The threshold is the bundled count so
        // the test passes offline.
        assert!(romacode_shared::catalog::known_len() >= 300, "the full provider catalog must be reachable");
        for id in ["anthropic", "openai", "openrouter", "google", "groq", "deepseek",
                   "mistral", "xai", "zai", "ollama", "lmstudio", "cerebras",
                   "together", "fireworks"] {
            assert!(catalog::find(id).is_some(), "{id} must be in the catalog");
        }
    }

    #[test]
    fn flatten_includes_every_leaf() {
        let flat = flatten();
        assert!(flat.iter().any(|f| f.label == "Manual"));
        assert!(flat.iter().any(|f| f.label == "Theme"));
        assert!(flat.iter().any(|f| f.label == "Providers"));
        assert!(flat.iter().any(|f| f.label == "Reset settings"));
        assert!(flat.iter().all(|f| !f.path.is_empty()));
    }

    #[test]
    fn slash_menu_filters_by_label_and_path() {
        let mut m = SlashMenu::open();
        m.query = "permission".into();
        m.refresh();
        assert!(m.items.iter().any(|f| f.label == "Manual" || f.label == "Auto accept"));
        m.query = "model".into();
        m.refresh();
        assert!(m.items.iter().any(|f| f.label == "Models"));
    }

    #[test]
    fn viewport_keeps_selection_visible() {
        let mut t = Tree::root();
        // page is long: ~43 leaves + 8 headings; a 3-row viewport must scroll
        let avail = 3;
        t.sel = 0;
        t.clamp_viewport(avail);
        let mut vrow = t.selected_visual_row();
        assert!(t.top <= vrow && vrow < t.top + avail, "cursor row inside the viewport");
        t.sel = t.visible_leaves().len() - 1; // last leaf (Reset settings)
        t.clamp_viewport(avail);
        vrow = t.selected_visual_row();
        assert!(t.top <= vrow && vrow < t.top + avail, "cursor row must be inside the viewport");
        t.sel = 0;
        t.clamp_viewport(avail);
        vrow = t.selected_visual_row();
        assert!(t.top <= vrow && vrow < t.top + avail, "cursor row inside the viewport again");
        // scrolling back up parks the viewport exactly on the cursor's row
        assert_eq!(t.top, vrow);
    }

    #[test]
    fn filter_jumps_across_sections() {
        let mut t = Tree::root();
        t.query = "volume".into();
        let vis = t.visible_leaves();
        assert_eq!(vis.len(), 1, "Volume is the only match");
        assert_eq!(vis[0].2, "ALERTS", "the match carries its section");
        // filtering hides empty sections: no APPEARANCE heading on the page
        let headings: Vec<&str> = t
            .rows()
            .iter()
            .filter_map(|r| match r {
                VRow::Heading(s) => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(headings, ["ALERTS"]);
    }
}
