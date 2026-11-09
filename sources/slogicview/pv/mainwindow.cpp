/*
 * This file is part of the PulseView project.
 *
 * Copyright (C) 2012 Joel Holdsworth <joel@airwebreathe.org.uk>
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

#ifdef ENABLE_DECODE
#include <libsigrokdecode/libsigrokdecode.h>
#endif

#include <algorithm>
#include <cassert>
#include <cstdarg>
#include <cstdint>
#include <iterator>

#include <QAction>
#include <QApplication>
#include <QCloseEvent>
#include <QDebug>
#include <QDockWidget>
#include <QFileDialog>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QLabel>
#include <QMessageBox>
#include <QPushButton>
#include <QSettings>
#include <QShortcut>
#include <QStatusBar>
#include <QTabBar>
#include <QVBoxLayout>
#include <QWidget>

#include "mainwindow.hpp"

#include "config.h"

#include "application.hpp"
#include "devicemanager.hpp"
#include "devices/device.hpp"
#include "devices/hardwaredevice.hpp"
#include "dialogs/settings.hpp"
#include "dock/triggerdock.hpp"
#include "globalsettings.hpp"
#include "toolbars/mainbar.hpp"
#include "util.hpp"
#include "views/trace/view.hpp"
#include "views/trace/standardbar.hpp"

#ifdef ENABLE_DECODE
#include "dock/protocoldock.hpp"
#include "dock/measuredock.hpp"
#include "dock/searchdock.hpp"
#include "subwindows/decoder_selector/subwindow.hpp"
#include "views/decoder_binary/view.hpp"
#include "views/tabular_decoder/view.hpp"
#endif

#include <libsigrokcxx/libsigrokcxx.hpp>

using std::dynamic_pointer_cast;
using std::make_shared;
using std::shared_ptr;
using std::string;

namespace pv {

using toolbars::MainBar;

const QString MainWindow::WindowTitle = QStringLiteral(PV_TITLE);

MainWindow::MainWindow(DeviceManager &device_manager, QWidget *parent) :
	QMainWindow(parent),
	device_manager_(device_manager),
	session_selector_(this),
	icon_red_(":/icons/status-red.svg"),
	icon_green_(":/icons/status-green.svg"),
	icon_grey_(":/icons/status-grey.svg")
{
	setup_ui();
	restore_ui_settings();
	connect(this, SIGNAL(session_error_raised(const QString, const QString)),
		this, SLOT(on_session_error_raised(const QString, const QString)));
}

MainWindow::~MainWindow()
{
	// Make sure we no longer hold any shared pointers to widgets after the
	// destructor finishes (goes for sessions and sub windows alike)

	while (!sessions_.empty())
		remove_session(sessions_.front());

	sub_windows_.clear();
}

void MainWindow::show_session_error(const QString text, const QString info_text)
{
	// TODO Emulate noquote()
	qDebug() << "Notifying user of session error: " << text << "; " << info_text;

	QMessageBox msg;
	msg.setText(text + "\n\n" + info_text);
	msg.setStandardButtons(QMessageBox::Ok);
	msg.setIcon(QMessageBox::Warning);
	msg.exec();
}

shared_ptr<views::ViewBase> MainWindow::get_active_view() const
{
	// If there's only one view, use it...
	if (view_docks_.size() == 1)
		return view_docks_.begin()->second;

	// ...otherwise find the dock widget the widget with focus is contained in
	QObject *w = QApplication::focusWidget();
	QDockWidget *dock = nullptr;

	while (w) {
		dock = qobject_cast<QDockWidget*>(w);
		if (dock)
			break;
		w = w->parent();
	}

	// Get the view contained in the dock widget
	for (auto& entry : view_docks_)
		if (entry.first == dock)
			return entry.second;

	return nullptr;
}

shared_ptr<views::ViewBase> MainWindow::add_view(views::ViewType type,
	Session &session)
{
	GlobalSettings settings;
	shared_ptr<views::ViewBase> v;

	QMainWindow *main_window = nullptr;
	for (auto& entry : session_windows_)
		if (entry.first.get() == &session)
			main_window = entry.second;

	assert(main_window);

	shared_ptr<MainBar> main_bar = session.main_bar();

	// Only use the view type in the name if it's not the main view
	QString title;
	if (main_bar)
		title = QString("%1 (%2)").arg(session.name(), views::ViewTypeNames[type]);
	else
		title = session.name();

	QDockWidget* dock = new QDockWidget(title, main_window);
	dock->setObjectName(QString::fromUtf8("ViewDock"));
	main_window->addDockWidget(Qt::TopDockWidgetArea, dock);

	// Insert a QMainWindow into the dock widget to allow for a tool bar
	QMainWindow *dock_main = new QMainWindow(dock);
	dock_main->setObjectName(QString::fromUtf8("ViewContainer"));
	dock_main->setWindowFlags(Qt::Widget);  // Remove Qt::Window flag

	if (type == views::ViewTypeTrace)
		// This view will be the main view if there's no main bar yet
		v = make_shared<views::trace::View>(session, (main_bar ? false : true), dock_main);
#ifdef ENABLE_DECODE
	if (type == views::ViewTypeDecoderBinary)
		v = make_shared<views::decoder_binary::View>(session, false, dock_main);
	if (type == views::ViewTypeTabularDecoder)
		v = make_shared<views::tabular_decoder::View>(session, false, dock_main);
#endif

	if (!v)
		return nullptr;

	view_docks_[dock] = v;
	session.register_view(v);

	dock_main->setCentralWidget(v.get());
	dock->setWidget(dock_main);

	dock->setContextMenuPolicy(Qt::PreventContextMenu);
	dock->setFeatures(QDockWidget::DockWidgetMovable |
		QDockWidget::DockWidgetFloatable | QDockWidget::DockWidgetClosable);

	/* The main view's dock title would merely duplicate the session tab
	 * name, so its title bar is hidden. Closing the main view is done by
	 * closing the session tab instead (which asks for confirmation). */
	const bool is_main_view = (type == views::ViewTypeTrace) && !main_bar;

	if (is_main_view)
		dock->setTitleBarWidget(new QWidget(dock));
	else {
		QAbstractButton *close_btn =
			dock->findChildren<QAbstractButton*>("qt_dockwidget_closebutton")  // clazy:exclude=detaching-temporary
				.front();

		connect(close_btn, SIGNAL(clicked(bool)),
			this, SLOT(on_view_close_clicked()));
	}

	connect(&session, SIGNAL(trigger_event(int, util::Timestamp)),
		qobject_cast<views::ViewBase*>(v.get()),
		SLOT(trigger_event(int, util::Timestamp)));

	connect(&session, SIGNAL(session_error_raised(const QString, const QString)),
		this, SLOT(on_session_error_raised(const QString, const QString)));

	if (type == views::ViewTypeTrace) {
		views::trace::View *tv =
			qobject_cast<views::trace::View*>(v.get());

		if (!main_bar) {
			/* Initial view, create the main bar */
			main_bar = make_shared<MainBar>(session, this, tv);
			dock_main->addToolBar(main_bar.get());
			session.set_main_bar(main_bar);

			connect(main_bar.get(), SIGNAL(new_view(Session*, int)),
				this, SLOT(on_new_view(Session*, int)));
			connect(main_bar.get(), SIGNAL(show_decoder_selector(Session*)),
				this, SLOT(on_show_decoder_selector(Session*)));

			main_bar->action_view_show_cursors()->setChecked(tv->cursors_shown());
		} else {
			/* Additional view, create a standard bar */
			pv::views::trace::StandardBar *standard_bar =
				new pv::views::trace::StandardBar(session, this, tv);
			dock_main->addToolBar(standard_bar);

			standard_bar->action_view_show_cursors()->setChecked(tv->cursors_shown());
		}
	}

	v->setFocus();

	return v;
}

