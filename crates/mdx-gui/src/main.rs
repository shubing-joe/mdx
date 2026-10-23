//! `mdx-gui` — the desktop shell around the `mdx` pipeline.
//!
//! The binary is a thin bootstrap: it turns the optional command-line path into
//! an [`app::MdxApp`], installs egui's image loaders — the preview draws
//! rasterised page SVGs through them — and hands control to `eframe`.
//!
//! ```text
//! cargo run -p mdx-gui -- notes/design.md
//! ```

mod app;
mod diagnostics;
mod editor;
mod preview;

use std::path::PathBuf;

/// Window size the shell opens with.
const WINDOW_SIZE: [f32; 2] = [1280.0, 860.0];

/// Smallest window the panel layout still works in.
const MIN_WINDOW_SIZE: [f32; 2] = [720.0, 480.0];

fn main() -> eframe::Result {
    // Anything after the binary name is the file to open; a missing file is
    // reported in the status bar, not by exiting.
    let initial = std::env::args_os().nth(1).map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("mdx")
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE),
        ..Default::default()
    };

    eframe::run_native(
        "mdx",
        options,
        Box::new(move |cc| {
            // SVG pages and data-URI images reach egui through these loaders.
            egui_extras::install_image_loaders(&cc.egui_ctx);
            Ok(Box::new(app::MdxApp::new(cc, initial)))
        }),
    )
}
