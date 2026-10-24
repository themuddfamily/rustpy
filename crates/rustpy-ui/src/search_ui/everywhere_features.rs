//! Bounded additional providers and learned navigation ranking. No disk scans in paint.
use super::*;
use std::io::Read;

#[derive(Clone, Debug)]
pub enum Destination {
    File(PathBuf, usize),
    Settings(&'static str),
    Git(String),
    Copy(String),
    Tab(EverywhereTab),
}
#[derive(Clone, Debug)]
pub struct ExtraItem {
    pub label: String,
    pub detail: String,
    pub destination: Destination,
    pub category: EverywhereTab,
}
#[derive(Default)]
pub struct Snapshot {
    pub sources: Vec<(PathBuf, String)>,
    pub entries: Vec<ExtraItem>,
    pub git_count: usize,
}
pub fn snapshot(
    root: PathBuf,
    paths: Vec<PathBuf>,
    buffers: HashMap<PathBuf, String>,
    git_limit: usize,
    progress: &ProgressReporter,
) -> Result<Snapshot, rustpy_core::TaskError> {
    let mut snapshot = Snapshot::default();
    let mut bytes = 0usize;
    for path in paths.into_iter().take(256) {
        progress.check()?;
        let text = if let Some(text) = buffers.get(&path) {
            if text.len() > 128 * 1024 {
                continue;
            }
            text.clone()
        } else {
            let Ok(file) = std::fs::File::open(&path) else {
                continue;
            };
            if !file
                .metadata()
                .is_ok_and(|meta| meta.is_file() && meta.len() <= 128 * 1024)
            {
                continue;
            }
            let mut text = String::new();
            if file.take(128 * 1024 + 1).read_to_string(&mut text).is_err()
                || text.len() > 128 * 1024
            {
                continue;
            }
            text
        };
        bytes += text.len();
        if bytes > 4 * 1024 * 1024 {
            break;
        }
        snapshot.sources.push((path, text));
    }
    progress.check()?;
    let endpoints = rustpy_semantic::endpoints::scan_project(&snapshot.sources);
    for endpoint in endpoints.into_iter().take(512) {
        snapshot.entries.push(ExtraItem {
            label: format!("{} {}", endpoint.methods.join("|"), endpoint.path),
            detail: format!("{} · {}", endpoint.framework, endpoint.handler),
            destination: Destination::File(endpoint.file, endpoint.line),
            category: EverywhereTab::Endpoints,
        });
    }
    progress.check()?;
    if let Some(repo) = rustpy_vcs::GitRepo::discover(&root) {
        if let Ok(commits) = repo.log_graph(git_limit.min(4096), true) {
            snapshot.git_count = commits.len();
            for commit in commits {
                for name in commit.refs.iter().take(256) {
                    snapshot.entries.push(ExtraItem {
                        label: name.clone(),
                        detail: format!("Git branch/tag at {} — open in Log", commit.short_id),
                        destination: Destination::Git(commit.id.clone()),
                        category: EverywhereTab::Git,
                    });
                }
                snapshot.entries.push(ExtraItem {
                    label: format!("{} {}", commit.short_id, commit.summary),
                    detail: format!("Git commit · {}", commit.author),
                    destination: Destination::Git(commit.id),
                    category: EverywhereTab::Git,
                });
            }
        }
    }
    Ok(snapshot)
}

pub const SETTINGS: &[(&str, &str)] = &[
    ("Appearance and theme", "appearance"),
    ("Keymap and shortcuts", "keymap"),
    ("Editor font", "font"),
    ("Color scheme", "scheme"),
    ("Inspections and severities", "inspections"),
    ("Python interpreter", "interpreter"),
    ("Live templates", "templates"),
    ("Plugins", "plugins"),
    ("Code completion", "completion"),
    ("Code style", "style"),
];
pub const TABS: &[(EverywhereTab, &str, &str)] = &[
    (EverywhereTab::All, "All", "all"),
    (EverywhereTab::Classes, "Classes", "classes"),
    (EverywhereTab::Files, "Files", "files"),
    (EverywhereTab::Symbols, "Symbols", "symbols"),
    (EverywhereTab::Actions, "Actions", "actions"),
    (EverywhereTab::Text, "Text", "text"),
    (EverywhereTab::Git, "Git", "git"),
    (EverywhereTab::Endpoints, "Endpoints", "endpoints"),
    (EverywhereTab::Settings, "Settings", "settings"),
];

/// Small arithmetic grammar; finite input/depth/operations, no interpreter execution.
pub fn calculate(input: &str) -> Option<f64> {
    if input.len() > 256 || !input.bytes().any(|b| b.is_ascii_digit()) {
        return None;
    }
    struct Parser<'a> {
        bytes: &'a [u8],
        at: usize,
        work: usize,
    }
    impl Parser<'_> {
        fn spaces(&mut self) {
            while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
                self.at += 1;
            }
        }
        fn expression(&mut self, depth: usize, minimum: u8) -> Option<f64> {
            self.work += 1;
            if depth > 16 || self.work > 128 {
                return None;
            }
            self.spaces();
            let mut value = match self.bytes.get(self.at).copied()? {
                b'+' | b'-' => {
                    let negative = self.bytes[self.at] == b'-';
                    self.at += 1;
                    let n = self.expression(depth + 1, 3)?;
                    if negative {
                        -n
                    } else {
                        n
                    }
                }
                b'(' => {
                    self.at += 1;
                    let n = self.expression(depth + 1, 0)?;
                    self.spaces();
                    if self.bytes.get(self.at) != Some(&b')') {
                        return None;
                    }
                    self.at += 1;
                    n
                }
                _ => {
                    let start = self.at;
                    while self
                        .bytes
                        .get(self.at)
                        .is_some_and(|b| b.is_ascii_digit() || *b == b'.')
                    {
                        self.at += 1;
                    }
                    std::str::from_utf8(&self.bytes[start..self.at])
                        .ok()?
                        .parse()
                        .ok()?
                }
            };
            loop {
                self.spaces();
                let op = self.bytes.get(self.at).copied().unwrap_or(0);
                let precedence = match op {
                    b'+' | b'-' => 1,
                    b'*' | b'/' | b'%' => 2,
                    _ => break,
                };
                if precedence < minimum {
                    break;
                }
                self.at += 1;
                let rhs = self.expression(depth + 1, precedence + 1)?;
                value = match op {
                    b'+' => value + rhs,
                    b'-' => value - rhs,
                    b'*' => value * rhs,
                    b'/' => value / rhs,
                    _ => value % rhs,
                };
                if !value.is_finite() {
                    return None;
                }
            }
            Some(value)
        }
    }
    let mut parser = Parser {
        bytes: input.as_bytes(),
        at: 0,
        work: 0,
    };
    let value = parser.expression(0, 0)?;
    parser.spaces();
    (parser.at == input.len() && value.is_finite()).then_some(value)
}

