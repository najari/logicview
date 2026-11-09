# PulseView Dark Design System

This document describes the design system introduced in the 2026 UI redesign.
It replaces the previous Workbench theme and icon set entirely.

## Architecture

- **Tokens**: `pv/theme.hpp` / `pv/theme.cpp` — every visual value lives in
  `pv::Theme`. Widget code and view painting must use these tokens instead of
  hardcoded colors. Keep them in sync with the QSS file.
- **Stylesheet**: `themes/pulseview-dark/pulseview-dark.qss` — registered in
  `Themes[]` (`pv/globalsettings.cpp`) as "PulseView Dark" (index 3, default).
  The header of the QSS file lists the token hex values.
- **Palette**: `Theme::dark_palette()` is applied by
  `GlobalSettings::apply_theme()` for the PulseView Dark theme.
- **Icons**: `icons/*.svg` — original icon set, 16×16 grid, 1.5 px stroke,
  round caps/joins, foreground `#C8CDD5`. QSS-only helper icons (chevrons,
  branch arrows, checkmark) live in the same directory.

## Color tokens

| Token | Value | Usage |
|---|---|---|
| bg_base | `#1B1D22` | application window background |
| bg_panel | `#21242B` | docks, toolbars, panels |
| bg_elevated | `#262A32` | menus, popups, dialogs, cards |
| bg_canvas | `#14161A` | waveform viewport background |
| bg_input | `#17191E` | line edits, spin/combo boxes |
| bg_hover | `#2C3039` | hovered interactive surfaces |
| bg_active | `#333845` | pressed/checked surfaces |
| border | `#353A44` | default outlines |
| border_strong | `#474E5B` | emphasized outlines |
| text_primary | `#E4E7EC` | primary text |
| text_secondary | `#9AA1AD` | secondary text, icons |
| text_disabled | `#596070` | disabled text, dense edge walls |
| accent | `#3D8BFD` | selection, focus, primary actions |
| accent_hover | `#5E9FFF` | hovered accent |
| accent_pressed | `#2E6FD6` | pressed accent |
| success | `#34C77B` | logic-high, capture running |
| warning | `#E5B567` | trigger markers, warnings |
| error | `#F26D6D` | logic-low, errors |

Dimensions: spacing steps 4/8/12/16 px, corner radius 4 px (controls) and
6 px (cards/popups), icon size 16 px.

## Trace colors

`Theme::trace_palette()` provides 12 high-distinction colors for dark canvas
backgrounds (cyan, amber, green, pink, purple, yellow, teal, deep orange,
blue, light green, orchid, lime). They are used by:

- `SignalBase::LogicSignalColors` / `AnalogSignalColors` (default assignment)
- `TracePalette` (color picker grid, 8×2: 12 colors + 4 neutrals)

## View painting rules

- Viewport background: `bg_canvas` (set in `views/trace/viewport.cpp`).
- Grid lines: `grid_line`; ruler text: `text_secondary`.
- Logic signals: high `success`, low `error`, edges `text_disabled`
  (dense transitions blend into a dim wall instead of a bright block),
  sampling points `text_secondary`.
- Cursors: line `cursor_line`, fill `cursor_fill` (accent at ~11 % alpha,
  default in `GlobalSettings::set_dark_theme_default_colors()`).
- Error overlays: `error`; hover marker: `text_secondary`.

## Dock panels

Four DSView-inspired panels live in `pv/dock/` and are registered per session
in `MainWindow::add_session()`:

- **ProtocolDock** (`protocoldock.cpp`) — right area, decoder stack cards
  (add/remove/visibility/channel mapping/options), decode table shortcut.
- **MeasureDock** (`measuredock.cpp`) — right area (tabbed with Protocol),
  cursor measurements (T1/T2/ΔT/frequency/samples) and per-signal
  measurements (frequency/period/duty/edges for logic, min/max/Vpp for
  analog), 250 ms polling.
- **TriggerDock** (`triggerdock.cpp`) — right area (tabbed), per-channel
  trigger conditions via the same path as the per-signal trigger actions;
  graceful empty state when the device lacks trigger support.
- **SearchDock** (`searchdock.cpp`) — bottom area, annotation search with
  result list and jump-to-location (Enter/Shift+Enter navigation).

Panel toggles live in the main bar's **Panels** menu (labeled button with
`dock-panels.svg` icon; menu items show icon + panel name and are checkable).
The main bar uses `Qt::ToolButtonTextBesideIcon` throughout so every action
shows a text label; acquisition parameters have "Samples"/"Rate" caption
labels that follow the visibility of their value editors. Note the Qt quirk:
a `SessionWorkspace` without a central widget misplaces `addDockWidget()`
areas — use `splitDockWidget()`/`resizeDocks()` (see `mainwindow.cpp`
comments).

## UI QA tooling

`main.cpp` supports a headless screenshot hook (used with the demo capture):

```bash
PV_SCREENSHOT=/path/out.png PV_SCREENSHOT_DELAY=6000 PV_SCREENSHOT_QUIT=1 \
  LD_LIBRARY_PATH=$(pwd)/sr-darwin/lib build-darwin/pulseview/build/pulseview
# Open the settings dialog instead of grabbing the main window:
PV_OPEN_SETTINGS=1 PV_SCREENSHOT=/path/out.png ...
```

Storing stale per-signal colors: `defaults delete org.sigrok.PulseView`
resets sessions so new default colors can be checked.

Screenshot evidence of the redesign lives in `../ui-captures/redesign-*.png`.