void MainWindow::remove_view(shared_ptr<views::ViewBase> view)
{
	for (shared_ptr<Session> session : sessions_) {
		if (!session->has_view(view))
			continue;

		// Find the dock the view is contained in and remove it
		for (auto& entry : view_docks_)
			if (entry.second == view) {
				// Remove the view from the session
				session->deregister_view(view);

				// Remove the view from its parent; otherwise, Qt will
				// call deleteLater() on it, which causes a double free
				// since the shared_ptr in view_docks_ doesn't know
				// that Qt keeps a pointer to the view around
				view->setParent(nullptr);

				// Delete the view's dock widget and all widgets inside it
				entry.first->deleteLater();

				// Remove the dock widget from the list and stop iterating
				view_docks_.erase(entry.first);
				break;
			}
	}
}

shared_ptr<subwindows::SubWindowBase> MainWindow::add_subwindow(
	subwindows::SubWindowType type, Session &session)
{
	GlobalSettings settings;
	shared_ptr<subwindows::SubWindowBase> w;

	QMainWindow *main_window = nullptr;
	for (auto& entry : session_windows_)
		if (entry.first.get() == &session)
			main_window = entry.second;

	assert(main_window);

	QString title = "";

	switch (type) {
#ifdef ENABLE_DECODE
		case subwindows::SubWindowTypeDecoderSelector:
			title = tr("Decoder Selector");
			break;
#endif
		default:
			break;
	}

	QDockWidget* dock = new QDockWidget(title, main_window);
	dock->setObjectName(title);
	main_window->addDockWidget(Qt::TopDockWidgetArea, dock);

	// Insert a QMainWindow into the dock widget to allow for a tool bar
	QMainWindow *dock_main = new QMainWindow(dock);
	dock_main->setWindowFlags(Qt::Widget);  // Remove Qt::Window flag

#ifdef ENABLE_DECODE
	if (type == subwindows::SubWindowTypeDecoderSelector)
		w = make_shared<subwindows::decoder_selector::SubWindow>(session, dock_main);
#endif

	if (!w)
		return nullptr;

	sub_windows_[dock] = w;
	dock_main->setCentralWidget(w.get());
	dock->setWidget(dock_main);

	dock->setContextMenuPolicy(Qt::PreventContextMenu);
	dock->setFeatures(QDockWidget::DockWidgetMovable |
		QDockWidget::DockWidgetFloatable | QDockWidget::DockWidgetClosable);

	QAbstractButton *close_btn =
		dock->findChildren<QAbstractButton*>  // clazy:exclude=detaching-temporary
			("qt_dockwidget_closebutton").front();

	// Allow all subwindows to be closed via ESC.
	close_btn->setShortcut(QKeySequence(Qt::Key_Escape));

	connect(close_btn, SIGNAL(clicked(bool)),
		this, SLOT(on_sub_window_close_clicked()));

	if (w->has_toolbar())
		dock_main->addToolBar(w->create_toolbar(dock_main));

	if (w->minimum_width() > 0)
		dock->setMinimumSize(w->minimum_width(), 0);

	return w;
}

