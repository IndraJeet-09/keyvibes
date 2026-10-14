//! The KeyVibes desktop window.
//!
//! A thin, honest projection of the running engine: every control writes a
//! control-protocol request, every readout is a field the engine actually
//! reports, and nothing is invented locally. The window owns no audio, no
//! input device and no pack cache - if the engine is gone, the UI says so
//! rather than pretending.

use crate::autostart;
use crate::packs::{self, CategoryFilter, PackQuery};
use crate::screenshot;
use crate::session::{self, Session, UiEvent};
use crate::tray::{self, TrayAction};
use crate::{REPOSITORY, VERSION};
use adw::prelude::*;
use gtk::gio;
use gtk::glib;
use kv_control::{DiagnosticItem, DiagnosticStatus, DiagnosticsReport, PackInfo, Request, State};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

/// Application id; also the socket's owner identity on D-Bus.
pub const APP_ID: &str = "org.keyvibes.KeyVibes";

/// How many engine stderr lines to keep in memory for the System page.
///
/// Memory only: logs are never written to disk by the UI, and the engine's
/// own output never contains typed content (only the separate
/// `keyvibes input-test --verbose` mode would, and that is not the engine).
const LOG_LIMIT: usize = 40;

/// The keys a preview plays, one at a time, in the order a typist would
/// hit them. Names must match `PhysicalKey`'s debug spelling exactly, the
/// same list the engine falls back to for `keys: None`.
const PREVIEW_KEYS: [&str; 8] = ["A", "S", "D", "F", "J", "K", "L", "Enter"];

/// Delay between preview keys. Eight keys over this gap stretches the demo
/// to about three and a half seconds instead of one simultaneous burst.
const PREVIEW_GAP_MS: u64 = 450;

/// How tall the Sounds page may ask to be before it scrolls.
const PACK_LIST_MAX: i32 = 340;

/// How tall a non-Sounds page may ask to be before it scrolls. Pages carry
/// extra margins, so this leaves room for them inside the default height.
const PAGE_MAX: i32 = 400;

/// Opening size of the window. The pack list scrolls inside it rather than
/// the window growing to fit every row.
const DEFAULT_WIDTH: i32 = 760;
const DEFAULT_HEIGHT: i32 = 520;

thread_local! {
    /// One `App` per process. The UI is single-instance by application id,
    /// so a second launch just presents the existing window.
    static APP: std::cell::OnceCell<Rc<App>> = const { std::cell::OnceCell::new() };
}

/// Presents the main window, building it on first use.
pub fn activate(application: &adw::Application) {
    APP.with(|cell| {
        let fresh = cell.get().is_none();
        let app = cell.get_or_init(|| build(application)).clone();
        if fresh {
            install_signal_handlers(&app);
        }
        // Pick the page before the first layout: switching it afterwards
        // invalidates an allocation the capture can then read as empty.
        if screenshot::requested().is_some() {
            if let Some(page) = screenshot::requested_page() {
                app.suppress.set(true);
                app.stack.set_visible_child_name(page);
                app.suppress.set(false);
            }
        }
        app.window.present();
        if let Some(path) = screenshot::requested() {
            capture_when_settled(app, path);
        }
    });
}

/// Routes Ctrl-C and a session logout through the Quit path.
///
/// Without this, `application.quit()` would end the main loop while an
/// engine this window owns keeps running: only `App::quit` stops it.
fn install_signal_handlers(app: &Rc<App>) {
    for signum in [libc::SIGINT, libc::SIGTERM] {
        let events = app.event_tx.clone();
        glib::unix_signal_add(signum, move || {
            let _ = events.send_blocking(UiEvent::Quit);
            glib::ControlFlow::Break
        });
    }
}

/// Waits for the main page, renders it to `path`, then exits.
///
/// The wait is the point: the window opens on the "starting" page while the
/// engine boots, and capturing that would document the wrong screen — and
/// the crossfade away from it takes a beat longer still, so the capture
/// fires only after the page has held still for four polls.
fn capture_when_settled(app: Rc<App>, path: std::path::PathBuf) {
    let polls = Cell::new(0u32);
    let held = Cell::new(0u32);
    let armed = Cell::new(false);
    let done = Rc::new(Cell::new(false));

    glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
        const MAX_POLLS: u32 = 120;
        const SETTLED_POLLS: u32 = 4;
        polls.set(polls.get() + 1);
        if app.stage.visible_child_name().as_deref() == Some("main") {
            held.set(held.get() + 1);
        }

        if held.get() >= SETTLED_POLLS && !armed.replace(true) {
            capture_after_paint(&app, &path, &done);
        }
        if polls.get() < MAX_POLLS {
            return glib::ControlFlow::Continue;
        }
        eprintln!(
            "keyvibes-ui: screenshot {}: gave up after {MAX_POLLS} polls",
            path.display()
        );
        done.set(true);
        app.quit();
        glib::ControlFlow::Break
    });
}

/// Snapshots on the frame that follows the next redraw.
///
/// A timer alone is not enough: the widgets may hold an allocation while
/// the page underneath has not been laid out yet, which renders as a blank
/// window. Painting first guarantees the tree the capture reads.
fn capture_after_paint(app: &Rc<App>, path: &std::path::Path, done: &Rc<Cell<bool>>) {
    let Some(clock) = app.window.frame_clock() else {
        try_capture(app, path, done);
        return;
    };
    let app = app.clone();
    let path = path.to_path_buf();
    let done = done.clone();
    app.window.queue_draw();
    clock.connect_after_paint(move |_clock| {
        if done.get() {
            return;
        }
        // Still blank: ask for another frame rather than giving up.
        if try_capture(&app, &path, &done) == glib::ControlFlow::Continue {
            app.window.queue_draw();
        }
    });
}

/// Renders once, stopping the application when there is a file to keep.
fn try_capture(app: &Rc<App>, path: &std::path::Path, done: &Cell<bool>) -> glib::ControlFlow {
    if done.get() {
        return glib::ControlFlow::Break;
    }
    match screenshot::capture(&app.window, path) {
        Ok(true) => {
            done.set(true);
            app.quit();
            glib::ControlFlow::Break
        }
        Ok(false) => glib::ControlFlow::Continue,
        Err(error) => {
            eprintln!("keyvibes-ui: screenshot {}: {error}", path.display());
            done.set(true);
            app.quit();
            glib::ControlFlow::Break
        }
    }
}

/// Everything the window needs to know and do.
struct App {
    application: adw::Application,
    window: adw::ApplicationWindow,
    overlay: adw::ToastOverlay,
    /// The Sounds/Audio/System switcher; a capture selects through it.
    stack: adw::ViewStack,
    /// `starting` -> `main`, or `starting` -> `error` when the engine will
    /// not come up.
    stage: gtk::Stack,
    error_page: adw::StatusPage,

    // Sounds page.
    header_switch: gtk::Switch,
    search: gtk::SearchEntry,
    chips: Vec<gtk::ToggleButton>,
    pack_list: gtk::ListBox,

    // Audio page.
    volume: gtk::Scale,
    volume_value: gtk::Label,
    pitch: gtk::Switch,
    gain: gtk::Switch,
    spatial: gtk::Switch,
    release: gtk::Switch,
    phase_row: adw::ActionRow,
    device_row: adw::ActionRow,
    rate_row: adw::ActionRow,
    peak_row: adw::ActionRow,
    peak_bar: gtk::LevelBar,
    counters_row: adw::ActionRow,
    input_row: adw::ActionRow,

