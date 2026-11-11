//! The application's state. `Shell` is the one view: pages are methods on it, so background work
//! has a single place to report to and every page can read what the others know.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

use gpui::{px, FocusHandle, ListAlignment, ListState, ScrollHandle, UniformListScrollHandle};
use hm_ai::types::ModelInventory;
use hm_core::{Area, CancelToken, Finding, ScanContext};
use hm_dev::types::{DockerSummary, ListeningPort, ProjectInfo};
use hm_system::types::{AppInfo, DirNode, HealthReport, StartupItem, SystemSnapshot};

use crate::flows::clean::Sheet;
use crate::logic::selection::Selection;
use crate::ui::scrollbar::ScrollbarState;

/// How the app was launched. Read once at startup from the environment.
#[derive(Clone, Debug, Default)]
pub struct RunMode {
    /// `HEALTHYMAC_DEMO=1`: invented data, and nothing real can be touched.
    pub demo: bool,
    /// `HEALTHYMAC_DEMO=stress`: the same, with thousands of findings.
    pub stress: bool,
    /// `HEALTHYMAC_DRY_RUN=1`: real data, but cleaning only simulates.
    pub dry_run: bool,
    /// `HEALTHYMAC_HOME=<path>`: the whole app runs against this home.
    pub home_override: Option<PathBuf>,
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty() && value != "0")
}

