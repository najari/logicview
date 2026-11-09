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

#include <QComboBox>
#include <QFrame>
#include <QHBoxLayout>
#include <QIcon>
#include <QLabel>
#include <QPixmap>
#include <QScrollArea>
#include <QVBoxLayout>

#include "triggerdock.hpp"

#include <pv/data/signalbase.hpp>
#include <pv/devices/device.hpp>
#include <pv/session.hpp>
#include <pv/views/viewbase.hpp>
#include <pv/views/trace/logicsignal.hpp>
#include <pv/views/trace/signal.hpp>
#include <pv/views/trace/view.hpp>

#include <libsigrokcxx/libsigrokcxx.hpp>

using std::dynamic_pointer_cast;
using std::make_pair;
using std::shared_ptr;
using std::vector;

using sigrok::Capability;
using sigrok::ConfigKey;
using sigrok::TriggerMatchType;

using pv::data::SignalBase;
using pv::views::trace::LogicSignal;

namespace pv {
namespace dock {

/// Combo box item data value used for the "no trigger" entry
static const int NoTriggerTypeId = -1;

TriggerDock::TriggerDock(Session &session, QWidget *parent) :
	QWidget(parent),
	session_(session)
{
	setObjectName(QString::fromUtf8("TriggerDock"));

	QVBoxLayout *root_layout = new QVBoxLayout(this);
	root_layout->setContentsMargins(8, 8, 8, 8);
	root_layout->setSpacing(6);

	QLabel *title_label = new QLabel(tr("Trigger"), this);
	title_label->setObjectName(QString::fromUtf8("TriggerDockTitle"));
	root_layout->addWidget(title_label);

	// Empty-state hint, centered in the available space when shown
	hint_label_ = new QLabel(this);
	hint_label_->setObjectName(QString::fromUtf8("TriggerHint"));
	hint_label_->setAlignment(Qt::AlignCenter);
	hint_label_->setWordWrap(true);
	root_layout->addWidget(hint_label_, 1);

	scroll_area_ = new QScrollArea(this);
	scroll_area_->setWidgetResizable(true);
	scroll_area_->setFrameShape(QFrame::NoFrame);

	QWidget *list_container = new QWidget(scroll_area_);
	list_layout_ = new QVBoxLayout(list_container);
	list_layout_->setContentsMargins(0, 0, 0, 0);
	list_layout_->setSpacing(4);
	scroll_area_->setWidget(list_container);

	root_layout->addWidget(scroll_area_, 1);

	connect(&session_, SIGNAL(device_changed()),
		this, SLOT(on_device_changed()));
	connect(&session_, SIGNAL(signals_changed()),
		this, SLOT(on_signals_changed()));
	connect(&session_, SIGNAL(capture_state_changed(int)),
		this, SLOT(on_capture_state_changed(int)));

	rebuild();
}

const vector<int32_t> TriggerDock::get_device_trigger_types() const
{
	// Same capability check as LogicSignal::get_trigger_types()

	// We may not be associated with a device
	if (!session_.device())
		return vector<int32_t>();

	const auto sr_dev = session_.device()->device();
	if (sr_dev->config_check(ConfigKey::TRIGGER_MATCH, Capability::LIST)) {
		const Glib::VariantContainerBase gvar =
			sr_dev->config_list(ConfigKey::TRIGGER_MATCH);

		vector<int32_t> ttypes;

		for (unsigned int i = 0; i < gvar.get_n_children(); i++) {
			Glib::VariantBase tmp_vb;
			gvar.get_child(tmp_vb, i);

			Glib::Variant<int32_t> tmp_v =
				Glib::VariantBase::cast_dynamic< Glib::Variant<int32_t> >(tmp_vb);

			ttypes.push_back(tmp_v.get());
		}

		return ttypes;
	} else {
		return vector<int32_t>();
	}
}

void TriggerDock::rebuild()
{
	// Clear the previous channel rows
	rows_.clear();
	while (QLayoutItem *item = list_layout_->takeAt(0)) {
		delete item->widget();
		delete item;
	}

	const vector<int32_t> trigger_types = get_device_trigger_types();

	if (trigger_types.empty()) {
		hint_label_->setText(
			tr("Current device does not support trigger setup"));
		hint_label_->show();
		scroll_area_->hide();
		return;
	}

	// Collect the logic channels in signal order
	vector< shared_ptr<SignalBase> > logic_bases;
	for (const shared_ptr<SignalBase>& base : session_.signalbases())
		if (base->type() == SignalBase::LogicChannel)
			logic_bases.push_back(base);

	if (logic_bases.empty()) {
		hint_label_->setText(tr("No logic channels available"));
		hint_label_->show();
		scroll_area_->hide();
		return;
	}

	hint_label_->hide();
	scroll_area_->show();

	for (const shared_ptr<SignalBase>& base : logic_bases) {
		// The current trigger setting of this channel is tracked by
		// the logic signals of the trace views, take the first one
		const TriggerMatchType *current_match = nullptr;

		for (const shared_ptr<views::ViewBase>& view : session_.views()) {
			if (view->get_type() != views::ViewTypeTrace)
				continue;

			views::trace::View *trace_view =
				dynamic_cast<views::trace::View*>(view.get());
			if (!trace_view)
				continue;

			shared_ptr<LogicSignal> signal = dynamic_pointer_cast<LogicSignal>(
				trace_view->get_signal_by_signalbase(base));
			if (signal) {
				current_match = signal->trigger_match();
				break;
			}
		}

		add_channel_row(base, current_match, trigger_types);
	}

	list_layout_->addStretch();

	// Only allow triggers to be changed when we're stopped
	on_capture_state_changed(session_.get_capture_state());
}

void TriggerDock::add_channel_row(shared_ptr<SignalBase> base,
	const TriggerMatchType *current_match, const vector<int32_t> &trigger_types)
{
	QWidget *row = new QWidget(this);
	QHBoxLayout *row_layout = new QHBoxLayout(row);
	row_layout->setContentsMargins(2, 0, 2, 0);
	row_layout->setSpacing(6);

	// Channel color dot
	QPixmap dot(12, 12);
	dot.fill(base->color());
	QLabel *dot_label = new QLabel(row);
	dot_label->setPixmap(dot);
	dot_label->setFixedSize(12, 12);
	row_layout->addWidget(dot_label);

	QLabel *name_label = new QLabel(base->display_name(), row);
	name_label->setObjectName(QString::fromUtf8("TriggerChannelName"));
	row_layout->addWidget(name_label, 1);

	QComboBox *combo = new QComboBox(row);
	combo->addItem(QIcon(QString::fromUtf8(":/icons/trigger-none.svg")),
		tr("None"), NoTriggerTypeId);

	for (int32_t type_id : trigger_types) {
		QString icon_path, label;

		switch (type_id) {
		case SR_TRIGGER_ZERO:
			icon_path = QString::fromUtf8(":/icons/trigger-low.svg");
			label = tr("Low");
			break;
		case SR_TRIGGER_ONE:
			icon_path = QString::fromUtf8(":/icons/trigger-high.svg");
			label = tr("High");
			break;
		case SR_TRIGGER_RISING:
			icon_path = QString::fromUtf8(":/icons/trigger-rising.svg");
			label = tr("Rising");
			break;
		case SR_TRIGGER_FALLING:
			icon_path = QString::fromUtf8(":/icons/trigger-falling.svg");
			label = tr("Falling");
			break;
		case SR_TRIGGER_EDGE:
			icon_path = QString::fromUtf8(":/icons/trigger-change.svg");
			label = tr("Change");
			break;
		default:
			// Unknown match types are not offered
			continue;
		}

		combo->addItem(QIcon(icon_path), label, type_id);
	}

	const int current_id = current_match ? current_match->id() : NoTriggerTypeId;
	const int current_index = combo->findData(current_id);
	combo->setCurrentIndex((current_index >= 0) ? current_index : 0);

	connect(combo, SIGNAL(activated(int)),
		this, SLOT(on_trigger_combo_activated(int)));

	row_layout->addWidget(combo);

	rows_.push_back(make_pair(combo, base));
	list_layout_->addWidget(row);
}

void TriggerDock::apply_trigger(shared_ptr<SignalBase> base,
	const TriggerMatchType *match)
{
	// Apply through the logic signals so that the change takes the same
	// path as the per-signal trigger actions, updating the trigger
	// markers in every trace view that shows this channel
	for (const shared_ptr<views::ViewBase>& view : session_.views()) {
		if (view->get_type() != views::ViewTypeTrace)
			continue;

		views::trace::View *trace_view =
			dynamic_cast<views::trace::View*>(view.get());
		if (!trace_view)
			continue;

		shared_ptr<LogicSignal> signal = dynamic_pointer_cast<LogicSignal>(
			trace_view->get_signal_by_signalbase(base));
		if (signal)
			signal->set_trigger_match(match);
	}
}

void TriggerDock::on_device_changed()
{
	rebuild();
}

void TriggerDock::on_signals_changed()
{
	rebuild();
}

void TriggerDock::on_capture_state_changed(int state)
{
	const bool enable = (state == Session::Stopped);

	for (auto& row : rows_)
		row.first->setEnabled(enable);
}

void TriggerDock::on_trigger_combo_activated(int index)
{
	QComboBox *combo = qobject_cast<QComboBox*>(sender());
	if (!combo)
		return;

	for (const auto& row : rows_) {
		if (row.first != combo)
			continue;

		const int type_id = combo->itemData(index).toInt();
		apply_trigger(row.second,
			(type_id == NoTriggerTypeId) ? nullptr : TriggerMatchType::get(type_id));
		break;
	}
}

} // namespace dock
} // namespace pv