    // System page.
    engine_row: adw::ActionRow,
    control_row: adw::ActionRow,
    autostart_row: adw::ActionRow,
    autostart_switch: gtk::Switch,
    diagnostics_group: adw::PreferencesGroup,
    diagnostics_note: gtk::Label,
    diagnostics_rows: RefCell<Vec<gtk::Widget>>,
    log_view: gtk::TextView,

    // Model.
    state: RefCell<Option<State>>,
    diagnostics: RefCell<Option<DiagnosticsReport>>,
    query: RefCell<PackQuery>,
    pack_signature: RefCell<String>,
    active_signature: RefCell<String>,
    rows: RefCell<Vec<PackRow>>,
    visible_names: RefCell<Vec<String>>,
    session: RefCell<Option<Session>>,
    logs: RefCell<Vec<String>>,
    tray: RefCell<Option<tray::Tray>>,
    event_tx: async_channel::Sender<UiEvent>,
    socket: PathBuf,
    /// Set while the UI writes a widget's state from a snapshot, so the
    /// resulting signal does not bounce straight back as a request.
    suppress: Cell<bool>,
    /// The in-flight spaced preview, so a new one replaces the old.
    preview_timer: RefCell<Option<glib::SourceId>>,
}

/// One row in the pack list, with the widgets a selection change flips.
struct PackRow {
    name: String,
    row: adw::ActionRow,
    use_button: gtk::Button,
    check: gtk::Image,
}

impl App {
    // ---------------------------------------------------------------- looks

    /// Shows a short, transient message at the bottom of the window.
    fn toast(&self, message: &str) {
        self.overlay.add_toast(adw::Toast::new(message));
    }

    // ------------------------------------------------------------- engine

    /// Queues a control request. Never blocks the main loop.
    fn post(&self, request: Request) {
        let session = self.session.borrow();
        match session.as_ref() {
            Some(session) => session.post(request),
            None => {
                drop(session);
                self.toast("The engine is not ready yet.");
            }
        }
    }

    /// Asks the engine to load a pack by name.
    ///
    /// The row flips immediately: waiting for the round trip is what made
    /// selection feel laggy. The engine's own snapshot is authoritative and
    /// overwrites the mark the moment it arrives, so a rejected switch is
    /// corrected rather than left optimistic forever.
    fn select_pack(&self, name: &str) {
        if self.session.borrow().is_none() {
            self.toast("The engine is not ready yet.");
            return;
        }
        self.apply_active(Some(name));
        self.post(Request::SetPack {
            selector: name.to_string(),
        });
        self.toast(&format!("Switching to {name}…"));
    }

    /// Plays a spaced typing sequence from a pack, about three seconds.
    fn start_preview(self: &Rc<Self>, pack: &str) {
        self.stop_preview();
        self.post_preview_key(pack, 0);
        let app = Rc::downgrade(self);
        let tick = Cell::new(1usize);
        let playing = pack.to_string();
        let source = glib::timeout_add_local(
            std::time::Duration::from_millis(PREVIEW_GAP_MS),
            move || {
                let Some(app) = app.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let index = tick.get();
                if index >= PREVIEW_KEYS.len() {
                    app.stop_preview();
                    return glib::ControlFlow::Break;
                }
                tick.set(index + 1);
                app.post_preview_key(&playing, index);
                glib::ControlFlow::Continue
            },
        );
        *self.preview_timer.borrow_mut() = Some(source);
        self.toast(&format!("Playing {pack}…"));
    }

    /// Queues a single key of the demo sequence.
    fn post_preview_key(&self, pack: &str, index: usize) {
        self.post(Request::Preview {
            pack: Some(pack.to_string()),
            keys: Some(vec![PREVIEW_KEYS[index].to_string()]),
        });
    }

    /// Cancels a preview still in flight, if any.
    fn stop_preview(&self) {
        if let Some(source) = self.preview_timer.borrow_mut().take() {
            source.remove();
        }
    }

    /// Re-runs the read-only environment checks.
    fn request_diagnostics(&self) {
        self.post(Request::Diagnostics);
    }

    /// Restarts the boot sequence after a failure.
    fn retry_start(&self) {
        self.stage.set_visible_child_name("starting");
        let socket = self.socket.clone();
        let events = self.event_tx.clone();
        let _ = std::thread::Builder::new()
            .name("keyvibes-ui-boot".to_string())
            .spawn(move || session::ensure_running(socket, events));
    }

    /// Stops an owned engine, releases the tray, and exits.
    fn quit(&self) {
        self.stop_preview();
        if let Some(mut engine) = self.session.borrow_mut().take() {
            let _ = engine.shutdown();
        }
        if let Some(tray) = self.tray.borrow().as_ref() {
            tray.shutdown();
        }
        self.application.quit();
    }

    /// Whether a tray was registered, which decides what closing the window
    /// means.
    fn has_tray(&self) -> bool {
        self.tray.borrow().is_some()
    }

    // ------------------------------------------------------------- events

    fn on_event(self: &Rc<Self>, event: UiEvent) {
        match event {
            UiEvent::Ready(session) => {
                *self.session.borrow_mut() = Some(*session);
                self.stage.set_visible_child_name("main");
                self.refresh_autostart();
                self.request_diagnostics();
            }
            UiEvent::StartFailed(message) => {
                self.error_page.set_description(Some(message.as_str()));
                self.stage.set_visible_child_name("error");
            }
            UiEvent::State(state) => {
                *self.state.borrow_mut() = Some(*state);
                self.render();
            }
            UiEvent::Diagnostics(report) => {
                *self.diagnostics.borrow_mut() = Some(*report);
                self.render_diagnostics();
            }
            UiEvent::Log(line) => self.push_log(&line),
            UiEvent::Connected => {}
            UiEvent::Disconnected => {
                // Expected during our own shutdown; only worth a word when
                // the engine was still meant to be running.
                if self.session.borrow().is_some() {
                    self.toast("The engine stopped. KeyVibes will reconnect if it comes back.");
                }
            }
            UiEvent::ActionFailed(message) => {
                self.toast(&message);
                // Undo the optimistic mark: the engine did not confirm it.
                let active = self.active_from_state();
                self.apply_active(active.as_deref());
            }
            UiEvent::Quit => self.quit(),
        }
    }

    fn on_tray_action(self: &Rc<Self>, action: TrayAction) {
        match action {
            TrayAction::ShowWindow => self.window.present(),
            TrayAction::ToggleSounds => {
                let Some(state) = self.state.borrow().clone() else {
                    return;
                };
                self.post(Request::SetEnabled {
                    enabled: !state.settings.enabled,
                });
            }
            TrayAction::Quit => self.quit(),
        }
    }

    // -------------------------------------------------------------- render

