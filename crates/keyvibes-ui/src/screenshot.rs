//! An opt-in capture hook for docs and CI.
//!
//! Setting `KEYVIBES_SCREENSHOT=/some/path.png` makes the window render
//! itself to that file through GSK — the same renderer the screen uses —
//! and then quit. Nothing is captured, and no file is touched, unless the
//! variable is present, so a normal launch never pays for any of this.
//!
//! A compositor grab is not an option on this desktop: Xwayland runs
//! rootless, so the X root window holds no pixels at all.

use gtk::prelude::*;

/// The capture destination requested through `KEYVIBES_SCREENSHOT`.
pub fn requested() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("KEYVIBES_SCREENSHOT")?;
    if path.is_empty() {
        return None;
    }
    Some(std::path::PathBuf::from(path))
}

/// The page named by `KEYVIBES_SCREENSHOT_PAGE`, if it is one of the three
/// the window can show. Anything else is ignored rather than trusted.
pub fn requested_page() -> Option<&'static str> {
    const PAGES: [&str; 3] = ["sounds", "audio", "system"];
    let requested = std::env::var("KEYVIBES_SCREENSHOT_PAGE").ok()?;
    PAGES.into_iter().find(|page| *page == requested)
}

/// Renders `window` into `path`.
///
/// Returns `Ok(false)` — rather than an error — when the frame is still
/// flat, which is what an unallocated child produces: the caller keeps
/// polling instead of documenting an empty window.
///
/// The window's own CSS background is not part of any child, so it is laid
/// down first as a backdrop; without it the file would have a transparent
/// border around the content.
pub fn capture(window: &adw::ApplicationWindow, path: &std::path::Path) -> Result<bool, String> {
    let surface = window
        .surface()
        .ok_or_else(|| "the window is not mapped yet".to_string())?;
    let (width, height) = (surface.width(), surface.height());
    if width <= 0 || height <= 0 {
        return Err(format!("the window surface is {width}x{height}"));
    }

    let bounds = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let snapshot = gtk::Snapshot::new();
    let backdrop = window
        .style_context()
        .lookup_color("window_bg_color")
        .unwrap_or_else(|| gtk::gdk::RGBA::new(1.0, 1.0, 1.0, 1.0));
    snapshot.append_color(&backdrop, &bounds);

    let child = window
        .child()
        .ok_or_else(|| "the window has no content".to_string())?;
    // An un-allocated child snapshots to nothing, so the file would be the
    // bare backdrop. Report that as "not ready" instead of a blank window.
    if child.width() <= 0 || child.height() <= 0 {
        return Ok(false);
    }
    window.snapshot_child(&child, &snapshot);

    let node = snapshot
        .to_node()
        .ok_or_else(|| "the window rendered nothing".to_string())?;

    let renderer = gtk::gsk::Renderer::for_surface(&surface)
        .ok_or_else(|| "no GSK renderer for this surface".to_string())?;
    if !renderer.is_realized() {
        renderer
            .realize(Some(&surface))
            .map_err(|error| format!("the renderer refused to realize: {error}"))?;
    }
    let texture = renderer.render_texture(&node, None);
    renderer.unrealize();

    texture
        .save_to_png(path)
        .map(|()| true)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}