impl RunMode {
    pub fn from_env() -> Self {
        let demo_value = std::env::var("HEALTHYMAC_DEMO").unwrap_or_default();
        Self {
            demo: !demo_value.is_empty() && demo_value != "0",
            stress: demo_value == "stress",
            dry_run: env_flag("HEALTHYMAC_DRY_RUN"),
            home_override: std::env::var_os("HEALTHYMAC_HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
        }
    }

    /// Whether settings may be written to disk. Never in a demo, and a dry run against the real home
    /// leaves the real config alone too.
    pub fn persists_config(&self) -> bool {
        !self.demo && (!self.dry_run || self.home_override.is_some())
    }

    /// Whether anything real can be changed from this run.
    pub fn is_simulated(&self) -> bool {
        self.demo || self.dry_run
    }

    /// Signals reach real processes, which no test home contains, so only a normal run sends them.
    pub fn may_signal_processes(&self) -> bool {
        !self.is_simulated() && self.home_override.is_none()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Resumen,
    Area(Area),
    Modelos,
    Proyectos,
    Puertos,
    Apps,
    Arranque,
    Mapa,
    Monitor,
    Ajustes,
}

impl Page {
    pub fn serial(self) -> u64 {
        match self {
            Page::Resumen => 0,
            Page::Area(Area::System) => 1,
            Page::Area(Area::Developer) => 2,
            Page::Area(Area::LocalAi) => 3,
            Page::Area(Area::Files) => 4,
            Page::Area(Area::Apps) => 5,
            Page::Modelos => 6,
            Page::Proyectos => 7,
            Page::Puertos => 8,
            Page::Apps => 9,
            Page::Arranque => 10,
            Page::Mapa => 11,
            Page::Monitor => 12,
            Page::Ajustes => 13,
        }
    }
}

/// Data that is fetched in the background and may be refreshed while the old copy stays on screen.
#[derive(Debug)]
pub struct Loaded<T> {
    pub data: Option<T>,
    pub loading: bool,
    pub error: Option<String>,
    pub at: Option<Instant>,
}

impl<T> Default for Loaded<T> {
    fn default() -> Self {
        Self {
            data: None,
            loading: false,
            error: None,
            at: None,
        }
    }
}

impl<T> Loaded<T> {
    pub fn begin(&mut self) {
        self.loading = true;
    }

    pub fn finish(&mut self, result: Result<T, String>) {
        self.loading = false;
        self.at = Some(Instant::now());
        match result {
            Ok(data) => {
                self.data = Some(data);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub fn is_stale(&self, seconds: u64) -> bool {
        self.at.is_none_or(|at| at.elapsed().as_secs() >= seconds)
    }
}

// ---- Scan --------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ScannerMeta {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub area: Area,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ScannerStatus {
    Waiting,
    Running,
    Done { count: usize, bytes: u64 },
    Failed(String),
}

#[derive(Default)]
pub struct ScanState {
    pub running: bool,
    pub cancelling: bool,
    pub finished_once: bool,
    pub was_cancelled: bool,
    pub statuses: HashMap<&'static str, ScannerStatus>,
    pub cancel: Option<CancelToken>,
    pub finished_at: Option<std::time::SystemTime>,
}

impl ScanState {
    pub fn progress(&self) -> (usize, usize) {
        let done = self
            .statuses
            .values()
            .filter(|status| {
                matches!(
                    status,
                    ScannerStatus::Done { .. } | ScannerStatus::Failed(_)
                )
            })
            .count();
        (done, self.statuses.len())
    }

    pub fn failures(&self) -> Vec<(&'static str, String)> {
        let mut failures: Vec<_> = self
            .statuses
            .iter()
            .filter_map(|(id, status)| match status {
                ScannerStatus::Failed(error) => Some((*id, error.clone())),
                _ => None,
            })
            .collect();
        failures.sort();
        failures
    }
}

/// One row of an area's findings list, in display order.
#[derive(Clone, Debug)]
pub enum FindingRow {
    /// Findings of one scanner. `members` index into `Shell::findings`.
    Scanner {
        scanner_id: String,
        members: std::rc::Rc<Vec<usize>>,
        total: u64,
    },
    /// A sub-heading inside a scanner.
    Group {
        title: String,
        members: std::rc::Rc<Vec<usize>>,
        total: u64,
    },
    Item {
        index: usize,
        /// Set when the item is alone under its scanner or group and stands in for that heading,
        /// so the same name and size are not shown twice in a row.
        heading: Option<Heading>,
    },
    Gap,
}

/// The heading a lone item replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Heading {
    Scanner,
    Group(String),
}

pub struct AreaView {
    pub rows: Vec<FindingRow>,
    pub list: ListState,
    pub bar: ScrollbarState,
    pub expanded: HashSet<String>,
}

impl Default for AreaView {
    fn default() -> Self {
        Self {
            rows: Vec::new(),
            list: page_list(0),
            bar: ScrollbarState::default(),
            expanded: HashSet::new(),
        }
    }
}

pub fn page_list(count: usize) -> ListState {
    ListState::new(count, ListAlignment::Top, px(400.))
}

/// A scrollable page body with its scrollbar.
#[derive(Clone, Default)]
pub struct ScrollPane {
    pub handle: ScrollHandle,
    pub bar: ScrollbarState,
}

// ---- Live monitor ------------------------------------------------------------------------------

pub const HISTORY_LEN: usize = 90;

#[derive(Default)]
pub struct LiveState {
    pub snapshot: Option<SystemSnapshot>,
    pub in_flight: bool,
    pub cpu: VecDeque<f32>,
    pub memory: VecDeque<f32>,
    pub rx: VecDeque<f32>,
    pub tx: VecDeque<f32>,
    pub health: Option<HealthReport>,
    pub ring_from: f32,
    pub ring_to: f32,
    pub ring_key: u64,
}

pub fn push_history(history: &mut VecDeque<f32>, value: f32) {
    if history.len() == HISTORY_LEN {
        history.pop_front();
    }
    history.push_back(value);
}

// ---- Pages -------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSort {
    Size,
    LastUsed,
    Name,
}

#[derive(Clone, Debug)]
pub enum ModelRow {
    Hero,
    Runtimes,
    TableHeader,
    Model(usize),
    NoModels,
    DuplicatesHeader,
    Duplicate(usize),
    Gap,
}

pub struct ModelsState {
    pub inventory: Loaded<ModelInventory>,
    pub rows: Vec<ModelRow>,
    pub list: ListState,
    pub bar: ScrollbarState,
    pub sort: ModelSort,
    pub order: Vec<usize>,
    pub runtimes_at: Option<Instant>,
    pub runtimes_loading: bool,
    pub hovered_marker: Option<usize>,
}

impl Default for ModelsState {
    fn default() -> Self {
        Self {
            inventory: Loaded::default(),
            rows: Vec::new(),
            list: page_list(0),
            bar: ScrollbarState::default(),
            sort: ModelSort::Size,
            order: Vec::new(),
            runtimes_at: None,
            runtimes_loading: false,
            hovered_marker: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectFilter {
    All,
    Idle,
}

pub struct ProjectsState {
    pub projects: Loaded<Vec<ProjectInfo>>,
    pub docker: Loaded<Option<DockerSummary>>,
    pub list: ListState,
    pub bar: ScrollbarState,
    pub expanded: HashSet<PathBuf>,
    pub filter: ProjectFilter,
    pub order: Vec<usize>,
}

impl Default for ProjectsState {
    fn default() -> Self {
        Self {
            projects: Loaded::default(),
            docker: Loaded::default(),
            list: page_list(0),
            bar: ScrollbarState::default(),
            expanded: HashSet::new(),
            filter: ProjectFilter::All,
            order: Vec::new(),
        }
    }
}

#[derive(Default)]
pub struct PortsState {
    pub ports: Loaded<Vec<ListeningPort>>,
    pub scroll: UniformListScrollHandle,
    pub bar: ScrollbarState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppSort {
    Size,
    LastUsed,
    Name,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppsTab {
    Installed,
    Leftovers,
}

pub struct AppsState {
    pub apps: Loaded<Vec<AppInfo>>,
    pub icons: HashMap<PathBuf, std::sync::Arc<gpui::Image>>,
    pub sort: AppSort,
    pub tab: AppsTab,
    pub order: Vec<usize>,
    pub scroll: UniformListScrollHandle,
    pub bar: ScrollbarState,
}

impl Default for AppsState {
    fn default() -> Self {
        Self {
            apps: Loaded::default(),
            icons: HashMap::new(),
            sort: AppSort::Size,
            tab: AppsTab::Installed,
            order: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            bar: ScrollbarState::default(),
        }
    }
}

#[derive(Default)]
pub struct StartupState {
    pub items: Loaded<Vec<StartupItem>>,
    pub scroll: UniformListScrollHandle,
    pub bar: ScrollbarState,
}

#[derive(Default)]
pub struct DiskState {
    pub root: Option<DirNode>,
    pub root_path: Option<PathBuf>,
    pub error: Option<String>,
    pub loading: bool,
    pub started: Option<Instant>,
    pub cancel: Option<CancelToken>,
    /// Indices into `children`, from the root to the folder being shown.
    pub trail: Vec<usize>,
    pub hover: Option<HoverTile>,
    pub deepening: Option<PathBuf>,
    pub serial: u64,
}

/// What the pointer is over in the treemap: child indices relative to the current folder.
#[derive(Clone, Debug, PartialEq)]
pub struct HoverTile {
    pub path: Vec<usize>,
}

#[derive(Default)]
pub struct SettingsState {
    pub scroll: ScrollPane,
    pub history: Loaded<Vec<hm_core::audit::AuditEntry>>,
    pub full_disk_access: Option<bool>,
    pub save_error: Option<String>,
}

// ---- The shell ---------------------------------------------------------------------------------

pub struct Toast {
    pub message: String,
    pub serial: u64,
}

pub struct Shell {
    pub mode: RunMode,
    pub ctx: ScanContext,
    pub config_error: Option<String>,
    pub focus: FocusHandle,

    pub page: Page,

    pub scanners: Vec<ScannerMeta>,
    pub scan: ScanState,
    pub findings: Vec<Finding>,
    pub selection: Selection,
    pub views: HashMap<Area, AreaView>,

    pub live: LiveState,
    pub monitor_tx: Option<std::sync::mpsc::Sender<()>>,

    pub resumen_scroll: ScrollPane,
    pub monitor_scroll: ScrollPane,
    pub models: ModelsState,
    pub projects: ProjectsState,
    pub ports: PortsState,
    pub apps: AppsState,
    pub startup: StartupState,
    pub disk: DiskState,
    pub settings: SettingsState,

    pub sheet: Option<Sheet>,
    /// The sheet is playing its way out; it is removed when that ends.
    pub sheet_closing: bool,
    /// Counts sheets opened from nothing, so each one gets a fresh entrance.
    pub sheet_opened: u64,
    pub toast: Option<Toast>,
    pub toast_serial: u64,
    /// Where the pointer pressed the drag strip, so that a press with no movement is not a move.
    pub titlebar_pressed: bool,
}

impl Shell {
    /// A new context for one job. Never reuse one: engines cache sweeps keyed partly on `now`, so an
    /// old context right after a clean would be handed the results from before it.
    pub fn fresh_ctx(&self) -> ScanContext {
        ScanContext::with_home(self.ctx.home.clone(), self.ctx.config.clone())
    }
}
