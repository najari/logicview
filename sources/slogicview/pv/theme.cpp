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

#include "theme.hpp"

namespace pv {
namespace Theme {

// The PulseView Dark design system. Keep the values below in sync with
// themes/pulseview-dark/pulseview-dark.qss.

QColor bg_base()      { return QColor(0x1B, 0x1D, 0x22); }
QColor bg_panel()     { return QColor(0x21, 0x24, 0x2B); }
QColor bg_elevated()  { return QColor(0x26, 0x2A, 0x32); }
QColor bg_canvas()    { return QColor(0x14, 0x16, 0x1A); }
QColor bg_input()     { return QColor(0x17, 0x19, 0x1E); }
QColor bg_hover()     { return QColor(0x2C, 0x30, 0x39); }
QColor bg_active()    { return QColor(0x33, 0x38, 0x45); }

QColor border()        { return QColor(0x35, 0x3A, 0x44); }
QColor border_strong() { return QColor(0x47, 0x4E, 0x5B); }

QColor text_primary()   { return QColor(0xE4, 0xE7, 0xEC); }
QColor text_secondary() { return QColor(0x9A, 0xA1, 0xAD); }
QColor text_disabled()  { return QColor(0x59, 0x60, 0x70); }

QColor accent()         { return QColor(0x3D, 0x8B, 0xFD); }
QColor accent_hover()   { return QColor(0x5E, 0x9F, 0xFF); }
QColor accent_pressed() { return QColor(0x2E, 0x6F, 0xD6); }
QColor on_accent()      { return QColor(0xFF, 0xFF, 0xFF); }

QColor success() { return QColor(0x34, 0xC7, 0x7B); }
QColor warning() { return QColor(0xE5, 0xB5, 0x67); }
QColor error()   { return QColor(0xF2, 0x6D, 0x6D); }

QColor grid_line()      { return QColor(0x23, 0x27, 0x2F); }
QColor cursor_line()    { return QColor(0xE4, 0xE7, 0xEC); }
QColor cursor_fill()    { return QColor(0x3D, 0x8B, 0xFD, 28); }
QColor selection_fill() { return QColor(0x3D, 0x8B, 0xFD, 56); }

const vector<QColor>& trace_palette()
{
	static const vector<QColor> palette = {
		QColor(0x4F, 0xC3, 0xF7),  // cyan
		QColor(0xFF, 0xB7, 0x4D),  // amber
		QColor(0x81, 0xC7, 0x84),  // green
		QColor(0xF0, 0x62, 0x92),  // pink
		QColor(0x95, 0x75, 0xCD),  // purple
		QColor(0xFF, 0xF1, 0x76),  // yellow
		QColor(0x4D, 0xB6, 0xAC),  // teal
		QColor(0xFF, 0x8A, 0x65),  // deep orange
		QColor(0x90, 0xCA, 0xF9),  // blue
		QColor(0xAE, 0xD5, 0x81),  // light green
		QColor(0xCE, 0x93, 0xD8),  // orchid
		QColor(0xE6, 0xEE, 0x9C),  // lime
	};

	return palette;
}

QPalette dark_palette()
{
	QPalette p;

	p.setColor(QPalette::Window, bg_panel());
	p.setColor(QPalette::WindowText, text_primary());
	p.setColor(QPalette::Base, bg_input());
	p.setColor(QPalette::AlternateBase, bg_panel());
	p.setColor(QPalette::ToolTipBase, bg_elevated());
	p.setColor(QPalette::ToolTipText, text_primary());
	p.setColor(QPalette::Text, text_primary());
	p.setColor(QPalette::Button, bg_panel());
	p.setColor(QPalette::ButtonText, text_primary());
	p.setColor(QPalette::BrightText, error());
	p.setColor(QPalette::Light, border_strong());
	p.setColor(QPalette::Midlight, bg_hover());
	p.setColor(QPalette::Mid, border());
	p.setColor(QPalette::Dark, bg_base());
	p.setColor(QPalette::Shadow, bg_canvas());
	p.setColor(QPalette::Link, accent());
	p.setColor(QPalette::LinkVisited, accent_pressed());
	p.setColor(QPalette::Highlight, accent());
	p.setColor(QPalette::HighlightedText, on_accent());

	p.setColor(QPalette::Disabled, QPalette::WindowText, text_disabled());
	p.setColor(QPalette::Disabled, QPalette::Text, text_disabled());
	p.setColor(QPalette::Disabled, QPalette::ButtonText, text_disabled());
	p.setColor(QPalette::Disabled, QPalette::Highlight, bg_active());
	p.setColor(QPalette::Disabled, QPalette::HighlightedText, text_disabled());

	return p;
}

} // namespace Theme
} // namespace pv
