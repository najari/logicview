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

#include <algorithm>
#include <cmath>

#include <QFontDatabase>
#include <QGridLayout>
#include <QGroupBox>
#include <QLabel>
#include <QTimer>
#include <QVBoxLayout>

#include "measuredock.hpp"

#include <pv/data/analog.hpp>
#include <pv/data/analogsegment.hpp>
#include <pv/data/logic.hpp>
#include <pv/data/logicsegment.hpp>
#include <pv/data/signalbase.hpp>
#include <pv/globalsettings.hpp>
#include <pv/session.hpp>
#include <pv/util.hpp>
#include <pv/views/viewbase.hpp>
#include <pv/views/trace/cursor.hpp>
#include <pv/views/trace/cursorpair.hpp>
#include <pv/views/trace/signal.hpp>
#include <pv/views/trace/view.hpp>
#include <pv/views/trace/viewport.hpp>

using std::max;
using std::min;
using std::pair;
using std::shared_ptr;

using pv::data::Analog;
using pv::data::AnalogSegment;
using pv::data::Logic;
using pv::data::LogicSegment;
using pv::data::SignalBase;
using pv::util::SIPrefix;
using pv::util::Timestamp;

namespace pv {
namespace dock {

/// Update interval of the measurement values in milliseconds
static const int UpdateInterval = 250;

/// Maximum number of edges to enumerate for logic measurements; wider
/// visible ranges are subsampled, making the result an estimate
static const double MaxEdgeEnumerationSamples = 1e6;

/// Row indices into name_labels_/value_labels_
enum MeasureRow {
	RowCursorT1 = 0,
	RowCursorT2,
	RowCursorDelta,
	RowCursorFrequency,
	RowCursorSamples,
	RowSignalName,
	RowSignalFrequency,
	RowSignalPeriod,
	RowSignalDutyCycle,
	RowSignalRisingEdges,
	RowSignalMin,
	RowSignalMax,
	RowSignalVpp,
	RowCount
};

MeasureDock::MeasureDock(Session &session, QWidget *parent) :
	QWidget(parent),
	session_(session)
{
	setObjectName(QString::fromUtf8("MeasureDock"));

	name_labels_.resize(RowCount);
	value_labels_.resize(RowCount);

	const QFont fixed_font = QFontDatabase::systemFont(QFontDatabase::FixedFont);

	QVBoxLayout *root_layout = new QVBoxLayout(this);
	root_layout->setContentsMargins(8, 8, 8, 8);
	root_layout->setSpacing(6);

	QLabel *title_label = new QLabel(tr("Measurements"), this);
	title_label->setObjectName(QString::fromUtf8("MeasureDockTitle"));
	root_layout->addWidget(title_label);

	auto add_row = [this, &fixed_font](QGridLayout *grid, int grid_row,
			int id, const QString &name) {
		QLabel *name_label = new QLabel(name, this);
		name_label->setObjectName(QString::fromUtf8("MeasureName"));

		QLabel *value_label = new QLabel(QString::fromUtf8("—"), this);
		value_label->setObjectName(QString::fromUtf8("MeasureValue"));
		value_label->setFont(fixed_font);
		value_label->setAlignment(Qt::AlignRight | Qt::AlignVCenter);
		value_label->setTextInteractionFlags(Qt::TextSelectableByMouse);

		grid->addWidget(name_label, grid_row, 0);
		grid->addWidget(value_label, grid_row, 1);

		name_labels_[id] = name_label;
		value_labels_[id] = value_label;
	};

	// --- Cursor measurements ---
	QGroupBox *cursor_box = new QGroupBox(tr("Cursor Measurements"), this);
	cursor_box->setObjectName(QString::fromUtf8("MeasureCard"));
	QGridLayout *cursor_grid = new QGridLayout(cursor_box);

	cursor_hint_label_ = new QLabel(
		tr("Show cursors to measure (toolbar button)"), cursor_box);
	cursor_hint_label_->setObjectName(QString::fromUtf8("MeasureHint"));
	cursor_grid->addWidget(cursor_hint_label_, 0, 0, 1, 2);

	add_row(cursor_grid, 1, RowCursorT1, tr("T1"));
	add_row(cursor_grid, 2, RowCursorT2, tr("T2"));
	add_row(cursor_grid, 3, RowCursorDelta, QString::fromUtf8("ΔT"));
	add_row(cursor_grid, 4, RowCursorFrequency, tr("Frequency"));
	add_row(cursor_grid, 5, RowCursorSamples, tr("Samples"));

	root_layout->addWidget(cursor_box);

	// --- Signal measurements ---
	QGroupBox *signal_box = new QGroupBox(tr("Signal Measurements"), this);
	signal_box->setObjectName(QString::fromUtf8("MeasureCard"));
	QGridLayout *signal_grid = new QGridLayout(signal_box);

	signal_hint_label_ = new QLabel(
		tr("Select a signal in the trace view to measure it"), signal_box);
	signal_hint_label_->setObjectName(QString::fromUtf8("MeasureHint"));
	signal_grid->addWidget(signal_hint_label_, 0, 0, 1, 2);

	add_row(signal_grid, 1, RowSignalName, tr("Signal"));
	add_row(signal_grid, 2, RowSignalFrequency, tr("Frequency"));
	add_row(signal_grid, 3, RowSignalPeriod, tr("Period"));
	add_row(signal_grid, 4, RowSignalDutyCycle, tr("Duty Cycle"));
	add_row(signal_grid, 5, RowSignalRisingEdges, tr("Rising Edges"));
	add_row(signal_grid, 6, RowSignalMin, tr("Min"));
	add_row(signal_grid, 7, RowSignalMax, tr("Max"));
	add_row(signal_grid, 8, RowSignalVpp, tr("Vpp"));

	root_layout->addWidget(signal_box);
	root_layout->addStretch();

	update_timer_ = new QTimer(this);
	connect(update_timer_, SIGNAL(timeout()),
		this, SLOT(update_measurements()));
	update_timer_->start(UpdateInterval);

	update_measurements();
}

views::trace::View* MeasureDock::trace_view() const
{
	for (const shared_ptr<views::ViewBase>& view : session_.views())
		if (view->get_type() == views::ViewTypeTrace)
			return dynamic_cast<views::trace::View*>(view.get());

	return nullptr;
}

shared_ptr<SignalBase> MeasureDock::selected_signal(views::trace::View *view) const
{
	for (const shared_ptr<views::trace::Signal>& signal : view->signals())
		if (signal->selected() && signal->enabled())
			return signal->base();

	return nullptr;
}

void MeasureDock::set_row_visible(int row, bool visible)
{
	name_labels_[row]->setVisible(visible);
	value_labels_[row]->setVisible(visible);
}

void MeasureDock::update_measurements()
{
	views::trace::View *view = trace_view();

	update_cursor_section(view);
	update_signal_section(view);
}

void MeasureDock::update_cursor_section(views::trace::View *view)
{
	const bool have_cursors = view && view->cursors_shown();

	cursor_hint_label_->setVisible(!have_cursors);
	set_row_visible(RowCursorT1, have_cursors);
	set_row_visible(RowCursorT2, have_cursors);

	if (!have_cursors) {
		set_row_visible(RowCursorDelta, false);
		set_row_visible(RowCursorFrequency, false);
		set_row_visible(RowCursorSamples, false);
		return;
	}

	const Timestamp t1 = view->cursors()->first()->time();
	const Timestamp t2 = view->cursors()->second()->time();

	Timestamp delta = t2 - t1;
	if (delta < 0)
		delta = -delta;

	// Show the same rows that the cursor ruler label is configured to show
	GlobalSettings settings;
	const bool show_interval = settings.value(
		GlobalSettings::Key_View_CursorShowInterval).value<bool>();
	const bool show_frequency = settings.value(
		GlobalSettings::Key_View_CursorShowFrequency).value<bool>();
	const bool show_samples = settings.value(
		GlobalSettings::Key_View_CursorShowSamples).value<bool>();

	value_labels_[RowCursorT1]->setText(
		util::format_time_si(t1, SIPrefix::unspecified, 6, "s", false));
	value_labels_[RowCursorT2]->setText(
		util::format_time_si(t2, SIPrefix::unspecified, 6, "s", false));

	set_row_visible(RowCursorDelta, show_interval);
	if (show_interval)
		value_labels_[RowCursorDelta]->setText(
			util::format_time_si(delta, SIPrefix::unspecified, 6, "s", false));

	set_row_visible(RowCursorFrequency, show_frequency);
	if (show_frequency) {
		const double delta_s = delta.convert_to<double>();
		value_labels_[RowCursorFrequency]->setText((delta_s > 0) ?
			util::format_value_si(1.0 / delta_s, SIPrefix::unspecified, 4, "Hz", false) :
			QString::fromUtf8("—"));
	}

	set_row_visible(RowCursorSamples, show_samples);
	if (show_samples) {
		const double samplerate = session_.get_samplerate();
		value_labels_[RowCursorSamples]->setText((samplerate > 0) ?
			QString::number((delta * samplerate).convert_to<uint64_t>()) :
			QString::fromUtf8("—"));
	}
}

void MeasureDock::update_signal_section(views::trace::View *view)
{
	shared_ptr<SignalBase> signal = view ? selected_signal(view) : nullptr;
	const bool have_signal = (signal != nullptr);

	signal_hint_label_->setVisible(!have_signal);
	set_row_visible(RowSignalName, have_signal);

	// Hide all measurement rows, the signal-specific update re-enables them
	for (int row = RowSignalFrequency; row < RowCount; row++)
		set_row_visible(row, false);

	if (!have_signal)
		return;

	value_labels_[RowSignalName]->setText(signal->display_name());

	if (signal->logic_data())
		update_logic_measurements(signal, view);
	else if (signal->analog_data())
		update_analog_measurements(signal, view);
}

void MeasureDock::update_logic_measurements(shared_ptr<SignalBase> signal,
	views::trace::View *view)
{
	set_row_visible(RowSignalFrequency, true);
	set_row_visible(RowSignalPeriod, true);
	set_row_visible(RowSignalDutyCycle, true);
	set_row_visible(RowSignalRisingEdges, true);

	auto set_na = [this]() {
		value_labels_[RowSignalFrequency]->setText(QString::fromUtf8("—"));
		value_labels_[RowSignalPeriod]->setText(QString::fromUtf8("—"));
		value_labels_[RowSignalDutyCycle]->setText(QString::fromUtf8("—"));
		value_labels_[RowSignalRisingEdges]->setText(QString::fromUtf8("—"));
	};

	shared_ptr<Logic> logic = signal->logic_data();
	if (logic->logic_segments().empty()) {
		set_na();
		return;
	}

	const uint32_t segment_id = min(view->current_segment(),
		(uint32_t)logic->logic_segments().size() - 1);
	shared_ptr<LogicSegment> segment = logic->logic_segments().at(segment_id);

	const double samplerate = segment->samplerate();
	const uint64_t sample_count = segment->get_sample_count();

	if ((samplerate <= 0) || (sample_count < 2)) {
		set_na();
		return;
	}

	// Measure over the currently visible range of the view
	const Timestamp t_start = view->offset();
	const Timestamp t_end = t_start + Timestamp(view->viewport()->width()) *
		view->scale();

	const int64_t start_sample = max(
		(int64_t)floor((t_start * samplerate).convert_to<double>()), (int64_t)0);
	const int64_t end_sample = min(
		(int64_t)ceil((t_end * samplerate).convert_to<double>()),
		(int64_t)(sample_count - 1));

	if (end_sample <= start_sample) {
		set_na();
		return;
	}

	// Enumerate the edges. For very wide visible ranges the edges are
	// subsampled, in which case the results are estimates (hence the
	// visible-range scope of these measurements)
	const float min_length = max(1.0f,
		(float)((end_sample - start_sample) / MaxEdgeEnumerationSamples));

	vector<LogicSegment::EdgePair> edges;
	segment->get_subsampled_edges(edges, start_sample, end_sample,
		min_length, signal->logic_bit_index());

	if (edges.size() < 2) {
		set_na();
		return;
	}

	// Each entry holds the level valid from its sample position on, so a
	// rising edge is a pair of consecutive entries going from low to high
	uint64_t rising_edges = 0;
	int64_t first_rising = -1, last_rising = -1;
	uint64_t high_samples = 0;

	for (size_t i = 0; i + 1 < edges.size(); i++) {
		if (edges[i].second)
			high_samples += edges[i + 1].first - edges[i].first;

		if (!edges[i].second && edges[i + 1].second) {
			if (first_rising < 0)
				first_rising = edges[i + 1].first;
			last_rising = edges[i + 1].first;
			rising_edges++;
		}
	}

	value_labels_[RowSignalRisingEdges]->setText(QString::number(rising_edges));

	const int64_t span = edges.back().first - edges.front().first;
	if (span > 0)
		value_labels_[RowSignalDutyCycle]->setText(
			QString::number(100.0 * high_samples / span, 'f', 1) + " %");
	else
		value_labels_[RowSignalDutyCycle]->setText(QString::fromUtf8("—"));

	if (rising_edges >= 2) {
		// Average period derived from the first and last rising edge
		const double period_s = (double)(last_rising - first_rising) /
			(rising_edges - 1) / samplerate;

		value_labels_[RowSignalPeriod]->setText(
			util::format_value_si(period_s, SIPrefix::unspecified, 6, "s", false));
		value_labels_[RowSignalFrequency]->setText((period_s > 0) ?
			util::format_value_si(1.0 / period_s, SIPrefix::unspecified, 4, "Hz", false) :
			QString::fromUtf8("—"));
	} else {
		value_labels_[RowSignalPeriod]->setText(QString::fromUtf8("—"));
		value_labels_[RowSignalFrequency]->setText(QString::fromUtf8("—"));
	}
}

void MeasureDock::update_analog_measurements(shared_ptr<SignalBase> signal,
	views::trace::View *view)
{
	set_row_visible(RowSignalMin, true);
	set_row_visible(RowSignalMax, true);
	set_row_visible(RowSignalVpp, true);

	auto set_na = [this]() {
		value_labels_[RowSignalMin]->setText(QString::fromUtf8("—"));
		value_labels_[RowSignalMax]->setText(QString::fromUtf8("—"));
		value_labels_[RowSignalVpp]->setText(QString::fromUtf8("—"));
	};

	shared_ptr<Analog> analog = signal->analog_data();
	if (analog->analog_segments().empty()) {
		set_na();
		return;
	}

	const uint32_t segment_id = min(view->current_segment(),
		(uint32_t)analog->analog_segments().size() - 1);
	shared_ptr<AnalogSegment> segment = analog->analog_segments().at(segment_id);

	if (segment->get_sample_count() == 0) {
		set_na();
		return;
	}

	// The segment tracks its min/max continuously, so the values cover the
	// whole capture instead of just the visible range
	const pair<float, float> min_max = segment->get_min_max();

	value_labels_[RowSignalMin]->setText(
		util::format_value_si(min_max.first, SIPrefix::unspecified, 4, "V", false));
	value_labels_[RowSignalMax]->setText(
		util::format_value_si(min_max.second, SIPrefix::unspecified, 4, "V", false));
	value_labels_[RowSignalVpp]->setText(
		util::format_value_si(min_max.second - min_max.first,
			SIPrefix::unspecified, 4, "V", false));
}

} // namespace dock
} // namespace pv