shared_ptr<Session> MainWindow::add_session()
{
	static int last_session_id = 1;
	QString name = tr("Session %1").arg(last_session_id++);

	shared_ptr<Session> session = make_shared<Session>(device_manager_, name);

	connect(session.get(), SIGNAL(add_view(ViewType, Session*)),
		this, SLOT(on_add_view(ViewType, Session*)));
	connect(session.get(), SIGNAL(name_changed()),
		this, SLOT(on_session_name_changed()));
	connect(session.get(), SIGNAL(device_changed()),
		this, SLOT(on_session_device_changed()));
	connect(session.get(), SIGNAL(capture_state_changed(int)),
		this, SLOT(on_session_capture_state_changed(int)));

	sessions_.push_back(session);

	QMainWindow *window = new QMainWindow();
	window->setObjectName(QString::fromUtf8("SessionWorkspace"));
	window->setWindowFlags(Qt::Widget);  // Remove Qt::Window flag
	session_windows_[session] = window;

	hide_welcome_page();

	int index = session_selector_.addTab(window, name);
	session_selector_.setCurrentIndex(index);
	last_focused_session_ = session;

	window->setDockNestingEnabled(true);

	shared_ptr<views::ViewBase> main_view =
		add_view(views::ViewTypeTrace, *session);
	update_status_bar(session.get());

#ifdef ENABLE_DECODE
	// Add the protocol decoder dock to the right of the session workspace
	QDockWidget *protocol_dock = new QDockWidget(tr("Protocol Decoders"), window);
	protocol_dock->setObjectName(QString::fromUtf8("ProtocolDockWidget"));

	dock::ProtocolDock *protocol_panel =
		new dock::ProtocolDock(*session, protocol_dock);
	protocol_dock->setWidget(protocol_panel);

	protocol_dock->setContextMenuPolicy(Qt::PreventContextMenu);
	protocol_dock->setFeatures(QDockWidget::DockWidgetMovable |
		QDockWidget::DockWidgetFloatable | QDockWidget::DockWidgetClosable);

	// Note: the session workspace has no central widget, in which case Qt
	// does not honor the right dock area and places the dock at the bottom
	// instead. Splitting with the trace view dock achieves the intended
	// side-by-side layout.
	window->addDockWidget(Qt::RightDockWidgetArea, protocol_dock);

	QDockWidget *trace_dock = nullptr;
	for (auto& entry : view_docks_)
		if (entry.second == main_view)
			trace_dock = entry.first;

	if (trace_dock) {
		window->splitDockWidget(trace_dock, protocol_dock, Qt::Horizontal);
		window->resizeDocks({protocol_dock}, {360}, Qt::Horizontal);
	}

	connect(protocol_panel, SIGNAL(decode_table_requested(Session*)),
		this, SLOT(on_decode_table_requested(Session*)));

	// Bring the decoder settings up as soon as the user adds a decoder
	connect(session.get(), &Session::decoders_added, protocol_dock,
		[protocol_dock]() { protocol_dock->show(); protocol_dock->raise(); });

	// Hidden by default like the other panels, toggled from the Panels
	// menu so that the tool bar gets the full window width
	protocol_dock->hide();

	// Make the dock's toggle action available on the main bar
	shared_ptr<MainBar> main_bar = session->main_bar();
	if (main_bar) {
		QAction *toggle_action = protocol_dock->toggleViewAction();
		toggle_action->setIcon(QIcon(":/icons/dock-protocol.svg"));
		toggle_action->setToolTip(tr("Show/hide the protocol decoder panel"));
		main_bar->add_panel_action(toggle_action);
	}
#endif

	// Add the measurement dock to the right of the session workspace
	QDockWidget *measure_dock = new QDockWidget(tr("Measurements"), window);
	measure_dock->setObjectName(QString::fromUtf8("MeasureDockWidget"));

	dock::MeasureDock *measure_panel =
		new dock::MeasureDock(*session, measure_dock);
	measure_dock->setWidget(measure_panel);

	measure_dock->setContextMenuPolicy(Qt::PreventContextMenu);
	measure_dock->setFeatures(QDockWidget::DockWidgetMovable |
		QDockWidget::DockWidgetFloatable | QDockWidget::DockWidgetClosable);

	window->addDockWidget(Qt::RightDockWidgetArea, measure_dock);

#ifdef ENABLE_DECODE
	// Share the right dock area with the protocol decoder dock
	window->tabifyDockWidget(protocol_dock, measure_dock);
	protocol_dock->raise();
#else
	// Same right-area quirk as for the protocol dock above: without a
	// central widget the right dock area is not honored, so split
	// explicitly with the trace view dock
	QDockWidget *trace_dock = nullptr;
	for (auto& entry : view_docks_)
		if (entry.second == main_view)
			trace_dock = entry.first;

	if (trace_dock) {
		window->splitDockWidget(trace_dock, measure_dock, Qt::Horizontal);
		window->resizeDocks({measure_dock}, {360}, Qt::Horizontal);
	}
#endif

	// Hidden by default, toggled from the main bar
	measure_dock->hide();

	// Note: main_bar is only declared above when ENABLE_DECODE is set
	shared_ptr<MainBar> measure_main_bar = session->main_bar();
	if (measure_main_bar) {
		QAction *toggle_action = measure_dock->toggleViewAction();
		toggle_action->setIcon(QIcon(":/icons/dock-measure.svg"));
		toggle_action->setToolTip(tr("Show/hide the measurement panel"));
		measure_main_bar->add_panel_action(toggle_action);
	}

	// Add the trigger dock, tabified with the other right-side docks
	QDockWidget *trigger_dock = new QDockWidget(tr("Trigger"), window);
	trigger_dock->setObjectName(QString::fromUtf8("TriggerDockWidget"));

	dock::TriggerDock *trigger_panel =
		new dock::TriggerDock(*session, trigger_dock);
	trigger_dock->setWidget(trigger_panel);

	trigger_dock->setContextMenuPolicy(Qt::PreventContextMenu);
	trigger_dock->setFeatures(QDockWidget::DockWidgetMovable |
		QDockWidget::DockWidgetFloatable | QDockWidget::DockWidgetClosable);

	window->addDockWidget(Qt::RightDockWidgetArea, trigger_dock);
	window->tabifyDockWidget(measure_dock, trigger_dock);

	// Hidden by default, toggled from the main bar
	trigger_dock->hide();

	shared_ptr<MainBar> trigger_main_bar = session->main_bar();
	if (trigger_main_bar) {
		QAction *toggle_action = trigger_dock->toggleViewAction();
		toggle_action->setIcon(QIcon(":/icons/dock-trigger.svg"));
		toggle_action->setToolTip(tr("Show/hide the trigger panel"));
		trigger_main_bar->add_panel_action(toggle_action);
	}

#ifdef ENABLE_DECODE
	// Add the search dock to the bottom of the session workspace
	QDockWidget *search_dock = new QDockWidget(tr("Search"), window);
	search_dock->setObjectName(QString::fromUtf8("SearchDockWidget"));

	dock::SearchDock *search_panel =
		new dock::SearchDock(*session, search_dock);
	search_dock->setWidget(search_panel);

	search_dock->setContextMenuPolicy(Qt::PreventContextMenu);
	search_dock->setFeatures(QDockWidget::DockWidgetMovable |
		QDockWidget::DockWidgetFloatable | QDockWidget::DockWidgetClosable);

	window->addDockWidget(Qt::BottomDockWidgetArea, search_dock);

	// Same no-central-widget quirk as for the protocol dock above: split
	// with the trace view dock to keep the bottom area honored
	if (trace_dock) {
		window->splitDockWidget(trace_dock, search_dock, Qt::Vertical);
		window->resizeDocks({search_dock}, {220}, Qt::Vertical);
	}

	// Hidden by default, toggled from the main bar
	search_dock->hide();

	if (main_bar) {
		QAction *toggle_action = search_dock->toggleViewAction();
		toggle_action->setIcon(QIcon(":/icons/dock-search.svg"));
		toggle_action->setToolTip(tr("Show/hide the search panel"));
		main_bar->add_panel_action(toggle_action);
	}
#endif

	return session;
}

