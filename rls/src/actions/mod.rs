//! Actions that the RLS can perform: responding to requests, watching files,
//! etc.

use crate::config::Config;
use crate::config::FmtConfig;
use crate::Span;
use log::{debug, error, info, trace};
use rls_analysis::AnalysisHost;
use rls_span as span;
use rls_vfs::{FileContents, Vfs};
use serde_json::{self, json};
use url::Url;
use walkdir::WalkDir;

use crate::actions::format::Rustfmt;
use crate::actions::post_build::{AnalysisQueue, BuildResults, PostBuildHandler};
use crate::actions::progress::{BuildDiagnosticsNotifier, BuildProgressNotifier};
use crate::build::*;
use crate::concurrency::{ConcurrentJob, Jobs};
use crate::lsp_data;
use crate::lsp_data::*;
use crate::project_model::{ProjectModel, RacerFallbackModel, RacerProjectModel};
use crate::server::Output;

use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

// TODO: Support non-`file` URI schemes in VFS. We're currently ignoring them because
// we don't want to crash the RLS in case a client opens a file under different URI scheme
// like with git:/ or perforce:/ (Probably even http:/? We currently don't support remote schemes).
macro_rules! ignore_non_file_uri {
    ($expr: expr, $uri: expr, $log_name: expr) => {
        $expr.map_err(|_| {
            trace!("{}: Non-`file` URI scheme, ignoring: {:?}", $log_name, $uri);
            ()
        })
    };
}

macro_rules! parse_file_path {
    ($uri: expr, $log_name: expr) => {
        ignore_non_file_uri!(parse_file_path($uri), $uri, $log_name)
    };
}

pub mod diagnostics;
pub mod format;
pub mod hover;
pub mod notifications;
pub mod post_build;
pub mod progress;
pub mod requests;
pub mod run;
pub mod work_pool;

/// Persistent context shared across all requests and notifications.
pub enum ActionContext {
    /// Context after server initialization.
    Init(InitActionContext),
    /// Context before initialization.
    Uninit(UninitActionContext),
}

impl ActionContext {
    /// Construct a new, uninitialized context.
    pub fn new(
        analysis: Arc<AnalysisHost>,
        vfs: Arc<Vfs>,
        config: Arc<Mutex<Config>>,
    ) -> ActionContext {
        ActionContext::Uninit(UninitActionContext::new(analysis, vfs, config))
    }

    /// Initialize this context, returns `Err(())` if it has already been initialized.
    pub fn init<O: Output>(
        &mut self,
        current_project: PathBuf,
        init_options: InitializationOptions,
        client_capabilities: lsp_data::ClientCapabilities,
        out: &O,
    ) -> Result<(), ()> {
        let ctx = match *self {
            ActionContext::Uninit(ref uninit) => {
                let ctx = InitActionContext::new(
                    Arc::clone(&uninit.analysis),
                    Arc::clone(&uninit.vfs),
                    Arc::clone(&uninit.config),
                    client_capabilities,
                    current_project,
                    uninit.pid,
                    init_options.cmd_run,
                );
                ctx.init(init_options, out);
                ctx
            }
            ActionContext::Init(_) => return Err(()),
        };
        *self = ActionContext::Init(ctx);
        Ok(())
    }

    /// Returns an initialiased wrapped context, or `Err(())` if not initialised.
    pub fn inited(&self) -> Result<InitActionContext, ()> {
        match *self {
            ActionContext::Uninit(_) => Err(()),
            ActionContext::Init(ref ctx) => Ok(ctx.clone()),
        }
    }

    pub fn pid(&self) -> u32 {
        match self {
            ActionContext::Uninit(ctx) => ctx.pid,
            ActionContext::Init(ctx) => ctx.pid,
        }
    }
}

/// Persistent context shared across all requests and actions after the RLS has
/// been initialized.
#[derive(Clone)]
pub struct InitActionContext {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    // Queues analysis jobs so that we don't over-use the CPU.
    analysis_queue: Arc<AnalysisQueue>,

    current_project: PathBuf,
    project_model: Arc<Mutex<Option<Arc<ProjectModel>>>>,

