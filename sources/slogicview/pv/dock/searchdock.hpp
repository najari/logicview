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

#ifndef PULSEVIEW_PV_DOCK_SEARCHDOCK_HPP
#define PULSEVIEW_PV_DOCK_SEARCHDOCK_HPP

#include <cstdint>
#include <memory>
#include <vector>

#include <QString>
#include <QWidget>

using std::shared_ptr;
using std::vector;

class QCheckBox;
class QComboBox;
class QLabel;
class QLineEdit;
class QListWidget;
class QPushButton;
class QTimer;

namespace pv {

class Session;

namespace data {
class DecodeSignal;
namespace decode {
class Annotation;
}
}

namespace views {
namespace trace {
class View;
}
}

namespace dock {

/**
 * Dockable panel that searches the annotations of all decode signals in a
 * session for a substring and allows jumping to the matching annotation
 * in the trace view.
 */
class SearchDock : public QWidget
{
	Q_OBJECT

public:
	explicit SearchDock(Session &session, QWidget *parent = nullptr);

private Q_SLOTS:
	void on_signals_changed();
	void on_query_edited();
	void on_scope_changed(int index);
	void on_case_toggled(bool checked);

	void schedule_search();
	void run_search();

	void on_result_row_changed(int row);
	void on_previous();
	void on_next();

private:
	bool eventFilter(QObject *watched, QEvent *event) override;

	void rebuild_scope_list();
	vector< shared_ptr<data::DecodeSignal> > scoped_signals() const;
	views::trace::View* trace_view() const;
	uint32_t current_segment_id() const;
	void goto_result(int index);

private:
	/// Maximum number of results kept to keep the UI responsive
	static const unsigned int MaxResults = 500;

	/// Debounce delay for re-running the search while typing, in ms
	static const int SearchDelay = 300;

	struct Result {
		shared_ptr<data::DecodeSignal> signal;
		const data::decode::Annotation *annotation;
		QString matched_text;
	};

	Session &session_;

	QLineEdit *search_edit_;
	QComboBox *scope_combo_;
	QCheckBox *case_box_;
	QPushButton *prev_button_;
	QPushButton *next_button_;
	QLabel *hint_label_;
	QLabel *status_label_;
	QListWidget *results_list_;
	QTimer *search_timer_;

	/// Decode signals shown in the scope combo, entry 0 means "all"
	vector< shared_ptr<data::DecodeSignal> > known_signals_;

	vector<Result> results_;
	bool results_capped_;
};

} // namespace dock
} // namespace pv

#endif // PULSEVIEW_PV_DOCK_SEARCHDOCK_HPP