void MainWindow::remove_session(shared_ptr<Session> session)
{
	// Stop capture while the session still exists so that the UI can be
	// updated in case we're currently running. If so, this will schedule a
	// call to our on_capture_state_changed() slot for the next run of the
	// event loop. We need to have this executed immediately or else it will
	// be dismissed since the session object will be deleted by the time we
	// leave this method and the event loop gets a chance to run again.
	session->stop_capture();
	QApplication::processEvents();

	for (const shared_ptr<views::ViewBase>& view : session->views())
		remove_view(view);

	QMainWindow *window = session_windows_.at(session);
	session_selector_.removeTab(session_selector_.indexOf(window));

	session_windows_.erase(session);

	if (last_focused_session_ == session)
		last_focused_session_.reset();

	// Remove the session from our list of sessions (which also destroys it)
	sessions_.remove_if([&](shared_ptr<Session> s) {
		return s == session; });

	if (sessions_.empty()) {
		// Without any session there is no useful UI left, so we show the
		// welcome page instead of an empty window
		show_welcome_page();

		// Update the window title if there is no view left to
		// generate focus change events
		setWindowTitle(WindowTitle);
		update_status_bar(nullptr);
	}
}

void MainWindow::show_welcome_page()
{
	if (welcome_page_)
		return;

	welcome_page_ = new QWidget();
	welcome_page_->setObjectName(QString::fromUtf8("WelcomePage"));

	QLabel *icon_label = new QLabel();
	icon_label->setPixmap(QIcon(":/icons/pulseview.svg").pixmap(64, 64));

	QLabel *title_label = new QLabel(WindowTitle);
	title_label->setObjectName(QString::fromUtf8("WelcomeTitle"));

	QLabel *hint_label = new QLabel(
		tr("No session is open. Create a new session or open a capture file."));
	hint_label->setObjectName(QString::fromUtf8("WelcomeHint"));

	QPushButton *new_session_btn = new QPushButton(
		QIcon(":/icons/document-new.svg"), tr("New Session"));
	connect(new_session_btn, SIGNAL(clicked(bool)),
		this, SLOT(on_welcome_new_session_clicked()));

	QPushButton *open_btn = new QPushButton(
		QIcon(":/icons/document-open.svg"), tr("Open Capture..."));
	open_btn->setDefault(true);
	connect(open_btn, SIGNAL(clicked(bool)),
		this, SLOT(on_welcome_open_clicked()));

	QHBoxLayout *button_layout = new QHBoxLayout();
	button_layout->setSpacing(12);
	button_layout->addWidget(new_session_btn);
	button_layout->addWidget(open_btn);

	QVBoxLayout *layout = new QVBoxLayout(welcome_page_);
	layout->setSpacing(12);
	layout->addStretch();
	layout->addWidget(icon_label, 0, Qt::AlignHCenter);
	layout->addWidget(title_label, 0, Qt::AlignHCenter);
	layout->addWidget(hint_label, 0, Qt::AlignHCenter);
	layout->addSpacing(8);
	layout->addLayout(button_layout);
	layout->setAlignment(button_layout, Qt::AlignHCenter);
	layout->addStretch();

	int index = session_selector_.addTab(welcome_page_, tr("Welcome"));
	session_selector_.setCurrentIndex(index);

	// The welcome page is a placeholder, not a session - it can't be closed
	session_selector_.tabBar()->setTabButton(index, QTabBar::RightSide, nullptr);
}

