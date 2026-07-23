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

Time & Focus provides a locale-aware month calendar with an on-demand daily agenda, an Evolution
handoff for event editing, quick timer presets, and labeled custom timers. The agenda uses the
range-based `scripts/next_event.sh --agenda-json` contract while the original next-event modes stay
available to the urgency card, lock screen, and legacy Waybar fallback.

The calendar's weekday/month names follow the system locale by default; set `locale` in
`config.toml` (e.g. `locale = "de_DE.UTF-8"`) to override it independently of the environment.

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
~/.config/waybar/launch.sh --replace
```

Tracked config rollback in this repository:

```bash
git checkout 2187ecb^ -- setup.sh hypr/hyprland-config/startup.conf hypr/hyprland-config/base.conf niri/config.kdl tools/bar/README.md
rm -f systemd/user/vigil.service tests/vigil_desktop_contract.sh
```
