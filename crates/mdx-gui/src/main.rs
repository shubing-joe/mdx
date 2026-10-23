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
    // reported in the status bar, not by exiting. Files dropped on the window
    // (see `app::MdxApp`) are the other way in.
    let initial = std::env::args_os().nth(1).map(PathBuf::from);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("mdx")
            .with_inner_size(WINDOW_SIZE)
            .with_min_inner_size(MIN_WINDOW_SIZE)
            // The OS drop target a drag of a `.md` file lands on. Back ends
            // that let it be turned off default to accepting drops; asking for
            // it keeps the shell's only way of opening a dragged file.
            // API: https://docs.rs/egui/0.37.0/egui/viewport/struct.ViewportBuilder.html#method.with_drag_and_drop
            .with_drag_and_drop(true),
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
