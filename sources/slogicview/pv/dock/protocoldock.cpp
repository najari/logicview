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
#include <cassert>

#include <QCheckBox>
#include <QComboBox>
#include <QFormLayout>
#include <QGroupBox>
#include <QHBoxLayout>
#include <QLabel>
#include <QPushButton>
#include <QScrollArea>
#include <QTimer>
#include <QToolButton>
#include <QVBoxLayout>

#include <libsigrokdecode/libsigrokdecode.h>

#include "protocoldock.hpp"

#include <pv/binding/decoder.hpp>
#include <pv/data/decodesignal.hpp>
#include <pv/data/signalbase.hpp>
#include <pv/globalsettings.hpp>
#include <pv/session.hpp>
#include <pv/strnatcmp.hpp>
#include <pv/widgets/decodergroupbox.hpp>
#include <pv/widgets/decodermenu.hpp>

using std::dynamic_pointer_cast;
using std::make_pair;
using std::sort;

using pv::data::DecodeSignal;
using pv::data::SignalBase;
using pv::data::decode::DecodeChannel;
using pv::data::decode::Decoder;

namespace pv {
namespace dock {

ProtocolDock::ProtocolDock(Session &session, QWidget *parent) :
	QWidget(parent),
	session_(session),
	cards_layout_(nullptr)
{
	setObjectName(QString::fromUtf8("ProtocolDock"));

	QVBoxLayout *root_layout = new QVBoxLayout(this);
	root_layout->setContentsMargins(8, 8, 8, 8);
	root_layout->setSpacing(6);

	// Toolbar row: panel title and "add decoder" button
	QHBoxLayout *toolbar_layout = new QHBoxLayout;
	QLabel *title_label = new QLabel(tr("Protocol Decoding"), this);
	title_label->setObjectName(QString::fromUtf8("ProtocolDockTitle"));

	QPushButton *add_button = new QPushButton(tr("Add Decoder"), this);
	add_button->setIcon(QIcon(":/icons/add-decoder.svg"));
	add_button->setToolTip(tr("Add a protocol decoder to this session"));

	pv::widgets::DecoderMenu *decoder_menu =
		new pv::widgets::DecoderMenu(add_button, "logic", true);
	decoder_menu->setStyleSheet("QMenu { menu-scrollable: 1; }");
	add_button->setMenu(decoder_menu);

	toolbar_layout->addWidget(title_label);
	toolbar_layout->addStretch();
	toolbar_layout->addWidget(add_button);
	root_layout->addLayout(toolbar_layout);

	// Scrollable list of decoder cards
	QScrollArea *scroll_area = new QScrollArea(this);
	scroll_area->setWidgetResizable(true);
	scroll_area->setFrameShape(QFrame::NoFrame);

	QWidget *cards_container = new QWidget();
	cards_layout_ = new QVBoxLayout(cards_container);
	cards_layout_->setContentsMargins(0, 0, 0, 0);
	cards_layout_->setSpacing(6);
	cards_layout_->addStretch();

	scroll_area->setWidget(cards_container);
	root_layout->addWidget(scroll_area, 1);

	// Bottom row: open the tabular decode data view
	QPushButton *table_button = new QPushButton(tr("Show Decode Table"), this);
	table_button->setToolTip(tr("Open a tabular view of the decoded data"));
	root_layout->addWidget(table_button);

#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
	connect(&delete_signal_mapper_, SIGNAL(mappedObject(QObject*)),
		this, SLOT(on_delete_decode_signal(QObject*)));
	connect(&show_hide_decoder_mapper_, SIGNAL(mappedObject(QObject*)),
		this, SLOT(on_show_hide_stacked_decoder(QObject*)));
	connect(&delete_decoder_mapper_, SIGNAL(mappedObject(QObject*)),
		this, SLOT(on_delete_stacked_decoder(QObject*)));
#else
	connect(&delete_signal_mapper_, SIGNAL(mapped(QObject*)),
		this, SLOT(on_delete_decode_signal(QObject*)));
	connect(&show_hide_decoder_mapper_, SIGNAL(mapped(QObject*)),
		this, SLOT(on_show_hide_stacked_decoder(QObject*)));
	connect(&delete_decoder_mapper_, SIGNAL(mapped(QObject*)),
		this, SLOT(on_delete_stacked_decoder(QObject*)));
#endif

	connect(decoder_menu, SIGNAL(decoder_selected(srd_decoder*)),
		this, SLOT(on_add_decoder_selected(srd_decoder*)));
	connect(table_button, SIGNAL(clicked()),
		this, SLOT(on_show_decode_table()));
	connect(&session_, SIGNAL(signals_changed()),
		this, SLOT(on_signals_changed()));

	rebuild_decoder_list();
}

void ProtocolDock::rebuild_decoder_list()
{
	// Remove all existing cards but keep the trailing stretch. Widgets are
	// deleted later since we may be called from within a signal emitted by
	// one of the card widgets (e.g. the delete button)
	while (cards_layout_->count() > 1) {
		QLayoutItem *item = cards_layout_->takeAt(0);
		if (item->widget())
			item->widget()->deleteLater();
		delete item;
	}

	bindings_.clear();
	current_signals_.clear();
	signal_by_checkbox_.clear();
	checkbox_by_signal_.clear();
	stacked_decoder_by_group_.clear();
	channel_by_combo_.clear();
	init_state_by_combo_.clear();

	for (const shared_ptr<SignalBase> &b : session_.signalbases()) {
		if (!b->is_decode_signal())
			continue;

		shared_ptr<DecodeSignal> signal = dynamic_pointer_cast<DecodeSignal>(b);
		if (!signal)
			continue;

		current_signals_.push_back(signal);
		cards_layout_->insertWidget(cards_layout_->count() - 1,
			create_decoder_card(signal));

		// Note: stacking/removing decoders, changing channel assignments and
		// renaming does not emit signals_changed(), so listen to the decode
		// signal's own notifications as well
		connect(signal.get(), SIGNAL(enabled_changed(bool)),
			this, SLOT(on_signal_enabled_changed(bool)),
			Qt::UniqueConnection);
		connect(signal.get(), SIGNAL(name_changed(const QString&)),
			this, SLOT(on_decoder_stack_changed()),
			Qt::UniqueConnection);
		connect(signal.get(), SIGNAL(channels_updated()),
			this, SLOT(on_decoder_stack_changed()),
			Qt::UniqueConnection);
		connect(signal.get(), SIGNAL(decoder_stacked(void*)),
			this, SLOT(on_decoder_stack_changed()),
			Qt::UniqueConnection);
		connect(signal.get(), SIGNAL(decoder_removed(void*)),
			this, SLOT(on_decoder_stack_changed()),
			Qt::UniqueConnection);
	}
}

QWidget* ProtocolDock::create_decoder_card(shared_ptr<DecodeSignal> signal)
{
	QGroupBox *card = new QGroupBox(signal->name());
	card->setObjectName(QString::fromUtf8("DecoderCard"));

	QVBoxLayout *card_layout = new QVBoxLayout(card);
	card_layout->setContentsMargins(6, 6, 6, 6);
	card_layout->setSpacing(4);

	// Header row: visibility check box, settings toggle, delete button
	QHBoxLayout *header_layout = new QHBoxLayout;

	QCheckBox *visible_cb = new QCheckBox(tr("Visible"), card);
	visible_cb->setChecked(signal->enabled());
	visible_cb->setToolTip(tr("Show/hide this decoder in the trace view"));
	signal_by_checkbox_[visible_cb] = signal.get();
	checkbox_by_signal_[signal.get()] = visible_cb;
	header_layout->addWidget(visible_cb);
	header_layout->addStretch();

	QToolButton *settings_button = new QToolButton(card);
	settings_button->setText(tr("Settings"));
	settings_button->setCheckable(true);
	settings_button->setChecked(true);
	settings_button->setToolTip(
		tr("Show/hide channel mapping and decoder options"));
	header_layout->addWidget(settings_button);

	QToolButton *delete_button = new QToolButton(card);
	delete_button->setText(tr("Delete"));
	delete_button->setToolTip(tr("Remove this decoder from the session"));
	delete_signal_mapper_.setMapping(delete_button, signal.get());
	connect(delete_button, SIGNAL(clicked()),
		&delete_signal_mapper_, SLOT(map()));
	header_layout->addWidget(delete_button);

	card_layout->addLayout(header_layout);

	// Details: channel mapping and options for every decoder in the stack
	QWidget *details = new QWidget(card);
	QVBoxLayout *details_layout = new QVBoxLayout(details);
	details_layout->setContentsMargins(0, 0, 0, 0);
	details_layout->setSpacing(4);

	const vector< shared_ptr<Decoder> > &stack = signal->decoder_stack();

	if (stack.empty()) {
		QLabel *const l = new QLabel(
			tr("<p><i>No decoders in the stack</i></p>"), details);
		l->setAlignment(Qt::AlignCenter);
		details_layout->addWidget(l);
	} else {
		for (int i = 0; i < (int)stack.size(); i++) {
			shared_ptr<Decoder> dec(stack[i]);
			create_decoder_form(signal, i, dec, details, details_layout);
		}
	}

	card_layout->addWidget(details);

	connect(settings_button, SIGNAL(toggled(bool)),
		details, SLOT(setVisible(bool)));
	connect(visible_cb, SIGNAL(toggled(bool)),
		this, SLOT(on_visible_toggled(bool)));

	return card;
}

void ProtocolDock::create_decoder_form(shared_ptr<DecodeSignal> signal,
	int index, shared_ptr<Decoder> &dec, QWidget *parent, QVBoxLayout *layout)
{
	GlobalSettings settings;

	assert(dec);
	const srd_decoder *const decoder = dec->get_srd_decoder();
	assert(decoder);

	const bool decoder_deletable = index > 0;

	pv::widgets::DecoderGroupBox *const group =
		new pv::widgets::DecoderGroupBox(
			QString::fromUtf8(decoder->name),
			tr("%1:\n%2").arg(QString::fromUtf8(decoder->longname),
				QString::fromUtf8(decoder->desc)),
			nullptr, decoder_deletable);
	group->set_decoder_visible(dec->visible());

	stacked_decoder_by_group_[group] = make_pair(signal.get(), index);

	if (decoder_deletable) {
		delete_decoder_mapper_.setMapping(group, group);
		connect(group, SIGNAL(delete_decoder()),
			&delete_decoder_mapper_, SLOT(map()));
	}

	show_hide_decoder_mapper_.setMapping(group, group);
	connect(group, SIGNAL(show_hide_decoder()),
		&show_hide_decoder_mapper_, SLOT(map()));

	QFormLayout *const decoder_form = new QFormLayout;
	group->add_layout(decoder_form);

	const vector<DecodeChannel> channels = signal->get_channels();

	// Add the channels
	for (const DecodeChannel &ch : channels) {
		// Ignore channels not part of the decoder we create the form for
		if (ch.decoder_ != dec)
			continue;

		QComboBox *const combo = create_channel_selector(parent, &ch);
		QComboBox *const combo_init_state =
			create_channel_selector_init_state(parent, &ch);

		channel_by_combo_[combo] = make_pair(signal.get(), ch.id);
		init_state_by_combo_[combo_init_state] = make_pair(signal.get(), ch.id);

		connect(combo, SIGNAL(currentIndexChanged(int)),
			this, SLOT(on_channel_selected(int)));
		connect(combo_init_state, SIGNAL(currentIndexChanged(int)),
			this, SLOT(on_init_state_changed(int)));

		QHBoxLayout *const hlayout = new QHBoxLayout;
		hlayout->addWidget(combo);
		hlayout->addWidget(combo_init_state);

		if (!settings.value(GlobalSettings::Key_Dec_InitialStateConfigurable).toBool())
			combo_init_state->hide();

		const QString required_flag = ch.is_optional ? QString() : QString("*");
		decoder_form->addRow(tr("<b>%1</b> (%2) %3")
			.arg(ch.name, ch.desc, required_flag), hlayout);
	}

	// Add the options
	shared_ptr<binding::Decoder> binding(
		new binding::Decoder(signal, dec));
	binding->add_properties_to_form(decoder_form, true);

	bindings_.push_back(binding);

	layout->addWidget(group);
}

QComboBox* ProtocolDock::create_channel_selector(QWidget *parent,
	const DecodeChannel *ch)
{
	const auto sigs(session_.signalbases());

	// Sort signals in natural order
	vector< shared_ptr<SignalBase> > sig_list(sigs.begin(), sigs.end());
	sort(sig_list.begin(), sig_list.end(),
		[](const shared_ptr<SignalBase> &a, const shared_ptr<SignalBase> &b) {
			return strnatcasecmp(a->name().toStdString(),
				b->name().toStdString()) < 0; });

	QComboBox *selector = new QComboBox(parent);

	selector->addItem("-", QVariant::fromValue((void*)nullptr));

	if (!ch->assigned_signal)
		selector->setCurrentIndex(0);

	for (const shared_ptr<SignalBase> &b : sig_list) {
		assert(b);
		if (b->logic_data() && b->enabled()) {
			selector->addItem(b->name(), QVariant::fromValue(b));

			if (ch->assigned_signal == b)
				selector->setCurrentIndex(selector->count() - 1);
		}
	}

	return selector;
}

QComboBox* ProtocolDock::create_channel_selector_init_state(QWidget *parent,
	const DecodeChannel *ch)
{
	QComboBox *selector = new QComboBox(parent);

	selector->addItem("0", QVariant::fromValue((int)SRD_INITIAL_PIN_LOW));
	selector->addItem("1", QVariant::fromValue((int)SRD_INITIAL_PIN_HIGH));
	selector->addItem("X", QVariant::fromValue((int)SRD_INITIAL_PIN_SAME_AS_SAMPLE0));

	selector->setCurrentIndex(ch->initial_pin_state);

	selector->setToolTip(tr("Initial (assumed) pin value before the first sample"));

	return selector;
}

void ProtocolDock::on_add_decoder_selected(srd_decoder *decoder)
{
	vector<const srd_decoder*> decoders;
	decoders.push_back(decoder);

	session_.on_new_decoders_selected(decoders);
}

void ProtocolDock::on_signals_changed()
{
	rebuild_decoder_list();
}

void ProtocolDock::on_decoder_stack_changed()
{
	// decoder_removed is emitted before the stack is updated, so defer the
	// rebuild until the signal emission has finished
	QTimer::singleShot(0, this, SLOT(rebuild_decoder_list()));
}

void ProtocolDock::on_delete_decode_signal(QObject *signal_obj)
{
	for (const shared_ptr<DecodeSignal> &s : current_signals_)
		if (s.get() == signal_obj) {
			session_.remove_decode_signal(s);
			return;
		}
}

void ProtocolDock::on_visible_toggled(bool enabled)
{
	auto it = signal_by_checkbox_.find(sender());
	if (it != signal_by_checkbox_.end())
		it->second->set_enabled(enabled);
}

void ProtocolDock::on_signal_enabled_changed(bool enabled)
{
	auto it = checkbox_by_signal_.find(sender());
	if (it != checkbox_by_signal_.end())
		it->second->setChecked(enabled);
}

void ProtocolDock::on_show_hide_stacked_decoder(QObject *group_obj)
{
	auto it = stacked_decoder_by_group_.find(group_obj);
	if (it == stacked_decoder_by_group_.end())
		return;

	const bool state =
		it->second.first->toggle_decoder_visibility(it->second.second);

	pv::widgets::DecoderGroupBox *group =
		qobject_cast<pv::widgets::DecoderGroupBox*>(group_obj);
	if (group)
		group->set_decoder_visible(state);
}

void ProtocolDock::on_delete_stacked_decoder(QObject *group_obj)
{
	auto it = stacked_decoder_by_group_.find(group_obj);
	if (it == stacked_decoder_by_group_.end())
		return;

	it->second.first->remove_decoder(it->second.second);

	// decoder_removed is emitted before the stack is updated, rebuild now
	// that remove_decoder() has finished
	rebuild_decoder_list();
}

void ProtocolDock::on_channel_selected(int)
{
	QComboBox *cb = qobject_cast<QComboBox*>(sender());

	auto it = channel_by_combo_.find(cb);
	if (it == channel_by_combo_.end())
		return;

	// Determine signal that was selected
	shared_ptr<SignalBase> signal =
		cb->itemData(cb->currentIndex()).value<shared_ptr<SignalBase>>();

	it->second.first->assign_signal(it->second.second, signal);
}

void ProtocolDock::on_init_state_changed(int)
{
	QComboBox *cb = qobject_cast<QComboBox*>(sender());

	auto it = init_state_by_combo_.find(cb);
	if (it == init_state_by_combo_.end())
		return;

	// Determine initial pin state that was selected
	int init_state = cb->itemData(cb->currentIndex()).value<int>();

	it->second.first->set_initial_pin_state(it->second.second, init_state);
}

void ProtocolDock::on_show_decode_table()
{
	decode_table_requested(&session_);
}

} // namespace dock
} // namespace pv