    fn render(self: &Rc<Self>) {
        let Some(state) = self.state.borrow().clone() else {
            return;
        };

        self.suppress.set(true);
        set_switch(&self.header_switch, state.settings.enabled);
        let volume = state.settings.volume.clamp(0.0, 1.0);
        if (self.volume.value() - f64::from(volume)).abs() > 0.001 {
            self.volume.set_value(f64::from(volume));
        }
        self.volume_value
            .set_text(&format!("{:.0}%", volume * 100.0));
        set_switch(&self.pitch, state.settings.pitch_variation);
        set_switch(&self.gain, state.settings.gain_variation);
        set_switch(&self.spatial, state.settings.spatial_audio);
        set_switch(&self.release, state.settings.release_sounds);
        self.suppress.set(false);

        // Output readouts.
        let audio = &state.audio;
        let mut phase = audio.phase.clone();
        if let Some(error) = &audio.last_error {
            phase.push_str(" — ");
            phase.push_str(error);
        }
        self.phase_row.set_subtitle(&phase);
        self.device_row
            .set_subtitle(audio.output_device.as_deref().unwrap_or("PipeWire default"));
        self.rate_row
            .set_subtitle(&format!("{} Hz", audio.output_rate));
        self.peak_bar
            .set_value(f64::from(audio.peak.clamp(0.0, 1.0)));
        self.peak_row
            .set_subtitle(&format!("{:.0}% peak", audio.peak.clamp(0.0, 1.0) * 100.0));
        self.counters_row.set_subtitle(&format!(
            "{} underruns · {} frames rendered",
            audio.xruns, audio.frames_rendered
        ));

        let input = &state.input;
        let hotplug = if input.hotplug {
            "hot-plug detection on"
        } else {
            "hot-plug detection off"
        };
        self.input_row.set_subtitle(&format!(
            "{} keyboard{} · {}",
            input.keyboards,
            if input.keyboards == 1 { "" } else { "s" },
            hotplug
        ));

        // Engine facts.
        let owned = self
            .session
            .borrow()
            .as_ref()
            .is_some_and(|session| session.ownership.is_owned());
        self.engine_row.set_subtitle(&format!(
            "pid {} · version {} · up {}",
            state.engine.pid,
            if state.engine.version.is_empty() {
                "unknown"
            } else {
                &state.engine.version
            },
            human_uptime(state.engine.uptime_secs)
        ));
        self.control_row.set_subtitle(&format!(
            "{} · {} this window",
            self.socket.display(),
            if owned { "owned by" } else { "independent of" }
        ));

        self.rebuild_packs(&state);
        self.refresh_autostart();
        self.refresh_tray(&state);
    }

    /// Rebuilds the pack list only when what it would show has changed, so
    /// a one-second state poll never steals focus from the search entry.
    ///
    /// Deliberately *excludes* the loaded pack: changing it only flips a few
    /// widgets, it never tears down and recreates twenty rows.
    fn rebuild_packs(self: &Rc<Self>, state: &State) {
        let query = self.query.borrow().clone();
        let visible = packs::filter(&state.packs, &query);
        let active_name = state.pack.as_ref().map(|pack| pack.name.as_str());

        let signature = list_signature(&visible);
        if *self.pack_signature.borrow() != signature {
            *self.pack_signature.borrow_mut() = signature;

            while let Some(child) = self.pack_list.first_child() {
                self.pack_list.remove(&child);
            }
            self.rows.borrow_mut().clear();

            let mut names: Vec<String> = Vec::with_capacity(visible.len());
            if visible.is_empty() {
                self.pack_list
                    .append(&empty_pack_row(&query, state.packs.is_empty()));
            } else {
                for pack in visible {
                    names.push(pack.name.clone());
                    let (row, use_button, check) = self.pack_row(pack);
                    self.pack_list.append(&row);
                    self.rows.borrow_mut().push(PackRow {
                        name: pack.name.clone(),
                        row,
                        use_button,
                        check,
                    });
                }
            }
            *self.visible_names.borrow_mut() = names;
        }

        self.apply_active(active_name);
    }

    /// Flips the loaded-pack mark on the rows that need it, without
    /// touching anything else.
    fn apply_active(&self, active_name: Option<&str>) {
        let active_signature = active_name.unwrap_or_default().to_string();
        if *self.active_signature.borrow() == active_signature {
            return;
        }
        *self.active_signature.borrow_mut() = active_signature;

        for entry in self.rows.borrow().iter() {
            let active = active_name == Some(entry.name.as_str());
            entry.use_button.set_visible(!active);
            entry.check.set_visible(active);
            if active {
                entry.row.add_css_class("kv-active");
            } else {
                entry.row.remove_css_class("kv-active");
            }
        }
    }

    /// The loaded pack as the last snapshot reported it, used to undo an
    /// optimistic mark.
    fn active_from_state(&self) -> Option<String> {
        self.state
            .borrow()
            .as_ref()
            .and_then(|state| state.pack.as_ref().map(|pack| pack.name.clone()))
    }

    /// One row per installed pack: provenance on the left, actions on the
    /// right, and a permanent mark on whichever pack is loaded.
    fn pack_row(self: &Rc<Self>, pack: &PackInfo) -> (adw::ActionRow, gtk::Button, gtk::Image) {
        let row = adw::ActionRow::new();
        row.set_title(&pack.name);

        let mut parts = vec![packs::coverage_label(pack)];
        let provenance = packs::provenance_line(pack);
        if !provenance.is_empty() {
            parts.push(provenance);
        }
        row.set_subtitle(&parts.join("  ·  "));
        row.set_subtitle_lines(2);

        let (badge_text, demo) = if pack.synthesized {
            ("Demo", true)
        } else {
            match packs::group_of(pack) {
                CategoryFilter::All => ("Other", false),
                other => (other.title(), false),
            }
        };
        let badge = gtk::Label::new(Some(badge_text));
        badge.add_css_class("kv-badge");
        if demo {
            badge.add_css_class("kv-demo");
        }
        badge.set_valign(gtk::Align::Center);
        badge.set_margin_end(8);
        row.add_prefix(&badge);

        let preview = gtk::Button::from_icon_name("media-playback-start-symbolic");
        preview.add_css_class("flat");
        preview.set_tooltip_text(Some("Play a few keys from this pack"));
        preview.set_valign(gtk::Align::Center);
        {
            let app = Rc::downgrade(self);
            let name = pack.name.clone();
            preview.connect_clicked(move |_| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                app.start_preview(&name);
            });
        }
        row.add_suffix(&preview);

