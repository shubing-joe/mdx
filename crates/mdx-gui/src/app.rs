//! GUI shell: application state, panel layout and the background render
//! pipeline.
//!
//! Two rules shape this module:
//!
//! * **The UI thread never renders.** Parsing, asset resolution, formula and
//!   diagram conversion and the Typst run all happen on a worker thread started
//!   with [`std::thread`]; the result comes back over an `mpsc` channel and is
//!   applied on a later frame, so typing stays responsive while a large
//!   document is laid out. Exactly one worker runs at a time.
//! * **Every failure is visible.** File-system errors and failed renders land in
//!   the status bar, parser and asset diagnostics land in the diagnostics pane.
//!   Nothing on the way is unwrapped, and a missing file never panics.
//!
//! The document itself is rendered through the same three crates the CLI uses
//! ([`mdx_parse`] → [`mdx_assets`] → [`mdx_typst`]) with the same options, so
//! the preview matches what `mdx render` would write. The one deliberate
//! difference is [`asset_policy`]: a viewer is expected to fetch the remote
//! images a document refers to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use egui::{Context, Key, Modifiers, RichText, Ui};
use mdx_assets::{AssetPolicy, AssetStore};
use mdx_core::{Block, DiagCode, Diagnostic, Doc, Inline, Severity};

use crate::editor::EditorState;
use crate::{diagnostics, preview};

/// How long the buffer must stop changing before it is rendered again, in
/// seconds. Long enough to swallow a burst of keystrokes, short enough to feel
/// immediate.
const RENDER_DEBOUNCE: f64 = 0.35;

/// Default and limits of the editor pane, in points.
const SOURCE_WIDTH: f32 = 560.0;
const SOURCE_MIN_WIDTH: f32 = 280.0;
const SOURCE_MAX_WIDTH: f32 = 1200.0;

/// Default and limits of the diagnostics pane, in points.
const DIAGNOSTICS_HEIGHT: f32 = 180.0;
const DIAGNOSTICS_MIN_HEIGHT: f32 = 60.0;
const DIAGNOSTICS_MAX_HEIGHT: f32 = 600.0;

/// Width of the two path fields in the toolbar, in points.
const PATH_FIELD_WIDTH: f32 = 320.0;

/// Which view the right-hand pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    /// Rendered pages, drawn by [`preview::show`].
    Preview,
    /// The `mdx-core` document model as JSON — `mdx ast`.
    Ast,
    /// Resolved asset manifest as JSON — `mdx assets`.
    Assets,
}

/// One unit of background work.
#[derive(Debug, Clone)]
enum Job {
    /// Parse, resolve and render the SVG preview of the buffer hashed as `key`.
    Preview { key: u64 },
    /// Render the buffer as a PDF and write it to `output`.
    Pdf { output: PathBuf },
}

impl Job {
    /// What the status bar says while the job runs.
    fn label(&self) -> &'static str {
        match self {
            Self::Preview { .. } => "Rendering preview…",
            Self::Pdf { .. } => "Writing PDF…",
        }
    }
}

/// What a worker sends back.
enum JobMessage {
    Preview(Box<PreviewOutput>),
    Pdf { result: Result<String, String> },
}

/// The preview job's result: the document model, its assets and the pages.
struct PreviewOutput {
    /// Hash of the buffer this run was started for.
    key: u64,
    doc: Doc,
    assets: AssetStore,
    pages: Result<Vec<String>, Diagnostic>,
    diagnostics: Vec<Diagnostic>,
    /// `mdx ast` view, serialised on the worker thread so the frame that
    /// adopts the result never has to walk a large document.
    ast_json: String,
    /// `mdx assets` view, serialised on the worker thread.
    assets_json: String,
}

/// Everything one render needs, built on the worker thread.
struct Pipeline {
    doc: Doc,
    assets: AssetStore,
    /// LaTeX → Typst expressions, keyed by the LaTeX as written.
    math: BTreeMap<String, String>,
    /// Mermaid source → SVG, keyed by the source as written.
    diagrams: BTreeMap<String, String>,
    diagnostics: Vec<Diagnostic>,
}

