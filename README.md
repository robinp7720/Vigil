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
environment import steps.

The user-only `${XDG_RUNTIME_DIR}/vigil.sock` also provides live desktop-context snapshots
and validated actions to Luma. Quick-settings footers launch Luma with the current page and output;
Luma can open the corresponding detailed quick-settings page through the same socket.

The lockscreen uses two deliberately narrow IPC commands:

```bash
vigil control-center open
vigil control-center toggle
vigil control-center open --context notifications --output DP-5
vigil control-center close
vigil lockscreen snapshot
```

When no output is supplied, the control center opens on the currently focused
output. A single quick-settings window is retargeted between outputs.
`toggle` dismisses a visible panel on the same output; a specific `--context`
switches to that page first if needed. Another output retargets the panel.
Every bar also has a desktop-search button:
left-click opens Luma on that output, and right-click opens window search.
When supervised by systemd, launched applications get independent user scopes
with the current Wayland environment. They survive a Vigil restart, and exited
launcher processes are reaped without blocking the desktop controls.

The snapshot contains only an active timer label and remaining duration,
network and power summaries, and the unread-notification count. It never
contains notification titles, bodies, senders, or actions. The shared session
lock script closes Vigil before Hyprlock starts so an open panel cannot remain
above the lock transition.

Time & Focus provides a locale-aware month calendar with an on-demand daily agenda, an Evolution
handoff for event editing, quick timer presets, and labeled custom timers. The agenda uses the
range-based `scripts/next_event.sh --agenda-json` contract while the compact next-event modes stay
available to the urgency card and lock screen.

The calendar's weekday/month names follow the system locale by default; set `locale` in
`config.toml` (e.g. `locale = "de_DE.UTF-8"`) to override it independently of the environment.

## AI token usage

The `ai_usage` module shows today's combined Codex and Claude Code token count on
the primary bar. Hover for provider totals; click for a dedicated graphical
breakdown with a provider-share ring, stacked token-composition bars, and Codex
quota meters. Hover a count for its exact value. The same page is linked from
System Resources. Days follow the computer's local timezone.

Quota cards estimate time left at the recent consumption pace and compare it
with the next reset. The earliest estimated exhaustion before a reset also
appears beside the bar's token count. The estimate uses reported quota percentage
changes over the last hour, requiring at least 15 minutes and two percentage
points of change. It reconstructs from logs after restart and across midnight;
different quota windows and resets are kept separate. Reports older than 15
minutes pause the estimate, and flat usage is not treated as unlimited time.
These are approximate forecasts assuming the recent pace continues. Claude's
local logs lack quota data, so its remaining-time estimate is unavailable.

Vigil reads local JSONL session records every 15 seconds from
`${CODEX_HOME:-~/.codex}/{sessions,archived_sessions}` and
`${CLAUDE_CONFIG_DIR:-~/.claude}/projects`, including subagent logs. It reads new
bytes incrementally, deduplicates repeated usage reports, and handles sessions
resumed across midnight. No credentials or network access are needed. The module
does not store prompts or responses.

Totals include cached input; reasoning is already included in Codex output.
These are locally recorded tokens, not billing totals or an account-wide usage
meter. Codex quota information is labeled with its report time; Claude's session
records do not provide account quota information. A provider with no activity
today shows zero; missing logs are marked unavailable.

Run `vigil ai-usage` for the same daily accounting as JSON. Remove `"ai_usage"`
from `[modules].full` to hide the indicator. Custom provider homes must be in the
environment of `vigil.service` as well as any interactive shell.

## Native notifications

Vigil owns `org.freedesktop.Notifications`. New notifications appear in a measured
three-card stack below the bar on the focused output while the ambient center
island keeps its current context. Notifications remain searchable in a richer
seven-day/100-item inbox. Toast timeouts pause on hover; critical notifications
remain visible until dismissed.

Open the notification status button to mark the inbox read. Right-click it to
toggle Do Not Disturb. DND queues low and normal notifications silently while
letting critical alerts through. Images are decoded into private manager-owned
cache files; progress, markup, hyperlinks, sounds, replacement IDs, and up to
three application actions are handled natively. State stays
user-only under `${XDG_STATE_HOME:-~/.local/state}/vigil/notifications.json`;
cached image data lives under `${XDG_CACHE_HOME:-~/.cache}/vigil/notifications`.
Vigil intentionally starts with a clean history and does not import another
notification server's data.

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
cargo run -- --config ../../bar/config.toml __preview --scenario ai-usage
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

Hyprland integration uses the native Lua IPC API (Hyprland 0.55+ with a Lua configuration).