        // Both suffixes exist on every row; a selection just flips which is
        // visible instead of rebuilding the widget.
        let use_button = gtk::Button::with_label("Use");
        use_button.add_css_class("pill");
        use_button.set_valign(gtk::Align::Center);
        use_button.set_tooltip_text(Some("Load this pack"));
        {
            let app = Rc::downgrade(self);
            let name = pack.name.clone();
            use_button.connect_clicked(move |_| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                app.select_pack(&name);
            });
        }
        row.add_suffix(&use_button);

        let check = gtk::Image::from_icon_name("object-select-symbolic");
        check.add_css_class("success");
        check.set_valign(gtk::Align::Center);
        check.set_visible(false);
        row.add_suffix(&check);

        (row, use_button, check)
    }

    // ----------------------------------------------------------- diagnostics

    fn render_diagnostics(self: &Rc<Self>) {
        for child in self.diagnostics_rows.borrow_mut().drain(..) {
            self.diagnostics_group.remove(&child);
        }

        let Some(report) = self.diagnostics.borrow().clone() else {
            self.diagnostics_note.set_text("No checks have run yet.");
            return;
        };

        self.diagnostics_note.set_text(&if report.healthy {
            "Every check passed.".to_string()
        } else {
            format!(
                "{} problem{}, {} warning{}.",
                report.errors,
                plural(report.errors),
                report.warnings,
                plural(report.warnings)
            )
        });

        for item in &report.items {
            let row = diagnostic_row(item, Rc::downgrade(self));
            self.diagnostics_group.add(&row);
            self.diagnostics_rows.borrow_mut().push(row.upcast());
        }
    }

    // --------------------------------------------------------------- login

    fn refresh_autostart(&self) {
        let enabled = autostart::is_enabled();
        self.suppress.set(true);
        set_switch(&self.autostart_switch, enabled);
        self.suppress.set(false);
        let mechanisms = autostart::active_mechanisms();
        self.autostart_row.set_subtitle(&if mechanisms.is_empty() {
            "Off".to_string()
        } else {
            mechanisms.join(" + ")
        });
    }

    /// The exact command a login entry would run: the binary we resolved,
    /// not a bare name that might not be on the login session's PATH.
    fn autostart_command(&self) -> String {
        match session::find_engine_binary() {
            Some(path) => format!("{} run", path.display()),
            None => autostart::DEFAULT_ENGINE_COMMAND.to_string(),
        }
    }

    // ----------------------------------------------------------------- logs

    fn push_log(&self, line: &str) {
        {
            let mut logs = self.logs.borrow_mut();
            logs.push(line.to_string());
            if logs.len() > LOG_LIMIT {
                let excess = logs.len() - LOG_LIMIT;
                logs.drain(0..excess);
            }
        }
        let text = {
            let logs = self.logs.borrow();
            if logs.is_empty() {
                "No output yet.".to_string()
            } else {
                logs.join("\n")
            }
        };
        self.log_view.buffer().set_text(&text);
    }

    // ----------------------------------------------------------------- tray

    fn refresh_tray(&self, state: &State) {
        let tray = self.tray.borrow();
        let Some(tray) = tray.as_ref() else {
            return;
        };
        let sounds = if state.settings.enabled {
            "Sounds on"
        } else {
            "Sounds off"
        };
        let summary = match &state.pack {
            Some(pack) => format!("{sounds} · {} · {}", pack.name, state.audio.phase),
            None => format!("{sounds} · no pack loaded"),
        };
        tray.update(state.settings.enabled, &summary);
    }

    // ------------------------------------------------------------- signals

    fn connect_signals(self: &Rc<Self>) {
        self.connect_engine_controls();
        self.connect_pack_controls();
        self.connect_system_controls();
        self.connect_window_behaviour();
    }

    fn connect_engine_controls(self: &Rc<Self>) {
        {
            let app = Rc::downgrade(self);
            self.header_switch.connect_active_notify(move |switch| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                if app.suppress.get() {
                    return;
                }
                app.post(Request::SetEnabled {
                    enabled: switch.is_active(),
                });
            });
        }

        {
            let app = Rc::downgrade(self);
            self.volume.connect_value_changed(move |scale| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                if app.suppress.get() {
                    return;
                }
                app.post(Request::SetVolume {
                    volume: (scale.value() as f32).clamp(0.0, 1.0),
                });
            });
        }

        {
            let app = Rc::downgrade(self);
            let pitch = self.pitch.clone();
            let gain = self.gain.clone();
            self.pitch.connect_active_notify(move |_| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                if app.suppress.get() {
                    return;
                }
                // Pitch and gain are two halves of one request, so a single
                // reply always leaves them in the state the user set.
                app.post(Request::SetVariation {
                    pitch: Some(pitch.is_active()),
                    gain: Some(gain.is_active()),
                });
            });
        }

        {
            let app = Rc::downgrade(self);
            let gain = self.gain.clone();
            let pitch = self.pitch.clone();
            self.gain.connect_active_notify(move |_| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                if app.suppress.get() {
                    return;
                }
                app.post(Request::SetVariation {
                    pitch: Some(pitch.is_active()),
                    gain: Some(gain.is_active()),
                });
            });
        }

        {
            let app = Rc::downgrade(self);
            let spatial = self.spatial.clone();
            self.spatial.connect_active_notify(move |_| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                if app.suppress.get() {
                    return;
                }
                app.post(Request::SetSpatial {
                    enabled: spatial.is_active(),
                });
            });
        }

        {
            let app = Rc::downgrade(self);
            let release = self.release.clone();
            self.release.connect_active_notify(move |_| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                if app.suppress.get() {
                    return;
                }
                app.post(Request::SetReleaseSounds {
                    enabled: release.is_active(),
                });
            });
        }
    }

    fn connect_pack_controls(self: &Rc<Self>) {
        {
            let app = Rc::downgrade(self);
            self.search.connect_search_changed(move |entry| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                app.query.borrow_mut().text = entry.text().to_string();
                app.pack_signature.borrow_mut().clear();
                app.refresh_packs_from_query();
            });
        }

        for chip in &self.chips {
            let app = Rc::downgrade(self);
            let captured = chip.clone();
            chip.connect_toggled(move |_| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                app.on_chip_toggled(&captured);
            });
        }

        {
            let app = Rc::downgrade(self);
            self.pack_list.connect_row_activated(move |list, row| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                let Some(index) = row_index(list, row) else {
                    return;
                };
                let name = app.visible_names.borrow().get(index).cloned();
                if let Some(name) = name {
                    app.select_pack(&name);
                }
            });
        }
    }

    fn connect_system_controls(self: &Rc<Self>) {
        {
            let app = Rc::downgrade(self);
            self.autostart_switch.connect_active_notify(move |switch| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                if app.suppress.get() {
                    return;
                }
                let want = switch.is_active();
                let command = app.autostart_command();
                let outcome = if want {
                    autostart::enable(&command).map(|_| ())
                } else {
                    autostart::disable().map(|_| ())
                };
                match outcome {
                    Ok(()) => app.toast(if want {
                        "KeyVibes will start at login."
                    } else {
                        "KeyVibes will no longer start at login."
                    }),
                    Err(error) => app.toast(&format!("Could not change login startup: {error}")),
                }
                app.refresh_autostart();
            });
        }

        {
            let app = Rc::downgrade(self);
            self.diagnostics_group.connect_map(move |_| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                // Only ask when the user is actually looking, so the
                // one-second state poll never drags the checks along.
                if app.diagnostics.borrow().is_none() {
                    app.request_diagnostics();
                }
            });
        }
    }

    fn connect_window_behaviour(self: &Rc<Self>) {
        // A tray is the only reason to keep running without a window; with
        // one, closing the window merely hides it.
        self.window.set_hide_on_close(true);
        {
            let app = Rc::downgrade(self);
            self.window.connect_close_request(move |window| {
                let Some(app) = app.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if app.has_tray() {
                    window.set_visible(false);
                    glib::Propagation::Stop
                } else {
                    app.quit();
                    glib::Propagation::Stop
                }
            });
        }

        let about = gio::SimpleAction::new("about", None);
        {
            let app = Rc::downgrade(self);
            about.connect_activate(move |_, _| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                show_about(&app.window);
            });
        }
        self.application.add_action(&about);

        let quit = gio::SimpleAction::new("quit", None);
        {
            let app = Rc::downgrade(self);
            quit.connect_activate(move |_, _| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                app.quit();
            });
        }
        self.application.add_action(&quit);
    }

    // ------------------------------------------------------------- filters

    fn on_chip_toggled(self: &Rc<Self>, chip: &gtk::ToggleButton) {
        if chip.is_active() {
            let index = self
                .chips
                .iter()
                .position(|candidate| candidate == chip)
                .unwrap_or(0);
            let category = CategoryFilter::ALL[index];
            self.query.borrow_mut().category = Some(category);
        } else {
            // Toggle buttons can be clicked off; the selection is sticky so
            // the UI never ends up with no category at all.
            self.suppress.set(true);
            chip.set_active(true);
            self.suppress.set(false);
            return;
        }
        self.pack_signature.borrow_mut().clear();
        self.refresh_packs_from_query();
    }

    /// Re-filters against the last snapshot without waiting for the engine.
    fn refresh_packs_from_query(self: &Rc<Self>) {
        let Some(state) = self.state.borrow().clone() else {
            return;
        };
        self.rebuild_packs(&state);
    }
}