/// Status line text, plus whether it describes a failure.
#[derive(Debug, Clone)]
struct Status {
    text: String,
    error: bool,
}

impl Status {
    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }
}

/// The whole application: the buffer, the last render, and the view state.
pub struct MdxApp {
    /// Path the buffer was last read from or written to. Asset resolution uses
    /// its directory.
    path: Option<PathBuf>,
    /// Editable path field; what Save, Reload and the PDF default read.
    path_text: String,
    /// Editable output path of the PDF export.
    pdf_path_text: String,
    /// The buffer itself.
    source: String,
    /// Whether the buffer differs from what is on disk.
    dirty: bool,
    /// `hash_source(&source, toc)`.
    ///
    /// Invariant: refreshed at every place either input changes — [`MdxApp::load`],
    /// the editor pane and the "Contents" toggle — so [`MdxApp::render_key`] can
    /// stay O(1) instead of hashing the whole buffer on every frame.
    source_hash: u64,
    /// Frame time of the last edit, used to debounce re-rendering.
    edited_at: f64,

    /// Document model of the last successful run.
    doc: Option<Doc>,
    /// Assets of the last successful run.
    assets: Option<AssetStore>,
    /// Diagnostics of the last successful run, in pipeline order.
    diagnostics: Vec<Diagnostic>,
    /// SVG page strings of the last successful run.
    pages: Vec<String>,
    /// `mdx ast` view of the last run, serialised once per run.
    ast_json: String,
    /// `mdx assets` view of the last run, serialised once per run.
    assets_json: String,
    /// One-line summary of the last run, shown in the status bar.
    summary: String,
    /// Hash of what is currently rendered; `None` until the first run lands.
    rendered_key: Option<u64>,

    /// Preview zoom, owned by `preview::show`.
    zoom: f32,
    /// Width of the editor pane, in points.
    split: f32,
    /// Active tab of the right-hand pane.
    tab: Tab,
    /// Source pane preferences and caret position.
    editor: EditorState,
    /// Whether the diagnostics pane is open.
    show_diagnostics: bool,
    /// Whether renders include a table of contents.
    toc: bool,

    /// Transient message shown in the status bar.
    status: Status,
    /// Label of the job currently running, if any.
    busy: Option<&'static str>,
    /// Receiver of the running job; exactly one job is in flight at a time.
    receiver: Option<Receiver<JobMessage>>,
    /// Job waiting for the current one to finish.
    pending: Option<Job>,
    /// Whether the "discard unsaved changes?" dialog is up.
    confirm_reload: bool,
}

impl MdxApp {
    /// Build the application, loading `path` when the caller passed one.
    pub fn new(cc: &eframe::CreationContext<'_>, path: Option<PathBuf>) -> Self {
        let mut app = Self {
            path: None,
            path_text: String::new(),
            pdf_path_text: String::new(),
            source: String::new(),
            dirty: false,
            source_hash: hash_source("", false),
            // An app that has never been edited should render at once.
            edited_at: f64::NEG_INFINITY,

            doc: None,
            assets: None,
            diagnostics: Vec::new(),
            pages: Vec::new(),
            ast_json: String::new(),
            assets_json: String::new(),
            summary: String::from("No render yet"),
            rendered_key: None,

            zoom: preview::DEFAULT_ZOOM,
            split: SOURCE_WIDTH,
            tab: Tab::Preview,
            editor: EditorState::default(),
            show_diagnostics: true,
            toc: false,

            status: Status::info("Ready — pass a Markdown file or type a path"),
            busy: None,
            receiver: None,
            pending: None,
            confirm_reload: false,
        };

        let ctx = cc.egui_ctx.clone();
        if let Some(path) = path {
            app.path_text = path.display().to_string();
            app.pdf_path_text = default_pdf_path(&path).display().to_string();
            app.load(&ctx);
        }

        app
    }

    /// Hash of what the current view state should render.
    ///
    /// Reads the cached [`MdxApp::source_hash`]; see its invariant.
    fn render_key(&self) -> u64 {
        self.source_hash
    }