    previous_build_results: Arc<Mutex<BuildResults>>,
    build_queue: BuildQueue,
    file_to_crates: Arc<Mutex<HashMap<PathBuf, HashSet<Crate>>>>,
    // Keep a record of builds/post-build tasks currently in flight so that
    // mutating actions can block until the data is ready.
    active_build_count: Arc<AtomicUsize>,
    // Whether we've shown an error message from Cargo since the last successful
    // build.
    shown_cargo_error: Arc<AtomicBool>,
    // Set to true when a potentially mutating request is received. Set to false
    // if a change arrives. We can thus tell if the RLS has been quiescent while
    // waiting to mutate the client state.
    pub quiescent: Arc<AtomicBool>,

    prev_changes: Arc<Mutex<HashMap<PathBuf, u64>>>,

    config: Arc<Mutex<Config>>,
    jobs: Arc<Mutex<Jobs>>,
    client_capabilities: Arc<lsp_data::ClientCapabilities>,
    client_supports_cmd_run: bool,
    /// Set/confirmed true once a `workspace/didChangeWatchedFile` is processed
    /// Used to avoid other notifications like didSave causing double cargo builds
    client_use_change_watched: bool,
    /// Whether the server is performing cleanup (after having received
    /// 'shutdown' request), just before final 'exit' request.
    pub shut_down: Arc<AtomicBool>,
    pub pid: u32,
}

/// Persistent context shared across all requests and actions before the RLS has
/// been initialized.
pub struct UninitActionContext {
    analysis: Arc<AnalysisHost>,
    vfs: Arc<Vfs>,
    config: Arc<Mutex<Config>>,
    pid: u32,
}

impl UninitActionContext {
    fn new(
        analysis: Arc<AnalysisHost>,
        vfs: Arc<Vfs>,
        config: Arc<Mutex<Config>>,
    ) -> UninitActionContext {
        UninitActionContext { analysis, vfs, config, pid: ::std::process::id() }
    }
}

impl InitActionContext {
    fn new(
        analysis: Arc<AnalysisHost>,
        vfs: Arc<Vfs>,
        config: Arc<Mutex<Config>>,
        client_capabilities: lsp_data::ClientCapabilities,
        current_project: PathBuf,
        pid: u32,
        client_supports_cmd_run: bool,
    ) -> InitActionContext {
        let build_queue = BuildQueue::new(Arc::clone(&vfs), Arc::clone(&config));
        let analysis_queue = Arc::new(AnalysisQueue::init());
        InitActionContext {
            analysis,
            analysis_queue,
            vfs,
            config,
            jobs: Arc::default(),
            current_project,
            project_model: Arc::default(),
            previous_build_results: Arc::default(),
            build_queue,
            file_to_crates: Arc::default(),
            active_build_count: Arc::new(AtomicUsize::new(0)),
            shown_cargo_error: Arc::new(AtomicBool::new(false)),
            quiescent: Arc::new(AtomicBool::new(false)),
            prev_changes: Arc::default(),
            client_capabilities: Arc::new(client_capabilities),
            client_supports_cmd_run,
            client_use_change_watched: false,
            shut_down: Arc::new(AtomicBool::new(false)),
            pid,
        }
    }

    pub fn invalidate_project_model(&self) {
        *self.project_model.lock().unwrap() = None;
    }

    pub fn project_model(&self) -> Result<Arc<ProjectModel>, anyhow::Error> {
        let cached: Option<Arc<ProjectModel>> = self.project_model.lock().unwrap().clone();
        match cached {
            Some(pm) => Ok(pm),
            None => {
                info!("loading cargo project model");
                let pm = ProjectModel::load(&self.current_project.join("Cargo.toml"), &self.vfs)?;
                let pm = Arc::new(pm);
                *self.project_model.lock().unwrap() = Some(Arc::clone(&pm));
                Ok(pm)
            }
        }
    }

    pub fn racer_cache(&self) -> racer::FileCache {
        struct RacerVfs(Arc<Vfs>);
        impl racer::FileLoader for RacerVfs {
            fn load_file(&self, path: &Path) -> io::Result<String> {
                match self.0.load_file(path) {
                    Ok(FileContents::Text(t)) => Ok(t),
                    Ok(FileContents::Binary(_)) => {
                        Err(io::Error::new(io::ErrorKind::Other, rls_vfs::Error::BadFileKind))
                    }
                    Err(err) => Err(io::Error::new(io::ErrorKind::Other, err)),
                }
            }
        }
        racer::FileCache::new(RacerVfs(Arc::clone(&self.vfs)))
    }