void MainWindow::hide_welcome_page()
{
	if (!welcome_page_)
		return;

	session_selector_.removeTab(session_selector_.indexOf(welcome_page_));
	welcome_page_->deleteLater();
	welcome_page_ = nullptr;
}

void MainWindow::on_welcome_new_session_clicked()
{
	add_default_session();
}

void MainWindow::on_welcome_open_clicked()
{
	QSettings settings;
	const QString dir = settings.value("MainWindow/OpenDirectory").toString();

	const QString file_name = QFileDialog::getOpenFileName(
		this, tr("Open File"), dir, tr(
			"sigrok Sessions (*.sr);;"
			"All Files (*)"));

	if (!file_name.isEmpty()) {
		add_session_with_file(file_name.toStdString(), "", "");

		const QString abs_path = QFileInfo(file_name).absolutePath();
		settings.setValue("MainWindow/OpenDirectory", abs_path);
	}
}

void MainWindow::add_session_with_file(string open_file_name,
	string open_file_format, string open_setup_file_name)
{
	shared_ptr<Session> session = add_session();
	session->load_init_file(open_file_name, open_file_format, open_setup_file_name);
}

void MainWindow::add_default_session()
{
	// Only add the default session if there would be no session otherwise
	if (sessions_.size() > 0)
		return;

	shared_ptr<Session> session = add_session();

	// Prefer a device found using the user's scan specification, then any
	// auto-detected physical device, then the virtual demo device (which
	// generates synthetic data and supports full acquisition control).
	// Only when no device at all is available, show the bundled read-only
	// demonstration capture.
	shared_ptr<devices::HardwareDevice> user_device, other_device, demo_device;
	for (const shared_ptr<devices::HardwareDevice>& dev : device_manager_.devices()) {
		if (dev == device_manager_.user_spec_device()) {
			user_device = dev;
		} else if (dev->hardware_device()->driver()->name() != "demo") {
			other_device = dev;
		} else {
			demo_device = dev;
		}
	}
	if (user_device)
		session->select_device(user_device);
	else if (other_device)
		session->select_device(other_device);
	else if (demo_device)
		session->select_device(demo_device);
	else {
		const string demo_capture = PV_DATA_DIR "/demo/slogic32-demo.sr";
		const string demo_setup = PV_DATA_DIR "/demo/slogic32-demo.pvs";
		if (QFileInfo::exists(QString::fromStdString(demo_capture)))
			session->load_init_file(demo_capture, "", demo_setup);
		else
			qWarning() << "Bundled demo capture is missing:"
				<< QString::fromStdString(demo_capture);
	}
}

void MainWindow::save_sessions()
{
	QSettings settings;
	int id = 0;

	for (shared_ptr<Session>& session : sessions_) {
		// Ignore sessions using the demo device or no device at all
		if (session->device()) {
			shared_ptr<devices::HardwareDevice> device =
				dynamic_pointer_cast< devices::HardwareDevice >
				(session->device());

			if (device &&
				device->hardware_device()->driver()->name() == "demo")
				continue;

			settings.beginGroup("Session" + QString::number(id++));
			settings.remove("");  // Remove all keys in this group
			session->save_settings(settings);
			settings.endGroup();
		}
	}

	settings.setValue("sessions", id);
}

void MainWindow::restore_sessions()
{
	QSettings settings;
	int i, session_count;

	session_count = settings.value("sessions", 0).toInt();

	for (i = 0; i < session_count; i++) {
		settings.beginGroup("Session" + QString::number(i));
		shared_ptr<Session> session = add_session();
		session->restore_settings(settings);
		settings.endGroup();
	}
}

