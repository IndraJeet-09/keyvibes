# Supported Environment (Phase 15)

**Status:** Implemented
**Date:** 2026-10-09
**Verified by:** `keyvibes compatibility-test`

KeyVibes is a Linux sound engine, not a desktop application. Its entire
platform surface is two interfaces:

| Interface | Provided by | Used for |
|---|---|---|
| evdev (`/dev/input/event*`) | the kernel | reading key presses |
| PipeWire (user session) | `pipewire` + `wireplumber` | rendering the mixed stream |

Everything else - window systems, toolkits, portals, notifications - is
deliberately out of scope.

## Supported

* Linux with evdev (`CONFIG_INPUT_EVDEV`). Tested on x86-64.
* A running PipeWire session owned by the logged-in user
  (`pipewire`, `pipewire-pulse`, `wireplumber`).
* Any logind/TTY session: a graphical login, a TTY login, a systemd user
  service, a container with `/dev/input` passed through.
* 48 kHz is the reference rate; `--rate` accepts other rates PipeWire
  exposes.

## Not required, and never read

KeyVibes does not depend on a display server, a compositor, a desktop shell,
or a session type. The following variables are never read by any file in the
workspace (this is enforced by `keyvibes compatibility-test`, which strips
comments before scanning so that prose like "no X11 here" cannot mask a
real access):

```text
DISPLAY
WAYLAND_DISPLAY
XDG_SESSION_TYPE
XDG_SESSION_DESKTOP
XDG_CURRENT_DESKTOP
DESKTOP_SESSION
```

So the answer to "does it work on X11?" and "does it work on Wayland?" is the
same: it works on both, and it works on neither - the engine never asks.

## Environment variables KeyVibes *does* read

| Variable | Purpose |
|---|---|
| `KEYVIBES_CONFIG` | override the config file path |
| `KEYVIBES_PACK` | override the pack to load (not a path) |
| `KEYVIBES_PACK_DIR` | prepend a sound-pack search directory |
| `XDG_CONFIG_HOME` | config location, default `~/.config` |
| `XDG_DATA_HOME` | pack location, default `~/.local/share` |
| `HOME` | last-resort location for both of the above |

`XDG_CONFIG_HOME` and `XDG_DATA_HOME` are base-directory conventions, not
desktop-session detection: they are honoured identically under a TTY, a
container, and any desktop.

## Permissions

Reading a keyboard needs read access to `/dev/input/event*`. Either:

* add yourself to the `input` group, or
* grant an ACL: `setfacl -Rm u:$USER:/dev/input`.

Rendering needs nothing beyond a reachable PipeWire socket. See
`docs/linux-permissions.md`.

## Dependency policy

No crate may introduce a display server, a toolkit, a portal, or a
compositor client - directly or transitively. The deny list checked against
every `Cargo.toml` and against `cargo tree` is:

```text
x11, x11rb, x11-dl, x11-clipboard, xcb, xkbcommon, libxkbcommon,
wayland-client, wayland-sys, wayland-protocols, wayland-backend,
wayland-scanner, wayland-dlopen, dbus, libdbus, dbus-crossroads, ashpd,
xdg-portal, gtk, glib, gdk, winit, tao, global-hotkey
```

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| `No keyboards found.` | not in the `input` group, or no keyboard attached |
| `evdev discovers keyboards` fails in `compatibility-test` | `/dev/input` not readable |
| `PipeWire ... not reachable` | session not started: `systemctl --user start pipewire` |
| `compatibility-test` reports a desktop crate | a dependency was added that should not be |

## Verifying

```bash
cargo run --quiet --bin keyvibes -- compatibility-test
```

The command exits non-zero if any check fails. Checks that need a resource
this session does not have (a PipeWire socket) are reported as `NOT RUN`,
never as a pass.