    pub fn racer_session<'c>(&self, cache: &'c racer::FileCache) -> racer::Session<'c> {
        let pm: Box<dyn racer::ProjectModelProvider> = match self.project_model() {
            Ok(pm) => Box::new(RacerProjectModel(pm)),
            Err(e) => {
                error!("failed to fetch project model, using fallback: {}", e);
                Box::new(RacerFallbackModel)
            }
        };
        racer::Session::with_project_model(cache, pm)
    }

    /// Depending on user configuration, we might use either external Rustfmt or
    /// the one we're shipping with.
    /// Locks config to read `rustfmt_path` key.
    fn formatter(&self) -> Rustfmt {
        let rustfmt = self
            .config
            .lock()
            .unwrap()
            .rustfmt_path
            .clone()
            .map(|path| (path, self.current_project.clone()));

        Rustfmt::from(rustfmt)
    }

    fn fmt_config(&self) -> FmtConfig {
        FmtConfig::from(&self.current_project)
    }

    fn file_edition(&self, file: PathBuf) -> Option<Edition> {
        let files_to_crates = self.file_to_crates.lock().unwrap();

        let editions: HashSet<_> = files_to_crates
            .get(&file)
            .map(|crates| crates.iter().map(|c| c.edition).collect())
            .unwrap_or_default();

        let mut iter = editions.into_iter();
        match (iter.next(), iter.next()) {
            (ret @ Some(_), None) => ret,
            (Some(_), Some(_)) => None,
            _ => {
                // fall back on checking the root manifest for package edition
                let manifest_path =
                    cargo::util::important_paths::find_root_manifest_for_wd(&file).ok()?;
                edition_from_manifest(manifest_path)
            }
        }
    }

    fn init<O: Output>(&self, init_options: InitializationOptions, out: &O) {
        let current_project = self.current_project.clone();

        let needs_inference = {
            let mut config = self.config.lock().unwrap();

            if let Some(init_config) = init_options.settings.map(|s| s.rust) {
                config.update(init_config);
            }
            config.needs_inference()
        };

        if needs_inference {
            let config = Arc::clone(&self.config);
            // Spawn another thread since we're shelling out to Cargo and this can
            // cause a non-trivial amount of time due to disk access
            thread::spawn(move || {
                let mut config = config.lock().unwrap();
                if let Err(e) = config.infer_defaults(&current_project) {
                    debug!("Encountered an error while trying to infer config defaults: {:?}", e);
                }
            });
        }

        if !init_options.omit_init_build {
            self.build_current_project(BuildPriority::Cargo, out);
        }
    }

    fn build<O: Output>(&self, project_path: &Path, priority: BuildPriority, out: &O) {
        let (job, token) = ConcurrentJob::new();
        self.add_job(job);

        let pbh = {
            let config = self.config.lock().unwrap();
            PostBuildHandler {
                analysis: Arc::clone(&self.analysis),
                analysis_queue: Arc::clone(&self.analysis_queue),
                previous_build_results: Arc::clone(&self.previous_build_results),
                file_to_crates: Arc::clone(&self.file_to_crates),
                project_path: project_path.to_owned(),
                show_warnings: config.show_warnings,
                related_information_support: self.client_capabilities.related_information_support,
                shown_cargo_error: Arc::clone(&self.shown_cargo_error),
                active_build_count: Arc::clone(&self.active_build_count),
                crate_blacklist: config.crate_blacklist.as_ref().clone(),
                notifier: Box::new(BuildDiagnosticsNotifier::new(out.clone())),
                blocked_threads: vec![],
                _token: token,
            }
        };

        let notifier = Box::new(BuildProgressNotifier::new(out.clone()));

        self.active_build_count.fetch_add(1, Ordering::SeqCst);
        self.build_queue.request_build(project_path, priority, notifier, pbh);
    }

    fn build_current_project<O: Output>(&self, priority: BuildPriority, out: &O) {
        self.build(&self.current_project, priority, out);
    }

    pub fn add_job(&self, job: ConcurrentJob) {
        self.jobs.lock().unwrap().add(job);
    }

    pub fn wait_for_concurrent_jobs(&self) {
        self.jobs.lock().unwrap().wait_for_all();
    }

    /// Block until any builds and analysis tasks are complete.
    pub fn block_on_build(&self) {
        self.build_queue.block_on_build();
    }

    /// Returns `true` if there are no builds pending or in progress.
    fn build_ready(&self) -> bool {
        self.build_queue.build_ready()
    }

    /// Returns `true` if there are no builds or post-build (analysis) tasks pending
    /// or in progress.
    fn analysis_ready(&self) -> bool {
        self.active_build_count.load(Ordering::SeqCst) == 0
    }

    /// See docs on VersionOrdering
    fn check_change_version(&self, file_path: &Path, version_num: u64) -> VersionOrdering {
        let file_path = file_path.to_owned();
        let mut prev_changes = self.prev_changes.lock().unwrap();

        if prev_changes.contains_key(&file_path) {
            let prev_version = prev_changes[&file_path];
            if version_num <= prev_version {
                debug!(
                    "Out of order or duplicate change {:?}, prev: {}, current: {}",
                    file_path, prev_version, version_num,
                );

                if version_num == prev_version {
                    return VersionOrdering::Duplicate;
                } else {
                    return VersionOrdering::OutOfOrder;
                }
            }
        }

        prev_changes.insert(file_path, version_num);
        VersionOrdering::Ok
    }

    fn reset_change_version(&self, file_path: &Path) {
        let file_path = file_path.to_owned();
        let mut prev_changes = self.prev_changes.lock().unwrap();
        prev_changes.remove(&file_path);
    }

    fn convert_pos_to_span(&self, file_path: PathBuf, pos: Position) -> Span {
        trace!("convert_pos_to_span: {:?} {:?}", file_path, pos);

        let pos = ls_util::position_to_rls(pos);
        let line = self.vfs.load_line(&file_path, pos.row).unwrap();
        trace!("line: `{}`", line);

        let (start, end) = find_word_at_pos(&line, pos.col);
        trace!("start: {}, end: {}", start.0, end.0);

        Span::from_positions(
            span::Position::new(pos.row, start),
            span::Position::new(pos.row, end),
            file_path,
        )
    }
}