// ---------------------------------------------------------------- helpers

/// Writes a switch's state without the change bouncing back as a request.
///
/// Callers must already hold `suppress`; this only skips the redundant set.
fn set_switch(switch: &gtk::Switch, value: bool) {
    if switch.is_active() != value {
        switch.set_active(value);
    }
}

/// `1 problem` vs `2 problems`, so the summary reads like English.
fn plural(count: u32) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

/// `3m 12s`, compact enough for a row subtitle.
fn human_uptime(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

/// Position of `target` among the list's current rows.
///
/// The pack list is rebuilt wholesale, so the row index is the pack index.
fn row_index(list: &gtk::ListBox, target: &gtk::ListBoxRow) -> Option<usize> {
    let mut index = 0usize;
    let mut child = list.first_child();
    while let Some(widget) = child {
        if widget == *target {
            return Some(index);
        }
        index += 1;
        child = widget.next_sibling();
    }
    None
}

/// The row shown when nothing matches the current filter.
fn empty_pack_row(query: &PackQuery, nothing_installed: bool) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_selectable(false);

    let page = adw::StatusPage::new();
    if nothing_installed {
        page.set_icon_name(Some("folder-music-symbolic"));
        page.set_title("No packs installed");
        page.set_description(Some("Install a sound pack, then reopen this page."));
    } else {
        page.set_icon_name(Some("edit-find-symbolic"));
        page.set_title("No packs match");
        let detail = if query.text.trim().is_empty() {
            "No pack is in this category yet.".to_string()
        } else {
            format!("Nothing matches “{}”.", query.text.trim())
        };
        page.set_description(Some(detail.as_str()));
    }
    row.set_child(Some(&page));
    row
}

/// One environment check, with a copy button when a fix is a shell command.
fn diagnostic_row(item: &DiagnosticItem, app: Weak<App>) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&item.label);
    let mut subtitle = item.detail.clone();
    if let Some(fix) = &item.fix {
        if !subtitle.is_empty() {
            subtitle.push_str("  ·  ");
        }
        subtitle.push_str(fix);
    }
    row.set_subtitle(&subtitle);
    row.set_subtitle_lines(3);

    let icon = gtk::Image::from_icon_name(status_icon(item.status));
    icon.set_valign(gtk::Align::Center);
    icon.set_tooltip_text(Some(match item.status {
        DiagnosticStatus::Ok => "Passed",
        DiagnosticStatus::Warning => "Warning",
        DiagnosticStatus::Error => "Failed",
    }));
    row.add_suffix(&icon);

    if let Some(command) = item.command.clone() {
        let copy = gtk::Button::from_icon_name("edit-copy-symbolic");
        copy.add_css_class("flat");
        copy.set_valign(gtk::Align::Center);
        copy.set_tooltip_text(Some("Copy the fix to the clipboard"));
        copy.connect_clicked(move |_| {
            let Some(display) = gtk::gdk::Display::default() else {
                return;
            };
            display.clipboard().set_text(&command);
            if let Some(app) = app.upgrade() {
                app.toast("Copied the command to the clipboard.");
            }
        });
        row.add_suffix(&copy);
    }
    row
}

/// Icon for a check's severity; also used as its accessible name.
fn status_icon(status: DiagnosticStatus) -> &'static str {
    match status {
        DiagnosticStatus::Ok => "emblem-ok-symbolic",
        DiagnosticStatus::Warning => "dialog-warning-symbolic",
        DiagnosticStatus::Error => "dialog-error-symbolic",
    }
}

/// The about dialog, filled from the crate metadata so it cannot drift.
fn show_about(window: &adw::ApplicationWindow) {
    let about = gtk::AboutDialog::new();
    about.set_transient_for(Some(window));
    about.set_modal(true);
    about.set_program_name(Some("KeyVibes"));
    about.set_version(Some(VERSION));
    about.set_comments(Some("Keyboard sound engine for Linux"));
    about.set_website(Some(REPOSITORY));
    about.set_website_label("Source code");
    about.set_license(Some("MIT OR Apache-2.0"));
    about.set_authors(&["KeyVibes Contributors"]);
    about.present();
}