    /// Path the toolbar's field names, or `None` when it is empty and nothing
    /// was loaded.
    fn target_path(&self) -> Option<PathBuf> {
        let typed = self.path_text.trim();
        if typed.is_empty() {
            self.path.clone()
        } else {
            Some(PathBuf::from(typed))
        }
    }

    /// Apply Ctrl+S / Ctrl+R / Ctrl+E before any widget can consume the keys.
    fn handle_shortcuts(&mut self, ctx: &Context) {
        let save = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::S));
        let reload = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::R));
        let export = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::E));

        if save {
            self.save();
        }
        if reload {
            self.request_reload(ctx);
        }
        if export {
            self.render_pdf(ctx);
        }
    }

    /// Read the toolbar's path into the buffer.
    ///
    /// Nothing is cleared when the file cannot be read: the buffer, the loaded
    /// path and the last render all stay as they were, and the reason appears in
    /// the status bar.
    fn load(&mut self, ctx: &Context) {
        let Some(path) = self.target_path() else {
            self.status = Status::error("No file path — type one in the toolbar");
            return;
        };

        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.path = Some(path.clone());
                self.path_text = path.display().to_string();
                self.pdf_path_text = default_pdf_path(&path).display().to_string();
                self.source = text;
                self.source_hash = hash_source(&self.source, self.toc);
                self.dirty = false;
                self.rendered_key = None;
                self.status = Status::info(format!("Loaded {}", path.display()));
                ctx.request_repaint();
            }
            Err(error) => {
                // Keep the path fields in step with what was attempted, but do
                // not claim the file was loaded.
                self.path_text = path.display().to_string();
                if self.pdf_path_text.trim().is_empty() {
                    self.pdf_path_text = default_pdf_path(&path).display().to_string();
                }
                self.status = Status::error(format!("Cannot read {}: {error}", path.display()));
            }
        }
    }

    /// Reload, asking first when that would discard unsaved edits.
    fn request_reload(&mut self, ctx: &Context) {
        if self.dirty {
            self.confirm_reload = true;
            ctx.request_repaint();
        } else {
            self.load(ctx);
        }
    }

    /// Write the buffer to the toolbar's path.
    fn save(&mut self) {
        let Some(path) = self.target_path() else {
            self.status = Status::error("No file path — type one in the toolbar");
            return;
        };
        let moved = self.path.as_deref() != Some(path.as_path());

        match std::fs::write(&path, self.source.as_bytes()) {
            Ok(()) => {
                self.path = Some(path.clone());
                self.path_text = path.display().to_string();
                if self.pdf_path_text.trim().is_empty() {
                    self.pdf_path_text = default_pdf_path(&path).display().to_string();
                }
                if moved {
                    // Relative assets now resolve against a new directory.
                    self.rendered_key = None;
                }
                self.dirty = false;
                self.status = Status::info(format!(
                    "Saved {} ({})",
                    path.display(),
                    human_bytes(self.source.len())
                ));
            }
            Err(error) => {
                self.status = Status::error(format!("Cannot write {}: {error}", path.display()));
            }
        }
    }

    /// Queue a PDF render of the current buffer.
    fn render_pdf(&mut self, ctx: &Context) {
        let Some(path) = self.target_path() else {
            self.status = Status::error("No file path — type one in the toolbar");
            return;
        };

        let typed = self.pdf_path_text.trim();
        let output = if typed.is_empty() {
            default_pdf_path(&path)
        } else {
            PathBuf::from(typed)
        };
        self.pdf_path_text = output.display().to_string();
        self.pending = Some(Job::Pdf { output });
        ctx.request_repaint();
    }

    /// Start the queued job, or re-render an edited buffer once it settles.
    fn schedule(&mut self, ctx: &Context) {
        if self.receiver.is_none()
            && let Some(job) = self.pending.take()
        {
            self.start(ctx, job);
        }
        if self.receiver.is_some() || self.pending.is_some() {
            return;
        }

        // Nothing to render while the shell is still pristine.
        if self.path.is_none() && self.source.trim().is_empty() {
            return;
        }

        let key = self.render_key();
        if self.rendered_key == Some(key) {
            return;
        }

        let now = ctx.input(|input| input.time);
        let quiet_for = now - self.edited_at;
        if quiet_for >= RENDER_DEBOUNCE {
            self.start(ctx, Job::Preview { key });
        } else {
            // Wake up again exactly when the debounce expires.
            ctx.request_repaint_after(Duration::from_secs_f64(RENDER_DEBOUNCE - quiet_for));
        }
    }

    /// Hand `job` to a worker thread and remember its channel.
    fn start(&mut self, ctx: &Context, job: Job) {
        let (sender, receiver) = mpsc::channel();
        // The worker owns everything it touches: it must not read `self`.
        let source = self.source.clone();
        let directory = document_directory(self.path.as_deref());
        let toc = self.toc;
        let context = ctx.clone();
        let label = job.label();

        let worker = std::thread::Builder::new()
            .name("mdx-render".to_owned())
            .spawn(move || {
                let message = match job {
                    Job::Preview { key } => {
                        JobMessage::Preview(Box::new(preview_output(&source, &directory, toc, key)))
                    }
                    Job::Pdf { output } => JobMessage::Pdf {
                        result: pdf_output(&source, &directory, toc, &output),
                    },
                };
                // A closed channel only means the shell moved on.
                let _ = sender.send(message);
                context.request_repaint();
            });

        match worker {
            Ok(_handle) => {
                self.receiver = Some(receiver);
                self.busy = Some(label);
            }
            Err(error) => {
                self.status = Status::error(format!("Cannot start the render thread: {error}"));
            }
        }
    }

    /// Apply whatever a finished worker sent, without ever blocking.
    fn pump(&mut self) {
        enum Poll {
            Idle,
            Message(Box<JobMessage>),
            Gone,
        }

        let poll = match self.receiver.as_ref() {
            None => Poll::Idle,
            Some(receiver) => match receiver.try_recv() {
                Ok(message) => Poll::Message(Box::new(message)),
                Err(TryRecvError::Empty) => Poll::Idle,
                Err(TryRecvError::Disconnected) => Poll::Gone,
            },
        };

        match poll {
            Poll::Idle => {}
            Poll::Message(message) => {
                self.receiver = None;
                self.busy = None;
                match *message {
                    JobMessage::Preview(output) => self.apply_preview(*output),
                    JobMessage::Pdf { result } => self.apply_pdf(result),
                }
            }
            Poll::Gone => {
                self.receiver = None;
                self.busy = None;
                self.status = Status::error("The render worker stopped unexpectedly");
            }
        }
    }

    /// Adopt a finished preview render.
    fn apply_preview(&mut self, output: PreviewOutput) {
        let PreviewOutput {
            key,
            doc,
            assets,
            pages,
            mut diagnostics,
            ast_json,
            assets_json,
        } = output;

        self.pages = match pages {
            Ok(pages) => pages,
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                Vec::new()
            }
        };

        let (errors, warnings, _) = count_severities(&diagnostics);
        self.summary = match (errors, warnings) {
            (0, 0) => format!("{} page(s)", self.pages.len()),
            (0, warnings) => format!("{} page(s), {warnings} warning(s)", self.pages.len()),
            (errors, _) => format!("{} page(s), {errors} error(s)", self.pages.len()),
        };

        self.ast_json = ast_json;
        self.assets_json = assets_json;
        self.doc = Some(doc);
        self.assets = Some(assets);
        self.diagnostics = diagnostics;
        self.rendered_key = Some(key);
    }

    /// Report the outcome of a PDF render.
    fn apply_pdf(&mut self, result: Result<String, String>) {
        self.status = match result {
            Ok(message) => Status::info(message),
            Err(message) => Status::error(message),
        };
    }

    /// Toolbar: paths, the three actions and the view toggles.
    fn toolbar(&mut self, ui: &mut Ui, ctx: &Context) {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("mdx").strong());
            ui.separator();

            ui.label("File");
            ui.add(egui::TextEdit::singleline(&mut self.path_text).desired_width(PATH_FIELD_WIDTH));
            if ui
                .button("Reload")
                .on_hover_text("Read the file again (Ctrl+R)")
                .clicked()
            {
                self.request_reload(ctx);
            }
            if ui
                .add_enabled(self.dirty, egui::Button::new("Save"))
                .on_hover_text("Write the buffer to this path (Ctrl+S)")
                .clicked()
            {
                self.save();
            }

            ui.separator();
            ui.label("PDF");
            ui.add(
                egui::TextEdit::singleline(&mut self.pdf_path_text).desired_width(PATH_FIELD_WIDTH),
            );
            if ui
                .button("Render PDF")
                .on_hover_text("Render the document and write it to this path (Ctrl+E)")
                .clicked()
            {
                self.render_pdf(ctx);
            }

            ui.separator();
            ui.checkbox(&mut self.editor.show_line_numbers, "Line numbers");
            ui.checkbox(&mut self.editor.highlight_current_line, "Current line");
            if ui.checkbox(&mut self.toc, "Contents").changed() {
                // `toc` is part of the render key: refresh it here, its only
                // mutation site.
                self.source_hash = hash_source(&self.source, self.toc);
            }
            ui.checkbox(&mut self.show_diagnostics, "Diagnostics");

            ui.separator();
            ui.selectable_value(&mut self.tab, Tab::Preview, "Preview");
            ui.selectable_value(&mut self.tab, Tab::Ast, "AST");
            ui.selectable_value(&mut self.tab, Tab::Assets, "Assets");
        });
        ui.add_space(2.0);
    }

    /// Status bar: the last message, the running job and the view metrics.
    fn status_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let (colour, marker) = if self.status.error {
                (ui.visuals().error_fg_color, "!")
            } else {
                (ui.visuals().weak_text_color(), "·")
            };
            ui.colored_label(colour, marker);
            ui.label(RichText::new(self.status.text.as_str()).color(colour));

            if let Some(busy) = self.busy {
                ui.separator();
                ui.spinner();
                ui.label(RichText::new(busy).weak());
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.dirty {
                    ui.label(RichText::new("unsaved").color(ui.visuals().warn_fg_color));
                    ui.separator();
                }
                ui.label(
                    RichText::new(format!(
                        "L{}:C{}",
                        self.editor.caret_line(),
                        self.editor.caret_column()
                    ))
                    .weak(),
                );
                ui.separator();
                ui.label(RichText::new(self.summary.as_str()).weak());
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "{} asset(s)",
                        self.assets.as_ref().map_or(0, AssetStore::len)
                    ))
                    .weak(),
                );
            });
        });
    }

    /// Diagnostics pane: the peer-owned list, in a scroll area of its own.
    fn diagnostics_pane(&mut self, ui: &mut Ui) {
        egui::ScrollArea::vertical()
            .id_salt("mdx_diagnostics_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| diagnostics::show(ui, &self.diagnostics));
    }

    /// Source pane: the editor, plus the dirty bookkeeping around it.
    fn source_pane(&mut self, ui: &mut Ui) {
        if self.editor.show(ui, &mut self.source) {
            self.dirty = true;
            self.source_hash = hash_source(&self.source, self.toc);
            self.edited_at = ui.input(|input| input.time);
        }
    }

    /// Right-hand pane: the active tab.
    fn view_pane(&mut self, ui: &mut Ui) {
        match self.tab {
            Tab::Preview => preview::show(ui, &self.pages, &mut self.zoom),
            Tab::Ast => {
                let details = match self.doc.as_ref() {
                    Some(doc) => format!(
                        "{} block(s), {} heading(s) — `mdx ast`",
                        doc.blocks.len(),
                        doc.toc.len()
                    ),
                    None => String::from("Nothing rendered yet"),
                };
                json_pane(
                    ui,
                    "Document model",
                    &details,
                    "mdx_ast_json",
                    &self.ast_json,
                );
            }
            Tab::Assets => {
                let details = match self.assets.as_ref() {
                    Some(assets) => format!("{} asset(s) — `mdx assets`", assets.len()),
                    None => String::from("Nothing rendered yet"),
                };
                json_pane(
                    ui,
                    "Asset manifest",
                    &details,
                    "mdx_assets_json",
                    &self.assets_json,
                );
            }
        }
    }

    /// The "discard unsaved changes?" dialog, if it is up.
    fn reload_dialog(&mut self, ctx: &Context) {
        if !self.confirm_reload {
            return;
        }

        let mut open = true;
        let mut discard = false;
        let mut keep_editing = false;
        let path = self.path_text.trim().to_owned();
        egui::Window::new("Reload from disk?")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!(
                    "{path} has unsaved changes; reloading discards them."
                ));
                ui.horizontal(|ui| {
                    if ui.button("Discard and reload").clicked() {
                        discard = true;
                    }
                    if ui.button("Keep editing").clicked() {
                        keep_editing = true;
                    }
                });
            });

        self.confirm_reload = open && !discard && !keep_editing;
        if discard {
            self.load(ctx);
        }
    }
}

