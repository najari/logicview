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

#ifndef PULSEVIEW_PV_DOCK_MEASUREDOCK_HPP
#define PULSEVIEW_PV_DOCK_MEASUREDOCK_HPP

#include <cstdint>
#include <memory>
#include <vector>

#include <QWidget>

using std::shared_ptr;
using std::vector;

class QLabel;
class QTimer;

namespace pv {

class Session;

namespace data {
class SignalBase;
}

namespace views {
namespace trace {
class View;
}
}

namespace dock {

/**
 * Dockable panel that shows cursor-based and signal-based measurements,
 * inspired by DSView's MeasureDock:
 *
 * - Cursor measurements: T1/T2 times of the trace view's cursor pair,
 *   the interval between them, its reciprocal (frequency) and the number
 *   of samples it spans.
 * - Signal measurements: for the currently selected signal, logic signals
 *   get frequency/period/duty cycle/edge count estimates derived from the
 *   edges within the currently visible range, analog signals get
 *   min/max/Vpp of the whole capture.
 *
 * Values refresh through a 250 ms timer, which keeps the panel in sync
 * with cursor movements, selection changes and incoming capture data
 * without needing dedicated change notifications.
 */
class MeasureDock : public QWidget
{
	Q_OBJECT

public:
	explicit MeasureDock(Session &session, QWidget *parent = nullptr);

private Q_SLOTS:
	void update_measurements();

private:
	views::trace::View* trace_view() const;

	shared_ptr<data::SignalBase> selected_signal(views::trace::View *view) const;

	void update_cursor_section(views::trace::View *view);
	void update_signal_section(views::trace::View *view);

	void update_logic_measurements(shared_ptr<data::SignalBase> signal,
		views::trace::View *view);
	void update_analog_measurements(shared_ptr<data::SignalBase> signal,
		views::trace::View *view);

	void set_row_visible(int row, bool visible);

private:
	Session &session_;

	QTimer *update_timer_;

	/// Empty-state hints, shown when there is nothing to measure
	QLabel *cursor_hint_label_, *signal_hint_label_;

	/// Row name labels and value labels, indexed by the Row enum in the .cpp
	vector<QLabel*> name_labels_, value_labels_;
};

} // namespace dock
} // namespace pv

#endif // PULSEVIEW_PV_DOCK_MEASUREDOCK_HPP
