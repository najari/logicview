/*
 * This file is part of the PulseView project.
 *
 * Copyright (C) 2026 Soeren Apel <soeren@apelpie.net>
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, see <http://www.gnu.org/licenses/>.
 */

#ifndef PULSEVIEW_PV_THEME_HPP
#define PULSEVIEW_PV_THEME_HPP

#include <vector>

#include <QColor>
#include <QPalette>

using std::vector;

namespace pv {

/**
 * Central design tokens of the PulseView Dark design system.
 *
 * Every visual value (colors, the trace palette) lives here so that
 * stylesheets, widget code and view painting stay in sync. Views and
 * widgets must use these tokens instead of hardcoded colors.
 */
namespace Theme {

// Surface colors, darkest to lightest
QColor bg_base();      ///< Application window background
QColor bg_panel();     ///< Dock panels, toolbars
QColor bg_elevated();  ///< Menus, popups, dialogs
QColor bg_canvas();    ///< Waveform viewport background
QColor bg_input();     ///< Line edits, spin boxes, combo boxes
QColor bg_hover();     ///< Hovered interactive surfaces
QColor bg_active();    ///< Pressed/checked interactive surfaces

// Lines and outlines
QColor border();
QColor border_strong();

// Text
QColor text_primary();
QColor text_secondary();
QColor text_disabled();

// Brand accent (selection, focus, primary actions)
QColor accent();
QColor accent_hover();
QColor accent_pressed();
QColor on_accent();

// Semantic states
QColor success();
QColor warning();
QColor error();

// Waveform view tokens
QColor grid_line();
QColor cursor_line();
QColor cursor_fill();
QColor selection_fill();

/// High-distinction trace colors for dark backgrounds
const vector<QColor>& trace_palette();

/// Application palette matching the tokens above
QPalette dark_palette();

} // namespace Theme
} // namespace pv

#endif // PULSEVIEW_PV_THEME_HPP