impl eframe::App for MdxApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();

        self.handle_shortcuts(&ctx);
        self.pump();
        self.schedule(&ctx);

        egui::Panel::top("mdx_toolbar").show(ui, |ui| self.toolbar(ui, &ctx));
        egui::Panel::bottom("mdx_status").show(ui, |ui| self.status_bar(ui));
        if self.show_diagnostics {
            egui::Panel::bottom("mdx_diagnostics")
                .resizable(true)
                .default_size(DIAGNOSTICS_HEIGHT)
                .size_range(DIAGNOSTICS_MIN_HEIGHT..=DIAGNOSTICS_MAX_HEIGHT)
                .show(ui, |ui| self.diagnostics_pane(ui));
        }

        let split = self.split;
        let source = egui::Panel::left("mdx_source")
            .resizable(true)
            .default_size(split)
            .size_range(SOURCE_MIN_WIDTH..=SOURCE_MAX_WIDTH)
            .show(ui, |ui| self.source_pane(ui));
        let width = source.response.rect.width();
        // Guard against the panel handing back a separator's rectangle.
        if width.is_finite() && width >= SOURCE_MIN_WIDTH - 1.0 {
            self.split = width;
        }

        egui::CentralPanel::default().show(ui, |ui| self.view_pane(ui));

        self.reload_dialog(&ctx);
    }
}