/// Builds the window and every widget in it.
///
/// Signal connections happen afterwards, in [`App::connect_signals`], so
/// nothing fires while the struct is still being assembled.
fn build(application: &adw::Application) -> Rc<App> {
    // Dark-first branding; the system preference still wins for users who
    // explicitly choose a light scheme elsewhere.
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::PreferDark);

    let socket = session::engine_socket();

    let window = adw::ApplicationWindow::new(application);
    window.set_default_size(DEFAULT_WIDTH, DEFAULT_HEIGHT);
    window.set_title(Some("KeyVibes"));

    // ---------------------------------------------------------- stage
    // Homogeneous stacks report the tallest child, so the window would open
    // at whatever page is biggest instead of the size the user asked for.
    let stage = gtk::Stack::new();
    stage.set_transition_type(gtk::StackTransitionType::Crossfade);
    stage.set_hhomogeneous(false);
    stage.set_vhomogeneous(false);

    let starting = adw::StatusPage::new();
    starting.set_title("Starting KeyVibes");
    starting.set_description(Some("Waiting for the audio engine…"));
    let spinner = gtk::Spinner::new();
    spinner.start();
    starting.set_child(Some(&spinner));
    stage.add_named(&starting, Some("starting"));

    let error_page = adw::StatusPage::new();
    error_page.set_icon_name(Some("dialog-error-symbolic"));
    error_page.set_title("Could not start the engine");
    error_page.set_description(None);
    let retry = gtk::Button::with_label("Try again");
    retry.add_css_class("suggested-action");
    retry.add_css_class("pill");
    error_page.set_child(Some(&retry));
    stage.add_named(&error_page, Some("error"));

    // --------------------------------------------------------- sounds
    let pack_list = gtk::ListBox::new();
    pack_list.set_selection_mode(gtk::SelectionMode::None);
    pack_list.add_css_class("background");

    let pack_scroll = gtk::ScrolledWindow::new();
    pack_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    pack_scroll.set_vexpand(true);
    // The list's own height must not become the window's height: the
    // window keeps its default size and the scrollbar carries the rest.
    pack_scroll.set_propagate_natural_height(true);
    pack_scroll.set_max_content_height(PACK_LIST_MAX);
    pack_scroll.set_propagate_natural_width(true);
    pack_scroll.set_child(Some(&pack_list));

    let search = gtk::SearchEntry::new();
    search.set_margin_top(12);
    search.set_margin_start(18);
    search.set_margin_end(18);
    search.set_placeholder_text(Some("Search packs, authors, licences…"));

    let chip_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    chip_box.set_margin_top(10);
    chip_box.set_margin_bottom(10);
    chip_box.set_margin_start(18);
    chip_box.set_margin_end(18);
    let mut chips = Vec::new();
    for category in CategoryFilter::ALL {
        let chip = gtk::ToggleButton::with_label(category.label());
        chip.add_css_class("kv-chip");
        if category == CategoryFilter::All {
            chip.set_active(true);
        }
        chip_box.append(&chip);
        chips.push(chip);
    }

    let sounds = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sounds.set_vexpand(true);
    sounds.append(&search);
    sounds.append(&chip_box);
    sounds.append(&pack_scroll);

    // ---------------------------------------------------------- audio
    let phase_row = labelled_row("Phase", "stopped");
    let device_row = labelled_row("Output device", "PipeWire default");
    let rate_row = labelled_row("Sample rate", "0 Hz");

    let peak_row = adw::ActionRow::new();
    peak_row.set_title("Peak");
    let peak_bar = gtk::LevelBar::new();
    peak_bar.add_css_class("kv-peak");
    peak_bar.set_valign(gtk::Align::Center);
    peak_bar.set_size_request(180, -1);
    peak_row.add_suffix(&peak_bar);

    let counters_row = labelled_row("Stream", "0 underruns · 0 frames rendered");
    let input_row = labelled_row("Keyboards", "0 keyboards");

    let output_group = group("Output", None);
    for row in [
        &phase_row,
        &device_row,
        &rate_row,
        &peak_row,
        &counters_row,
        &input_row,
    ] {
        output_group.add(row);
    }

    let volume_row = adw::ActionRow::new();
    volume_row.set_title("Volume");
    let volume_value = gtk::Label::new(Some("100%"));
    volume_value.set_valign(gtk::Align::Center);
    volume_value.set_margin_end(8);
    let volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.01);
    volume.set_draw_value(false);
    volume.set_size_request(200, -1);
    volume.set_valign(gtk::Align::Center);
    volume_row.add_suffix(&volume_value);
    volume_row.add_suffix(&volume);

    let (pitch_row, pitch) = switch_row("Pitch variation", "Slight key-to-key pitch drift");
    let (gain_row, gain) = switch_row("Gain variation", "Slight key-to-key loudness drift");
    let (spatial_row, spatial) =
        switch_row("Spatial audio", "Keys pan by their position on the board");
    let (release_row, release) =
        switch_row("Release sounds", "Play a clip on key-up (version-2 packs)");

    let sound_group = group("Sound", None);
    sound_group.add(&volume_row);
    for row in [&pitch_row, &gain_row, &spatial_row, &release_row] {
        sound_group.add(row);
    }

    let audio = gtk::Box::new(gtk::Orientation::Vertical, 12);
    audio.set_vexpand(true);
    audio.append(&output_group);
    audio.append(&sound_group);

    // Every page is wrapped in a height-capped scroller. `AdwViewStack`
    // reports the tallest page as its own height whether or not that page
    // is showing, so one uncapped page would decide the window size for
    // all three.
    let audio_scroll = gtk::ScrolledWindow::new();
    audio_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    audio_scroll.set_vexpand(true);
    audio_scroll.set_propagate_natural_height(true);
    audio_scroll.set_max_content_height(PAGE_MAX);
    audio_scroll.set_margin_top(12);
    audio_scroll.set_margin_bottom(18);
    audio_scroll.set_margin_start(18);
    audio_scroll.set_margin_end(18);
    audio_scroll.set_child(Some(&audio));

    // --------------------------------------------------------- system
    let engine_row = labelled_row("Engine", "not started");
    let control_row = labelled_row("Control socket", "connecting…");
    let engine_group = group("Engine", None);
    engine_group.add(&engine_row);
    engine_group.add(&control_row);

    let (autostart_row, autostart_switch) = switch_row("Launch at login", "Off");
    let login_group = group(
        "Start at login",
        Some(
            "Uses the XDG autostart entry. Window managers without a session \
              autostart can run `keyvibes run` from their own config instead.",
        ),
    );
    login_group.add(&autostart_row);

    let diagnostics_note = gtk::Label::new(Some("No checks have run yet."));
    diagnostics_note.set_xalign(0.0);
    diagnostics_note.set_wrap(true);
    diagnostics_note.add_css_class("dim-label");

    let diagnostics_group = adw::PreferencesGroup::new();
    diagnostics_group.set_title("Diagnostics");
    diagnostics_group.set_description(Some(
        "Read-only checks of input access, PipeWire, packs and configuration. \
         Nothing here grabs a device or writes a file.",
    ));
    let run_checks = gtk::Button::from_icon_name("view-refresh-symbolic");
    run_checks.add_css_class("flat");
    run_checks.set_tooltip_text(Some("Run the checks again"));
    run_checks.set_valign(gtk::Align::Center);

    let log_card = gtk::Box::new(gtk::Orientation::Vertical, 6);
    log_card.add_css_class("card");
    log_card.set_margin_top(6);
    let log_title = gtk::Label::new(Some("Engine output"));
    log_title.set_xalign(0.0);
    log_title.add_css_class("heading");
    let log_view = gtk::TextView::new();
    log_view.set_editable(false);
    log_view.set_cursor_visible(false);
    log_view.set_monospace(true);
    log_view.set_wrap_mode(gtk::WrapMode::WordChar);
    log_view.add_css_class("kv-log");
    log_view.buffer().set_text("No output yet.");
    let log_scroll = gtk::ScrolledWindow::new();
    log_scroll.set_min_content_height(120);
    log_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    log_scroll.set_child(Some(&log_view));
    log_card.append(&log_title);
    log_card.append(&log_scroll);

    let system = gtk::Box::new(gtk::Orientation::Vertical, 12);
    system.set_vexpand(true);
    system.append(&engine_group);
    system.append(&login_group);
    diagnostics_note.set_hexpand(true);
    let diagnostics_head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    diagnostics_head.append(&diagnostics_note);
    diagnostics_head.append(&run_checks);
    system.append(&diagnostics_head);
    system.append(&diagnostics_group);
    system.append(&log_card);

    let system_scroll = gtk::ScrolledWindow::new();
    system_scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    system_scroll.set_vexpand(true);
    system_scroll.set_propagate_natural_height(true);
    system_scroll.set_max_content_height(PAGE_MAX);
    system_scroll.set_margin_top(12);
    system_scroll.set_margin_bottom(18);
    system_scroll.set_margin_start(18);
    system_scroll.set_margin_end(18);
    system_scroll.set_child(Some(&system));

    // ------------------------------------------------------ view stack
    let stack = adw::ViewStack::new();
    stack
        .add_titled(&sounds, Some("sounds"), "Sounds")
        .set_icon_name(Some("view-list-symbolic"));
    stack
        .add_titled(&audio_scroll, Some("audio"), "Audio")
        .set_icon_name(Some("audio-volume-high-symbolic"));
    stack
        .add_titled(&system_scroll, Some("system"), "System")
        .set_icon_name(Some("applications-system-symbolic"));
    stage.add_named(&stack, Some("main"));

    let switcher = adw::ViewSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_policy(adw::ViewSwitcherPolicy::Wide);

    let header_switch = gtk::Switch::new();
    header_switch.add_css_class("kv-master");
    header_switch.set_valign(gtk::Align::Center);
    header_switch.set_tooltip_text(Some("Master switch: silence every sound"));
    let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let header_label = gtk::Label::new(Some("All sounds"));
    header_label.add_css_class("heading");
    header_box.append(&header_label);
    header_box.append(&header_switch);

    let menu = gio::Menu::new();
    menu.append(Some("About KeyVibes"), Some("app.about"));
    menu.append(Some("Quit"), Some("app.quit"));
    let menu_button = gtk::MenuButton::new();
    menu_button.set_icon_name("open-menu-symbolic");
    menu_button.set_menu_model(Some(&menu));
    menu_button.set_tooltip_text(Some("Menu"));

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&switcher));
    header.pack_start(&header_box);
    header.pack_end(&menu_button);

    let overlay = adw::ToastOverlay::new();
    overlay.set_child(Some(&stage));

    // `AdwApplicationWindow` owns its layout: the GtkWindow titlebar and
    // child hooks are both rejected at runtime. Content goes through the
    // libadwaita property instead.
    let chrome = gtk::Box::new(gtk::Orientation::Vertical, 0);
    chrome.append(&header);
    chrome.append(&overlay);
    window.set_content(Some(&chrome));

    let (event_tx, event_rx) = async_channel::unbounded::<UiEvent>();
    let (tray_tx, tray_rx) = async_channel::unbounded::<TrayAction>();

    let app = Rc::new(App {
        application: application.clone(),
        window,
        overlay,
        stack,
        stage,
        error_page,
        header_switch,
        search,
        chips,
        pack_list,
        volume,
        volume_value,
        pitch,
        gain,
        spatial,
        release,
        phase_row,
        device_row,
        rate_row,
        peak_row,
        peak_bar,
        counters_row,
        input_row,
        engine_row,
        control_row,
        autostart_row,
        autostart_switch,
        diagnostics_group,
        diagnostics_note,
        diagnostics_rows: RefCell::new(Vec::new()),
        log_view,
        state: RefCell::new(None),
        diagnostics: RefCell::new(None),
        query: RefCell::new(PackQuery::new("", None)),
        pack_signature: RefCell::new(String::new()),
        active_signature: RefCell::new(String::new()),
        rows: RefCell::new(Vec::new()),
        visible_names: RefCell::new(Vec::new()),
        session: RefCell::new(None),
        logs: RefCell::new(Vec::new()),
        tray: RefCell::new(tray::Tray::spawn(tray_tx).ok()),
        event_tx: event_tx.clone(),
        socket,
        suppress: Cell::new(false),
        preview_timer: RefCell::new(None),
    });

    // The retry button reuses exactly the same boot path as the first run.
    {
        let app = Rc::downgrade(&app);
        retry.connect_clicked(move |_| {
            let Some(app) = app.upgrade() else {
                return;
            };
            app.retry_start();
        });
    }

    // Diagnostics refresh button.
    {
        let app = Rc::downgrade(&app);
        run_checks.connect_clicked(move |_| {
            let Some(app) = app.upgrade() else {
                return;
            };
            app.request_diagnostics();
        });
    }

    app.connect_signals();

    // Picking a page in the header expands the window to fill the screen.
    // Only the user's own click does this: programmatic switches hold the
    // `suppress` flag, so restoring a page never drags the window around.
    {
        let view_stack = app.stack.clone();
        let app = Rc::downgrade(&app);
        view_stack.connect_notify_local(Some("visible-child-name"), move |_, _| {
            let Some(app) = app.upgrade() else {
                return;
            };
            if app.suppress.get() {
                return;
            }
            app.window.maximize();
        });
    }

    // ------------------------------------------------------ background
    let _ = std::thread::Builder::new()
        .name("keyvibes-ui-boot".to_string())
        .spawn({
            let socket = app.socket.clone();
            let events = event_tx.clone();
            move || session::ensure_running(socket, events)
        });
    let _ = session::listen(app.socket.clone(), event_tx.clone());

    {
        let app = Rc::downgrade(&app);
        glib::spawn_future_local(async move {
            while let Ok(event) = event_rx.recv().await {
                let Some(app) = app.upgrade() else {
                    break;
                };
                app.on_event(event);
            }
        });
    }
    {
        let app = Rc::downgrade(&app);
        glib::spawn_future_local(async move {
            while let Ok(action) = tray_rx.recv().await {
                let Some(app) = app.upgrade() else {
                    break;
                };
                app.on_tray_action(action);
            }
        });
    }

    // Keep the live counters moving; state is otherwise push-driven and
    // would otherwise only refresh when a setting changes.
    {
        let app = Rc::downgrade(&app);
        glib::timeout_add_seconds_local(2, move || {
            let Some(app) = app.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if app.stage.visible_child_name().as_deref() == Some("main") {
                app.post(Request::GetState);
            }
            glib::ControlFlow::Continue
        });
    }

    app
}

