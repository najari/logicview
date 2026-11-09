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

#include <deque>

#include <QCheckBox>
#include <QComboBox>
#include <QHBoxLayout>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QListWidget>
#include <QPushButton>
#include <QTimer>
#include <QVBoxLayout>

#include "searchdock.hpp"

#include <pv/data/decodesignal.hpp>
#include <pv/data/decode/annotation.hpp>
#include <pv/data/decode/row.hpp>
#include <pv/data/signalbase.hpp>
#include <pv/session.hpp>
#include <pv/util.hpp>
#include <pv/views/viewbase.hpp>
#include <pv/views/trace/view.hpp>

using std::deque;
using std::dynamic_pointer_cast;

using pv::data::DecodeSignal;
using pv::data::SignalBase;
using pv::data::decode::Annotation;
using pv::util::SIPrefix;
using pv::util::Timestamp;

namespace pv {
namespace dock {

SearchDock::SearchDock(Session &session, QWidget *parent) :
	QWidget(parent),
	session_(session),
	results_capped_(false)
{
	setObjectName(QString::fromUtf8("SearchDock"));

	QVBoxLayout *root_layout = new QVBoxLayout(this);
	root_layout->setContentsMargins(8, 8, 8, 8);
	root_layout->setSpacing(6);

	// Title row: panel title and result status
	QHBoxLayout *title_layout = new QHBoxLayout;
	QLabel *title_label = new QLabel(tr("Search"), this);
	title_label->setObjectName(QString::fromUtf8("SearchDockTitle"));

	status_label_ = new QLabel(this);
	status_label_->setObjectName(QString::fromUtf8("SearchStatus"));

	title_layout->addWidget(title_label);
	title_layout->addStretch();
	title_layout->addWidget(status_label_);
	root_layout->addLayout(title_layout);

	// Search row: query input, scope selector, match case, prev/next
	QHBoxLayout *search_layout = new QHBoxLayout;
	search_layout->setContentsMargins(0, 0, 0, 0);

	search_edit_ = new QLineEdit(this);
	search_edit_->setPlaceholderText(tr("Search annotations..."));
	search_edit_->setClearButtonEnabled(true);
	search_edit_->installEventFilter(this);

	scope_combo_ = new QComboBox(this);
	scope_combo_->setToolTip(tr("Limit the search to a single decoder"));

	case_box_ = new QCheckBox(tr("Match Case"), this);

	prev_button_ = new QPushButton(tr("Prev"), this);
	prev_button_->setToolTip(tr("Jump to the previous match (Shift+Enter)"));
	next_button_ = new QPushButton(tr("Next"), this);
	next_button_->setToolTip(tr("Jump to the next match (Enter)"));

	search_layout->addWidget(search_edit_, 1);
	search_layout->addWidget(scope_combo_);
	search_layout->addWidget(case_box_);
	search_layout->addWidget(prev_button_);
	search_layout->addWidget(next_button_);
	root_layout->addLayout(search_layout);

	// Hint shown when there is nothing to search
	hint_label_ = new QLabel(
		tr("Add a protocol decoder to search its annotations."), this);
	hint_label_->setObjectName(QString::fromUtf8("SearchHint"));
	root_layout->addWidget(hint_label_);

	results_list_ = new QListWidget(this);
	results_list_->setUniformItemSizes(true);
	root_layout->addWidget(results_list_, 1);

	search_timer_ = new QTimer(this);
	search_timer_->setSingleShot(true);
	search_timer_->setInterval(SearchDelay);

	connect(search_timer_, SIGNAL(timeout()),
		this, SLOT(run_search()));
	connect(search_edit_, SIGNAL(textChanged(const QString&)),
		this, SLOT(on_query_edited()));
	connect(scope_combo_, SIGNAL(currentIndexChanged(int)),
		this, SLOT(on_scope_changed(int)));
	connect(case_box_, SIGNAL(toggled(bool)),
		this, SLOT(on_case_toggled(bool)));
	connect(prev_button_, SIGNAL(clicked()),
		this, SLOT(on_previous()));
	connect(next_button_, SIGNAL(clicked()),
		this, SLOT(on_next()));
	connect(results_list_, SIGNAL(currentRowChanged(int)),
		this, SLOT(on_result_row_changed(int)));
	connect(&session_, SIGNAL(signals_changed()),
		this, SLOT(on_signals_changed()));

	rebuild_scope_list();
	run_search();
}

bool SearchDock::eventFilter(QObject *watched, QEvent *event)
{
	// Enter jumps to the next match, Shift+Enter to the previous one
	if ((watched == search_edit_) && (event->type() == QEvent::KeyPress)) {
		QKeyEvent *key_event = static_cast<QKeyEvent*>(event);
		if ((key_event->key() == Qt::Key_Return) ||
			(key_event->key() == Qt::Key_Enter)) {
			if (key_event->modifiers() & Qt::ShiftModifier)
				on_previous();
			else
				on_next();
			return true;
		}
	}

	return QWidget::eventFilter(watched, event);
}

views::trace::View* SearchDock::trace_view() const
{
	for (const shared_ptr<views::ViewBase>& view : session_.views())
		if (view->get_type() == views::ViewTypeTrace)
			return dynamic_cast<views::trace::View*>(view.get());

	return nullptr;
}

uint32_t SearchDock::current_segment_id() const
{
	views::trace::View *view = trace_view();
	return view ? view->current_segment() : 0;
}

void SearchDock::rebuild_scope_list()
{
	// Remember the currently selected decoder to restore it afterwards
	data::DecodeSignal *selected = nullptr;
	const int prev_index = scope_combo_->currentIndex();
	if ((prev_index > 0) && (prev_index <= (int)known_signals_.size()))
		selected = known_signals_[prev_index - 1].get();

	for (const shared_ptr<DecodeSignal>& signal : known_signals_)
		disconnect(signal.get(), nullptr, this, nullptr);

	known_signals_.clear();

	for (const shared_ptr<SignalBase>& base : session_.signalbases()) {
		if (!base->is_decode_signal())
			continue;

		shared_ptr<DecodeSignal> signal = dynamic_pointer_cast<DecodeSignal>(base);
		if (!signal)
			continue;

		known_signals_.push_back(signal);

		// Re-run the search when the underlying annotations change
		connect(signal.get(), SIGNAL(new_annotations()),
			this, SLOT(schedule_search()), Qt::UniqueConnection);
		connect(signal.get(), SIGNAL(decode_finished()),
			this, SLOT(schedule_search()), Qt::UniqueConnection);
		connect(signal.get(), SIGNAL(decode_reset()),
			this, SLOT(schedule_search()), Qt::UniqueConnection);
	}

	scope_combo_->blockSignals(true);
	scope_combo_->clear();
	scope_combo_->addItem(tr("All decoders"));

	int restore_index = 0;
	for (size_t i = 0; i < known_signals_.size(); i++) {
		scope_combo_->addItem(known_signals_[i]->name());
		if (known_signals_[i].get() == selected)
			restore_index = i + 1;
	}

	scope_combo_->setCurrentIndex(restore_index);
	scope_combo_->blockSignals(false);
}

vector< shared_ptr<DecodeSignal> > SearchDock::scoped_signals() const
{
	const int index = scope_combo_->currentIndex();
	if ((index > 0) && (index <= (int)known_signals_.size()))
		return {known_signals_[index - 1]};

	return known_signals_;
}

void SearchDock::on_signals_changed()
{
	rebuild_scope_list();
	run_search();
}

void SearchDock::on_query_edited()
{
	search_timer_->start();
}

void SearchDock::schedule_search()
{
	search_timer_->start();
}

void SearchDock::on_scope_changed(int index)
{
	(void)index;
	run_search();
}

void SearchDock::on_case_toggled(bool checked)
{
	(void)checked;
	run_search();
}

void SearchDock::run_search()
{
	results_.clear();
	results_capped_ = false;
	results_list_->clear();

	const QString query = search_edit_->text();
	const vector< shared_ptr<DecodeSignal> > signals = scoped_signals();

	if (!query.isEmpty()) {
		const Qt::CaseSensitivity cs = case_box_->isChecked() ?
			Qt::CaseSensitive : Qt::CaseInsensitive;
		const uint32_t segment_id = current_segment_id();

		for (const shared_ptr<DecodeSignal>& signal : signals) {
			const deque<const Annotation*> *annotations =
				signal->get_all_annotations_by_segment(segment_id);
			if (!annotations)
				continue;

			for (const Annotation *ann : *annotations) {
				QString matched_text;
				const vector<QString> *texts = ann->annotations();
				if (texts)
					for (const QString& text : *texts)
						if (text.contains(query, cs)) {
							matched_text = text;
							break;
						}

				if (matched_text.isEmpty())
					continue;

				results_.push_back({signal, ann, matched_text});

				if (results_.size() >= MaxResults) {
					results_capped_ = true;
					break;
				}
			}

			if (results_capped_)
				break;
		}

		for (size_t i = 0; i < results_.size(); i++) {
			const Result &result = results_[i];
			const Annotation *ann = result.annotation;

			QString time_text;
			const double samplerate = result.signal->get_samplerate();
			if (samplerate > 0) {
				const Timestamp t = ann->start_sample() / samplerate;
				time_text = util::format_time_si(t,
					SIPrefix::unspecified, 6, "s", false);
			} else {
				time_text = QStringLiteral("@%1").arg(ann->start_sample());
			}

			const QString row_title =
				ann->row() ? ann->row()->title() : QString();

			QListWidgetItem *item = new QListWidgetItem(
				QStringLiteral("%1 | %2 | %3 | %4")
					.arg(result.signal->name(), row_title,
						time_text, result.matched_text),
				results_list_);
			item->setData(Qt::UserRole, (int)i);
			item->setToolTip(result.matched_text);
		}
	}

	// Update the empty states and the status line
	const bool have_decoders = !known_signals_.empty();

	hint_label_->setVisible(!have_decoders);
	results_list_->setVisible(have_decoders);
	status_label_->setVisible(have_decoders);

	search_edit_->setEnabled(have_decoders);
	scope_combo_->setEnabled(have_decoders);
	case_box_->setEnabled(have_decoders);
	prev_button_->setEnabled(!results_.empty());
	next_button_->setEnabled(!results_.empty());

	if (!have_decoders)
		return;

	if (query.isEmpty())
		status_label_->setText(tr("Enter a search term"));
	else if (results_.empty())
		status_label_->setText(tr("No results"));
	else if (results_capped_)
		status_label_->setText(
			tr("Showing first %1 results").arg(results_.size()));
	else
		status_label_->setText(
			tr("%n result(s)", nullptr, (int)results_.size()));
}

void SearchDock::goto_result(int index)
{
	views::trace::View *view = trace_view();
	if (!view)
		return;

	const Result &result = results_[index];
	view->focus_on_range(result.annotation->start_sample(),
		result.annotation->end_sample());
}

void SearchDock::on_result_row_changed(int row)
{
	if ((row < 0) || (row >= (int)results_.size()))
		return;

	goto_result(row);
}

void SearchDock::on_previous()
{
	if (results_.empty())
		return;

	int row = results_list_->currentRow();
	row = (row <= 0) ? (int)results_.size() - 1 : row - 1;
	results_list_->setCurrentRow(row);
}

void SearchDock::on_next()
{
	if (results_.empty())
		return;

	int row = results_list_->currentRow();
	row = (row < 0) ? 0 : (row + 1) % (int)results_.size();
	results_list_->setCurrentRow(row);
}

} // namespace dock
} // namespace pv
