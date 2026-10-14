//! Status-notifier (system tray) integration.
//!
//! The tray is a *convenience*, never a requirement: if no status-notifier
//! host is running the spawn fails and the UI simply closes on window close
//! instead of hiding. Nothing about KeyVibes depends on it.
//!
//! The icon is drawn in code rather than loaded from a theme so the tray
//! looks the same on every desktop, including ones with a sparse icon theme.

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};
use std::sync::Arc;

/// What the user asked for while looking at the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Bring the main window to the front.
    ShowWindow,
    /// Flip the master sounds switch.
    ToggleSounds,
    /// Shut the engine down (when this UI owns it) and exit.
    Quit,
}

/// A running tray, and the handle used to keep its contents current.
pub struct Tray {
    handle: ksni::blocking::Handle<KeyVibesTray>,
}

impl Tray {
    /// Registers with the session's status-notifier host.
    ///
    /// Fails - harmlessly - on a desktop with no SNI host; the caller keeps
    /// running without a tray rather than refusing to start.
    pub fn spawn(actions: async_channel::Sender<TrayAction>) -> Result<Tray, String> {
        let handle = KeyVibesTray::new(actions)
            .spawn()
            .map_err(|error| format!("no status-notifier host is running: {error}"))?;
        Ok(Tray { handle })
    }

    /// Refreshes the tooltip and the sounds checkmark.
    ///
    /// Cheap and synchronous: the update is a D-Bus property change, so it
    /// cannot stall the GTK main loop for more than a few milliseconds.
    pub fn update(&self, enabled: bool, summary: &str) {
        let summary = summary.to_string();
        self.handle.update(move |tray| {
            tray.enabled = enabled;
            tray.summary = summary;
        });
    }

    /// Asks the host to drop the tray. Does not block on the shutdown
    /// completing, because the process is about to exit either way.
    pub fn shutdown(&self) {
        let _ = self.handle.shutdown();
    }
}

/// The tray's own state, owned by the tray service thread.
pub struct KeyVibesTray {
    /// Mirrors the engine's master switch.
    enabled: bool,
    /// One line describing what the engine is doing right now.
    summary: String,
    /// Channel back to the GTK main loop.
    actions: Arc<async_channel::Sender<TrayAction>>,
}

impl KeyVibesTray {
    fn new(actions: async_channel::Sender<TrayAction>) -> Self {
        Self {
            enabled: false,
            summary: "Starting…".to_string(),
            actions: Arc::new(actions),
        }
    }
}

impl ksni::Tray for KeyVibesTray {
    /// Stable across sessions and across machines: the host uses it to
    /// remember the tray's position.
    fn id(&self) -> String {
        "org.keyvibes.KeyVibes".to_string()
    }