/// One line of JSON-rendered app state, in a scroll area.
fn json_pane(ui: &mut Ui, title: &str, details: &str, salt: &'static str, json: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(title).strong());
        ui.separator();
        ui.label(RichText::new(details).weak());
        ui.separator();
        ui.label(RichText::new(human_bytes(json.len())).weak());
    });
    ui.separator();
    egui::ScrollArea::both()
        .id_salt(salt)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            let text = if json.is_empty() {
                "Nothing rendered yet."
            } else {
                json
            };
            ui.add(egui::Label::new(RichText::new(text).monospace()).selectable(true));
        });
}

/// Run the whole pipeline for `source`, on the calling (worker) thread.
fn run_pipeline(source: &str, directory: &Path) -> Pipeline {
    let (mut doc, report) = mdx_parse::parse(source, &mdx_parse::ParseOptions::default());
    let (assets, asset_report) = mdx_assets::resolve_all(&mut doc, directory, &asset_policy());

    let mut diagnostics = report.diagnostics;
    diagnostics.extend(asset_report.diagnostics);

    // Formula and diagram failures are reported the same way `mdx render`
    // reports them, so the pane explains a missing figure instead of leaving
    // the reader with a silently degraded page.
    let math = math_map(&doc, &mut diagnostics);
    let diagrams = diagram_map(&doc, &mut diagnostics);
    Pipeline {
        doc,
        assets,
        math,
        diagrams,
        diagnostics,
    }
}