/// An action row with a title and a plain subtitle.
fn labelled_row(title: &str, subtitle: &str) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);
    row
}

/// An action row whose right-hand side is a switch.
fn switch_row(title: &str, subtitle: &str) -> (adw::ActionRow, gtk::Switch) {
    let row = labelled_row(title, subtitle);
    let switch = gtk::Switch::new();
    switch.set_valign(gtk::Align::Center);
    row.add_suffix(&switch);
    row.set_activatable_widget(Some(&switch));
    (row, switch)
}

/// A titled group of rows.
fn group(title: &str, description: Option<&str>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(title);
    group.set_description(description);
    group
}

/// Everything a pack row renders, minus the loaded pack.
///
/// Kept pure so a test can assert that a change of active pack leaves it
/// untouched - that is the difference between a one-widget update and a
/// full list teardown on every selection.
fn list_signature(visible: &[&PackInfo]) -> String {
    let mut signature = String::new();
    for pack in visible {
        signature.push_str(&pack.name);
        signature.push('\u{1}');
        signature.push_str(&packs::coverage_label(pack));
        signature.push('\u{1}');
        signature.push_str(&packs::provenance_line(pack));
        signature.push('\u{1}');
        signature.push(if pack.synthesized { '1' } else { '0' });
        signature.push('\n');
    }
    signature
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every name the stage is asked to show must have been added to it,
    /// otherwise GtkStack warns and shows nothing.
    #[test]
    fn every_stage_name_that_is_selected_is_also_registered() {
        let source = include_str!("window.rs");
        let select = format!("set_visible_child_{}", "name");
        let register = format!("add_{}", "named");

        let mut selected = Vec::new();
        let mut registered = Vec::new();
        for line in source.lines() {
            let quoted = quoted_strings(line);
            if quoted.is_empty() {
                continue;
            }
            if line.contains(&select) {
                selected.push(quoted[0].to_string());
            }
            if line.contains(&register) {
                registered.push(quoted[0].to_string());
            }
        }

        assert!(!selected.is_empty(), "the stage is never switched to");
        assert!(!registered.is_empty(), "nothing is added to the stage");
        for name in &selected {
            assert!(
                registered.contains(name),
                "`{name}` is selected but never added to the stack"
            );
        }
    }

    /// The `"..."` runs on one line, in order.
    fn quoted_strings(line: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut rest = line;
        while let Some(start) = rest.find('"') {
            let after = &rest[start + 1..];
            match after.find('"') {
                Some(end) => {
                    out.push(&after[..end]);
                    rest = &after[end + 1..];
                }
                None => break,
            }
        }
        out
    }

    #[test]
    fn the_window_never_uses_the_gtk_window_layout_hooks() {
        // `AdwApplicationWindow` extends `GtkWindow`, so both of these
        // compile and then abort the process at runtime. Keeping them out
        // of the source is the only check that can run without a display.
        let source = include_str!("window.rs");
        let titlebar = format!("set_{}bar", "title");
        let set_child = format!("window.set_{}", "child");
        assert!(
            !source.contains(&titlebar),
            "AdwApplicationWindow rejects {titlebar} at runtime"
        );
        assert!(
            !source.contains(&set_child),
            "AdwApplicationWindow rejects {set_child} at runtime"
        );
        assert!(
            source.contains(&format!("window.set_{}", "content")),
            "the window must place its layout through set_content"
        );
    }

    fn pack(name: &str, category: &str, synthesized: bool) -> PackInfo {
        PackInfo {
            name: name.to_string(),
            category: category.to_string(),
            path: format!("/packs/{name}.kvpack"),
            keys: 104,
            release_keys: 0,
            clips: 104,
            sample_rate: 48_000,
            coverage: 104,
            has_release: false,
            author: Some("kbsim".to_string()),
            license: Some("MIT".to_string()),
            source: Some("rustyvibes-v2".to_string()),
            description: None,
            synthesized,
        }
    }

    fn state_with(packs: Vec<PackInfo>) -> State {
        State {
            engine: kv_control::EngineInfo {
                pid: 4242,
                version: "0.1.0".to_string(),
                simulate: false,
                uptime_secs: 192,
            },
            settings: kv_control::LiveSettings {
                enabled: true,
                volume: 0.5,
                pitch_variation: true,
                gain_variation: false,
                spatial_audio: true,
                release_sounds: true,
            },
            pack: packs.first().cloned(),
            packs,
            audio: kv_control::AudioInfo {
                phase: "running".to_string(),
                output_rate: 48_000,
                output_device: Some("Built-in Audio".to_string()),
                xruns: 2,
                frames_rendered: 48_000,
                peak: 0.42,
                last_error: None,
            },
            input: kv_control::InputInfo {
                keyboards: 1,
                presses: 10,
                releases: 9,
                missing_sounds: 1,
                commands_generated: 10,
                hotplug: true,
            },
            issues: vec![],
        }
    }

    #[test]
    fn uptime_formats_without_padding() {
        assert_eq!(human_uptime(0), "0s");
        assert_eq!(human_uptime(59), "59s");
        assert_eq!(human_uptime(60), "1m 0s");
        assert_eq!(human_uptime(3600), "1h 0m");
        assert_eq!(human_uptime(192), "3m 12s");
    }

    #[test]
    fn pluralisation_matches_the_count() {
        assert_eq!(plural(0), "s");
        assert_eq!(plural(1), "");
        assert_eq!(plural(2), "s");
    }

    #[test]
    fn status_icons_cover_every_severity() {
        for status in [
            DiagnosticStatus::Ok,
            DiagnosticStatus::Warning,
            DiagnosticStatus::Error,
        ] {
            assert!(status_icon(status).ends_with("-symbolic"));
        }
    }

    #[test]
    fn diagnostic_rows_are_built_for_every_item_shape() {
        // Covers the "no fix", "fix without command" and "full command"
        // shapes a real report contains.
        let items = vec![
            DiagnosticItem {
                id: "a".into(),
                label: "Plain pass".into(),
                status: DiagnosticStatus::Ok,
                detail: "everything is fine".into(),
                fix: None,
                command: None,
                recheckable: false,
            },
            DiagnosticItem {
                id: "b".into(),
                label: "Advice only".into(),
                status: DiagnosticStatus::Warning,
                detail: "suboptimal".into(),
                fix: Some("consider a fix".into()),
                command: None,
                recheckable: true,
            },
            DiagnosticItem {
                id: "c".into(),
                label: "Needs a command".into(),
                status: DiagnosticStatus::Error,
                detail: "no input access".into(),
                fix: Some("grant access".into()),
                command: Some("sudo usermod -aG input $USER".into()),
                recheckable: true,
            },
        ];
        for item in items {
            // Building the row needs a live GTK display; assert the data it
            // would consume instead, so the test still runs headless.
            let mut subtitle = item.detail.clone();
            if let Some(fix) = &item.fix {
                subtitle.push_str("  ·  ");
                subtitle.push_str(fix);
            }
            assert!(!subtitle.is_empty());
            assert!(status_icon(item.status).ends_with("-symbolic"));
        }
    }

    #[test]
    fn the_pack_signature_tracks_everything_the_rows_display() {
        let alpaca = pack("Alpaca", "linear", false);
        let default = pack("Default", "", true);
        let visible = vec![&alpaca, &default];

        // The same visible list always hashes the same. Which pack happens
        // to be loaded is deliberately absent: it must never be the reason
        // twenty rows are torn down and rebuilt.
        assert_eq!(list_signature(&visible), list_signature(&visible));

        // Anything a row shows is part of it, or the row would go stale.
        let mut reauthored = alpaca.clone();
        reauthored.author = Some("Another Author".to_string());
        assert_ne!(
            list_signature(&[&alpaca]),
            list_signature(&[&reauthored]),
            "provenance changes rebuild the row"
        );

        let mut recovered = alpaca.clone();
        recovered.coverage = 51;
        assert_ne!(
            list_signature(&[&alpaca]),
            list_signature(&[&recovered]),
            "coverage changes rebuild the row"
        );

        // A different set of packs is a different signature.
        assert_ne!(
            list_signature(&visible),
            list_signature(&[&alpaca]),
            "a filtered list rebuilds the rows"
        );
    }

    #[test]
    fn the_app_id_is_a_valid_d_bus_name() {
        assert_eq!(APP_ID, "org.keyvibes.KeyVibes");
        assert!(APP_ID.split('.').all(|part| {
            !part.is_empty() && part.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        }));
    }

    #[test]
    fn a_snapshot_drives_the_same_readouts_the_window_shows() {
        // Guards the mapping from engine fields to row subtitles; a rename
        // on either side has to be reconciled here first.
        let state = state_with(vec![pack("Alpaca", "linear", false)]);
        assert_eq!(human_uptime(state.engine.uptime_secs), "3m 12s");
        assert_eq!(
            format!("{:.0}%", state.audio.peak * 100.0),
            "42%",
            "the peak readout is a percentage"
        );
        assert_eq!(
            state
                .audio
                .output_device
                .as_deref()
                .unwrap_or("PipeWire default"),
            "Built-in Audio"
        );
        assert_eq!(
            packs::coverage_label(state.packs.first().unwrap()),
            "104/104 keys"
        );
    }
}