void MainWindow::setup_ui()
{
	setObjectName(QString::fromUtf8("MainWindow"));

	welcome_page_ = nullptr;

	setCentralWidget(&session_selector_);

	// Set the window icon
	QIcon icon;
	icon.addFile(QString(":/icons/pulseview.svg"));
	setWindowIcon(icon);

	// Set up keyboard shortcuts that affect all views at once
	view_sticky_scrolling_shortcut_ = new QShortcut(QKeySequence(Qt::Key_S), this, SLOT(on_view_sticky_scrolling_shortcut()));
	view_sticky_scrolling_shortcut_->setAutoRepeat(false);

	view_show_sampling_points_shortcut_ = new QShortcut(QKeySequence(Qt::Key_Period), this, SLOT(on_view_show_sampling_points_shortcut()));
	view_show_sampling_points_shortcut_->setAutoRepeat(false);

	view_show_analog_minor_grid_shortcut_ = new QShortcut(QKeySequence(Qt::Key_G), this, SLOT(on_view_show_analog_minor_grid_shortcut()));
	view_show_analog_minor_grid_shortcut_->setAutoRepeat(false);

	view_colored_bg_shortcut_ = new QShortcut(QKeySequence(Qt::Key_B), this, SLOT(on_view_colored_bg_shortcut()));
	view_colored_bg_shortcut_->setAutoRepeat(false);

	// Set up the tab area
	new_session_button_ = new QToolButton();
	new_session_button_->setObjectName(QString::fromUtf8("NewSessionButton"));
	new_session_button_->setIcon(QIcon(":/icons/document-new.svg"));
	new_session_button_->setToolTip(tr("Create New Session"));
	new_session_button_->setAutoRaise(true);
	new_session_button_->setIconSize(QSize(16, 16));

	// Run/stop lives in the main tool bar as a large button now; the
	// space bar shortcut still works application-wide
	run_stop_shortcut_ = new QShortcut(QKeySequence(Qt::Key_Space), this, SLOT(on_run_stop_clicked()));
	run_stop_shortcut_->setAutoRepeat(false);

	settings_button_ = new QToolButton();
	settings_button_->setObjectName(QString::fromUtf8("SettingsButton"));
	settings_button_->setIcon(QIcon(":/icons/preferences-system.svg"));
	settings_button_->setToolTip(tr("Settings"));
	settings_button_->setAutoRaise(true);
	settings_button_->setIconSize(QSize(16, 16));

	QFrame *separator2 = new QFrame();
	separator2->setFrameStyle(QFrame::VLine | QFrame::Plain);
	separator2->setObjectName(QString::fromUtf8("ControlSeparator"));

	QHBoxLayout* layout = new QHBoxLayout();
	layout->setContentsMargins(8, 0, 8, 0);
	layout->setSpacing(4);
	layout->addWidget(new_session_button_);
	layout->addWidget(separator2);
	layout->addWidget(settings_button_);

	static_tab_widget_ = new QWidget();
	static_tab_widget_->setObjectName(QString::fromUtf8("SessionControls"));
	static_tab_widget_->setLayout(layout);

	session_selector_.setObjectName(QString::fromUtf8("SessionSelector"));
	session_selector_.setCornerWidget(static_tab_widget_, Qt::TopRightCorner);
	session_selector_.setTabsClosable(true);
	session_selector_.setDocumentMode(true);
	session_selector_.setMovable(true);
	session_selector_.setElideMode(Qt::ElideRight);
	session_selector_.setUsesScrollButtons(true);

	QStatusBar *status_bar = statusBar();
	status_bar->setObjectName(QString::fromUtf8("AppStatusBar"));
	status_bar->setSizeGripEnabled(false);
	status_session_label_ = new QLabel(tr("No session"), status_bar);
	status_session_label_->setObjectName(QString::fromUtf8("StatusSession"));
	status_capture_label_ = new QLabel(tr("Ready"), status_bar);
	status_capture_label_->setObjectName(QString::fromUtf8("StatusCapture"));
	status_device_label_ = new QLabel(tr("No device"), status_bar);
	status_device_label_->setObjectName(QString::fromUtf8("StatusDevice"));

	QFrame *status_separator1 = new QFrame(status_bar);
	status_separator1->setFrameStyle(QFrame::VLine | QFrame::Plain);
	status_separator1->setObjectName(QString::fromUtf8("StatusSeparator"));
	QFrame *status_separator2 = new QFrame(status_bar);
	status_separator2->setFrameStyle(QFrame::VLine | QFrame::Plain);
	status_separator2->setObjectName(QString::fromUtf8("StatusSeparator"));

	status_bar->addWidget(status_session_label_);
	status_bar->addPermanentWidget(status_separator1);
	status_bar->addPermanentWidget(status_capture_label_);
	status_bar->addPermanentWidget(status_separator2);
	status_bar->addPermanentWidget(status_device_label_);

#if QT_VERSION >= QT_VERSION_CHECK(6, 0, 0)
	close_application_shortcut_ = new QShortcut(QKeySequence(Qt::CTRL | Qt::Key_Q), this, SLOT(close()));
	close_current_tab_shortcut_ = new QShortcut(QKeySequence(Qt::CTRL | Qt::Key_W), this, SLOT(on_close_current_tab()));
#else
	close_application_shortcut_ = new QShortcut(QKeySequence(Qt::CTRL + Qt::Key_Q), this, SLOT(close()));
	close_current_tab_shortcut_ = new QShortcut(QKeySequence(Qt::CTRL + Qt::Key_W), this, SLOT(on_close_current_tab()));
#endif
	close_application_shortcut_->setAutoRepeat(false);

	connect(new_session_button_, SIGNAL(clicked(bool)),
		this, SLOT(on_new_session_clicked()));
	connect(settings_button_, SIGNAL(clicked(bool)),
		this, SLOT(on_settings_clicked()));

	connect(&session_selector_, SIGNAL(tabCloseRequested(int)),
		this, SLOT(on_tab_close_requested(int)));
	connect(&session_selector_, SIGNAL(currentChanged(int)),
		this, SLOT(on_tab_changed(int)));


	connect(static_cast<QApplication *>(QCoreApplication::instance()),
		SIGNAL(focusChanged(QWidget*, QWidget*)),
		this, SLOT(on_focus_changed()));
}

void MainWindow::update_status_bar(Session *session)
{
	if (!status_session_label_ || !status_capture_label_ || !status_device_label_)
		return;

	if (!session) {
		status_session_label_->setText(tr("No session"));
		status_capture_label_->setText(tr("Ready"));
		status_device_label_->setText(tr("No device"));
		return;
	}

	status_session_label_->setText(session->name());

	switch (session->get_capture_state()) {
	case Session::Stopped:
		status_capture_label_->setText(tr("Ready"));
		break;
	case Session::AwaitingTrigger:
		status_capture_label_->setText(tr("Waiting for trigger"));
		break;
	case Session::Running:
		status_capture_label_->setText(tr("Acquiring"));
		break;
	}

	shared_ptr<devices::Device> device = session->device();
	status_device_label_->setText(device ?
		QString::fromStdString(device->display_name(device_manager_)) :
		tr("No device"));
}

