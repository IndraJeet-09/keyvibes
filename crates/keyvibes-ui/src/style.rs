//! The KeyVibes visual identity, applied on top of libadwaita.
//!
//! Everything here is additive: the app still follows the system's GTK
//! theme, light or dark, and only a handful of KeyVibes-specific classes are
//! layered on. Users who dislike the accent can restyle it from their own
//! GTK theme, because nothing is hardcoded in markup.

/// Brand accent, used for the active pack row and the category chips.
const ACCENT: &str = "#8b5cf6";

/// The stylesheet, embedded so the binary has no runtime data files.
const CSS: &str = r#"
@define-color kv_accent #8b5cf6;
@define-color kv_accent_soft alpha(@kv_accent, 0.16);

/* The one switch that silences everything. */
switch.kv-master:checked { background-color: @kv_accent; }
switch.kv-master:checked slider { background-color: #ffffff; }

/* Category filter chips under the search entry. */
.togglebutton.kv-chip {
    padding: 5px 14px;
    border-radius: 999px;
}
.togglebutton.kv-chip:checked {
    background-color: @kv_accent_soft;
    border-color: @kv_accent;
    color: @accent_color;
    font-weight: 600;
}

/* A category label in a pack row's prefix slot. */
label.kv-badge {
    padding: 2px 10px;
    border-radius: 999px;
    font-size: 0.85em;
    font-weight: 600;
    background-color: alpha(currentColor, 0.10);
}
label.kv-badge.kv-demo {
    background-color: alpha(@warning_color, 0.18);
    color: @warning_color;
}
/* The pack the engine has loaded. */
row.kv-active {
    background-color: @kv_accent_soft;
    font-weight: 600;
}

/* Engine stderr is diagnostic text, not prose. */
textview.kv-log {
    font-family: monospace;
    font-size: 0.92em;
}

/* Keeps the peak meter from jumping around while it idles. */
levelbar.kv-peak { min-height: 8px; }

/* The engine's "this needs you" line, pinned under the header bar. */
box.kv-alert {
    background-color: alpha(@warning_color, 0.12);
    border-bottom: 1px solid alpha(@warning_color, 0.30);
    padding: 7px 14px;
}
box.kv-alert label { color: @warning_color; }
"#;

/// Installs the stylesheet for the default display.
///
/// Called once from `startup`, before any window exists, so no widget is
/// ever laid out without the classes available.
pub fn install() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// The accent colour as a CSS string, for the rare place the UI needs it
/// outside a stylesheet rule (currently unused, kept so the palette lives
/// in exactly one place).
#[allow(dead_code)]
pub fn accent() -> &'static str {
    ACCENT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stylesheet_defines_every_class_the_ui_applies() {
        // The UI adds these classes; if CSS forgets one, the widget silently
        // falls back to the system look, which is exactly the kind of drift
        // this test exists to catch.
        for class in [
            "kv-master",
            "kv-chip",
            "kv-badge",
            "kv-demo",
            "kv-active",
            "kv-log",
            "kv-peak",
            "kv-alert",
        ] {
            assert!(
                CSS.contains(&format!(".{class}")),
                "style.css has no rule for .{class}"
            );
        }
    }

    #[test]
    fn the_stylesheet_never_hardcodes_a_light_background() {
        // Branding is layered on the system theme; forcing a background
        // here would fight dark mode.
        for forbidden in [
            "background: #fff",
            "background: white",
            "background-color: white",
        ] {
            assert!(
                !CSS.contains(forbidden),
                "the stylesheet must not pin a white background ({forbidden})"
            );
        }
    }

    #[test]
    fn the_accent_palette_lives_in_one_place() {
        assert!(CSS.contains(ACCENT), "the documented accent must match");
        assert_eq!(ACCENT, "#8b5cf6");
    }
}
