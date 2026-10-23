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
//!
//! A document gets in four ways — the command line, the path field, a file
//! dropped on the window and the recent-file list — and all four end in
//! [`MdxApp::load`], so a missing file is reported once, the same way. The
//! outline and the diagnostics pane both only ever produce a line number;
//! [`EditorState::reveal_line`] is what turns one into a caret and a scroll.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use egui::{Context, Key, Modifiers, OutputCommand, RichText, Ui};
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

/// Default and limits of the outline pane, in points.
const OUTLINE_WIDTH: f32 = 200.0;
const OUTLINE_MIN_WIDTH: f32 = 120.0;
const OUTLINE_MAX_WIDTH: f32 = 480.0;

/// Indentation of one outline level, in points.
const OUTLINE_INDENT: f32 = 12.0;

/// Width of the two path fields in the toolbar, in points.
const PATH_FIELD_WIDTH: f32 = 320.0;

/// Salt of the toolbar's path field, which `Ctrl+O` puts the caret in.
const PATH_FIELD_SALT: &str = "mdx_path_field";

/// Id of the overlay that explains a file being dragged over the window.
const DROP_OVERLAY_ID: &str = "mdx_drop_overlay";

/// How many paths the recent-file list keeps.
const RECENT_LIMIT: usize = 10;

/// Name of the recent-file list inside the state directory.
const RECENT_FILE: &str = "recent.json";

/// Suffix of the file a recent list is written through before it is renamed
/// into place.
const RECENT_TEMPORARY: &str = "json.tmp";

/// Extensions the shell opens: the Markdown files `mdx` renders.
const MARKDOWN_EXTENSIONS: [&str; 2] = ["md", "markdown"];

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
    /// Headings of this run with the line each one starts on, in source order:
    /// paired with the table of contents on the worker thread, where the buffer
    /// this document was made from is still at hand.
    outline: Vec<OutlineEntry>,
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

/// One row of the outline panel: a heading and the line it starts on.
struct OutlineEntry {
    /// Heading level, 1 for `#`.
    level: u8,
    title: String,
    /// 1-based line the heading starts on.
    line: usize,
}

/// Word and character counts of the buffer, as the status bar reports them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    words: usize,
    chars: usize,
}