pub fn match_score(candidate: &str, query: &str) -> Option<i32> {
    rustpy_search::navigation_score(candidate, query)
}

impl SearchEverywhere {
    pub fn clear_providers(&mut self) {
        if let Some(job) = self.provider_job.take() {
            job.cancel();
        }
        self.providers = Snapshot::default();
        self.provider_buffers.clear();
    }
    pub fn start_providers(
        &mut self,
        root: &Path,
        paths: Vec<PathBuf>,
        buffers: HashMap<PathBuf, String>,
    ) {
        if let Some(job) = self.provider_job.take() {
            job.cancel();
        }
        let paths = paths
            .into_iter()
            .filter(|path| {
                path.strip_prefix(root)
                    .unwrap_or(path)
                    .to_string_lossy()
                    .contains(&self.scope_filter)
            })
            .collect();
        self.provider_buffers = buffers.clone();
        self.providers = Snapshot::default();
        let root = root.to_path_buf();
        let git_limit = self.git_limit;
        self.provider_job = Some(
            TaskManager::global()
                .task("Search Everywhere providers")
                .hidden()
                .spawn(move |progress| snapshot(root, paths, buffers, git_limit, progress)),
        );
    }
    pub(super) fn add_pinned(&self, items: &mut Vec<NavItem>) {
        for path in &self.memory.pinned {
            if path.exists()
                && !items
                    .iter()
                    .any(|item| matches!(item, NavItem::File { path:p,.. } if p==path))
            {
                items.push(NavItem::File {
                    path: path.clone(),
                    score: 200,
                });
            }
        }
        items.sort_by_key(|item| std::cmp::Reverse(self.ranked_score(item) as i32));
    }
    fn identity(item: &NavItem) -> String {
        match item {
            NavItem::File { path, .. } => format!("file:{}", path.display()),
            NavItem::Symbol {
                path,
                name,
                container,
                ..
            } => format!("symbol:{}:{container}.{name}", path.display()),
            NavItem::Action { action, .. } => format!("action:{}", action.id()),
            NavItem::Tool { tool, .. } => format!("tool:{}", tool.name),
            NavItem::Extra { item, .. } => format!("extra:{:?}:{}", item.category, item.label),
        }
    }
    fn rank_features(&self, item: &NavItem) -> [f32; 4] {
        let path = match item {
            NavItem::File { path, .. } | NavItem::Symbol { path, .. } => Some(path),
            _ => None,
        };
        [
            item.score() as f32 / 1000.0,
            self.memory
                .accepted
                .get(&Self::identity(item))
                .copied()
                .unwrap_or(0)
                .min(20) as f32
                / 20.0,
            path.and_then(|p| self.recent_files.iter().position(|r| r == p))
                .map_or(0.0, |i| 1.0 / (i + 1) as f32),
            if path.is_some_and(|p| self.memory.pinned.contains(p)) {
                1.0
            } else {
                0.0
            },
        ]
    }
    pub(super) fn ranked_score(&self, item: &NavItem) -> f32 {
        let features = self.rank_features(item);
        let learned: f32 = features
            .iter()
            .zip(self.memory.weights)
            .map(|(feature, weight)| {
                feature
                    * if weight.is_finite() {
                        weight.clamp(-5.0, 5.0)
                    } else {
                        0.0
                    }
            })
            .sum();
        let abbreviation = self
            .memory
            .abbreviations
            .get(&self.query.trim().to_lowercase())
            .is_some_and(|id| *id == Self::identity(item));
        item.score() as f32
            + 100.0 * learned
            + features[2] * 40.0
            + features[3] * 100.0
            + if abbreviation { 200.0 } else { 0.0 }
    }
    pub(super) fn bind_abbreviation(&mut self, alias: &str, item: &NavItem) -> bool {
        let alias = alias.trim().to_lowercase();
        if alias.is_empty()
            || alias.len() > 32
            || alias.chars().any(char::is_whitespace)
            || !matches!(item, NavItem::Action { .. } | NavItem::File { .. })
            || (self.memory.abbreviations.len() >= 128
                && !self.memory.abbreviations.contains_key(&alias))
        {
            return false;
        }
        self.memory
            .abbreviations
            .insert(alias, Self::identity(item));
        true
    }
    pub(super) fn learn_selection(&mut self, item: &NavItem) {
        // Pairwise feedback updates the local linear ranker only when a lower row was chosen.
        if let Some(first) = self
            .items
            .first()
            .filter(|first| Self::identity(first) != Self::identity(item))
        {
            let selected = self.rank_features(item);
            let rejected = self.rank_features(first);
            for i in 0..4 {
                let old = self.memory.weights[i];
                self.memory.weights[i] = (if old.is_finite() { old } else { 0.0 }
                    + 0.1 * (selected[i] - rejected[i]))
                    .clamp(-5.0, 5.0);
            }
        }
        let id = Self::identity(item);
        if self.memory.accepted.len() < 512 || self.memory.accepted.contains_key(&id) {
            let count = self.memory.accepted.entry(id.clone()).or_default();
            *count = count.saturating_add(1);
        }
        let query = self.query.trim();
        if !query.is_empty() && query.len() <= 128 && self.memory.abbreviations.len() < 128 {
            self.memory
                .abbreviations
                .entry(query.to_lowercase())
                .or_insert(id);
        }
    }
    pub(super) fn additional_items(
        &self,
        root: &Path,
        query: &str,
        tab: EverywhereTab,
        items: &mut Vec<NavItem>,
    ) {
        if let Some(id) = self.memory.abbreviations.get(&query.to_lowercase()) {
            if matches!(
                tab,
                EverywhereTab::All | EverywhereTab::Actions | EverywhereTab::Settings
            ) {
                if let Some(action) = id.strip_prefix("action:").and_then(Keymap::action_by_id) {
                    items.push(NavItem::Action {
                        action,
                        score: 2000,
                    });
                }
            }
            if matches!(tab, EverywhereTab::All | EverywhereTab::Files) {
                if let Some(path) = id
                    .strip_prefix("file:")
                    .map(PathBuf::from)
                    .filter(|path| path.exists())
                {
                    items.push(NavItem::File { path, score: 2000 });
                }
            }
        }
        if query.is_empty() && tab == EverywhereTab::All {
            self.add_pinned(items);
            return;
        }
        if matches!(
            tab,
            EverywhereTab::All | EverywhereTab::Settings | EverywhereTab::Actions
        ) {
            for &(label, page) in SETTINGS {
                if let Some(score) = match_score(label, query) {
                    items.push(NavItem::Extra {
                        item: ExtraItem {
                            label: label.into(),
                            detail: "Open settings page".into(),
                            destination: Destination::Settings(page),
                            category: EverywhereTab::Settings,
                        },
                        score: score - 50,
                    });
                }
            }
            for (alias, action) in [
                ("gc", Action::GoToClass),
                ("gf", Action::GoToFile),
                ("gs", Action::GoToSymbol),
                ("se", Action::SearchEverywhere),
                ("ps", Action::TogglePowerSaveMode),
            ] {
                if query.eq_ignore_ascii_case(alias) {
                    items.push(NavItem::Action {
                        action,
                        score: 1000,
                    });
                }
            }
            for action in Action::ALL
                .iter()
                .filter(|action| action.id().contains("Toggle"))
            {
                if let Some(score) = match_score(action.english_text(), query) {
                    items.push(NavItem::Action {
                        action: *action,
                        score: score - 20,
                    });
                }
            }
        }
        if matches!(tab, EverywhereTab::All | EverywhereTab::Files) && !query.is_empty() {
            let mut dirs = HashSet::new();
            if let (Some(index), Some(scope)) = (&self.file_index, &self.project_scope) {
                if let Some(files) = index.files_for(scope) {
                    for path in files.iter().take(4096) {
                        for directory in path.ancestors().skip(1).take(32) {
                            if directory == root || !directory.starts_with(root) {
                                break;
                            }
                            if !dirs.insert(directory.to_path_buf()) {
                                continue;
                            }
                            let label = directory
                                .strip_prefix(root)
                                .unwrap_or(directory)
                                .to_string_lossy();
                            if let Some(score) = match_score(&label, query.trim_end_matches('/')) {
                                items.push(NavItem::File {
                                    path: directory.to_path_buf(),
                                    score: score - 10,
                                });
                            }
                        }
                    }
                }
            }
        }
        if matches!(tab, EverywhereTab::All | EverywhereTab::Text)
            && query.len() >= 2
            && !query.starts_with('/')
        {
            let needle = query.to_lowercase();
            let mut found = 0;
            'files: for (path, text) in &self.providers.sources {
                for (line, value) in text.lines().enumerate() {
                    if value.to_lowercase().contains(&needle) {
                        let label: String = value.trim().chars().take(160).collect();
                        items.push(NavItem::Extra {
                            item: ExtraItem {
                                label,
                                detail: format!("{}:{}", path.display(), line + 1),
                                destination: Destination::File(path.clone(), line),
                                category: EverywhereTab::Text,
                            },
                            score: 100,
                        });
                        found += 1;
                        if found >= 40 {
                            break 'files;
                        }
                    }
                }
            }
        }
        for item in &self.providers.entries {
            if tab != EverywhereTab::All && tab != item.category {
                continue;
            }
            if let Some(score) = match_score(&format!("{} {}", item.label, item.detail), query) {
                items.push(NavItem::Extra {
                    item: item.clone(),
                    score,
                });
            }
        }
        if tab == EverywhereTab::All {
            if let Some(value) = calculate(query.trim_start_matches('=').trim()) {
                items.push(NavItem::Extra {
                    item: ExtraItem {
                        label: format!("= {value}"),
                        detail: "Copy calculator result".into(),
                        destination: Destination::Copy(value.to_string()),
                        category: EverywhereTab::All,
                    },
                    score: 2000,
                });
            }
        }
        if query.starts_with('/') {
            let needle = query.trim_start_matches('/');
            for &(category, label, command) in TABS {
                if command.starts_with(needle) {
                    items.push(NavItem::Extra {
                        item: ExtraItem {
                            label: format!("/{command}"),
                            detail: format!("Search {label}"),
                            destination: Destination::Tab(category),
                            category,
                        },
                        score: 2500,
                    });
                }
            }
        }
        // Deduplicate actions contributed by settings toggles and the main provider.
        let mut identities = HashSet::new();
        items.retain(|item| identities.insert(Self::identity(item)));
    }
    pub(super) fn preview_ui(&self, ui: &mut egui::Ui) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let (path, line) = match item {
            NavItem::File { path, .. } => (Some(path), 0),
            NavItem::Symbol { path, line, .. } => (Some(path), *line),
            NavItem::Extra { item, .. } => match &item.destination {
                Destination::File(path, line) => (Some(path), *line),
                _ => {
                    ui.separator();
                    ui.label(&item.detail);
                    return;
                }
            },
            NavItem::Action { action, .. } => {
                ui.separator();
                ui.label(format!("{} · {}", action.english_text(), action.id()));
                return;
            }
            NavItem::Tool { tool, .. } => {
                ui.separator();
                ui.label(format!("External tool: {}", tool.name));
                return;
            }
        };
        if let Some((path, text)) =
            path.and_then(|path| self.providers.sources.iter().find(|(p, _)| p == path))
        {
            ui.separator();
            ui.label(format!("{}:{}", path.display(), line + 1));
            for (i, value) in text
                .lines()
                .enumerate()
                .skip(line.saturating_sub(2))
                .take(5)
            {
                let value: String = value.chars().take(200).collect();
                ui.monospace(format!("{} {value}", i + 1));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rebuild(popup: &mut SearchEverywhere, query: &str, tab: EverywhereTab) {
        popup.query = query.into();
        popup.tab = tab;
        popup.rebuild(Path::new("/project"), &Keymap::default_keymap(), &[]);
    }
    #[test]
    fn calculator_uses_precedence_and_refuses_invalid_or_exhausted_input() {
        assert_eq!(calculate("2+3*4"), Some(14.0));
        assert_eq!(calculate("(2+3)*-4"), Some(-20.0));
        for value in ["1/0", "1%0", "__import__('os')", "2+", "2 3"] {
            assert_eq!(calculate(value), None);
        }
        assert_eq!(
            calculate(&format!("{}1{}", "(".repeat(32), ")".repeat(32))),
            None
        );
    }
    #[test]
    fn real_providers_find_text_endpoints_git_settings_commands_and_action_abbreviations() {
        let mut popup = SearchEverywhere::default();
        popup.providers.sources.push((
            PathBuf::from("/project/main.py"),
            "first\nneedle actual text\n".into(),
        ));
        popup.providers.entries.push(ExtraItem {
            label: "GET /users".into(),
            detail: "FastAPI · users".into(),
            destination: Destination::File(PathBuf::from("/project/api.py"), 7),
            category: EverywhereTab::Endpoints,
        });
        popup.providers.entries.push(ExtraItem {
            label: "topic".into(),
            detail: "Git branch at abc".into(),
            destination: Destination::Git("abc".into()),
            category: EverywhereTab::Git,
        });
        rebuild(&mut popup, "needle", EverywhereTab::Text);
        assert!(
            matches!(&popup.items[0],NavItem::Extra {item:ExtraItem {destination:Destination::File(path,1),..},..} if path==Path::new("/project/main.py"))
        );
        rebuild(&mut popup, "users", EverywhereTab::Endpoints);
        assert!(matches!(
            &popup.items[0],
            NavItem::Extra {
                item: ExtraItem {
                    destination: Destination::File(_, 7),
                    ..
                },
                ..
            }
        ));
        rebuild(&mut popup, "topic", EverywhereTab::Git);
        assert!(
            matches!(&popup.items[0],NavItem::Extra {item:ExtraItem {destination:Destination::Git(id),..},..} if id=="abc")
        );
        rebuild(&mut popup, "font", EverywhereTab::Settings);
        assert!(popup.items.iter().any(|item| matches!(
            item,
            NavItem::Extra {
                item: ExtraItem {
                    destination: Destination::Settings("font"),
                    ..
                },
                ..
            }
        )));
        assert!(popup.bind_abbreviation(
            "banana",
            &NavItem::Action {
                action: Action::GoToClass,
                score: 0
            }
        ));
        rebuild(&mut popup, "banana", EverywhereTab::Actions);
        assert!(matches!(
            &popup.items[0],
            NavItem::Action {
                action: Action::GoToClass,
                ..
            }
        ));
        rebuild(&mut popup, "gc", EverywhereTab::Actions);
        assert!(popup.items.iter().any(|item| matches!(
            item,
            NavItem::Action {
                action: Action::GoToClass,
                ..
            }
        )));
        rebuild(&mut popup, "/text", EverywhereTab::All);
        assert!(matches!(
            &popup.items[0],
            NavItem::Extra {
                item: ExtraItem {
                    destination: Destination::Tab(EverywhereTab::Text),
                    ..
                },
                ..
            }
        ));
        rebuild(&mut popup, "=2+3*4", EverywhereTab::All);
        assert!(
            matches!(&popup.items[0],NavItem::Extra {item:ExtraItem {destination:Destination::Copy(value),..},..} if value=="14")
        );
    }
    #[test]
    fn local_feedback_updates_actual_ranking_and_scope_filters_results() {
        let mut popup = SearchEverywhere {
            query: "m".into(),
            ..Default::default()
        };
        let first = NavItem::File {
            path: "/project/main.py".into(),
            score: 800,
        };
        let selected = NavItem::File {
            path: "/project/model.py".into(),
            score: 700,
        };
        popup.recent_files.push("/project/model.py".into());
        popup.items = vec![first.clone(), selected.clone()];
        let before = popup.ranked_score(&selected);
        popup.learn_selection(&selected);
        assert!(popup.ranked_score(&selected) > before);
        assert!(popup.memory.weights.iter().any(|weight| *weight != 0.0));
        let encoded = serde_json::to_string(&popup.memory).unwrap();
        let restored: rustpy_settings::navigation_search::NavigationSearch =
            serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, popup.memory);
        popup.providers.sources = vec![
            ("/project/src/a.py".into(), "needle".into()),
            ("/project/tests/b.py".into(), "needle".into()),
        ];
        popup.scope_filter = "src/".into();
        rebuild(&mut popup, "needle", EverywhereTab::Text);
        assert_eq!(popup.items.len(), 1);
    }
    #[test]
    fn provider_snapshot_uses_unsaved_text_and_actual_endpoint_locations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("api.py");
        std::fs::write(&path, "disk = 1\n").unwrap();
        let text =
            "from fastapi import FastAPI\napp=FastAPI()\n@app.get('/fresh')\ndef fresh(): pass\n";
        let buffers = HashMap::from([(path.clone(), text.to_string())]);
        let captured = snapshot(
            directory.path().to_path_buf(),
            vec![path.clone()],
            buffers,
            100,
            &ProgressReporter::new(),
        )
        .unwrap();
        assert_eq!(captured.sources[0].1, text);
        assert!(captured.entries.iter().any(
            |item| matches!(&item.destination,Destination::File(found,3) if found==&path)
                && item.label.contains("/fresh")
        ));
        assert_eq!(std::fs::read_to_string(path).unwrap(), "disk = 1\n");
    }
}