/// Read package edition from the Cargo manifest
fn edition_from_manifest<P: AsRef<Path>>(manifest_path: P) -> Option<Edition> {
    #[derive(Debug, serde::Deserialize)]
    struct Manifest {
        package: Package,
    }
    #[derive(Debug, serde::Deserialize)]
    struct Package {
        edition: Option<String>,
    }

    let manifest: Manifest = toml::from_str(&std::fs::read_to_string(manifest_path).ok()?).ok()?;
    match manifest.package.edition {
        Some(edition) => Edition::try_from(edition.as_str()).ok(),
        None => Some(Edition::default()),
    }
}

/// Some notifications come with sequence numbers, we check that these are in
/// order. However, clients might be buggy about sequence numbers so we do cope
/// with them being wrong.
///
/// This enum defines the state of sequence numbers.
#[derive(Eq, PartialEq, Debug, Clone, Copy)]
pub enum VersionOrdering {
    /// Sequence number is in order (note that we don't currently check that
    /// sequence numbers are sequential, but we probably should).
    Ok,
    /// This indicates the client sent us multiple copies of the same notification
    /// and some should be ignored.
    Duplicate,
    /// Just plain wrong sequence number. No obvious way for us to recover.
    OutOfOrder,
}

/// Represents a text cursor between characters, pointing at the next character
/// in the buffer.
type Column = span::Column<span::ZeroIndexed>;

/// Returns a text cursor range for a found word inside `line` at which `pos`
/// text cursor points to. Resulting type represents a (`start`, `end`) range
/// between `start` and `end` cursors.
/// For example (4, 4) means an empty selection starting after first 4 characters.
fn find_word_at_pos(line: &str, pos: Column) -> (Column, Column) {
    let col = pos.0 as usize;
    let is_ident_char = |c: char| c.is_alphanumeric() || c == '_';

    let start = line
        .chars()
        .enumerate()
        .take(col)
        .filter(|&(_, c)| !is_ident_char(c))
        .last()
        .map(|(i, _)| i + 1)
        .unwrap_or(0) as u32;

    #[allow(clippy::filter_next)]
    let end = line
        .chars()
        .enumerate()
        .skip(col)
        .filter(|&(_, c)| !is_ident_char(c))
        .next()
        .map(|(i, _)| i)
        .unwrap_or(col) as u32;

    (span::Column::new_zero_indexed(start), span::Column::new_zero_indexed(end))
}