void MainWindow::save_ui_settings()
{
	QSettings settings;

	settings.beginGroup("MainWindow");
	settings.setValue("state", saveState());
	settings.setValue("geometry", saveGeometry());
	settings.endGroup();
}

void MainWindow::restore_ui_settings()
{
	QSettings settings;

	settings.beginGroup("MainWindow");

	if (settings.contains("geometry")) {
		restoreGeometry(settings.value("geometry").toByteArray());
		restoreState(settings.value("state").toByteArray());
	} else
		resize(1440, 900);

	settings.endGroup();
}

shared_ptr<Session> MainWindow::get_tab_session(int index) const
{
	// Find the session that belongs to the tab's main window
	for (auto& entry : session_windows_)
		if (entry.second == session_selector_.widget(index))
			return entry.first;

	return nullptr;
}

void MainWindow::closeEvent(QCloseEvent *event)
{
	bool data_saved = true;

	for (auto& entry : session_windows_)
		if (!entry.first->data_saved())
			data_saved = false;

	if (!data_saved && (QMessageBox::question(this, tr("Confirmation"),
		tr("There is unsaved data. Close anyway?"),
		QMessageBox::Yes | QMessageBox::No) == QMessageBox::No)) {
		event->ignore();
	} else {
		save_ui_settings();
		save_sessions();
		event->accept();
	}
}

QMenu* MainWindow::createPopupMenu()
{
	return nullptr;
}

bool MainWindow::restoreState(const QByteArray &state, int version)
{
	(void)state;
	(void)version;

	// Do nothing. We don't want Qt to handle this, or else it
	// will try to restore all the dock widgets and create havoc.

	return false;
}

void MainWindow::on_run_stop_clicked()
{
	GlobalSettings settings;
	bool all_sessions = settings.value(GlobalSettings::Key_General_StartAllSessions).toBool();

	if (all_sessions)
	{
		vector< shared_ptr<Session> > hw_sessions;

		// Make a list of all sessions where a hardware device is used
		for (const shared_ptr<Session>& s : sessions_) {
			shared_ptr<devices::HardwareDevice> hw_device =
					dynamic_pointer_cast< devices::HardwareDevice >(s->device());
			if (!hw_device)
				continue;
			hw_sessions.push_back(s);
		}

		// Stop all acquisitions if there are any running ones, start all otherwise
		bool any_running = any_of(hw_sessions.begin(), hw_sessions.end(),
				[](const shared_ptr<Session> &s)
				{ return (s->get_capture_state() == Session::AwaitingTrigger) ||
						(s->get_capture_state() == Session::Running); });

		for (shared_ptr<Session> s : hw_sessions)
			if (any_running)
				s->stop_capture();
			else
				s->start_capture([&](QString message) {Q_EMIT session_error_raised("Capture failed", message);});
	} else {

		shared_ptr<Session> session = last_focused_session_;

		if (!session)
			return;

		switch (session->get_capture_state()) {
		case Session::Stopped:
			session->start_capture([&](QString message) {Q_EMIT session_error_raised("Capture failed", message);});
			break;
		case Session::AwaitingTrigger:
		case Session::Running:
			session->stop_capture();
			break;
		}
	}
}

void MainWindow::on_add_view(views::ViewType type, Session *session)
{
	// We get a pointer and need a reference
	for (shared_ptr<Session>& s : sessions_)
		if (s.get() == session)
			add_view(type, *s);
}

void MainWindow::on_focus_changed()
{
	shared_ptr<views::ViewBase> view = get_active_view();

	if (view) {
		for (shared_ptr<Session> session : sessions_) {
			if (session->has_view(view)) {
				if (session != last_focused_session_) {
					// Activate correct tab if necessary
					shared_ptr<Session> tab_session = get_tab_session(
						session_selector_.currentIndex());
					if (tab_session != session)
						session_selector_.setCurrentWidget(
							session_windows_.at(session));

					on_focused_session_changed(session);
				}

				break;
			}
		}
	}

	if (sessions_.empty())
		setWindowTitle(WindowTitle);
}

void MainWindow::on_focused_session_changed(shared_ptr<Session> session)
{
	last_focused_session_ = session;

	setWindowTitle(session->name() + " - " + WindowTitle);

	// Update the state of the run/stop button, too
	update_status_bar(session.get());
}

void MainWindow::on_new_session_clicked()
{
	add_session();
}

void MainWindow::on_settings_clicked()
{
	dialogs::Settings dlg(device_manager_);
	dlg.exec();
}

void MainWindow::on_session_name_changed()
{
	// Update the corresponding dock widget's name(s)
	Session *session = qobject_cast<Session*>(QObject::sender());
	assert(session);

	for (const shared_ptr<views::ViewBase>& view : session->views()) {
		// Get the dock that contains the view
		for (auto& entry : view_docks_)
			if (entry.second == view) {
				entry.first->setObjectName(session->name());
				entry.first->setWindowTitle(session->name());
			}
	}

	// Update the tab widget by finding the main window and the tab from that
	for (auto& entry : session_windows_)
		if (entry.first.get() == session) {
			QMainWindow *window = entry.second;
			const int index = session_selector_.indexOf(window);
			session_selector_.setTabText(index, session->name());
		}

	// Refresh window title if the affected session has focus
	if (session == last_focused_session_.get()) {
		setWindowTitle(session->name() + " - " + WindowTitle);
		update_status_bar(session);
	}
}