#[cfg(test)]
mod keyboard_tests {
    use super::*;
    #[test]
    fn real_popup_enter_dispatches_calculator_and_slash_commands_keep_popup_open() {
        let context = egui::Context::default();
        let theme = UiTheme::dark();
        let keymap = Keymap::default_keymap();
        let mut popup = SearchEverywhere::default();
        for (query, command) in [("=2+3", false), ("/settings", true)] {
            popup.open(EverywhereTab::All);
            popup.query = query.into();
            let output = context.run_ui(egui::RawInput::default(), |ui| {
                popup.ui(ui.ctx(), &theme, Path::new("/project"), &keymap);
            });
            output.drop_without_applying_deltas();
            let input = egui::RawInput {
                events: vec![
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: Some(egui::Key::Enter),
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: Some(egui::Key::Enter),
                        pressed: false,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..Default::default()
            };
            let mut events = Vec::new();
            let output = context.run_ui(input, |ui| {
                events = popup.ui(ui.ctx(), &theme, Path::new("/project"), &keymap);
            });
            output.drop_without_applying_deltas();
            if command {
                assert!(popup.visible);
                assert_eq!(popup.tab, EverywhereTab::Settings);
                assert!(popup.query.is_empty());
            } else {
                assert!(!popup.visible);
                assert!(events.iter().any(|event|matches!(event,NavEvent::SearchDestination(Destination::Copy(value)) if value=="5")));
            }
        }
    }
    #[test]
    fn pinned_recent_file_survives_empty_query_and_memory_roundtrip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pinned.py");
        std::fs::write(&path, "value=1\n").unwrap();
        let mut popup = SearchEverywhere::default();
        popup.memory.pinned.push(path.clone());
        let encoded = serde_json::to_string(&popup.memory).unwrap();
        popup.memory = serde_json::from_str(&encoded).unwrap();
        popup.open(EverywhereTab::Files);
        popup.rebuild(directory.path(), &Keymap::default_keymap(), &[]);
        assert!(matches!(&popup.items[0],NavItem::File {path:found,..} if *found==path));
    }
}