/// Client file-watching request / filtering logic
/// We want to watch workspace 'Cargo.toml', root 'Cargo.lock' & the root 'target' dir
pub struct FileWatch {
    project_path: PathBuf,
    project_uri: String,
}

impl FileWatch {
    /// Construct a new `FileWatch`.
    pub fn new(ctx: &InitActionContext) -> Self {
        Self::from_project_root(ctx.current_project.clone())
    }

    pub fn from_project_root(root: PathBuf) -> Self {
        Self { project_uri: Url::from_file_path(&root).unwrap().into(), project_path: root }
    }

    /// Returns json config for desired file watches
    pub fn watchers_config(&self) -> serde_json::Value {
        fn watcher(pat: String) -> FileSystemWatcher {
            FileSystemWatcher { glob_pattern: pat, kind: None }
        }
        fn watcher_with_kind(pat: String, kind: WatchKind) -> FileSystemWatcher {
            FileSystemWatcher { glob_pattern: pat, kind: Some(kind) }
        }

        let project_str = self.project_path.to_str().unwrap();

        let mut watchers = vec![
            watcher(format!("{}/Cargo.lock", project_str)),
            // For target, we only watch if it gets deleted.
            watcher_with_kind(format!("{}/target", project_str), WatchKind::Delete),
        ];

        // Find any Cargo.tomls in the project
        for entry in WalkDir::new(project_str)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_name() == "Cargo.toml")
        {
            watchers.push(watcher(entry.path().display().to_string()));
        }