    fn title(&self) -> String {
        "KeyVibes".to_string()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![keycap_icon(32), keycap_icon(16)]
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            icon_pixmap: vec![keycap_icon(32)],
            title: "KeyVibes".to_string(),
            description: self.summary.clone(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.actions.send_blocking(TrayAction::ShowWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let show = self.actions.clone();
        let toggle = self.actions.clone();
        let quit = self.actions.clone();
        vec![
            MenuItem::Standard(StandardItem {
                label: "Show KeyVibes".to_string(),
                icon_name: "window-show-symbolic".to_string(),
                activate: Box::new(move |_tray| {
                    let _ = show.send_blocking(TrayAction::ShowWindow);
                }),
                ..Default::default()
            }),
            MenuItem::Checkmark(CheckmarkItem {
                label: "Sounds".to_string(),
                checked: self.enabled,
                activate: Box::new(move |_tray| {
                    let _ = toggle.send_blocking(TrayAction::ToggleSounds);
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: "Quit".to_string(),
                icon_name: "application-exit-symbolic".to_string(),
                activate: Box::new(move |_tray| {
                    let _ = quit.send_blocking(TrayAction::Quit);
                }),
                ..Default::default()
            }),
        ]
    }
}

/// Draws a stylised keycap: a dark base plate with a lighter top face.
///
/// Pure geometry, no dependencies, so the tray always has an icon even on a
/// system whose icon theme has never heard of KeyVibes.
fn keycap_icon(size: i32) -> ksni::Icon {
    let s = size as f32;
    // Outer plate and inner face, both inset and rounded.
    let plate_inset = s * 0.07;
    let face_inset = s * 0.20;
    let plate_radius = s * 0.22;
    let face_radius = plate_radius * 0.65;

    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let plate = rounded_rect_coverage(
                px,
                py,
                plate_inset,
                plate_inset,
                s - plate_inset,
                s - plate_inset,
                plate_radius,
            );
            let face = rounded_rect_coverage(
                px,
                py,
                face_inset,
                face_inset,
                s - face_inset,
                s - face_inset,
                face_radius,
            );

            // Accumulate in premultiplied form, face over plate over clear.
            let mut alpha = 0.0f32;
            let (mut pr, mut pg, mut pb) = (0.0f32, 0.0f32, 0.0f32);
            over(
                &mut alpha,
                &mut pr,
                &mut pg,
                &mut pb,
                plate,
                (0x3B, 0x27, 0x7A),
            );
            over(
                &mut alpha,
                &mut pr,
                &mut pg,
                &mut pb,
                face,
                (0x8B, 0x5C, 0xF6),
            );

            let (a, r, g, b) = if alpha > 0.0 {
                (
                    (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
                    (pr / alpha).round().clamp(0.0, 255.0) as u8,
                    (pg / alpha).round().clamp(0.0, 255.0) as u8,
                    (pb / alpha).round().clamp(0.0, 255.0) as u8,
                )
            } else {
                (0, 0, 0, 0)
            };
            // StatusNotifierItem wants ARGB32, network byte order.
            data.extend_from_slice(&[a, r, g, b]);
        }
    }
    ksni::Icon {
        width: size,
        height: size,
        data,
    }
}

/// Premultiplied source-over accumulation.
///
/// `red`/`green`/`blue` hold the destination already multiplied by its own
/// alpha, so compositing is `src * coverage + dst * (1 - coverage)`.
fn over(
    alpha: &mut f32,
    red: &mut f32,
    green: &mut f32,
    blue: &mut f32,
    coverage: f32,
    rgb: (u8, u8, u8),
) {
    if coverage <= 0.0 {
        return;
    }
    let sa = coverage;
    let keep = 1.0 - sa;
    *red = sa * rgb.0 as f32 + *red * keep;
    *green = sa * rgb.1 as f32 + *green * keep;
    *blue = sa * rgb.2 as f32 + *blue * keep;
    *alpha = sa + *alpha * keep;
}

/// Antialiased coverage of a rounded rectangle, in `[0, 1]`.
///
/// Negative inside, positive outside; the 0.5 crossing is the edge, and the
/// clamped band around it gives a one-pixel fade.
fn rounded_rect_coverage(px: f32, py: f32, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32) -> f32 {
    let cx = (x0 + x1) * 0.5;
    let cy = (y0 + y1) * 0.5;
    let half_w = ((x1 - x0) * 0.5 - radius).max(0.0);
    let half_h = ((y1 - y0) * 0.5 - radius).max(0.0);
    let dx = (px - cx).abs() - half_w;
    let dy = (py - cy).abs() - half_h;
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    let inside = dx.max(dy).min(0.0);
    let distance = outside + inside - radius;
    (0.5 - distance).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tray_icon_is_a_well_formed_argb_buffer() {
        for size in [16, 32] {
            let icon = keycap_icon(size);
            assert_eq!(icon.width, size);
            assert_eq!(
                icon.data.len(),
                (size * size * 4) as usize,
                "ARGB32 needs four bytes per pixel"
            );
            // The glyph has to actually cover the middle of the icon,
            // otherwise every host would show an invisible tray entry.
            let center = (((size / 2) * size + size / 2) * 4) as usize;
            assert!(
                icon.data[center] > 200,
                "the centre must be opaque, got alpha {}",
                icon.data[center]
            );
            // ...and be fully transparent in the corner, so it sits on the
            // host's tray background instead of drawing a coloured square.
            assert_eq!(icon.data[0], 0, "the corners stay transparent");
        }
    }

    #[test]
    fn the_icon_uses_the_brand_accent_for_the_key_face() {
        let icon = keycap_icon(32);
        // The face is drawn in the brand accent, so the exact colour has to
        // appear somewhere in the buffer rather than being mixed away.
        const ACCENT: (u8, u8, u8) = (0x8B, 0x5C, 0xF6);
        let found = icon.data.chunks_exact(4).any(|pixel| {
            pixel[0] == 0xFF && pixel[1] == ACCENT.0 && pixel[2] == ACCENT.1 && pixel[3] == ACCENT.2
        });
        assert!(found, "an opaque pixel must be exactly #8b5cf6");
    }

    #[test]
    fn source_over_composites_without_double_counting() {
        // Two fully opaque layers must show only the top one; the first
        // version of `over` added both and blew the channels past 255.
        let (mut a, mut r, mut g, mut b) = (0.0, 0.0, 0.0, 0.0);
        over(&mut a, &mut r, &mut g, &mut b, 1.0, (255, 0, 0));
        over(&mut a, &mut r, &mut g, &mut b, 1.0, (0, 255, 0));
        assert_eq!((a, r, g, b), (1.0, 0.0, 255.0, 0.0));
    }

    #[test]
    fn a_half_transparent_top_layer_blends_with_the_bottom() {
        let (mut a, mut r, mut g, mut b) = (0.0, 0.0, 0.0, 0.0);
        over(&mut a, &mut r, &mut g, &mut b, 1.0, (0, 0, 0));
        over(&mut a, &mut r, &mut g, &mut b, 0.5, (255, 255, 255));
        assert_eq!(a, 1.0);
        assert_eq!(r, 127.5, "half of the top layer over an opaque bottom");
    }

    #[test]
    fn rounded_rectangle_coverage_is_bimodal() {
        // A point well inside a large box is covered; one well outside is not.
        assert!(rounded_rect_coverage(50.0, 50.0, 10.0, 10.0, 90.0, 90.0, 8.0) > 0.9);
        assert_eq!(
            rounded_rect_coverage(0.0, 0.0, 10.0, 10.0, 90.0, 90.0, 8.0),
            0.0
        );
    }

    #[test]
    fn tray_actions_round_trip_through_the_channel() {
        let (tx, rx) = async_channel::unbounded();
        tx.send_blocking(TrayAction::ToggleSounds).unwrap();
        assert_eq!(
            rx.recv_blocking().unwrap(),
            TrayAction::ToggleSounds,
            "the tray must deliver actions in order"
        );
    }

    #[test]
    fn the_tray_id_is_stable_across_sessions() {
        // Hosts remember tray position by id; changing it would scatter
        // icons between releases.
        let (tx, _rx) = async_channel::unbounded();
        let tray = KeyVibesTray::new(tx);
        assert_eq!(ksni::Tray::id(&tray), "org.keyvibes.KeyVibes");
        assert_eq!(ksni::Tray::title(&tray), "KeyVibes");
    }

    #[test]
    fn the_menu_reflects_the_sounds_state() {
        let (tx, _rx) = async_channel::unbounded();
        let mut tray = KeyVibesTray::new(tx);
        let menu = ksni::Tray::menu(&tray);
        assert_eq!(menu.len(), 4, "show, sounds, separator, quit");
        tray.enabled = true;
        let menu = ksni::Tray::menu(&tray);
        match &menu[1] {
            MenuItem::Checkmark(item) => assert!(item.checked),
            _ => panic!("expected a checkmark item"),
        }
    }
}
