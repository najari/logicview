/*
 * This file is part of the PulseView project.
 *
 * Copyright (C) 2013 Joel Holdsworth <joel@airwebreathe.org.uk>
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

#include "tracepalette.hpp"

#include <pv/theme.hpp>

namespace pv {
namespace views {
namespace trace {

// The picker grid is built from the theme's trace palette plus a row of
// neutral tones that work on dark canvas backgrounds.
const QColor TracePalette::Colors[Cols * Rows] = {

	// Theme trace colors, first row
	QColor(0x4F, 0xC3, 0xF7),	// Cyan
	QColor(0xFF, 0xB7, 0x4D),	// Amber
	QColor(0x81, 0xC7, 0x84),	// Green
	QColor(0xF0, 0x62, 0x92),	// Pink
	QColor(0x95, 0x75, 0xCD),	// Purple
	QColor(0xFF, 0xF1, 0x76),	// Yellow
	QColor(0x4D, 0xB6, 0xAC),	// Teal
	QColor(0xFF, 0x8A, 0x65),	// Deep Orange

	// Theme trace colors, second row + neutral tones
	QColor(0x90, 0xCA, 0xF9),	// Blue
	QColor(0xAE, 0xD5, 0x81),	// Light Green
	QColor(0xCE, 0x93, 0xD8),	// Orchid
	QColor(0xE6, 0xEE, 0x9C),	// Lime
	Theme::text_primary(),
	Theme::text_secondary(),
	Theme::text_disabled(),
	Theme::bg_active(),
};

} // namespace trace
} // namespace views
} // namespace pv