        json!({ "watchers": watchers })
    }

    /// Returns if a file change is relevant to the files we actually wanted to watch
    // Implementation note: This is expected to be called a large number of times in a loop
    // so should be fast / avoid allocation.
    #[inline]
    fn relevant_change_kind(&self, change_uri: &Url, kind: FileChangeType) -> bool {
        let path = change_uri.as_str();

        // Prefix-matching file URLs on Windows require special attention -
        // - either file:c/... and file:///c:/ works
        // - drive letters are case-insensitive
        // - also protects against naive scheme-independent parsing
        //   (https://github.com/Microsoft/vscode-languageserver-node/issues/105)
        if cfg!(windows) {
            let changed_path = match change_uri.to_file_path() {
                Ok(path) => path,
                Err(_) => return false,
            };
            if !changed_path.starts_with(&self.project_path) {
                return false;
            }
        } else if !path.starts_with(&self.project_uri) {
            return false;
        }

        if path.ends_with("/Cargo.toml") {
            return true;
        }

        let local = &path[self.project_uri.len()..];
        local == "/Cargo.lock" || (local == "/target" && kind == FileChangeType::Deleted)
    }

    #[inline]
    pub fn is_relevant(&self, change: &FileEvent) -> bool {
        self.relevant_change_kind(&change.uri, change.typ)
    }

    #[inline]
    pub fn is_relevant_save_doc(&self, did_save: &DidSaveTextDocumentParams) -> bool {
        self.relevant_change_kind(&did_save.text_document.uri, FileChangeType::Changed)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_find_word_at_pos() {
        fn assert_range(test_str: &'static str, range: (u32, u32)) {
            assert!(test_str.chars().filter(|c| *c == '|').count() == 1);
            let col = test_str.find('|').unwrap() as u32;
            let line = test_str.replace('|', "");
            let (start, end) = find_word_at_pos(&line, Column::new_zero_indexed(col));
            let actual = (start.0, end.0);
            assert_eq!(range, actual, "Assertion failed for {:?}", test_str);
        }

        assert_range("|struct Def {", (0, 6));
        assert_range("stru|ct Def {", (0, 6));
        assert_range("struct| Def {", (0, 6));

        assert_range("struct |Def {", (7, 10));
        assert_range("struct De|f {", (7, 10));
        assert_range("struct Def| {", (7, 10));

        assert_range("struct Def |{", (11, 11));

        assert_range("|span::Position<T>", (0, 4));
        assert_range(" |span::Position<T>", (1, 5));
        assert_range("sp|an::Position<T>", (0, 4));
        assert_range("span|::Position<T>", (0, 4));
        assert_range("span::|Position<T>", (6, 14));
        assert_range("span::Position|<T>", (6, 14));
        assert_range("span::Position<|T>", (15, 16));
        assert_range("span::Position<T|>", (15, 16));
    }

    fn change(url: &str) -> FileEvent {
        FileEvent::new(Url::parse(url).unwrap(), FileChangeType::Changed)
    }

    fn did_save(url: &str) -> DidSaveTextDocumentParams {
        DidSaveTextDocumentParams {
            text_document: TextDocumentIdentifier::new(Url::parse(url).unwrap()),
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn file_watch_relevant_files() {
        let watch = FileWatch::from_project_root("/some/dir".into());

        assert!(watch.is_relevant(&change("file://localhost/some/dir/Cargo.toml")));
        assert!(watch.is_relevant(&change("file:///some/dir/Cargo.toml")));

        assert!(watch.is_relevant(&change("file:///some/dir/Cargo.lock")));
        assert!(watch.is_relevant(&change("file:///some/dir/inner/Cargo.toml")));

        assert!(!watch.is_relevant(&change("file:///some/dir/inner/Cargo.lock")));
        assert!(!watch.is_relevant(&change("file:///Cargo.toml")));
    }

    #[cfg(not(windows))]
    #[test]
    fn did_save_relevant_files() {
        let watch = FileWatch::from_project_root("/some/dir".into());

        assert!(watch.is_relevant_save_doc(&did_save("file:///some/dir/Cargo.lock")));
        assert!(watch.is_relevant_save_doc(&did_save("file:///some/dir/inner/Cargo.toml")));
        assert!(!watch.is_relevant_save_doc(&did_save("file:///some/dir/inner/Cargo.lock")));
        assert!(!watch.is_relevant_save_doc(&did_save("file:///Cargo.toml")));
    }

    #[cfg(windows)]
    #[test]
    fn file_watch_relevant_files() {
        let watch = FileWatch::from_project_root("C:/some/dir".into());

        assert!(watch.is_relevant(&change("file:c:/some/dir/Cargo.toml")));
        assert!(watch.is_relevant(&change("file:///c:/some/dir/Cargo.toml")));
        assert!(watch.is_relevant(&change("file:///C:/some/dir/Cargo.toml")));
        assert!(watch.is_relevant(&change("file:///c%3A/some/dir/Cargo.toml")));

        assert!(watch.is_relevant(&change("file:///c:/some/dir/Cargo.lock")));
        assert!(watch.is_relevant(&change("file:///c:/some/dir/inner/Cargo.toml")));

        assert!(!watch.is_relevant(&change("file:///c:/some/dir/inner/Cargo.lock")));
        assert!(!watch.is_relevant(&change("file:///c:/Cargo.toml")));
    }

    #[cfg(windows)]
    #[test]
    fn did_save_relevant_files() {
        let watch = FileWatch::from_project_root("C:/some/dir".into());

        assert!(watch.is_relevant_save_doc(&did_save("file:///c:/some/dir/Cargo.lock")));
        assert!(watch.is_relevant_save_doc(&did_save("file:///c:/some/dir/inner/Cargo.toml")));
        assert!(!watch.is_relevant_save_doc(&did_save("file:///c:/some/dir/inner/Cargo.lock")));
        assert!(!watch.is_relevant_save_doc(&did_save("file:///c:/Cargo.toml")));
    }

    #[test]
    fn explicit_edition_from_manifest() -> Result<(), std::io::Error> {
        use std::{fs::File, io::Write};

        let dir = tempfile::tempdir()?;

        let manifest_path = {
            let path = dir.path().join("Cargo.toml");
            let mut m = File::create(&path)?;
            writeln!(
                m,
                "[package]\n\
                 name = \"foo\"\n\
                 version = \"1.0.0\"\n\
                 edition = \"2018\""
            )?;
            path
        };

        assert_eq!(edition_from_manifest(manifest_path), Some(Edition::Edition2018));

        Ok(())
    }
}