/// SVG pages of `source` plus the document and assets behind them.
fn preview_output(source: &str, directory: &Path, toc: bool, key: u64) -> PreviewOutput {
    let pipeline = run_pipeline(source, directory);
    let options = render_options(&pipeline.doc, toc);
    let pages = mdx_typst::render_svg_pages(
        &pipeline.doc,
        &pipeline.assets,
        &pipeline.math,
        &pipeline.diagrams,
        &options,
    );

    // Serialising here keeps the UI thread out of the document walk.
    let ast_json =
        json_string(&serde_json::to_value(&pipeline.doc).unwrap_or(serde_json::Value::Null));
    let assets_json = json_string(&pipeline.assets.to_json());

    PreviewOutput {
        key,
        doc: pipeline.doc,
        assets: pipeline.assets,
        pages,
        diagnostics: pipeline.diagnostics,
        ast_json,
        assets_json,
    }
}

/// Write `source` as a PDF to `output`, returning the status-bar message.
fn pdf_output(source: &str, directory: &Path, toc: bool, output: &Path) -> Result<String, String> {
    let pipeline = run_pipeline(source, directory);
    let options = render_options(&pipeline.doc, toc);
    let bytes = mdx_typst::render_pdf(
        &pipeline.doc,
        &pipeline.assets,
        &pipeline.math,
        &pipeline.diagrams,
        &options,
    )
    .map_err(|diagnostic| diagnostic.to_string())?;

    std::fs::write(output, &bytes)
        .map_err(|error| format!("Cannot write {}: {error}", output.display()))?;

    let (errors, warnings, _) = count_severities(&pipeline.diagnostics);
    Ok(format!(
        "Wrote {} ({}, {errors} error(s), {warnings} warning(s))",
        output.display(),
        human_bytes(bytes.len())
    ))
}

