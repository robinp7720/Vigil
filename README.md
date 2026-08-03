# Vigil

Vigil is a Wayland-native desktop cockpit: it watches the session, keeps
urgent context visible, and hands deeper searches and actions to Luma.

## Prerequisite release build

Build the supervised binary before installing the desktop wiring:

```bash
cargo build --release
```

From the dotfiles repository root, use
`cargo build --manifest-path tools/bar/Cargo.toml --release`.

`config.example.toml` documents the complete tracked configuration shape. The
dotfiles checkout installs its live configuration at `~/.config/vigil/config.toml`.

`setup.sh` only links `%h/.local/bin/vigil` when
`tools/bar/target/release/vigil` already exists.

## Dotfiles integration

```bash
./setup.sh
systemctl --user daemon-reload
```

Hyprland and Niri restart `vigil.service` at session startup after their
environment import steps. The previous Waybar startup lines stay in the tracked
configs as commented fallback entries.

The user-only `${XDG_RUNTIME_DIR}/vigil.sock` also provides live desktop-context snapshots
and validated actions to Luma. Quick-settings footers launch Luma with the current page and output;
Luma can open the corresponding detailed quick-settings page through the same socket.

The lockscreen uses two deliberately narrow IPC commands:

```bash
vigil control-center close
vigil lockscreen snapshot
```

The snapshot contains only an active timer label and remaining duration,
network and power summaries, and the unread-notification count. It never
contains notification titles, bodies, senders, or actions. The shared session
lock script closes Vigil before Hyprlock starts so an open panel cannot remain
above the lock transition.

Time & Focus provides a locale-aware month calendar with an on-demand daily agenda, an Evolution
handoff for event editing, quick timer presets, and labeled custom timers. The agenda uses the
range-based `scripts/next_event.sh --agenda-json` contract while the original next-event modes stay
available to the urgency card, lock screen, and legacy Waybar fallback.

The calendar's weekday/month names follow the system locale by default; set `locale` in
`config.toml` (e.g. `locale = "de_DE.UTF-8"`) to override it independently of the environment.

## Native notifications

Vigil owns `org.freedesktop.Notifications` and replaces Dunst while the service
is active. New notifications can take over the ambient center island, appear
as a three-card toast stack on the focused output, and remain searchable in a
seven-day/100-item inbox. Toast timeouts pause on hover; critical notifications
remain visible until dismissed.

Open the notification status button to mark the inbox read. Right-click it to
toggle Do Not Disturb. DND queues low and normal notifications silently while
letting critical alerts through. Images, progress, markup, hyperlinks, sounds,
replacement IDs, and application actions are handled natively. State stays
user-only under `${XDG_STATE_HOME:-~/.local/state}/vigil/notifications.json`;
cached image data lives under `${XDG_CACHE_HOME:-~/.cache}/vigil/notifications`.
Vigil intentionally starts with a clean history and does not import Dunst data.

## Expressive cockpit and local routines

Vigil uses keyed widgets and a shared motion language for workspace, context,
status, and quick-settings changes. Bold motion and meaningful live-state
loops are enabled by the tracked configuration, while GTK's reduced-motion
setting disables transforms and looping effects.

The center island may offer one explicit action when live state suggests a
useful next step. Vigil never executes a suggestion automatically. The control
center shows up to three suggestions with their reasons and provides controls
to pause or clear local routine learning.

Successful discrete actions can become routine suggestions after three
matching occurrences on distinct days. Routine observations are stored
user-only at `${XDG_STATE_HOME:-~/.local/state}/vigil/routines.json`, expire
after 30 days by default, and never leave the machine.

For deterministic visual review in a graphical Wayland session:

```bash
cargo run -- --config ../../bar/config.toml __preview --scenario suggestion
cargo run -- --config ../../bar/config.toml __preview --scenario control-center
```

## Migration from Cockpit Bar

After building Vigil, run `./setup.sh` to install the new binary, configuration,
and service links and remove their old managed `cockpit-bar` links. Stop the old
process once before starting Vigil:

```bash
systemctl --user stop cockpit-bar.service || true
systemctl --user daemon-reload
systemctl --user restart vigil.service
```

Vigil imports existing timers from
`${XDG_STATE_HOME:-~/.local/state}/cockpit-bar/timers.json` when its own state
file does not exist.

## Rollback

Immediate session rollback back to Waybar:

```bash
systemctl --user stop vigil.service || true
systemctl --user reset-failed vigil.service || true
rm -f ~/.config/systemd/user/vigil.service
rm -f ~/.local/bin/vigil
rm -f ~/.config/vigil
systemctl --user daemon-reload
systemctl --user restart dunst.service
~/.config/waybar/launch.sh --replace
```

Tracked config rollback in this repository:

```bash
git checkout 2187ecb^ -- setup.sh hypr/hyprland-config/startup.conf hypr/hyprland-config/base.conf niri/config.kdl tools/bar/README.md
rm -f systemd/user/vigil.service tests/vigil_desktop_contract.sh
```
