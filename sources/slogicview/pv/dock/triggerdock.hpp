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

#ifndef PULSEVIEW_PV_DOCK_TRIGGERDOCK_HPP
#define PULSEVIEW_PV_DOCK_TRIGGERDOCK_HPP

#include <cstdint>
#include <memory>
#include <utility>
#include <vector>

#include <QWidget>

using std::pair;
using std::shared_ptr;
using std::vector;

class QComboBox;
class QLabel;
class QScrollArea;
class QVBoxLayout;

namespace sigrok {
class TriggerMatchType;
}

namespace pv {

class Session;

namespace data {
class SignalBase;
}

namespace dock {

/**
 * Dockable panel that gives an overview of the trigger configuration of
 * all logic channels, inspired by DSView's TriggerDock:
 *
 * - When the current device does not support triggers, an empty-state
 *   hint is shown instead of the channel list.
 * - Otherwise every logic channel gets a row with its color, name and a
 *   combo box to pick the trigger condition (None/Low/High/Rising/
 *   Falling/Change, limited to what the device supports).
 *
 * Changes are applied through LogicSignal::set_trigger_match(), the same
 * path that the per-signal trigger toolbar actions use, so the trigger
 * markers in the trace view stay in sync automatically.
 */
class TriggerDock : public QWidget
{
	Q_OBJECT

public:
	explicit TriggerDock(Session &session, QWidget *parent = nullptr);

private:
	/**
	 * Returns the trigger match types supported by the current device,
	 * or an empty vector if the device does not support triggers.
	 * Mirrors the capability check used by LogicSignal.
	 */
	const vector<int32_t> get_device_trigger_types() const;

	void rebuild();

	void add_channel_row(shared_ptr<data::SignalBase> base,
		const sigrok::TriggerMatchType *current_match,
		const vector<int32_t> &trigger_types);

	void apply_trigger(shared_ptr<data::SignalBase> base,
		const sigrok::TriggerMatchType *match);

private Q_SLOTS:
	void on_device_changed();
	void on_signals_changed();
	void on_capture_state_changed(int state);
	void on_trigger_combo_activated(int index);

private:
	Session &session_;

	/// Empty-state hint, shown when triggers are unavailable
	QLabel *hint_label_;

	QScrollArea *scroll_area_;
	QVBoxLayout *list_layout_;

	/// Rows as (combo box, signal base) pairs for the activation slot
	vector< pair<QComboBox*, shared_ptr<data::SignalBase>> > rows_;
};

} // namespace dock
} // namespace pv

#endif // PULSEVIEW_PV_DOCK_TRIGGERDOCK_HPP
