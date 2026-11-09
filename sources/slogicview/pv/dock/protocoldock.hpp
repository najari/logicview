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

#ifndef PULSEVIEW_PV_DOCK_PROTOCOLDOCK_HPP
#define PULSEVIEW_PV_DOCK_PROTOCOLDOCK_HPP

#include <cstdint>
#include <map>
#include <memory>
#include <utility>
#include <vector>

#include <QSignalMapper>
#include <QWidget>

using std::map;
using std::pair;
using std::shared_ptr;
using std::vector;

struct srd_decoder;

class QCheckBox;
class QComboBox;
class QVBoxLayout;

namespace pv {

class Session;

namespace data {
class DecodeSignal;
namespace decode {
class Decoder;
struct DecodeChannel;
}
}

namespace binding {
class Decoder;
}

namespace dock {

/**
 * Dockable panel that lists all decode signals of a session as cards,
 * allowing the user to add/remove decoders, toggle their visibility and
 * edit channel assignments and decoder options.
 */
class ProtocolDock : public QWidget
{
	Q_OBJECT

public:
	explicit ProtocolDock(Session &session, QWidget *parent = nullptr);

Q_SIGNALS:
	void decode_table_requested(Session *session);

private Q_SLOTS:
	void rebuild_decoder_list();

	void on_add_decoder_selected(srd_decoder *decoder);
	void on_signals_changed();
	void on_decoder_stack_changed();

	void on_delete_decode_signal(QObject *signal_obj);
	void on_visible_toggled(bool enabled);
	void on_signal_enabled_changed(bool enabled);

	void on_show_hide_stacked_decoder(QObject *group_obj);
	void on_delete_stacked_decoder(QObject *group_obj);

	void on_channel_selected(int);
	void on_init_state_changed(int);

	void on_show_decode_table();

private:
	QWidget* create_decoder_card(shared_ptr<data::DecodeSignal> signal);

	void create_decoder_form(shared_ptr<data::DecodeSignal> signal, int index,
		shared_ptr<data::decode::Decoder> &dec, QWidget *parent, QVBoxLayout *layout);

	QComboBox* create_channel_selector(QWidget *parent,
		const data::decode::DecodeChannel *ch);

	QComboBox* create_channel_selector_init_state(QWidget *parent,
		const data::decode::DecodeChannel *ch);

private:
	Session &session_;

	QVBoxLayout *cards_layout_;

	/// Decode signals currently shown, keeps them alive while cards exist
	vector< shared_ptr<data::DecodeSignal> > current_signals_;

	/// Property bindings of all decoder option forms, must stay alive
	vector< shared_ptr<binding::Decoder> > bindings_;

	QSignalMapper delete_signal_mapper_;
	QSignalMapper show_hide_decoder_mapper_, delete_decoder_mapper_;

	//@{
	/// Lookup tables used by the slot-based signal forwarding
	map<QObject*, data::DecodeSignal*> signal_by_checkbox_;
	map<QObject*, QCheckBox*> checkbox_by_signal_;
	map<QObject*, pair<data::DecodeSignal*, int> > stacked_decoder_by_group_;
	map<QObject*, pair<data::DecodeSignal*, uint16_t> > channel_by_combo_;
	map<QObject*, pair<data::DecodeSignal*, uint16_t> > init_state_by_combo_;
	//@}
};

} // namespace dock
} // namespace pv

#endif // PULSEVIEW_PV_DOCK_PROTOCOLDOCK_HPP