/// Render options shared by the preview and the PDF export.
///
/// These mirror the CLI's defaults: A4 with 20 mm margins and numbered pages,
/// the bundled fonts only, no PDF standard, and the fixed date — the document
/// never sees the wall clock.
fn render_options(doc: &Doc, toc: bool) -> mdx_typst::RenderOptions {
    mdx_typst::RenderOptions {
        page: mdx_typst::PageSetup::default(),
        toc,
        pdf_standard: None,
        fonts: Vec::new(),
        ignore_system_fonts: false,
        deterministic_today: None,
        title: doc.front.title.clone(),
    }
}

/// What the GUI lets the asset resolver do: local files plus the explicitly
/// gated remote path, under the default size cap.
fn asset_policy() -> AssetPolicy {
    AssetPolicy {
        allow_remote: true,
        ..AssetPolicy::default()
    }
}

/// Directory relative paths in the document resolve against.
fn document_directory(path: Option<&Path>) -> PathBuf {
    match path.and_then(Path::parent) {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Default PDF path for a Markdown file: the same name with a `.pdf` suffix.
fn default_pdf_path(path: &Path) -> PathBuf {
    path.with_extension("pdf")
}

/// Every LaTeX formula in `doc`, converted for the Typst writer.
///
/// A formula the converter rejects is reported as an `E_MATH_CONVERT` warning —
/// exactly what `mdx render` does — and left out of the map, so the Typst
/// writer passes the source LaTeX through instead of failing the document.
fn math_map(doc: &Doc, diagnostics: &mut Vec<Diagnostic>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    collect_math(&doc.blocks, &mut map, diagnostics);
    map
}

fn collect_math(
    blocks: &[Block],
    map: &mut BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for block in blocks {
        match block {
            Block::Math { latex, .. } => insert_math(latex, map, diagnostics),
            Block::Heading { inlines, .. } | Block::Paragraph { inlines } => {
                collect_math_inlines(inlines, map, diagnostics);
            }
            Block::BlockQuote { blocks } => collect_math(blocks, map, diagnostics),
            Block::List { items, .. } => {
                for item in items {
                    collect_math(item, map, diagnostics);
                }
            }
            Block::FootnoteDef { blocks, .. } => collect_math(blocks, map, diagnostics),
            Block::Table { header, rows, .. } => {
                for cell in header {
                    collect_math_inlines(cell, map, diagnostics);
                }
                for row in rows {
                    for cell in row {
                        collect_math_inlines(cell, map, diagnostics);
                    }
                }
            }
            _ => {}
        }
    }
}

fn collect_math_inlines(
    inlines: &[Inline],
    map: &mut BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for inline in inlines {
        match inline {
            Inline::Math { latex, .. } => insert_math(latex, map, diagnostics),
            Inline::Emph { inlines }
            | Inline::Strong { inlines }
            | Inline::Strike { inlines }
            | Inline::Link { inlines, .. } => collect_math_inlines(inlines, map, diagnostics),
            _ => {}
        }
    }
}

fn insert_math(latex: &str, map: &mut BTreeMap<String, String>, diagnostics: &mut Vec<Diagnostic>) {
    if map.contains_key(latex) {
        return;
    }
    match mdx_math::latex_to_typst(latex) {
        Ok(expression) => {
            map.insert(latex.to_owned(), expression);
        }
        Err(reason) => diagnostics.push(Diagnostic::warning(
            DiagCode::MathConvert,
            format!("math `{}` could not be converted: {reason}", excerpt(latex)),
        )),
    }
}

/// Every Mermaid diagram in `doc`, rendered for the Typst world.
///
/// A diagram that cannot be rendered is an `E_DIAGRAM_RENDER` error, exactly as
/// in `mdx render`: the document must not silently lose a figure.
fn diagram_map(doc: &Doc, diagnostics: &mut Vec<Diagnostic>) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    collect_diagrams(&doc.blocks, &mut map, diagnostics);
    map
}

fn collect_diagrams(
    blocks: &[Block],
    map: &mut BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for block in blocks {
        match block {
            Block::Diagram { source } => insert_diagram(source, map, diagnostics),
            Block::BlockQuote { blocks } => collect_diagrams(blocks, map, diagnostics),
            Block::List { items, .. } => {
                for item in items {
                    collect_diagrams(item, map, diagnostics);
                }
            }
            Block::FootnoteDef { blocks, .. } => collect_diagrams(blocks, map, diagnostics),
            _ => {}
        }
    }
}

fn insert_diagram(
    source: &str,
    map: &mut BTreeMap<String, String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if map.contains_key(source) {
        return;
    }
    let options = mdx_diagram::DiagramOptions::default();
    match mdx_diagram::render_svg(source, &options) {
        Ok(svg) => {
            map.insert(source.to_owned(), svg);
        }
        Err(reason) => diagnostics.push(Diagnostic::error(
            DiagCode::DiagramRender,
            format!(
                "diagram `{}` could not be rendered: {reason}",
                excerpt(source)
            ),
        )),
    }
}

/// First non-blank line of `text`, shortened for a diagnostic message.
fn excerpt(text: &str) -> String {
    /// Longest excerpt kept, in characters.
    const LIMIT: usize = 48;

    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut excerpt = String::new();
    for (index, character) in first.trim().chars().enumerate() {
        if index == LIMIT {
            excerpt.push('…');
            break;
        }
        excerpt.push(character);
    }
    excerpt
}

/// Error, warning and info counts of `diagnostics`.
fn count_severities(diagnostics: &[Diagnostic]) -> (usize, usize, usize) {
    let mut errors = 0;
    let mut warnings = 0;
    let mut infos = 0;
    for diagnostic in diagnostics {
        match diagnostic.severity {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
            Severity::Info => infos += 1,
        }
    }
    (errors, warnings, infos)
}

/// Serialise a JSON value for the AST and asset tabs.
fn json_string(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|error| format!("<cannot serialise: {error}>"))
}

/// FNV-1a over the buffer plus the render toggles.
///
/// Stable across runs and platforms, unlike `DefaultHasher`, so comparing it
/// with the last rendered hash means exactly "nothing to do".
fn hash_source(source: &str, toc: bool) -> u64 {
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in source.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    if toc {
        hash ^= 0x9e37_79b9_7f4a_7c15;
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// Byte count rendered for humans, with a fixed format per magnitude.
fn human_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    let value = bytes as f64;
    if value < KIB {
        format!("{bytes} B")
    } else if value < KIB * KIB {
        format!("{:.1} KiB", value / KIB)
    } else {
        format!("{:.1} MiB", value / (KIB * KIB))
    }
}