void MainWindow::on_session_device_changed()
{
	Session *session = qobject_cast<Session*>(QObject::sender());
	assert(session);

	// Ignore if caller is not the currently focused session
	// unless there is only one session
	if ((sessions_.size() > 1) && (session != last_focused_session_.get()))
		return;

	update_status_bar(session);
}

void MainWindow::on_session_capture_state_changed(int state)
{
	(void)state;

	Session *session = qobject_cast<Session*>(QObject::sender());
	assert(session);

	// Ignore if caller is not the currently focused session
	// unless there is only one session
	if ((sessions_.size() > 1) && (session != last_focused_session_.get()))
		return;

	update_status_bar(session);
}

void MainWindow::on_new_view(Session *session, int view_type)
{
	// We get a pointer and need a reference
	for (shared_ptr<Session>& s : sessions_)
		if (s.get() == session)
			add_view((views::ViewType)view_type, *s);
}

void MainWindow::on_view_close_clicked()
{
	// Find the dock widget that contains the close button that was clicked
	QObject *w = QObject::sender();
	QDockWidget *dock = nullptr;

	while (w) {
	    dock = qobject_cast<QDockWidget*>(w);
	    if (dock)
	        break;
	    w = w->parent();
	}

	// Get the view contained in the dock widget
	shared_ptr<views::ViewBase> view;

	for (auto& entry : view_docks_)
		if (entry.first == dock)
			view = entry.second;

	// Deregister the view
	for (shared_ptr<Session> session : sessions_) {
		if (!session->has_view(view))
			continue;

		// Also destroy the entire session if its main view is closing...
		if (view == session->main_view()) {
			// ...but only if data is saved or the user confirms closing
			if (session->data_saved() || (QMessageBox::question(this, tr("Confirmation"),
				tr("This session contains unsaved data. Close it anyway?"),
				QMessageBox::Yes | QMessageBox::No) == QMessageBox::Yes))
				remove_session(session);
			break;
		} else
			// All other views can be closed at any time as no data will be lost
			remove_view(view);
	}
}

void MainWindow::on_tab_changed(int index)
{
	shared_ptr<Session> session = get_tab_session(index);

	if (session)
		on_focused_session_changed(session);
}

void MainWindow::on_tab_close_requested(int index)
{
	shared_ptr<Session> session = get_tab_session(index);

	if (!session)
		return;

	if (session->data_saved() || (QMessageBox::question(this, tr("Confirmation"),
		tr("This session contains unsaved data. Close it anyway?"),
		QMessageBox::Yes | QMessageBox::No) == QMessageBox::Yes))
		remove_session(session);

	if (sessions_.empty())
		update_status_bar(nullptr);
}

void MainWindow::on_show_decoder_selector(Session *session)
{
#ifdef ENABLE_DECODE
	// Close dock widget if it's already showing and return
	for (auto& entry : sub_windows_) {
		QDockWidget* dock = entry.first;
		shared_ptr<subwindows::SubWindowBase> decoder_selector =
			dynamic_pointer_cast<subwindows::decoder_selector::SubWindow>(entry.second);

		if (decoder_selector && (&decoder_selector->session() == session)) {
			sub_windows_.erase(dock);
			dock->close();
			return;
		}
	}

	// We get a pointer and need a reference
	for (shared_ptr<Session>& s : sessions_)
		if (s.get() == session)
			add_subwindow(subwindows::SubWindowTypeDecoderSelector, *s);
#else
	(void)session;
#endif
}

void MainWindow::on_decode_table_requested(Session *session)
{
#ifdef ENABLE_DECODE
	// We get a pointer and need a reference
	for (shared_ptr<Session>& s : sessions_)
		if (s.get() == session)
			add_view(views::ViewTypeTabularDecoder, *s);
#else
	(void)session;
#endif
}

void MainWindow::on_sub_window_close_clicked()
{
	// Find the dock widget that contains the close button that was clicked
	QObject *w = QObject::sender();
	QDockWidget *dock = nullptr;

	while (w) {
	    dock = qobject_cast<QDockWidget*>(w);
	    if (dock)
	        break;
	    w = w->parent();
	}

	sub_windows_.erase(dock);
	dock->close();

	// Restore focus to the last used main view
	if (last_focused_session_)
		last_focused_session_->main_view()->setFocus();
}

void MainWindow::on_view_colored_bg_shortcut()
{
	GlobalSettings settings;

	bool state = settings.value(GlobalSettings::Key_View_ColoredBG).toBool();
	settings.setValue(GlobalSettings::Key_View_ColoredBG, !state);
}

void MainWindow::on_view_sticky_scrolling_shortcut()
{
	GlobalSettings settings;

	bool state = settings.value(GlobalSettings::Key_View_StickyScrolling).toBool();
	settings.setValue(GlobalSettings::Key_View_StickyScrolling, !state);
}

void MainWindow::on_view_show_sampling_points_shortcut()
{
	GlobalSettings settings;

	bool state = settings.value(GlobalSettings::Key_View_ShowSamplingPoints).toBool();
	settings.setValue(GlobalSettings::Key_View_ShowSamplingPoints, !state);
}

void MainWindow::on_view_show_analog_minor_grid_shortcut()
{
	GlobalSettings settings;

	bool state = settings.value(GlobalSettings::Key_View_ShowAnalogMinorGrid).toBool();
	settings.setValue(GlobalSettings::Key_View_ShowAnalogMinorGrid, !state);
}

void MainWindow::on_close_current_tab()
{
	int tab = session_selector_.currentIndex();

	on_tab_close_requested(tab);
}

void MainWindow::on_session_error_raised(const QString text, const QString info_text) {
	MainWindow::show_session_error(text, info_text);
}

} // namespace pv