impl Counts {
    /// Count `text`: characters are `char`s rather than bytes, and a word is a
    /// run of non-whitespace, the split `wc -w` uses.
    fn of(text: &str) -> Self {
        let mut words = 0;
        let mut in_word = false;
        let mut chars = 0;

        for character in text.chars() {
            chars += 1;
            if character.is_whitespace() {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
        }

        Self { words, chars }
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
    /// Headings of the last successful run, with the lines they start on.
    outline: Vec<OutlineEntry>,
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
    /// Whether the outline pane is open.
    show_outline: bool,
    /// Whether renders include a table of contents.
    toc: bool,

    /// Paths of the recent-file list, newest first; persisted between runs.
    recent: Vec<PathBuf>,
    /// Row of the diagnostics pane the user last clicked.
    ///
    /// `diagnostics::show` reports a click by copying the row, so the pane
    /// reads the clicked row back out of the frame's output. The index is
    /// consumed by [`MdxApp::follow_selected`] once the frame is drawn.
    selected: Option<usize>,
    /// Set by `Ctrl+O` and the `File` menu: the next toolbar puts the caret in
    /// the path field.
    focus_path: bool,

    /// Word and character counts of the buffer, and the [`MdxApp::source_hash`]
    /// they were counted for, so the status bar never walks the buffer twice
    /// for one revision of it.
    counted: Counts,
    counted_for: u64,
    /// Frame time the running job was started at.
    job_started: Option<f64>,
    /// Frame time the last build finished at, and how long it took.
    built_at: Option<f64>,
    build_time: Option<f64>,

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
            outline: Vec::new(),
            ast_json: String::new(),
            assets_json: String::new(),
            summary: String::from("No render yet"),
            rendered_key: None,

            zoom: preview::DEFAULT_ZOOM,
            split: SOURCE_WIDTH,
            tab: Tab::Preview,
            editor: EditorState::default(),
            show_diagnostics: true,
            show_outline: false,
            toc: false,

            recent: load_recent(),
            selected: None,
            focus_path: false,

            counted: Counts::default(),
            counted_for: hash_source("", false),
            job_started: None,
            built_at: None,
            build_time: None,

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

    /// Apply Ctrl+O / Ctrl+S / Ctrl+R / Ctrl+E / F5 / Esc before any widget can
    /// consume the keys.
    ///
    /// Escape is only *read*: menus and popups close themselves on it, so
    /// consuming it here would break them.
    fn handle_shortcuts(&mut self, ctx: &Context) {
        let open = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::O));
        let save = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::S));
        let reload = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::R));
        let export = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::E));
        let rebuild = ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::F5));
        let escape = ctx.input(|input| input.key_pressed(Key::Escape));

        if open {
            self.focus_path = true;
            ctx.request_repaint();
        }
        if save {
            self.save();
        }
        if reload {
            self.request_reload(ctx);
        }
        if export {
            self.render_pdf(ctx);
        }
        if rebuild {
            self.rebuild(ctx);
        }
        if escape {
            // The one dialog the shell owns; menus and popups are egui's.
            self.confirm_reload = false;
            self.focus_path = false;
        }
    }

    /// Render the buffer again, whatever the debounce and the last-render check
    /// would say.
    fn rebuild(&mut self, ctx: &Context) {
        if !self.has_document() {
            self.status = Status::error("Nothing to render — open a Markdown file first");
            return;
        }

        self.rendered_key = None;
        self.edited_at = f64::NEG_INFINITY;
        self.status = Status::info("Rebuilding…");
        ctx.request_repaint();
    }

    /// Whether the shell holds anything worth rendering.
    fn has_document(&self) -> bool {
        self.path.is_some() || !self.source.trim().is_empty()
    }

    /// Word and character counts of the buffer, recounted only when it changed.
    ///
    /// Every place that edits the buffer refreshes [`MdxApp::source_hash`], so
    /// this is a one-time walk per revision and a comparison per frame.
    fn counts(&mut self) -> Counts {
        if self.counted_for != self.source_hash {
            self.counted = Counts::of(&self.source);
            self.counted_for = self.source_hash;
        }
        self.counted
    }

    /// Remember `path` as the most recently used file and persist the list.
    fn remember_recent(&mut self, path: &Path) {
        self.recent.retain(|known| known.as_path() != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(RECENT_LIMIT);
        self.save_recent_list();
    }

    /// Write the recent-file list out.
    ///
    /// Failing to write it is reported but never stops the shell: the list is
    /// worth a status line, not a dialog.
    fn save_recent_list(&mut self) {
        if let Err(message) = save_recent(&self.recent) {
            self.status = Status::error(message);
        }
    }

    /// Open `path`: what the recent-file list and a dropped file both do.
    fn open_path(&mut self, ctx: &Context, path: PathBuf) {
        self.path_text = path.display().to_string();
        self.pdf_path_text = default_pdf_path(&path).display().to_string();
        self.load(ctx);
    }

    /// Read the toolbar's path into the buffer.
    ///
    /// Nothing is cleared when the file cannot be read: the buffer, the loaded
    /// path and the last render all stay as they were, and the reason appears in
    /// the status bar. A file that does load joins the recent list.
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
                self.remember_recent(&path);
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

    /// Open the Markdown file a drop handed over, and say what was ignored.
    ///
    /// egui reports a drop once, as a list of paths; the first Markdown file
    /// among them wins, and anything else is named in the status bar rather than
    /// silently dropped on the floor.
    fn handle_dropped_files(&mut self, ctx: &Context) {
        let dropped = dropped_paths(ctx);
        if dropped.is_empty() {
            return;
        }

        match dropped.iter().find(|path| is_markdown(path.as_path())) {
            Some(path) => {
                let ignored = dropped.len() - 1;
                let path = path.to_path_buf();
                self.open_path(ctx, path);
                if ignored > 0 {
                    self.status = Status::error(format!(
                        "Ignored {ignored} further dropped file(s) — drop one at a time"
                    ));
                }
            }
            None => {
                self.status = Status::error(ignored_drop_message(&dropped));
            }
        }
    }

    /// Paste a clipboard image as a Markdown reference at the caret.
    ///
    /// `TextEdit` only handles text, so a screenshot on the clipboard would
    /// otherwise be dropped. The PNG is read through `wl-paste`, stored beside
    /// the document under `assets/`, and the editor inserts the link at the
    /// caret.
    fn handle_image_paste(&mut self, ctx: &Context) {
        let pressed = ctx.input(|input| {
            input.key_pressed(Key::V) && input.modifiers.command && !input.modifiers.shift
        });
        if !pressed {
            return;
        }
        let Some(png) = clipboard_image_png() else {
            return;
        };
        let Some(path) = self.path.as_deref() else {
            self.status = Status::error("Save the document before pasting an image");
            return;
        };
        let directory = document_directory(Some(path)).join("assets");
        if let Err(error) = fs::create_dir_all(&directory) {
            self.status = Status::error(format!("Cannot create {}: {error}", directory.display()));
            return;
        }
        let name = format!("pasted-{}.png", fnv_hex(&png));
        let file = directory.join(&name);
        if let Err(error) = fs::write(&file, &png) {
            self.status = Status::error(format!("Cannot write {}: {error}", file.display()));
            return;
        }
        self.editor.insert_at_caret(format!("![](assets/{name})"));
        self.dirty = true;
        self.status = Status::info(format!("Saved pasted image to assets/{name}"));
        ctx.request_repaint();
    }

    /// Follow a click in the diagnostics pane: put the caret on that
    /// diagnostic's line and highlight it.
    /// Runs once the frame's panes have been drawn, so the index the pane read
    /// back is resolved against the diagnostics of the last run — the same
    /// slice the user saw.
    fn follow_selected(&mut self, ctx: &Context) {
        let Some(index) = self.selected.take() else {
            return;
        };
        let Some(diagnostic) = self.diagnostics.get(index) else {
            return;
        };

        match diagnostic.line {
            Some(line) => {
                let line = line as usize;
                self.editor.reveal_line(line);
                self.status = Status::info(format!("{} at line {line}", diagnostic.code.as_str()));
            }
            // A diagnostic without a position (an asset, a whole-document
            // failure) has no line to show.
            None => {
                self.status = Status::info(format!(
                    "{} has no line to jump to",
                    diagnostic.code.as_str()
                ));
            }
        }

        // The source pane is drawn earlier in the frame, so it needs one more
        // frame to carry the reveal out.
        ctx.request_repaint();
    }

    /// While a file is dragged over the window, say what dropping it will do.
    fn drop_overlay(&self, ui: &Ui) {
        let Some(accepted) = ui.input(|input| {
            if input.raw.hovered_files.is_empty() {
                return None;
            }
            Some(
                input
                    .raw
                    .hovered_files
                    .iter()
                    .any(|file| file.path.as_deref().is_some_and(is_markdown)),
            )
        }) else {
            return;
        };

        let (text, colour) = if accepted {
            (
                String::from("Release to open this Markdown file"),
                ui.visuals().text_color(),
            )
        } else {
            (
                String::from("Only .md and .markdown files open"),
                ui.visuals().error_fg_color,
            )
        };

        egui::Area::new(egui::Id::unique(DROP_OVERLAY_ID))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            // The overlay explains the drop; it must not eat the drop itself.
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(ui.visuals().extreme_bg_color)
                    .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(16, 10))
                    .show(ui, |ui| {
                        ui.label(RichText::new(text).strong().color(colour));
                    });
            });
    }

    /// The `File` menu: the toolbar's actions, plus the recent files.
    fn file_menu(&mut self, ui: &mut Ui, ctx: &Context) {
        ui.menu_button("File", |ui| {
            if ui
                .button("Open path field")
                .on_hover_text("Ctrl+O")
                .clicked()
            {
                self.focus_path = true;
                ui.close();
            }
            if ui
                .button("Reload")
                .on_hover_text("Read the file again (Ctrl+R)")
                .clicked()
            {
                self.request_reload(ctx);
                ui.close();
            }
            if ui
                .add_enabled(self.dirty, egui::Button::new("Save"))
                .on_hover_text("Write the buffer to this path (Ctrl+S)")
                .clicked()
            {
                self.save();
                ui.close();
            }
            if ui
                .button("Render PDF")
                .on_hover_text("Render the document and write it to this path (Ctrl+E)")
                .clicked()
            {
                self.render_pdf(ctx);
                ui.close();
            }
            if ui
                .button("Rebuild preview")
                .on_hover_text("Render the buffer again now (F5)")
                .clicked()
            {
                self.rebuild(ctx);
                ui.close();
            }

            ui.separator();

            ui.add_enabled_ui(!self.recent.is_empty(), |ui| {
                ui.menu_button("Recent files", |ui| {
                    let mut chosen = None;
                    for path in &self.recent {
                        let label = file_label(path);
                        if ui
                            .button(label)
                            .on_hover_text(path.display().to_string())
                            .clicked()
                        {
                            chosen = Some(path.clone());
                        }
                    }

                    ui.separator();
                    if ui.button("Clear recent list").clicked() {
                        self.recent.clear();
                        self.save_recent_list();
                        ui.close();
                    }

                    if let Some(path) = chosen {
                        ui.close();
                        self.open_path(ctx, path);
                    }
                });
            });
        });
    }

    /// Outline pane: the headings of the last render, in source order.
    fn outline_pane(&mut self, ui: &mut Ui) {
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Outline").strong());
            ui.separator();
            ui.label(RichText::new(format!("{} heading(s)", self.outline.len())).weak());
        });
        ui.separator();

        if self.outline.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("No headings yet — render a document.").weak());
            return;
        }

        // The heading a reveal landed on reads as selected, so the outline says
        // where the caret ended up.
        let revealed = self.editor.revealed_line();
        let mut chosen = None;
        egui::ScrollArea::vertical()
            .id_salt("mdx_outline_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for entry in &self.outline {
                    ui.horizontal(|ui| {
                        // One step of indentation per level, so nesting reads.
                        let indent = f32::from(entry.level.saturating_sub(1)) * OUTLINE_INDENT;
                        ui.add_space(indent);
                        let title = if entry.title.is_empty() {
                            "(untitled)"
                        } else {
                            entry.title.as_str()
                        };
                        let row = ui
                            .selectable_label(revealed == Some(entry.line), title)
                            .on_hover_text(format!("Line {}", entry.line));
                        if row.clicked() {
                            chosen = Some(entry.line);
                        }
                    });
                }
            });

        if let Some(line) = chosen {
            self.editor.reveal_line(line);
            self.status = Status::info(format!("Heading at line {line}"));
            // The reveal is carried out by the source pane's next frame.
            ui.ctx().request_repaint();
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
        if !self.has_document() {
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
                self.job_started = Some(ctx.input(|input| input.time));
            }
            Err(error) => {
                self.status = Status::error(format!("Cannot start the render thread: {error}"));
            }
        }
    }

    /// Apply whatever a finished worker sent, without ever blocking.
    ///
    /// `now` is the frame's own clock (`RawInput::time`), so the build time in
    /// the status bar never touches the wall clock.
    fn pump(&mut self, now: f64) {
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
                let started = self.job_started.take();
                match *message {
                    JobMessage::Preview(output) => {
                        self.built_at = Some(now);
                        self.build_time = started.map(|start| (now - start).max(0.0));
                        self.apply_preview(*output);
                    }
                    JobMessage::Pdf { result } => self.apply_pdf(result),
                }
            }
            Poll::Gone => {
                self.receiver = None;
                self.busy = None;
                self.job_started = None;
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
            outline,
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
        // The page count has a label of its own in the status bar, so the
        // summary is free to be about the diagnostics alone.
        self.summary = match (errors, warnings) {
            (0, 0) => String::from("No diagnostics"),
            (0, warnings) => format!("{warnings} warning(s)"),
            (errors, _) => format!("{errors} error(s)"),
        };

        self.ast_json = ast_json;
        self.assets_json = assets_json;
        self.doc = Some(doc);
        self.assets = Some(assets);
        self.diagnostics = diagnostics;
        self.outline = outline;
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

            self.file_menu(ui, ctx);
            let path_field = ui.add(
                egui::TextEdit::singleline(&mut self.path_text)
                    .id_salt(PATH_FIELD_SALT)
                    .desired_width(PATH_FIELD_WIDTH),
            );
            if std::mem::take(&mut self.focus_path) {
                path_field.request_focus();
            }
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
            ui.checkbox(&mut self.show_outline, "Outline");

            ui.separator();
            ui.selectable_value(&mut self.tab, Tab::Preview, "Preview");
            ui.selectable_value(&mut self.tab, Tab::Ast, "AST");
            ui.selectable_value(&mut self.tab, Tab::Assets, "Assets");
        });
        ui.add_space(2.0);
    }

    /// Status bar: the last message, the running job and the view metrics.
    ///
    /// The metrics are the ones that describe the buffer and the last build:
    /// caret, page count, words, characters, how long ago the build finished —
    /// and, on hover, how long it took.
    fn status_bar(&mut self, ui: &mut Ui) {
        let counts = self.counts();
        let now = ui.input(|input| input.time);
        let (build_text, build_hover) = build_label(now, self.built_at, self.build_time);

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
                ui.label(RichText::new(format!("{} page(s)", self.pages.len())).weak());
                ui.separator();
                ui.label(RichText::new(format!("{} word(s)", counts.words)).weak());
                ui.separator();
                ui.label(RichText::new(format!("{} char(s)", counts.chars)).weak());
                ui.separator();
                ui.label(RichText::new(build_text).weak())
                    .on_hover_text(build_hover);
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
    ///
    /// `diagnostics::show` (slice 19, signature frozen) reports a click the only
    /// way its contract allows: the clicked row puts its
    /// [`diagnostics::clipboard_line`] on the clipboard. The commands the pane
    /// added this frame therefore name the clicked row, which is what turns a
    /// row click into a caret jump — see [`MdxApp::follow_selected`]. Its
    /// "copy all" button copies the same text when exactly one row is listed,
    /// and is followed the same way; that is harmless, and the alternative would
    /// be guessing at row geometry the peer owns.
    fn diagnostics_pane(&mut self, ui: &mut Ui) {
        let context = ui.ctx().clone();
        let before = context.output(|output| output.commands.len());

        egui::ScrollArea::vertical()
            .id_salt("mdx_diagnostics_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| diagnostics::show(ui, &self.diagnostics));

        let clicked = context.output(|output| {
            output
                .commands
                .iter()
                .skip(before)
                .rev()
                .find_map(|command| match command {
                    OutputCommand::CopyText(text) => self
                        .diagnostics
                        .iter()
                        .position(|diagnostic| diagnostics::clipboard_line(diagnostic) == *text),
                    _ => None,
                })
        });

        if let Some(index) = clicked {
            self.selected = Some(index);
            context.request_repaint();
        }
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
        self.handle_dropped_files(&ctx);
        self.handle_image_paste(&ctx);
        self.pump(ctx.input(|input| input.time));
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
        if self.show_outline {
            egui::Panel::right("mdx_outline")
                .resizable(true)
                .default_size(OUTLINE_WIDTH)
                .size_range(OUTLINE_MIN_WIDTH..=OUTLINE_MAX_WIDTH)
                .show(ui, |ui| self.outline_pane(ui));
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

        // The panes are drawn: follow a diagnostic click, and say what a file
        // dragged over the window would do.
        self.follow_selected(&ctx);
        self.drop_overlay(ui);
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
    let outline = outline_entries(&pipeline.doc, source);

    PreviewOutput {
        key,
        doc: pipeline.doc,
        assets: pipeline.assets,
        pages,
        diagnostics: pipeline.diagnostics,
        outline,
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

/// The clipboard's PNG payload, when the clipboard holds an image.
///
/// `wl-paste` is the Wayland clipboard tool Omarchy ships. When it is missing,
/// or the clipboard carries no image, this returns `None` and the paste falls
/// through to the text path.
fn clipboard_image_png() -> Option<Vec<u8>> {
    let types = Command::new("wl-paste").arg("--list-types").output().ok()?;
    let listing = String::from_utf8_lossy(&types.stdout);
    if !listing
        .lines()
        .any(|line| line.trim().starts_with("image/"))
    {
        return None;
    }
    let out = Command::new("wl-paste")
        .args(["--type", "image/png"])
        .output()
        .ok()?;
    (out.status.success() && !out.stdout.is_empty()).then_some(out.stdout)
}

/// 16 lowercase hex digits of the FNV-1a hash: one stable name per image.
fn fnv_hex(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
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

/// Status-bar text for the last build, and what its tooltip says.
///
/// `now` and `built_at` are frame times (`RawInput::time`), never the wall
/// clock: nothing the shell shows depends on when it happens to run.
fn build_label(now: f64, built_at: Option<f64>, took: Option<f64>) -> (String, String) {
    let Some(built_at) = built_at else {
        return (
            String::from("no build yet"),
            String::from("Nothing has been rendered yet"),
        );
    };

    let text = format!("built {} ago", duration_label(now - built_at));
    let hover = match took {
        Some(seconds) => format!("The last render took {}", duration_label(seconds)),
        None => String::from("How long the last render took was not measured"),
    };
    (text, hover)
}

/// A number of seconds as a short label with one unit: `0.42 s`, `3.2 s`,
/// `1.4 min`.
fn duration_label(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    if seconds < 10.0 {
        format!("{seconds:.2} s")
    } else if seconds < 90.0 {
        format!("{seconds:.1} s")
    } else {
        format!("{:.1} min", seconds / 60.0)
    }
}

/// Directory the shell keeps its own state in: `$XDG_STATE_HOME/mdx`, or
/// `$HOME/.local/state/mdx` when the variable is unset or not absolute.
///
/// `None` only when neither variable names anything usable; the recent list
/// then stays in memory for the session instead of being written anywhere.
fn state_dir() -> Option<PathBuf> {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        let state = PathBuf::from(state);
        if state.is_absolute() {
            return Some(state.join("mdx"));
        }
    }

    let home = PathBuf::from(std::env::var_os("HOME")?);
    if home.as_os_str().is_empty() {
        return None;
    }
    Some(home.join(".local").join("state").join("mdx"))
}

/// File the recent list lives in, if a state directory is available.
fn recent_path() -> Option<PathBuf> {
    state_dir().map(|directory| directory.join(RECENT_FILE))
}

/// The recent-file list as last written, newest first, capped at
/// [`RECENT_LIMIT`] entries.
///
/// A missing, unreadable or malformed file is an empty list rather than an
/// error: a list that half-parses is worth no more than the user's time, and
/// the shell has nothing to repair it with.
fn load_recent() -> Vec<PathBuf> {
    let Some(path) = recent_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(paths) = serde_json::from_str::<Vec<String>>(&text) else {
        return Vec::new();
    };

    let mut recent: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_empty() {
            continue;
        }
        let path = PathBuf::from(path);
        if recent.contains(&path) {
            continue;
        }
        recent.push(path);
        if recent.len() == RECENT_LIMIT {
            break;
        }
    }
    recent
}

/// Write `recent` where the next run will look for it, creating the directory.
///
/// The list is written next to itself and renamed into place, so a shell that
/// stops mid-write leaves the previous list intact instead of a truncated one.
fn save_recent(recent: &[PathBuf]) -> Result<(), String> {
    let Some(path) = recent_path() else {
        // Nowhere to write: the list lives in memory for this session.
        return Ok(());
    };

    if let Some(directory) = path.parent() {
        std::fs::create_dir_all(directory)
            .map_err(|error| format!("Cannot create {}: {error}", directory.display()))?;
    }

    let paths: Vec<String> = recent
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    let text = serde_json::to_string_pretty(&paths)
        .map_err(|error| format!("Cannot serialise the recent list: {error}"))?;

    let temporary = path.with_extension(RECENT_TEMPORARY);
    std::fs::write(&temporary, format!("{text}\n"))
        .map_err(|error| format!("Cannot write {}: {error}", temporary.display()))?;
    std::fs::rename(&temporary, &path)
        .map_err(|error| format!("Cannot write {}: {error}", path.display()))
}

/// Name a path is shown under: its file name, or the whole path when it has
/// none.
fn file_label(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

/// Whether `path` names a Markdown file the shell opens, by extension and
/// ignoring case.
fn is_markdown(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    MARKDOWN_EXTENSIONS
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
}

/// What a drop of nothing but non-Markdown files says.
fn ignored_drop_message(paths: &[PathBuf]) -> String {
    match paths {
        [path] => format!(
            "Ignored {}: not a Markdown file (.md or .markdown)",
            file_label(path)
        ),
        paths => format!(
            "Ignored {} dropped files: none of them is .md or .markdown",
            paths.len()
        ),
    }
}

/// Paths of the files dropped on the window this frame.
///
/// egui reports a drop exactly once, and a `DroppedFile`'s path is the native
/// path of the dropped file.
fn dropped_paths(ctx: &Context) -> Vec<PathBuf> {
    ctx.input(|input| {
        input
            .raw
            .dropped_files
            .iter()
            .map(|file| file.path().to_path_buf())
            .collect()
    })
}

/// `doc.toc` with the line each heading starts on, in source order.
///
/// [`Doc`] carries no positions, so the headings are found again in the buffer
/// the document was made from: the table of contents walks `doc.blocks` in
/// source order, so heading *n* of the buffer is entry *n* of the table of
/// contents. When the scan and the table of contents disagree — setext
/// underlines, headings inside lists, raw HTML — an entry keeps its own index
/// into the scan, so a click lands on the same line every time instead of
/// somewhere different.
fn outline_entries(doc: &Doc, source: &str) -> Vec<OutlineEntry> {
    let lines = heading_lines(source);

    doc.toc
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let line = match lines.get(index) {
                Some(line) => *line,
                // Fewer headings in the buffer than in the table of contents:
                // the last heading the scan found is the stable fallback.
                None => lines.last().copied().unwrap_or(1),
            };
            OutlineEntry {
                level: entry.level,
                title: entry.title.clone(),
                line,
            }
        })
        .collect()
}

/// 1-based lines of the ATX headings in `source`, in source order.
///
/// This is a scan of the buffer, not a parser: front matter, fenced code,
/// indented code and block quotes are all accounted for, but a heading written
/// in a form the scan does not know simply does not appear. It exists to give
/// the outline of [`outline_entries`] a line to point at.
fn heading_lines(source: &str) -> Vec<usize> {
    let mut lines = Vec::new();
    let mut fence: Option<&'static str> = None;
    let front_matter = front_matter_end(source);

    for (index, raw) in source.lines().enumerate() {
        let number = index + 1;
        let trimmed = raw.trim_start();

        // Front matter is YAML, not Markdown: skip it whole.
        if front_matter.is_some_and(|end| number <= end) {
            continue;
        }

        // Inside a fenced block everything is code, `#` lines included.
        if let Some(marker) = fence {
            if trimmed.starts_with(marker) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }

        // Four spaces (or a tab) of indentation opens an indented code block.
        if raw.starts_with("    ") || raw.starts_with('\t') {
            continue;
        }

        // `> # Title` is a heading in a block quote, and the table of contents
        // collects it as one.
        let mut body = trimmed;
        while let Some(rest) = body.strip_prefix('>') {
            body = rest.trim_start();
        }

        if is_atx_heading(body) {
            lines.push(number);
        }
    }

    lines
}

/// 1-based line the front matter of `source` ends on, if it opens one.
///
/// A `---` first line only opens front matter when a closing `---` or `...`
/// follows; without one it is a thematic break and the buffer is Markdown.
fn front_matter_end(source: &str) -> Option<usize> {
    let mut lines = source.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }

    for (index, line) in lines.enumerate() {
        let trimmed = line.trim_end();
        if trimmed == "---" || trimmed == "..." {
            // Line 1 opened the block, so this one is `index + 2`.
            return Some(index + 2);
        }
    }
    None
}

/// Whether `line` (quote markers already removed) opens an ATX heading.
fn is_atx_heading(line: &str) -> bool {
    let hashes = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if !(1..=6).contains(&hashes) {
        return false;
    }

    match line.chars().nth(hashes) {
        // A bare `#` is a heading as well.
        None => true,
        Some(character) => character == ' ' || character == '\t',
    }
}

#[cfg(test)]
mod image_tests {
    use super::fnv_hex;

    #[test]
    fn image_names_are_stable_per_content() {
        assert_eq!(fnv_hex(b""), "cbf29ce484222325");
        assert_eq!(fnv_hex(b"same"), fnv_hex(b"same"));
        assert_ne!(fnv_hex(b"a"), fnv_hex(b"b"));
        assert_eq!(fnv_hex(b"a").len(), 16);
    }
}
