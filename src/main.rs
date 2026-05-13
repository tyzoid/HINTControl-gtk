mod app;
mod backend;
mod logging;
mod models;

use app::{AppCommand, AppHandle, CommandKind, DataPayload, DataSourceKind, StartedApp, UiEvent};
use backend::{ClientRow, Row, Settings, SignalMetrics, Snapshot};
use gtk::cairo;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CheckButton, DrawingArea, Entry,
    FileChooserAction, FileChooserNative, Grid, Label, ListBox, ListBoxRow, Notebook, Orientation,
    PasswordEntry, PopoverMenu, ResponseType, ScrolledWindow, Stack, ToggleButton, Window,
};
use logging::{parse_level, set_level, LogLevel};
use models::{SsidConfig, WifiConfig};
use qrcode::QrCode;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::f64;
use std::fs;
use std::rc::Rc;
use std::time::{Duration, Instant};

const APP_ID: &str = "dev.zwander.hintcontrol.gtk";
const HISTORY_SECONDS: f64 = 120.0;
const MAX_SAMPLES: usize = 32;
const SIGNAL_AXIS_MIN: f64 = -120.0;
const SIGNAL_AXIS_MAX: f64 = 20.0;
const PAGE_OVERVIEW: usize = 0;
const PAGE_SIGNAL: usize = 1;
const PAGE_WIFI: usize = 2;
const PAGE_CLIENTS: usize = 3;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClientSort {
    Type,
    Hostname,
    Ip,
    Mac,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Clone, Copy)]
struct Sample {
    at: Instant,
    value: f64,
}

#[derive(Default)]
struct MetricRing {
    values: VecDeque<Sample>,
}

impl MetricRing {
    fn push(&mut self, at: Instant, value: Option<i64>) {
        self.values.push_back(Sample {
            at,
            value: value.map(|value| value as f64).unwrap_or(f64::NAN),
        });
        while self.values.len() > MAX_SAMPLES {
            self.values.pop_front();
        }
    }

    fn iter_visible(&self, now: Instant) -> impl Iterator<Item = Sample> + '_ {
        self.values
            .iter()
            .copied()
            .filter(move |sample| now.duration_since(sample.at).as_secs_f64() <= HISTORY_SECONDS)
    }

    fn latest(&self, now: Instant) -> Option<f64> {
        self.iter_visible(now)
            .filter(|sample| sample.value.is_finite())
            .last()
            .map(|sample| sample.value)
    }

    fn export_values(&self, now: Instant) -> Vec<(f64, Option<f64>)> {
        self.iter_visible(now)
            .map(|sample| {
                let offset = -(now.duration_since(sample.at).as_secs_f64());
                let value = sample.value.is_finite().then_some(sample.value);
                (offset, value)
            })
            .collect()
    }
}

#[derive(Default)]
struct SignalHistory {
    rsrp: MetricRing,
    rsrq: MetricRing,
    rssi: MetricRing,
    sinr: MetricRing,
    cqi: MetricRing,
}

impl SignalHistory {
    fn push(&mut self, at: Instant, metrics: Option<&SignalMetrics>) {
        self.rsrp.push(at, metrics.and_then(|m| m.rsrp));
        self.rsrq.push(at, metrics.and_then(|m| m.rsrq));
        self.rssi.push(at, metrics.and_then(|m| m.rssi));
        self.sinr.push(at, metrics.and_then(|m| m.sinr));
        self.cqi.push(at, metrics.and_then(|m| m.cqi));
    }
}

#[derive(Clone)]
struct SignalGraph {
    area: DrawingArea,
    warning: Label,
    latest_values: GtkBox,
    rsrp: ToggleButton,
    rsrq: ToggleButton,
    rssi: ToggleButton,
    sinr: ToggleButton,
    cqi: ToggleButton,
    history: Rc<RefCell<SignalHistory>>,
}

struct DetailRowWidgets {
    row: ListBoxRow,
    value: Label,
    copy_value: Rc<RefCell<String>>,
}

struct ClientRowWidgets {
    row: ListBoxRow,
    band: Label,
    name: Label,
    ip: Label,
    mac: Label,
    copy_ip: Rc<RefCell<String>>,
    copy_mac: Rc<RefCell<String>>,
    copy_hostname: Rc<RefCell<String>>,
}

impl SignalGraph {
    fn new(title: &str, disconnected_text: &str) -> (GtkBox, Self) {
        let root = GtkBox::new(Orientation::Vertical, 6);
        root.append(&section_heading(title));

        let area = DrawingArea::builder()
            .height_request(190)
            .hexpand(true)
            .vexpand(true)
            .build();
        let warning = Label::builder()
            .label(disconnected_text)
            .halign(Align::Center)
            .valign(Align::Center)
            .height_request(80)
            .build();
        warning.add_css_class("dim-label");
        warning.add_css_class("empty-state");

        let latest_values = GtkBox::new(Orientation::Horizontal, 8);
        let rsrp = metric_toggle("RSRP", "red");
        let rsrq = metric_toggle("RSRQ", "green");
        let rssi = metric_toggle("RSSI", "blue");
        let sinr = metric_toggle("SINR", "orange");
        let cqi = metric_toggle("CQI", "purple");
        for toggle in [&rsrp, &rsrq, &rssi, &sinr, &cqi] {
            latest_values.append(toggle);
        }

        root.append(&latest_values);
        root.append(&area);
        root.append(&warning);

        let graph = Self {
            area,
            warning,
            latest_values,
            rsrp,
            rsrq,
            rssi,
            sinr,
            cqi,
            history: Rc::new(RefCell::new(SignalHistory::default())),
        };
        graph.install_draw_func();
        graph.install_toggle_redraws();
        (root, graph)
    }

    fn install_toggle_redraws(&self) {
        for toggle in [&self.rsrp, &self.rsrq, &self.rssi, &self.sinr, &self.cqi] {
            let area = self.area.clone();
            toggle.connect_toggled(move |toggle| {
                if toggle.is_active() {
                    toggle.remove_css_class("muted-metric");
                } else {
                    toggle.add_css_class("muted-metric");
                }
                area.queue_draw();
            });
        }
    }

    fn install_draw_func(&self) {
        let history = self.history.clone();
        let rsrp = self.rsrp.clone();
        let rsrq = self.rsrq.clone();
        let rssi = self.rssi.clone();
        let sinr = self.sinr.clone();
        let cqi = self.cqi.clone();

        self.area.set_draw_func(move |_, ctx, width, height| {
            draw_signal_graph(
                ctx,
                width as f64,
                height as f64,
                &history.borrow(),
                rsrp.is_active(),
                rsrq.is_active(),
                rssi.is_active(),
                sinr.is_active(),
                cqi.is_active(),
            );
        });
    }

    fn push(&self, at: Instant, metrics: Option<&SignalMetrics>) {
        self.history.borrow_mut().push(at, metrics);
        let connected = metrics.is_some();
        fill_latest_signal_values(self);
        self.latest_values.set_visible(connected);
        self.area.set_visible(connected);
        self.warning.set_visible(!connected);
        self.rsrp.set_visible(connected);
        self.rsrq.set_visible(connected);
        self.rssi.set_visible(connected);
        self.sinr.set_visible(connected);
        self.cqi.set_visible(connected);
        self.area.queue_draw();
    }
}

struct UiRuntimeState {
    settings: Settings,
    snapshot: Snapshot,
    command_tx: AppHandle,
}

#[derive(Clone)]
struct Ui {
    state: Rc<RefCell<UiRuntimeState>>,
    events: Rc<RefCell<std::sync::mpsc::Receiver<UiEvent>>>,
    stack: Stack,
    footer: GtkBox,
    status: Label,
    status_reset_source: Rc<RefCell<Option<glib::SourceId>>>,
    status_reset_token: Rc<Cell<u64>>,
    current_page: Rc<Cell<usize>>,
    gateway_ip: Entry,
    username: Entry,
    password: PasswordEntry,
    remember: CheckButton,
    overview_list: GtkBox,
    notebook: Notebook,
    five_g_graph: SignalGraph,
    lte_graph: SignalGraph,
    device_list: ListBox,
    general_list: ListBox,
    sim_list: ListBox,
    device_rows: Rc<RefCell<HashMap<String, DetailRowWidgets>>>,
    general_rows: Rc<RefCell<HashMap<String, DetailRowWidgets>>>,
    sim_rows: Rc<RefCell<HashMap<String, DetailRowWidgets>>>,
    clients_search: Entry,
    clients_list: ListBox,
    client_sort_type: Button,
    client_sort_hostname: Button,
    client_sort_ip: Button,
    client_sort_mac: Button,
    client_rows: Rc<RefCell<Vec<ClientRow>>>,
    client_widgets: Rc<RefCell<HashMap<String, ClientRowWidgets>>>,
    client_sort: Rc<Cell<ClientSort>>,
    client_sort_direction: Rc<Cell<SortDirection>>,
    two_radio: CheckButton,
    five_radio: CheckButton,
    six_radio: CheckButton,
    ssid_list: ListBox,
    ssid_editors: Rc<RefCell<Vec<SsidEditor>>>,
    add_ssid: Button,
    save_wifi: Button,
    discard_wifi: Button,
    wifi_tab_label: Label,
    wifi_edit_baseline: Rc<RefCell<Option<WifiConfig>>>,
    wifi_draft: Rc<RefCell<Option<WifiConfig>>>,
    wifi_dirty: Rc<Cell<bool>>,
    suppress_wifi_dirty: Rc<Cell<bool>>,
    export_diagnostics: Button,
}

impl Ui {
    fn set_status(&self, message: &str, reset: bool) {
        let token = self.status_reset_token.get() + 1;
        self.status_reset_token.set(token);
        if let Some(source) = self.status_reset_source.borrow_mut().take() {
            source.remove();
        }
        self.status.set_text(message);
        if reset {
            let status = self.status.clone();
            let status_reset_source = self.status_reset_source.clone();
            let status_reset_token = self.status_reset_token.clone();
            let source = glib::timeout_add_local_once(Duration::from_secs(3), move || {
                if status_reset_token.get() == token {
                    status.set_text("Logged in");
                    status_reset_source.borrow_mut().take();
                }
            });
            *self.status_reset_source.borrow_mut() = Some(source);
        }
    }

    fn refresh(&self, show_status: bool) {
        if show_status {
            self.set_status("Refreshing...", false);
        }
        self.send_command(AppCommand::RefreshAll);
    }

    fn send_command(&self, command: AppCommand) {
        self.state.borrow().command_tx.send(command);
    }

    fn apply_state(&self, record_signal_sample: bool) {
        let state = self.state.borrow();
        let snapshot = &state.snapshot;

        self.gateway_ip.set_text(&snapshot.gateway_ip);
        self.username.set_text(&snapshot.username);
        if !snapshot.logged_in {
            self.password
                .set_text(state.settings.password.as_deref().unwrap_or_default());
        }
        self.stack.set_visible_child_name(if snapshot.logged_in {
            "details"
        } else {
            "login"
        });
        self.footer.set_visible(snapshot.logged_in);

        fill_overview(&self.overview_list, snapshot);
        reconcile_rows(
            &self.device_list,
            &self.device_rows,
            &filter_rows(
                &snapshot.device_summary,
                &["Manufacturer", "Model", "Serial", "Hardware", "Software"],
            ),
        );
        reconcile_rows(
            &self.general_list,
            &self.general_rows,
            &filter_rows(
                &snapshot.general_summary,
                &["APN", "Registration", "IPv6", "Uptime"],
            ),
        );
        reconcile_rows(
            &self.sim_list,
            &self.sim_rows,
            &filter_rows(&snapshot.sim_summary, &["ICCID", "IMEI", "IMSI", "MSISDN"]),
        );
        *self.client_rows.borrow_mut() = snapshot.clients.clone();
        refill_clients(self);
        if !self.wifi_dirty.get() && !self.wifi_editing() {
            apply_wifi(self, snapshot);
        } else {
            self.refresh_wifi_dirty_state();
            update_wifi_dirty_controls(
                &self.save_wifi,
                &self.discard_wifi,
                &self.wifi_tab_label,
                self.wifi_dirty.get(),
            );
        }

        if snapshot.logged_in && record_signal_sample {
            let now = Instant::now();
            self.five_g_graph
                .push(now, snapshot.five_g_metrics.as_ref());
            self.lte_graph.push(now, snapshot.lte_metrics.as_ref());
        }
    }

    fn tick_signal_page(&self) {
        if self.current_page.get() != PAGE_SIGNAL {
            return;
        }

        self.five_g_graph.area.queue_draw();
        self.lte_graph.area.queue_draw();
    }

    fn wifi_editing(&self) -> bool {
        self.ssid_editors
            .borrow()
            .iter()
            .any(|editor| editor.editing.get())
    }

    fn refresh_wifi_dirty_state(&self) {
        if self.suppress_wifi_dirty.get() {
            return;
        }
        let draft = match build_wifi_config_from_ui(self) {
            Ok(draft) => draft,
            Err(_) => {
                set_wifi_dirty(self, true);
                return;
            }
        };
        let baseline = self
            .wifi_edit_baseline
            .borrow()
            .clone()
            .or_else(|| self.state.borrow().snapshot.wifi.clone());
        let dirty = baseline.as_ref() != Some(&draft);
        *self.wifi_draft.borrow_mut() = dirty.then_some(draft);
        set_wifi_dirty(self, dirty);
    }

    fn ensure_wifi_edit_baseline(&self) {
        if self.wifi_edit_baseline.borrow().is_some() {
            return;
        }
        let baseline = self
            .state
            .borrow()
            .snapshot
            .wifi
            .clone()
            .or_else(|| build_wifi_config_from_ui(self).ok());
        *self.wifi_edit_baseline.borrow_mut() = baseline;
    }

    fn drain_events(&self) {
        loop {
            let event = self.events.borrow_mut().try_recv();
            match event {
                Ok(event) => self.handle_event(event),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    }

    fn handle_event(&self, event: UiEvent) {
        match event {
            UiEvent::LoginSucceeded { initial } => {
                let warning = initial.warning.clone();
                {
                    let mut state = self.state.borrow_mut();
                    state.settings = initial.settings.clone();
                    state.snapshot = initial.snapshot.clone();
                }
                if let Some(warning) = warning {
                    logging::error(&warning);
                    self.set_status(&warning, false);
                } else {
                    self.set_status("Logged in", false);
                }
                self.apply_page_polling();
                self.apply_state(true);
            }
            UiEvent::LoggedOut => {
                self.state.borrow_mut().snapshot.logged_in = false;
                self.set_status("Logged out", false);
                self.apply_state(false);
            }
            UiEvent::DataUpdated {
                source, payload, ..
            } => {
                self.apply_payload(source, payload);
                self.set_status("Logged in", false);
                self.apply_state(source == DataSourceKind::Signal);
            }
            UiEvent::DataError { source, error, .. } => {
                logging::error(format!("{source:?} refresh failed: {error}"));
                self.state.borrow_mut().snapshot.error = Some(error.clone());
                self.set_status(&format!("{source:?} refresh failed: {error}"), false);
            }
            UiEvent::CommandSucceeded { command, message } => {
                self.set_status(&message, true);
                if command == CommandKind::SaveWifi {
                    *self.wifi_edit_baseline.borrow_mut() = None;
                    *self.wifi_draft.borrow_mut() = None;
                    set_wifi_dirty(self, false);
                    self.apply_state(false);
                }
            }
            UiEvent::CommandFailed { command, error } => {
                logging::error(format!("{command:?} failed: {error}"));
                self.set_status(&format!("{command:?} failed: {error}"), false);
            }
            UiEvent::AuthReauthStarted => {
                self.set_status("Gateway authentication expired; reauthenticating...", false);
            }
            UiEvent::AuthExpired { message } => {
                let mut state = self.state.borrow_mut();
                state.snapshot.error = Some(message.clone());
                state.snapshot.lte_metrics = None;
                state.snapshot.five_g_metrics = None;
                drop(state);
                self.set_status(&message, false);
                self.apply_state(false);
            }
        }
    }

    fn apply_payload(&self, source: DataSourceKind, payload: DataPayload) {
        let mut state = self.state.borrow_mut();
        match (source, payload) {
            (DataSourceKind::Gateway, DataPayload::Gateway(payload)) => {
                state.snapshot.device_summary = payload.device_summary;
                state.snapshot.general_summary = payload.general_summary;
            }
            (DataSourceKind::Signal, DataPayload::Signal(payload)) => {
                state.snapshot.lte_summary = payload.lte_summary;
                state.snapshot.five_g_summary = payload.five_g_summary;
                state.snapshot.lte_metrics = payload.lte_metrics;
                state.snapshot.five_g_metrics = payload.five_g_metrics;
            }
            (DataSourceKind::Sim, DataPayload::Sim(rows)) => {
                state.snapshot.sim_summary = rows;
            }
            (DataSourceKind::Clients, DataPayload::Clients(rows)) => {
                state.snapshot.clients = rows;
            }
            (DataSourceKind::Wifi, DataPayload::Wifi(wifi)) => {
                state.snapshot.wifi = Some(*wifi);
            }
            _ => {}
        }
        state.snapshot.error = None;
    }

    fn apply_page_polling(&self) {
        let clients = if self.current_page.get() == PAGE_CLIENTS {
            Duration::from_secs(30)
        } else {
            Duration::from_secs(5 * 60)
        };
        let wifi = if self.current_page.get() == PAGE_WIFI {
            Duration::from_secs(60)
        } else {
            Duration::from_secs(10 * 60)
        };
        self.send_command(AppCommand::SetPollingRate {
            source: DataSourceKind::Signal,
            interval: Some(Duration::from_secs(5)),
        });
        self.send_command(AppCommand::SetPollingRate {
            source: DataSourceKind::Gateway,
            interval: Some(Duration::from_secs(5 * 60)),
        });
        self.send_command(AppCommand::SetPollingRate {
            source: DataSourceKind::Sim,
            interval: None,
        });
        self.send_command(AppCommand::SetPollingRate {
            source: DataSourceKind::Clients,
            interval: Some(clients),
        });
        self.send_command(AppCommand::SetPollingRate {
            source: DataSourceKind::Wifi,
            interval: Some(wifi),
        });
    }
}

fn main() {
    let (verbosity, gtk_args) = parse_args(std::env::args().collect());
    set_level(verbosity);
    let runtime = Rc::new(tokio::runtime::Runtime::new().expect("failed to start Tokio runtime"));
    let started = Rc::new(RefCell::new(Some(app::start(&runtime))));

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(move |app| {
        let started = started
            .borrow_mut()
            .take()
            .expect("application activated more than once");
        build_ui(app, started);
    });
    let _ = app.run_with_args(&gtk_args);
    runtime.block_on(async {});
}

fn parse_args(args: Vec<String>) -> (LogLevel, Vec<String>) {
    let mut level = LogLevel::Info;
    let mut filtered = Vec::with_capacity(args.len());
    let mut iter = args.into_iter();

    if let Some(program) = iter.next() {
        filtered.push(program);
    }

    while let Some(arg) = iter.next() {
        if arg == "-v" || arg == "--verbose" {
            match iter.next() {
                Some(next) if !next.starts_with('-') => {
                    if let Some(parsed) = parse_level(&next) {
                        level = parsed;
                    } else {
                        logging::error(format!(
                            "invalid verbose level '{next}'. Use error, info, debug, or trace."
                        ));
                        std::process::exit(2);
                    }
                }
                Some(next) => {
                    filtered.push(next);
                    level = LogLevel::Debug;
                }
                None => {
                    level = LogLevel::Debug;
                }
            }
            continue;
        }

        if let Some(value) = arg.strip_prefix("-v=") {
            if let Some(parsed) = parse_level(value) {
                level = parsed;
            } else {
                logging::error(format!(
                    "invalid verbose level '{value}'. Use error, info, debug, or trace."
                ));
                std::process::exit(2);
            }
            continue;
        }

        if let Some(value) = arg.strip_prefix("--verbose=") {
            if let Some(parsed) = parse_level(value) {
                level = parsed;
            } else {
                logging::error(format!(
                    "invalid verbose level '{value}'. Use error, info, debug, or trace."
                ));
                std::process::exit(2);
            }
            continue;
        }

        filtered.push(arg);
    }

    (level, filtered)
}

fn build_ui(app: &Application, started: StartedApp) {
    install_css();

    let state = Rc::new(RefCell::new(UiRuntimeState {
        settings: started.settings,
        snapshot: started.snapshot,
        command_tx: started.handle,
    }));
    let window = ApplicationWindow::builder()
        .application(app)
        .title("HINT Control")
        .default_width(700)
        .default_height(800)
        .decorated(true)
        .build();
    window.set_titlebar(Option::<&gtk::Widget>::None);

    let root = GtkBox::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(8);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let stack = Stack::new();
    stack.set_vexpand(true);
    root.append(&stack);

    let footer = GtkBox::new(Orientation::Horizontal, 8);
    let status = Label::builder()
        .xalign(0.0)
        .label("Logged in")
        .hexpand(true)
        .build();
    status.add_css_class("dim-label");
    footer.append(&status);
    root.append(&footer);

    let login = build_login_page();
    stack.add_named(&login.root, Some("login"));

    let details = build_details_page();
    stack.add_named(&details.root, Some("details"));

    window.set_child(Some(&root));

    let ui = Ui {
        state,
        events: Rc::new(RefCell::new(started.events)),
        stack,
        footer,
        status,
        status_reset_source: Rc::new(RefCell::new(None)),
        status_reset_token: Rc::new(Cell::new(0)),
        current_page: Rc::new(Cell::new(PAGE_OVERVIEW)),
        gateway_ip: login.gateway_ip,
        username: login.username,
        password: login.password,
        remember: login.remember,
        overview_list: details.overview_list,
        notebook: details.notebook,
        five_g_graph: details.five_g_graph,
        lte_graph: details.lte_graph,
        device_list: details.device_list,
        general_list: details.general_list,
        sim_list: details.sim_list,
        device_rows: Rc::new(RefCell::new(HashMap::new())),
        general_rows: Rc::new(RefCell::new(HashMap::new())),
        sim_rows: Rc::new(RefCell::new(HashMap::new())),
        clients_search: details.clients_search,
        clients_list: details.clients_list,
        client_sort_type: details.client_sort_type,
        client_sort_hostname: details.client_sort_hostname,
        client_sort_ip: details.client_sort_ip,
        client_sort_mac: details.client_sort_mac,
        client_rows: Rc::new(RefCell::new(Vec::new())),
        client_widgets: Rc::new(RefCell::new(HashMap::new())),
        client_sort: Rc::new(Cell::new(ClientSort::Type)),
        client_sort_direction: Rc::new(Cell::new(SortDirection::Ascending)),
        two_radio: details.two_radio,
        five_radio: details.five_radio,
        six_radio: details.six_radio,
        ssid_list: details.ssid_list,
        ssid_editors: details.ssid_editors,
        add_ssid: details.add_ssid,
        save_wifi: details.save_wifi.clone(),
        discard_wifi: details.discard_wifi.clone(),
        wifi_tab_label: details.wifi_tab_label,
        wifi_edit_baseline: Rc::new(RefCell::new(None)),
        wifi_draft: Rc::new(RefCell::new(None)),
        wifi_dirty: Rc::new(Cell::new(false)),
        suppress_wifi_dirty: Rc::new(Cell::new(false)),
        export_diagnostics: details.export_diagnostics,
    };

    ui.footer.append(&details.refresh);
    ui.footer.append(&details.reboot);
    ui.footer.append(&details.logout);

    wire_actions(
        &ui,
        login.login_button,
        details.refresh,
        ui.save_wifi.clone(),
        ui.discard_wifi.clone(),
        details.reboot,
        details.logout,
    );
    wire_wifi_dirty_tracking(&ui);
    wire_client_controls(&ui);
    let ui_for_page_change = ui.clone();
    ui.notebook.connect_switch_page(move |_, _, page_num| {
        ui_for_page_change.current_page.set(page_num as usize);
        ui_for_page_change.apply_page_polling();
        if ui_for_page_change.state.borrow().snapshot.logged_in
            && ui_for_page_change
                .status
                .text()
                .to_ascii_lowercase()
                .contains("failed")
        {
            ui_for_page_change.set_status("Logged in", false);
        }
    });
    ui.apply_state(false);
    apply_screenshot_automation(&ui);

    let ui_for_timer = ui.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        ui_for_timer.drain_events();
        glib::ControlFlow::Continue
    });

    let ui_for_signal_timer = ui.clone();
    glib::timeout_add_local(Duration::from_millis(250), move || {
        if ui_for_signal_timer.state.borrow().snapshot.logged_in {
            ui_for_signal_timer.tick_signal_page();
        }
        glib::ControlFlow::Continue
    });

    window.present();
}

fn apply_screenshot_automation(ui: &Ui) {
    if std::env::var_os("HINTCONTROL_SCREENSHOT_AUTOMATION").is_none() {
        return;
    }

    if std::env::var_os("HINTCONTROL_SCREENSHOT_AUTO_LOGIN").is_some() {
        let settings = ui.state.borrow().settings.clone();
        if let Some(password) = settings.password {
            ui.set_status("Logging in...", false);
            ui.send_command(AppCommand::Login {
                gateway_ip: settings.gateway_ip,
                username: settings.username,
                password,
                remember: true,
            });
        }
    }

    if let Ok(tab) = std::env::var("HINTCONTROL_SCREENSHOT_TAB") {
        let page = match tab.as_str() {
            "overview" => Some(0),
            "signal" => Some(1),
            "wifi" | "wi-fi" => Some(2),
            "clients" => Some(3),
            "device" => Some(4),
            _ => None,
        };
        if let Some(page) = page {
            ui.notebook.set_current_page(Some(page));
        }
    }
}

struct LoginPage {
    root: GtkBox,
    gateway_ip: Entry,
    username: Entry,
    password: PasswordEntry,
    remember: CheckButton,
    login_button: Button,
}

fn build_login_page() -> LoginPage {
    let root = GtkBox::new(Orientation::Vertical, 0);
    root.set_valign(Align::Center);
    root.set_halign(Align::Center);

    let panel = Grid::builder()
        .row_spacing(10)
        .column_spacing(10)
        .width_request(420)
        .build();
    panel.add_css_class("login-panel");

    let title = Label::builder()
        .label("Gateway Manager")
        .xalign(0.0)
        .build();
    title.add_css_class("title");
    let subtitle = Label::builder()
        .label("Connect to your gateway")
        .xalign(0.0)
        .build();
    subtitle.add_css_class("dim-label");

    let gateway_ip = Entry::builder().text("192.168.12.1").hexpand(true).build();
    let username = Entry::builder().text("admin").hexpand(true).build();
    let password = PasswordEntry::builder().hexpand(true).build();
    let remember = CheckButton::builder()
        .label("Remember credentials")
        .active(true)
        .build();
    let login_button = Button::with_label("Log In");
    login_button.add_css_class("suggested-action");
    login_button.set_hexpand(true);

    panel.attach(&title, 0, 0, 2, 1);
    panel.attach(&subtitle, 0, 1, 2, 1);
    panel.attach(
        &Label::builder()
            .label("Gateway address or IP")
            .xalign(0.0)
            .build(),
        0,
        2,
        1,
        1,
    );
    panel.attach(&gateway_ip, 1, 2, 1, 1);
    panel.attach(
        &Label::builder().label("Username").xalign(0.0).build(),
        0,
        3,
        1,
        1,
    );
    panel.attach(&username, 1, 3, 1, 1);
    panel.attach(
        &Label::builder().label("Password").xalign(0.0).build(),
        0,
        4,
        1,
        1,
    );
    panel.attach(&password, 1, 4, 1, 1);
    panel.attach(&remember, 1, 5, 1, 1);
    panel.attach(&login_button, 1, 6, 1, 1);

    root.append(&panel);
    LoginPage {
        root,
        gateway_ip,
        username,
        password,
        remember,
        login_button,
    }
}

struct DetailsPage {
    root: GtkBox,
    overview_list: GtkBox,
    notebook: Notebook,
    five_g_graph: SignalGraph,
    lte_graph: SignalGraph,
    device_list: ListBox,
    general_list: ListBox,
    sim_list: ListBox,
    clients_search: Entry,
    clients_list: ListBox,
    client_sort_type: Button,
    client_sort_hostname: Button,
    client_sort_ip: Button,
    client_sort_mac: Button,
    two_radio: CheckButton,
    five_radio: CheckButton,
    six_radio: CheckButton,
    ssid_list: ListBox,
    ssid_editors: Rc<RefCell<Vec<SsidEditor>>>,
    add_ssid: Button,
    refresh: Button,
    save_wifi: Button,
    discard_wifi: Button,
    wifi_tab_label: Label,
    reboot: Button,
    logout: Button,
    export_diagnostics: Button,
}

fn build_details_page() -> DetailsPage {
    let root = GtkBox::new(Orientation::Vertical, 8);
    let notebook = Notebook::new();
    notebook.set_vexpand(true);
    root.append(&notebook);

    let overview_list = GtkBox::new(Orientation::Vertical, 16);
    notebook.append_page(
        &page_container(&scrolled(&overview_list)),
        Some(&Label::new(Some("Overview"))),
    );

    let (signal_page, five_g_graph, lte_graph) = build_signal_page();
    notebook.append_page(
        &page_container(&signal_page),
        Some(&Label::new(Some("Signal"))),
    );

    let (
        wifi_page,
        two_radio,
        five_radio,
        six_radio,
        ssid_list,
        ssid_editors,
        add_ssid,
        save_wifi,
        discard_wifi,
    ) = build_wifi_page();
    let wifi_tab_label = Label::new(Some("Wi-Fi"));
    notebook.append_page(&page_container(&wifi_page), Some(&wifi_tab_label));

    let (
        clients_page,
        clients_search,
        clients_list,
        client_sort_type,
        client_sort_hostname,
        client_sort_ip,
        client_sort_mac,
    ) = build_clients_page();
    notebook.append_page(
        &page_container(&clients_page),
        Some(&Label::new(Some("Clients"))),
    );

    let (details_page, device_list, general_list, sim_list, export_diagnostics) =
        build_device_details_page();
    notebook.append_page(
        &page_container(&details_page),
        Some(&Label::new(Some("Device"))),
    );

    let refresh = Button::with_label("Refresh");
    let reboot = Button::with_label("Reboot Gateway");
    let logout = Button::with_label("Log Out");

    DetailsPage {
        root,
        overview_list,
        notebook,
        five_g_graph,
        lte_graph,
        device_list,
        general_list,
        sim_list,
        clients_search,
        clients_list,
        client_sort_type,
        client_sort_hostname,
        client_sort_ip,
        client_sort_mac,
        two_radio,
        five_radio,
        six_radio,
        ssid_list,
        ssid_editors,
        add_ssid,
        refresh,
        save_wifi,
        discard_wifi,
        wifi_tab_label,
        reboot,
        logout,
        export_diagnostics,
    }
}

fn build_signal_page() -> (GtkBox, SignalGraph, SignalGraph) {
    let root = GtkBox::new(Orientation::Vertical, 16);
    let (five_g_box, five_g_graph) = SignalGraph::new("5G", "5G network not connected");
    let (lte_box, lte_graph) = SignalGraph::new("LTE", "LTE network not connected");
    root.append(&five_g_box);
    root.append(&lte_box);
    (root, five_g_graph, lte_graph)
}

fn build_device_details_page() -> (ScrolledWindow, ListBox, ListBox, ListBox, Button) {
    let root = GtkBox::new(Orientation::Vertical, 16);
    let export = Button::with_label("Export Diagnostics Bundle");
    export.set_halign(Align::Start);
    root.append(&export);
    let device = section_list("Gateway", &root);
    let general = section_list("Connection", &root);
    let sim = section_list("SIM", &root);
    (scrolled(&root), device, general, sim, export)
}

fn build_clients_page() -> (GtkBox, Entry, ListBox, Button, Button, Button, Button) {
    let root = GtkBox::new(Orientation::Vertical, 16);
    root.append(&section_heading("Clients"));
    let search = Entry::builder()
        .placeholder_text("Search clients")
        .hexpand(true)
        .build();
    root.append(&search);

    let header = GtkBox::new(Orientation::Horizontal, 12);
    header.add_css_class("table-header");
    let type_button = Button::with_label("Type");
    type_button.set_width_request(80);
    let hostname_button = Button::with_label("Hostname");
    hostname_button.set_width_request(192);
    hostname_button.set_hexpand(true);
    let ip_button = Button::with_label("IP Address");
    ip_button.set_width_request(144);
    let mac_button = Button::with_label("MAC Address");
    mac_button.set_width_request(160);
    for button in [&type_button, &hostname_button, &ip_button, &mac_button] {
        button.add_css_class("flat");
        header.append(button);
    }
    root.append(&header);

    let list = ListBox::new();
    list.add_css_class("boxed-list");
    root.append(&scrolled(&list));
    (
        root,
        search,
        list,
        type_button,
        hostname_button,
        ip_button,
        mac_button,
    )
}

#[derive(Clone)]
struct SsidEditor {
    root: GtkBox,
    title_label: Label,
    summary: Label,
    edit_button: Button,
    name: Entry,
    key: PasswordEntry,
    grid: Grid,
    options: GtkBox,
    qr: DrawingArea,
    qr_button: Button,
    delete_button: Button,
    editing: Rc<Cell<bool>>,
    pending_delete: Rc<Cell<bool>>,
    broadcast: CheckButton,
    two: CheckButton,
    five: CheckButton,
    six: CheckButton,
    guest: CheckButton,
    encryption_mode: Rc<RefCell<String>>,
    encryption_version: Rc<RefCell<String>>,
    extra: Rc<RefCell<serde_json::Map<String, serde_json::Value>>>,
}

#[allow(clippy::type_complexity)]
fn build_wifi_page() -> (
    ScrolledWindow,
    CheckButton,
    CheckButton,
    CheckButton,
    ListBox,
    Rc<RefCell<Vec<SsidEditor>>>,
    Button,
    Button,
    Button,
) {
    let root = GtkBox::new(Orientation::Vertical, 16);
    root.set_margin_bottom(8);

    let radio_section = GtkBox::new(Orientation::Vertical, 6);
    radio_section.append(&section_heading("Radio Bands"));
    let radio_options = GtkBox::new(Orientation::Horizontal, 12);
    let two = CheckButton::with_label("2.4 GHz");
    let five = CheckButton::with_label("5 GHz");
    let six = CheckButton::with_label("6 GHz");
    radio_options.append(&two);
    radio_options.append(&five);
    radio_options.append(&six);
    radio_section.append(&radio_options);
    root.append(&radio_section);

    let ssid_header = GtkBox::new(Orientation::Horizontal, 8);
    let ssid_heading = section_heading("SSIDs");
    ssid_heading.set_hexpand(true);
    let add = Button::from_icon_name("list-add-symbolic");
    add.set_tooltip_text(Some("Add SSID"));
    ssid_header.append(&ssid_heading);
    ssid_header.append(&add);
    root.append(&ssid_header);
    let ssid_list = ListBox::new();
    ssid_list.add_css_class("boxed-list");
    root.append(&ssid_list);

    let actions = GtkBox::new(Orientation::Horizontal, 8);
    let save = Button::with_label("Save Wi-Fi");
    let discard = Button::with_label("Discard Changes");
    save.set_visible(false);
    discard.set_visible(false);
    actions.append(&save);
    actions.append(&discard);
    root.append(&actions);
    (
        scrolled(&root),
        two,
        five,
        six,
        ssid_list,
        Rc::new(RefCell::new(Vec::new())),
        add,
        save,
        discard,
    )
}

fn wire_actions(
    ui: &Ui,
    login: Button,
    refresh: Button,
    save_wifi: Button,
    discard_wifi: Button,
    reboot: Button,
    logout: Button,
) {
    let ui_login = ui.clone();
    login.connect_clicked(move |_| {
        ui_login.set_status("Logging in...", false);
        ui_login.send_command(AppCommand::Login {
            gateway_ip: ui_login.gateway_ip.text().to_string(),
            username: ui_login.username.text().to_string(),
            password: ui_login.password.text().to_string(),
            remember: ui_login.remember.is_active(),
        });
    });

    let ui_refresh = ui.clone();
    refresh.connect_clicked(move |_| {
        logging::info("Manual refresh requested");
        ui_refresh.refresh(true)
    });

    let ui_add_ssid = ui.clone();
    ui.add_ssid.connect_clicked(move |_| {
        let mut ssids = current_ssid_configs(&ui_add_ssid.ssid_editors);
        ssids.push(new_ssid_config(ssids.first()));
        fill_ssids(&ui_add_ssid, &ssids);
        mark_wifi_dirty(&ui_add_ssid);
    });

    let ui_save = ui.clone();
    save_wifi.connect_clicked(move |_| {
        match validate_wifi(&ui_save).and_then(|_| build_wifi_config_from_ui(&ui_save)) {
            Ok(wifi) => {
                ui_save.set_status("Saving Wi-Fi...", false);
                ui_save.send_command(AppCommand::SaveWifi(Box::new(wifi)));
            }
            Err(error) => ui_save.set_status(&format!("Wi-Fi save failed: {error}"), false),
        }
    });

    let ui_discard = ui.clone();
    discard_wifi.connect_clicked(move |_| {
        logging::info("Discarding Wi-Fi changes");
        *ui_discard.wifi_edit_baseline.borrow_mut() = None;
        *ui_discard.wifi_draft.borrow_mut() = None;
        set_wifi_dirty(&ui_discard, false);
        ui_discard.apply_state(false);
        ui_discard.set_status("Wi-Fi changes discarded", true);
    });

    let ui_reboot = ui.clone();
    reboot.connect_clicked(move |button| {
        show_reboot_confirmation(button, &ui_reboot);
    });

    let ui_export = ui.clone();
    ui.export_diagnostics.connect_clicked(move |button| {
        export_diagnostics(button, &ui_export);
    });

    let ui_logout = ui.clone();
    logout.connect_clicked(move |_| {
        ui_logout.send_command(AppCommand::Logout);
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_signal_graph(
    ctx: &cairo::Context,
    width: f64,
    height: f64,
    history: &SignalHistory,
    show_rsrp: bool,
    show_rsrq: bool,
    show_rssi: bool,
    show_sinr: bool,
    show_cqi: bool,
) {
    let plot_left = 46.0;
    let plot_top = 12.0;
    let now = Instant::now();
    let label_width = current_value_label_width(
        ctx, history, show_rsrp, show_rsrq, show_rssi, show_sinr, show_cqi, now,
    );
    let right_padding = (label_width + 18.0).max(44.0);
    let plot_right = width - right_padding;
    let plot_bottom = height - 28.0;
    let plot_width = (plot_right - plot_left).max(1.0);
    let plot_height = (plot_bottom - plot_top).max(1.0);

    ctx.set_source_rgb(0.96, 0.96, 0.96);
    let _ = ctx.paint();
    ctx.set_source_rgb(0.84, 0.84, 0.84);
    ctx.rectangle(plot_left, plot_top, plot_width, plot_height);
    let _ = ctx.stroke();

    let mut tick = SIGNAL_AXIS_MIN as i32;
    while tick <= SIGNAL_AXIS_MAX as i32 {
        let tick_value = tick as f64;
        let y = plot_top
            + (SIGNAL_AXIS_MAX - tick_value) / (SIGNAL_AXIS_MAX - SIGNAL_AXIS_MIN) * plot_height;
        if tick != SIGNAL_AXIS_MIN as i32 && tick != SIGNAL_AXIS_MAX as i32 {
            ctx.move_to(plot_left, y);
            ctx.line_to(plot_right, y);
            let _ = ctx.stroke();
        }
        tick += 20;
    }
    for seconds in [30.0, 60.0, 90.0] {
        let x = plot_right - seconds / HISTORY_SECONDS * plot_width;
        ctx.move_to(x, plot_top);
        ctx.line_to(x, plot_bottom);
        let _ = ctx.stroke();
    }

    // Keep the line geometry clipped to the chart area so points just past the
    // 120s window can still connect to the visible edge without drawing outside it.
    ctx.save().ok();
    ctx.rectangle(plot_left, plot_top, plot_width, plot_height);
    ctx.clip();
    draw_metric(
        ctx,
        &history.rsrp,
        show_rsrp,
        now,
        (0.78, 0.16, 0.16),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_left,
        plot_top,
        plot_width,
        plot_height,
    );
    draw_metric(
        ctx,
        &history.rsrq,
        show_rsrq,
        now,
        (0.18, 0.49, 0.20),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_left,
        plot_top,
        plot_width,
        plot_height,
    );
    draw_metric(
        ctx,
        &history.rssi,
        show_rssi,
        now,
        (0.08, 0.39, 0.75),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_left,
        plot_top,
        plot_width,
        plot_height,
    );
    draw_metric(
        ctx,
        &history.sinr,
        show_sinr,
        now,
        (0.94, 0.42, 0.0),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_left,
        plot_top,
        plot_width,
        plot_height,
    );
    draw_metric(
        ctx,
        &history.cqi,
        show_cqi,
        now,
        (0.48, 0.22, 0.72),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_left,
        plot_top,
        plot_width,
        plot_height,
    );
    ctx.restore().ok();

    ctx.set_source_rgb(0.35, 0.35, 0.35);
    let mut tick = SIGNAL_AXIS_MIN as i32;
    while tick <= SIGNAL_AXIS_MAX as i32 {
        let tick_value = tick as f64;
        let y = plot_top
            + (SIGNAL_AXIS_MAX - tick_value) / (SIGNAL_AXIS_MAX - SIGNAL_AXIS_MIN) * plot_height;
        ctx.move_to(4.0, y + 4.0);
        let _ = ctx.show_text(&tick.to_string());
        tick += 20;
    }

    draw_current_value(
        ctx,
        "RSRP",
        &history.rsrp,
        show_rsrp,
        now,
        (0.78, 0.16, 0.16),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_right,
        plot_top,
        plot_height,
    );
    draw_current_value(
        ctx,
        "RSRQ",
        &history.rsrq,
        show_rsrq,
        now,
        (0.18, 0.49, 0.20),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_right,
        plot_top,
        plot_height,
    );
    draw_current_value(
        ctx,
        "RSSI",
        &history.rssi,
        show_rssi,
        now,
        (0.08, 0.39, 0.75),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_right,
        plot_top,
        plot_height,
    );
    draw_current_value(
        ctx,
        "SINR",
        &history.sinr,
        show_sinr,
        now,
        (0.94, 0.42, 0.0),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_right,
        plot_top,
        plot_height,
    );
    draw_current_value(
        ctx,
        "CQI",
        &history.cqi,
        show_cqi,
        now,
        (0.48, 0.22, 0.72),
        SIGNAL_AXIS_MIN,
        SIGNAL_AXIS_MAX,
        plot_right,
        plot_top,
        plot_height,
    );
    ctx.set_source_rgb(0.35, 0.35, 0.35);
    draw_centered_text(ctx, "-120s", plot_left, plot_bottom + 18.0);
    draw_centered_text(ctx, "0s", plot_right, plot_bottom + 18.0);
}

#[allow(clippy::too_many_arguments)]
fn draw_metric(
    ctx: &cairo::Context,
    ring: &MetricRing,
    visible: bool,
    now: Instant,
    color: (f64, f64, f64),
    min: f64,
    max: f64,
    plot_left: f64,
    plot_top: f64,
    plot_width: f64,
    plot_height: f64,
) {
    if !visible {
        return;
    }
    ctx.set_source_rgb(color.0, color.1, color.2);
    ctx.set_line_width(2.0);
    let mut started = false;
    for sample in ring.values.iter().copied() {
        if !sample.value.is_finite() {
            started = false;
            continue;
        }
        let age = now.duration_since(sample.at).as_secs_f64();
        let x = plot_left + (HISTORY_SECONDS - age) / HISTORY_SECONDS * plot_width;
        let y = plot_top + (max - sample.value) / (max - min) * plot_height;
        if started {
            ctx.line_to(x, y);
        } else {
            ctx.move_to(x, y);
            started = true;
        }
    }
    let _ = ctx.stroke();
}

#[allow(clippy::too_many_arguments)]
fn current_value_label_width(
    ctx: &cairo::Context,
    history: &SignalHistory,
    show_rsrp: bool,
    show_rsrq: bool,
    show_rssi: bool,
    show_sinr: bool,
    show_cqi: bool,
    now: Instant,
) -> f64 {
    [
        current_value_label("RSRP", &history.rsrp, show_rsrp, now),
        current_value_label("RSRQ", &history.rsrq, show_rsrq, now),
        current_value_label("RSSI", &history.rssi, show_rssi, now),
        current_value_label("SINR", &history.sinr, show_sinr, now),
        current_value_label("CQI", &history.cqi, show_cqi, now),
    ]
    .into_iter()
    .flatten()
    .filter_map(|label| ctx.text_extents(&label).ok().map(|extents| extents.width()))
    .fold(0.0, f64::max)
}

fn current_value_label(
    label: &str,
    ring: &MetricRing,
    visible: bool,
    now: Instant,
) -> Option<String> {
    if !visible {
        return None;
    }
    ring.iter_visible(now)
        .filter(|sample| sample.value.is_finite())
        .last()
        .map(|sample| format!("{:.0} ({label})", sample.value))
}

#[allow(clippy::too_many_arguments)]
fn draw_current_value(
    ctx: &cairo::Context,
    label: &str,
    ring: &MetricRing,
    visible: bool,
    now: Instant,
    color: (f64, f64, f64),
    min: f64,
    max: f64,
    plot_right: f64,
    plot_top: f64,
    plot_height: f64,
) {
    if !visible {
        return;
    }
    let Some(text) = current_value_label(label, ring, visible, now) else {
        return;
    };
    let Some(sample) = ring
        .iter_visible(now)
        .filter(|sample| sample.value.is_finite())
        .last()
    else {
        return;
    };

    let y = plot_top + (max - sample.value) / (max - min) * plot_height;
    ctx.set_source_rgb(color.0, color.1, color.2);
    ctx.move_to(plot_right + 8.0, y + 4.0);
    let _ = ctx.show_text(&text);
}

fn draw_centered_text(ctx: &cairo::Context, text: &str, center_x: f64, baseline_y: f64) {
    let width = ctx
        .text_extents(text)
        .map(|extents| extents.width())
        .unwrap_or(0.0);
    ctx.move_to(center_x - width / 2.0, baseline_y);
    let _ = ctx.show_text(text);
}

fn metric_toggle(label: &str, css_class: &str) -> ToggleButton {
    let toggle = ToggleButton::builder().label(label).active(true).build();
    toggle.add_css_class("metric-chip");
    toggle.add_css_class(css_class);
    toggle
}

fn fill_latest_signal_values(graph: &SignalGraph) {
    let history = graph.history.borrow();
    let now = Instant::now();
    for (toggle, label, value) in [
        (&graph.rsrp, "RSRP", history.rsrp.latest(now)),
        (&graph.rsrq, "RSRQ", history.rsrq.latest(now)),
        (&graph.rssi, "RSSI", history.rssi.latest(now)),
        (&graph.sinr, "SINR", history.sinr.latest(now)),
        (&graph.cqi, "CQI", history.cqi.latest(now)),
    ] {
        let text = value
            .map(|value| format!("{value:.0} {label}"))
            .unwrap_or_else(|| label.to_string());
        toggle.set_label(&text);
    }
}

fn section_list(title: &str, root: &GtkBox) -> ListBox {
    let section = GtkBox::new(Orientation::Vertical, 7);
    section.append(&section_heading(title));
    let list = ListBox::new();
    list.add_css_class("boxed-list");
    section.append(&list);
    root.append(&section);
    list
}

fn section_heading(title: &str) -> Label {
    let heading = Label::builder().label(title).xalign(0.0).build();
    heading.add_css_class("section-heading");
    heading
}

fn page_container<W: IsA<gtk::Widget>>(child: &W) -> GtkBox {
    let container = GtkBox::new(Orientation::Vertical, 16);
    container.set_margin_top(14);
    container.set_margin_bottom(14);
    container.set_margin_start(14);
    container.set_margin_end(14);
    container.append(child);
    container
}

fn scrolled<W: IsA<gtk::Widget>>(child: &W) -> ScrolledWindow {
    ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

fn reconcile_rows(
    list: &ListBox,
    existing: &Rc<RefCell<HashMap<String, DetailRowWidgets>>>,
    rows: &[Row],
) {
    let mut seen = HashSet::new();
    let mut widgets = existing.borrow_mut();

    for (index, row) in rows.iter().enumerate() {
        seen.insert(row.label.clone());
        if let Some(row_widgets) = widgets.get(&row.label) {
            row_widgets.value.set_text(&row.value);
            *row_widgets.copy_value.borrow_mut() = row.value.clone();
            if row_widgets.row.index() != index as i32 {
                list.remove(&row_widgets.row);
                list.insert(&row_widgets.row, index as i32);
            }
        } else {
            let row_widgets = create_detail_row(list, row, index as i32);
            widgets.insert(row.label.clone(), row_widgets);
        }
    }

    let stale_labels: Vec<String> = widgets
        .keys()
        .filter(|label| !seen.contains(*label))
        .cloned()
        .collect();
    for label in stale_labels {
        if let Some(row_widgets) = widgets.remove(&label) {
            list.remove(&row_widgets.row);
        }
    }
}

fn create_detail_row(list: &ListBox, row: &Row, position: i32) -> DetailRowWidgets {
    let line = GtkBox::new(Orientation::Horizontal, 12);
    line.set_margin_top(6);
    line.set_margin_bottom(6);
    line.set_margin_start(8);
    line.set_margin_end(8);
    let label = Label::builder()
        .label(&row.label)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let value = Label::builder().label(&row.value).xalign(1.0).build();
    if is_network_identifier(&row.label) {
        value.add_css_class("monospace");
    }
    line.append(&label);
    line.append(&value);

    let copy_value = Rc::new(RefCell::new(row.value.clone()));
    let list_row = nonselectable_row(&line);
    add_copy_context(&list_row, "Copy Value", copy_value.clone());
    list.insert(&list_row, position);

    DetailRowWidgets {
        row: list_row,
        value,
        copy_value,
    }
}

fn filter_rows(rows: &[Row], labels: &[&str]) -> Vec<Row> {
    labels
        .iter()
        .filter_map(|label| rows.iter().find(|row| row.label == *label).cloned())
        .collect()
}

fn refill_clients(ui: &Ui) {
    let mut clients = ui.client_rows.borrow().clone();
    let query = ui.clients_search.text().to_ascii_lowercase();
    if !query.is_empty() {
        clients.retain(|client| {
            client.band.to_ascii_lowercase().contains(&query)
                || client.name.to_ascii_lowercase().contains(&query)
                || client.ip.to_ascii_lowercase().contains(&query)
                || client.mac.to_ascii_lowercase().contains(&query)
        });
    }

    match ui.client_sort.get() {
        ClientSort::Type => clients.sort_by(|a, b| a.band.cmp(&b.band)),
        ClientSort::Hostname => clients.sort_by(|a, b| a.name.cmp(&b.name)),
        ClientSort::Ip => clients.sort_by(|a, b| a.ip.cmp(&b.ip)),
        ClientSort::Mac => clients.sort_by(|a, b| a.mac.cmp(&b.mac)),
    }
    if ui.client_sort_direction.get() == SortDirection::Descending {
        clients.reverse();
    }
    update_client_sort_headers(ui);

    reconcile_clients(ui, &clients);
}

fn reconcile_clients(ui: &Ui, visible_clients: &[ClientRow]) {
    let all_keys: HashSet<String> = ui.client_rows.borrow().iter().map(client_key).collect();
    let mut widgets = ui.client_widgets.borrow_mut();

    let stale_keys: Vec<String> = widgets
        .keys()
        .filter(|key| !all_keys.contains(*key))
        .cloned()
        .collect();
    for key in stale_keys {
        if let Some(row_widgets) = widgets.remove(&key) {
            if row_widgets.row.parent().is_some() {
                ui.clients_list.remove(&row_widgets.row);
            }
        }
    }

    let visible_keys: HashSet<String> = visible_clients.iter().map(client_key).collect();
    let hidden_keys: Vec<String> = widgets
        .keys()
        .filter(|key| !visible_keys.contains(*key))
        .cloned()
        .collect();
    for key in hidden_keys {
        if let Some(row_widgets) = widgets.get(&key) {
            if row_widgets.row.parent().is_some() {
                ui.clients_list.remove(&row_widgets.row);
            }
        }
    }

    for (index, client) in visible_clients.iter().enumerate() {
        let key = client_key(client);
        let row_widgets = widgets
            .entry(key)
            .or_insert_with(|| create_client_row(client));
        update_client_row(row_widgets, client);
        if row_widgets.row.parent().is_none() {
            ui.clients_list.insert(&row_widgets.row, index as i32);
        } else if row_widgets.row.index() != index as i32 {
            ui.clients_list.remove(&row_widgets.row);
            ui.clients_list.insert(&row_widgets.row, index as i32);
        }
    }
}

fn create_client_row(client: &ClientRow) -> ClientRowWidgets {
    let line = GtkBox::new(Orientation::Horizontal, 12);
    line.set_margin_top(6);
    line.set_margin_bottom(6);
    line.set_margin_start(8);
    line.set_margin_end(8);
    let band = Label::builder()
        .label(&client.band)
        .xalign(0.0)
        .width_chars(10)
        .build();
    let name = Label::builder()
        .label(&client.name)
        .xalign(0.0)
        .hexpand(true)
        .build();
    let ip = Label::builder()
        .label(&client.ip)
        .xalign(0.0)
        .width_chars(16)
        .build();
    ip.add_css_class("monospace");
    let mac = Label::builder()
        .label(&client.mac)
        .xalign(0.0)
        .width_chars(18)
        .build();
    mac.add_css_class("monospace");

    line.append(&band);
    line.append(&name);
    line.append(&ip);
    line.append(&mac);

    let copy_ip = Rc::new(RefCell::new(client.ip.clone()));
    let copy_mac = Rc::new(RefCell::new(client.mac.clone()));
    let copy_hostname = Rc::new(RefCell::new(client.name.clone()));
    let row = nonselectable_row(&line);
    add_client_context(
        &row,
        copy_ip.clone(),
        copy_mac.clone(),
        copy_hostname.clone(),
    );

    ClientRowWidgets {
        row,
        band,
        name,
        ip,
        mac,
        copy_ip,
        copy_mac,
        copy_hostname,
    }
}

fn update_client_row(row_widgets: &ClientRowWidgets, client: &ClientRow) {
    row_widgets.band.set_text(&client.band);
    row_widgets.name.set_text(&client.name);
    row_widgets.ip.set_text(&client.ip);
    row_widgets.mac.set_text(&client.mac);
    *row_widgets.copy_ip.borrow_mut() = client.ip.clone();
    *row_widgets.copy_mac.borrow_mut() = client.mac.clone();
    *row_widgets.copy_hostname.borrow_mut() = client.name.clone();
}

fn client_key(client: &ClientRow) -> String {
    if !client.mac.trim().is_empty() {
        format!("mac:{}", client.mac)
    } else if !client.ip.trim().is_empty() {
        format!("ip:{}", client.ip)
    } else {
        format!("{}:{}:{}", client.band, client.name, client.ip)
    }
}

fn fill_overview(root: &GtkBox, snapshot: &Snapshot) {
    clear_box(root);

    let connection = GtkBox::new(Orientation::Vertical, 6);
    connection.append(&section_heading("Connection Summary"));

    let connection_cards = GtkBox::new(Orientation::Horizontal, 12);
    connection_cards.set_homogeneous(true);
    connection_cards.append(&connection_card(
        "5G",
        snapshot.five_g_metrics.as_ref(),
        &snapshot.five_g_summary,
    ));
    connection_cards.append(&connection_card(
        "LTE",
        snapshot.lte_metrics.as_ref(),
        &snapshot.lte_summary,
    ));

    connection.append(&connection_cards);
    root.append(&connection);

    let wifi = GtkBox::new(Orientation::Vertical, 6);
    wifi.append(&section_heading("Wi-Fi"));
    let wifi_widget = GtkBox::new(Orientation::Vertical, 8);
    wifi_widget.add_css_class("overview-card");
    let bands = GtkBox::new(Orientation::Horizontal, 8);
    if let Some(wifi_config) = &snapshot.wifi {
        bands.append(&band_chip(
            "2.4 GHz",
            wifi_config
                .two_gig
                .as_ref()
                .and_then(|band| band.is_radio_enabled)
                .unwrap_or(false),
        ));
        bands.append(&band_chip(
            "5 GHz",
            wifi_config
                .five_gig
                .as_ref()
                .and_then(|band| band.is_radio_enabled)
                .unwrap_or(false),
        ));
        bands.append(&band_chip(
            "6 GHz",
            wifi_config
                .six_gig
                .as_ref()
                .and_then(|band| band.is_radio_enabled)
                .unwrap_or(false),
        ));
        wifi_widget.append(&bands);
        let ssids = wifi_config.ssids.as_deref().unwrap_or_default();
        let broadcasting = ssids
            .iter()
            .filter(|ssid| ssid.is_broadcast_enabled.unwrap_or(false))
            .count();
        wifi_widget.append(
            &Label::builder()
                .label(format!(
                    "{} configured SSIDs, {broadcasting} broadcasting",
                    ssids.len()
                ))
                .xalign(0.0)
                .build(),
        );
    } else {
        wifi_widget.append(
            &Label::builder()
                .label("Wi-Fi configuration unavailable")
                .xalign(0.0)
                .build(),
        );
    }

    wifi.append(&wifi_widget);
    root.append(&wifi);

    append_overview_section(
        root,
        "Clients",
        &[format!("{} connected devices", snapshot.clients.len())],
    );
    append_overview_section(root, "Device", &device_summary(snapshot));
}

fn clear_list(list: &ListBox) {
    while let Some(row) = list
        .first_child()
        .and_then(|child| child.downcast::<ListBoxRow>().ok())
    {
        list.remove(&row);
    }
}

fn clear_box(box_: &GtkBox) {
    while let Some(child) = box_.first_child() {
        box_.remove(&child);
    }
}

fn connection_card(title: &str, metrics: Option<&SignalMetrics>, rows: &[Row]) -> GtkBox {
    let card = GtkBox::new(Orientation::Vertical, 8);
    let card_title = GtkBox::new(Orientation::Horizontal, 8);
    card.add_css_class("overview-card");
    card.set_hexpand(true);
    card_title.append(&section_heading(title));

    let state = if metrics.is_some() {
        "active"
    } else {
        "not connected"
    };
    card_title.append(&Label::builder().label(state).xalign(0.0).build());
    card.append(&card_title);

    let bars = row_value(rows, "Bars")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
        .min(4);
    card.append(&coverage_bars(bars));

    let cqi_text = metrics
        .and_then(|metrics| metrics.cqi)
        .map(cqi_quality)
        .unwrap_or("Connection Unavailable");
    let cqi = Label::builder().label(cqi_text).xalign(0.0).build();
    cqi.add_css_class("metric-chip");
    match cqi_text {
        "Connection Poor" => cqi.add_css_class("quality-poor"),
        "Connection Fair" => cqi.add_css_class("quality-fair"),
        "Connection Good" => cqi.add_css_class("quality-good"),
        _ => cqi.add_css_class("dim-label"),
    }
    card.append(&cqi);
    card
}

fn coverage_bars(active: usize) -> GtkBox {
    let bars = GtkBox::new(Orientation::Horizontal, 4);
    bars.set_valign(Align::End);
    for index in 0..5 {
        let bar = GtkBox::new(Orientation::Vertical, 0);
        bar.set_width_request(9);
        bar.set_height_request(8 + index as i32 * 5);
        bar.set_valign(Align::End);
        bar.add_css_class("coverage-bar");
        if index < active {
            bar.add_css_class("coverage-bar-good");
        } else {
            bar.add_css_class("coverage-bar-bad");
        }
        bars.append(&bar);
    }
    bars
}

fn band_chip(label: &str, enabled: bool) -> Label {
    let chip = Label::builder()
        .label(format!("{label} {}", if enabled { "✓" } else { "×" }))
        .xalign(0.0)
        .build();
    chip.add_css_class("metric-chip");
    chip.add_css_class(if enabled {
        "enabled-chip"
    } else {
        "disabled-chip"
    });
    chip
}

fn append_overview_section(root_container: &GtkBox, title: &str, lines: &[String]) {
    let root = GtkBox::new(Orientation::Vertical, 6);
    root.append(&section_heading(title));

    let section = GtkBox::new(Orientation::Vertical, 4);
    section.add_css_class("overview-card");
    for line in lines {
        section.append(&Label::builder().label(line).xalign(0.0).build());
    }

    root.append(&section);
    root_container.append(&root);
}

fn append_nonselectable<W: IsA<gtk::Widget>>(list: &ListBox, child: &W) {
    list.append(&nonselectable_row(child));
}

fn nonselectable_row<W: IsA<gtk::Widget>>(child: &W) -> ListBoxRow {
    let row = ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.set_child(Some(child));
    row
}

fn cqi_quality(cqi: i64) -> &'static str {
    match cqi {
        i64::MIN..=5 => "Connection Poor",
        6..=10 => "Connection Fair",
        11..=15 => "Connection Good",
        _ => "Connection Unavailable",
    }
}

fn device_summary(snapshot: &Snapshot) -> Vec<String> {
    let manufacturer = row_value(&snapshot.device_summary, "Manufacturer");
    let model = row_value(&snapshot.device_summary, "Model");
    let software = row_value(&snapshot.device_summary, "Software");
    let uptime = row_value(&snapshot.general_summary, "Uptime");
    vec![
        format!(
            "{} {}",
            manufacturer.unwrap_or_default(),
            model.unwrap_or_default()
        )
        .trim()
        .to_string(),
        format!("Software {}", software.unwrap_or("unavailable")),
        format!("Uptime {}", uptime.unwrap_or("unavailable")),
    ]
}

fn row_value<'a>(rows: &'a [Row], label: &str) -> Option<&'a str> {
    rows.iter()
        .find(|row| row.label == label)
        .map(|row| row.value.as_str())
}

fn is_network_identifier(label: &str) -> bool {
    matches!(label, "ICCID" | "IMEI" | "IMSI" | "MSISDN")
}

fn add_copy_context<W: IsA<gtk::Widget>>(widget: &W, label: &str, copy_value: Rc<RefCell<String>>) {
    let widget = widget.clone().upcast::<gtk::Widget>();

    let action_group = gtk::gio::SimpleActionGroup::new();
    let copy_action = gtk::gio::SimpleAction::new("copy", None);
    {
        let copy_value = copy_value.clone();
        let widget_for_action = widget.clone();
        copy_action.connect_activate(move |_, _| {
            let value = copy_value.borrow().clone();
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&value);
            }
            widget_for_action.grab_focus();
        });
    }
    action_group.add_action(&copy_action);
    widget.insert_action_group("context", Some(&action_group));

    let popover = PopoverMenu::builder().build();
    popover.add_css_class("copy-context-menu");
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_halign(Align::Start);
    popover.set_parent(&widget);

    let menu_box = GtkBox::new(Orientation::Vertical, 0);
    menu_box.add_css_class("copy-context-box");
    let copy_button = Button::with_label(label);
    copy_button.add_css_class("copy-context-button");
    copy_button.add_css_class("flat");
    copy_button.set_can_focus(false);
    copy_button.set_action_name(Some("context.copy"));
    let popover_for_button = popover.clone();
    copy_button.connect_clicked(move |_| popover_for_button.popdown());
    menu_box.append(&copy_button);
    popover.set_child(Some(&menu_box));

    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let popover_for_click = popover.clone();
    gesture.connect_pressed(move |_, _, x, y| {
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover_for_click.set_offset(0, 0);
        popover_for_click.set_pointing_to(Some(&rect));
        popover_for_click.popup();
    });
    widget.add_controller(gesture);
}

fn add_client_context<W: IsA<gtk::Widget>>(
    widget: &W,
    copy_ip: Rc<RefCell<String>>,
    copy_mac: Rc<RefCell<String>>,
    copy_hostname: Rc<RefCell<String>>,
) {
    let widget = widget.clone().upcast::<gtk::Widget>();

    let action_group = gtk::gio::SimpleActionGroup::new();
    for (action_name, copy_value) in [
        ("copy-ip", copy_ip),
        ("copy-mac", copy_mac),
        ("copy-hostname", copy_hostname),
    ] {
        let action = gtk::gio::SimpleAction::new(action_name, None);
        let widget_for_action = widget.clone();
        action.connect_activate(move |_, _| {
            let value = copy_value.borrow().clone();
            if let Some(display) = gtk::gdk::Display::default() {
                display.clipboard().set_text(&value);
            }
            widget_for_action.grab_focus();
        });
        action_group.add_action(&action);
    }
    widget.insert_action_group("client", Some(&action_group));

    let popover = PopoverMenu::builder().build();
    popover.add_css_class("copy-context-menu");
    popover.set_has_arrow(false);
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_halign(Align::Start);
    popover.set_parent(&widget);

    let menu_box = GtkBox::new(Orientation::Vertical, 0);
    menu_box.add_css_class("copy-context-box");
    for (label, action_name) in [
        ("Copy IP", "client.copy-ip"),
        ("Copy MAC", "client.copy-mac"),
        ("Copy Host", "client.copy-hostname"),
    ] {
        let button = Button::with_label(label);
        button.add_css_class("copy-context-button");
        button.add_css_class("flat");
        button.set_can_focus(false);
        button.set_action_name(Some(action_name));
        let popover_for_button = popover.clone();
        button.connect_clicked(move |_| popover_for_button.popdown());
        menu_box.append(&button);
    }
    popover.set_child(Some(&menu_box));

    let gesture = gtk::GestureClick::new();
    gesture.set_button(gtk::gdk::BUTTON_SECONDARY);
    let popover_for_click = popover.clone();
    gesture.connect_pressed(move |_, _, x, y| {
        let rect = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        popover_for_click.set_offset(0, 0);
        popover_for_click.set_pointing_to(Some(&rect));
        popover_for_click.popup();
    });
    widget.add_controller(gesture);
}

fn build_wifi_config_from_ui(ui: &Ui) -> Result<WifiConfig, String> {
    let mut wifi = ui
        .state
        .borrow()
        .snapshot
        .wifi
        .clone()
        .ok_or_else(|| "Wi-Fi configuration is not available".to_string())?;

    if let Some(band) = wifi.two_gig.as_mut() {
        band.is_radio_enabled = Some(ui.two_radio.is_active());
    }
    if let Some(band) = wifi.five_gig.as_mut() {
        band.is_radio_enabled = Some(ui.five_radio.is_active());
    }
    if let Some(band) = wifi.six_gig.as_mut() {
        band.is_radio_enabled = Some(ui.six_radio.is_active());
    }

    wifi.ssids = Some(
        ui.ssid_editors
            .borrow()
            .iter()
            .filter(|editor| !editor.pending_delete.get())
            .map(SsidEditor::to_config)
            .collect(),
    );
    Ok(wifi)
}

fn validate_wifi(ui: &Ui) -> Result<(), String> {
    for (index, editor) in ui.ssid_editors.borrow().iter().enumerate() {
        if editor.pending_delete.get() {
            continue;
        }
        if editor.name.text().trim().is_empty() {
            return Err(format!("SSID {} name is required", index + 1));
        }
        if editor.key.text().trim().is_empty() {
            return Err(format!("SSID {} password is required", index + 1));
        }
    }
    Ok(())
}

fn mark_wifi_dirty(ui: &Ui) {
    if ui.suppress_wifi_dirty.get() {
        return;
    }
    ui.ensure_wifi_edit_baseline();
    ui.refresh_wifi_dirty_state();
}

fn set_wifi_dirty(ui: &Ui, dirty: bool) {
    ui.wifi_dirty.set(dirty);
    update_wifi_dirty_controls(&ui.save_wifi, &ui.discard_wifi, &ui.wifi_tab_label, dirty);
}

fn update_wifi_dirty_controls(
    save_wifi: &Button,
    discard_wifi: &Button,
    tab_label: &Label,
    dirty: bool,
) {
    save_wifi.set_visible(dirty);
    discard_wifi.set_visible(dirty);
    tab_label.set_text(if dirty { "Wi-Fi *" } else { "Wi-Fi" });
}

fn wire_wifi_dirty_tracking(ui: &Ui) {
    for toggle in [&ui.two_radio, &ui.five_radio, &ui.six_radio] {
        let ui_dirty = ui.clone();
        toggle.connect_toggled(move |_| mark_wifi_dirty(&ui_dirty));
    }
}

fn wire_client_controls(ui: &Ui) {
    let ui_search = ui.clone();
    ui.clients_search
        .connect_changed(move |_| refill_clients(&ui_search));

    for (button, sort) in [
        (ui.client_sort_type.clone(), ClientSort::Type),
        (ui.client_sort_hostname.clone(), ClientSort::Hostname),
        (ui.client_sort_ip.clone(), ClientSort::Ip),
        (ui.client_sort_mac.clone(), ClientSort::Mac),
    ] {
        let ui_sort = ui.clone();
        button.connect_clicked(move |_| {
            if ui_sort.client_sort.get() == sort {
                ui_sort
                    .client_sort_direction
                    .set(match ui_sort.client_sort_direction.get() {
                        SortDirection::Ascending => SortDirection::Descending,
                        SortDirection::Descending => SortDirection::Ascending,
                    });
            } else {
                ui_sort.client_sort.set(sort);
                ui_sort.client_sort_direction.set(SortDirection::Ascending);
            }
            refill_clients(&ui_sort);
        });
    }
    update_client_sort_headers(ui);
}

fn update_client_sort_headers(ui: &Ui) {
    for (button, sort, label) in [
        (&ui.client_sort_type, ClientSort::Type, "Type"),
        (&ui.client_sort_hostname, ClientSort::Hostname, "Hostname"),
        (&ui.client_sort_ip, ClientSort::Ip, "IP Address"),
        (&ui.client_sort_mac, ClientSort::Mac, "MAC Address"),
    ] {
        if ui.client_sort.get() == sort {
            let marker = match ui.client_sort_direction.get() {
                SortDirection::Ascending => "↑",
                SortDirection::Descending => "↓",
            };
            button.set_label(&format!("{label} {marker}"));
        } else {
            button.set_label(label);
        }
    }
}

fn wire_ssid_dirty_tracking(ui: &Ui) {
    for editor in ui.ssid_editors.borrow().iter() {
        let ui_dirty = ui.clone();
        let ui_edit = ui.clone();
        editor.edit_button.connect_clicked(move |button| {
            if button.label().as_deref() == Some("Done") {
                ui_edit.ensure_wifi_edit_baseline();
            }
        });
        editor.name.connect_changed(move |_| {
            mark_wifi_dirty(&ui_dirty);
        });
        let ui_dirty = ui.clone();
        editor.key.connect_changed(move |_| {
            mark_wifi_dirty(&ui_dirty);
        });
        for toggle in [
            editor.broadcast.clone(),
            editor.two.clone(),
            editor.five.clone(),
            editor.six.clone(),
            editor.guest.clone(),
        ] {
            let ui_dirty = ui.clone();
            toggle.connect_toggled(move |_| {
                mark_wifi_dirty(&ui_dirty);
            });
        }
    }
}

fn apply_wifi(ui: &Ui, snapshot: &Snapshot) {
    ui.suppress_wifi_dirty.set(true);
    let Some(wifi) = &snapshot.wifi else {
        clear_list(&ui.ssid_list);
        ui.ssid_editors.borrow_mut().clear();
        ui.suppress_wifi_dirty.set(false);
        return;
    };
    if let Some(band) = &wifi.two_gig {
        ui.two_radio
            .set_active(band.is_radio_enabled.unwrap_or(false));
    }
    if let Some(band) = &wifi.five_gig {
        ui.five_radio
            .set_active(band.is_radio_enabled.unwrap_or(false));
    }
    if let Some(band) = &wifi.six_gig {
        ui.six_radio
            .set_active(band.is_radio_enabled.unwrap_or(false));
        ui.six_radio.set_visible(true);
    } else {
        ui.six_radio.set_visible(false);
    }

    fill_ssids(ui, wifi.ssids.as_deref().unwrap_or(&[]));
    ui.wifi_dirty.set(false);
    *ui.wifi_edit_baseline.borrow_mut() = None;
    *ui.wifi_draft.borrow_mut() = None;
    update_wifi_dirty_controls(&ui.save_wifi, &ui.discard_wifi, &ui.wifi_tab_label, false);
    ui.suppress_wifi_dirty.set(false);
}

fn fill_ssids(ui: &Ui, ssids: &[SsidConfig]) {
    {
        let existing = ui.ssid_editors.borrow();
        if existing.len() == ssids.len() {
            for (index, (editor, ssid)) in existing.iter().zip(ssids.iter()).enumerate() {
                update_ssid_editor(editor, index, ssid);
            }
            return;
        }
    }

    clear_list(&ui.ssid_list);
    let mut new_editors = Vec::new();
    for (index, ssid) in ssids.iter().enumerate() {
        let (row, editor) = ssid_editor_row(index, ssid);
        append_nonselectable(&ui.ssid_list, &row);
        new_editors.push(editor);
    }
    *ui.ssid_editors.borrow_mut() = new_editors;
    wire_ssid_delete_buttons(ui);
    wire_ssid_dirty_tracking(ui);
}

fn update_ssid_editor(editor: &SsidEditor, index: usize, ssid: &SsidConfig) {
    let title = ssid_title(index, ssid);
    editor.root.remove_css_class("pending-delete");
    editor.title_label.set_text(&title);
    editor.summary.set_text(&ssid_summary_text(ssid));
    editor
        .name
        .set_text(ssid.ssid_name.as_deref().unwrap_or_default());
    editor
        .key
        .set_text(ssid.wpa_key.as_deref().unwrap_or_default());
    editor
        .broadcast
        .set_active(ssid.is_broadcast_enabled.unwrap_or(false));
    editor.two.set_active(ssid.two_gig_ssid.unwrap_or(false));
    editor.five.set_active(ssid.five_gig_ssid.unwrap_or(false));
    editor.six.set_active(ssid.six_gig_ssid.unwrap_or(false));
    editor.guest.set_active(ssid.guest.unwrap_or(false));
    *editor.encryption_mode.borrow_mut() = ssid.encryption_mode.clone().unwrap_or_default();
    *editor.encryption_version.borrow_mut() = ssid.encryption_version.clone().unwrap_or_default();
    *editor.extra.borrow_mut() = ssid.extra.clone();

    let starts_editing = ssid.ssid_name.as_deref().unwrap_or_default().is_empty();
    editor.pending_delete.set(false);
    editor.editing.set(starts_editing);
    editor.title_label.set_visible(!starts_editing);
    editor.summary.set_visible(!starts_editing);
    editor.name.set_visible(starts_editing);
    editor.delete_button.set_visible(starts_editing);
    editor.edit_button.set_visible(true);
    editor
        .edit_button
        .set_label(if starts_editing { "Done" } else { "Edit" });
    editor.grid.set_visible(starts_editing);
    editor.options.set_visible(starts_editing);
}

fn wire_ssid_delete_buttons(ui: &Ui) {
    for editor in ui.ssid_editors.borrow().iter() {
        let editor = editor.clone();
        let ui_dirty = ui.clone();
        editor.delete_button.clone().connect_clicked(move |_| {
            mark_ssid_pending_delete(&editor);
            mark_wifi_dirty(&ui_dirty);
        });
    }
}

fn current_ssid_configs(editors: &Rc<RefCell<Vec<SsidEditor>>>) -> Vec<SsidConfig> {
    editors
        .borrow()
        .iter()
        .filter(|editor| !editor.pending_delete.get())
        .map(SsidEditor::to_config)
        .collect()
}

fn mark_ssid_pending_delete(editor: &SsidEditor) {
    editor.pending_delete.set(true);
    editor.editing.set(false);
    editor.root.add_css_class("pending-delete");
    editor
        .title_label
        .set_markup(&strikethrough_markup(&editor.name.text()));
    editor
        .summary
        .set_markup(&strikethrough_markup(&editor.summary.text()));
    editor.title_label.set_visible(true);
    editor.summary.set_visible(true);
    editor.name.set_visible(false);
    editor.delete_button.set_visible(false);
    editor.edit_button.set_visible(false);
    editor.grid.set_visible(false);
    editor.options.set_visible(false);
}

fn strikethrough_markup(text: &str) -> String {
    format!(
        "<span foreground=\"#c62828\" strikethrough=\"true\">{}</span>",
        glib::markup_escape_text(text)
    )
}

fn new_ssid_config(template: Option<&SsidConfig>) -> SsidConfig {
    SsidConfig {
        two_gig_ssid: Some(true),
        five_gig_ssid: Some(true),
        six_gig_ssid: Some(false),
        encryption_mode: template
            .and_then(|ssid| ssid.encryption_mode.clone())
            .or_else(|| Some("WPA".to_string())),
        encryption_version: template
            .and_then(|ssid| ssid.encryption_version.clone())
            .or_else(|| Some("WPA2".to_string())),
        guest: Some(false),
        is_broadcast_enabled: Some(true),
        ssid_name: Some(String::new()),
        wpa_key: Some(String::new()),
        extra: serde_json::Map::new(),
    }
}

fn ssid_summary_text(ssid: &SsidConfig) -> String {
    let mut parts = Vec::new();
    if ssid.two_gig_ssid.unwrap_or(false) {
        parts.push("2.4 GHz");
    }
    if ssid.five_gig_ssid.unwrap_or(false) {
        parts.push("5 GHz");
    }
    if ssid.six_gig_ssid.unwrap_or(false) {
        parts.push("6 GHz");
    }
    if ssid.guest.unwrap_or(false) {
        parts.push("Guest");
    }
    parts.join(" · ")
}

fn ssid_title(index: usize, ssid: &SsidConfig) -> String {
    ssid.ssid_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| format!("SSID {}", index + 1))
}

fn ssid_editor_row(index: usize, ssid: &SsidConfig) -> (GtkBox, SsidEditor) {
    let root = GtkBox::new(Orientation::Vertical, 8);
    root.set_margin_top(12);
    root.set_margin_bottom(12);
    root.set_margin_start(12);
    root.set_margin_end(12);

    let title = ssid_title(index, ssid);
    let header = GtkBox::new(Orientation::Horizontal, 8);
    let title_label = Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .build();
    header.append(&title_label);
    let edit_button = Button::with_label("Edit");
    let delete_button = Button::with_label("Delete");
    delete_button.add_css_class("destructive-action");
    let name = Entry::builder()
        .text(ssid.ssid_name.as_deref().unwrap_or_default())
        .hexpand(true)
        .build();
    header.append(&name);
    header.append(&edit_button);
    header.append(&delete_button);
    root.append(&header);

    let summary = Label::builder()
        .label(ssid_summary_text(ssid))
        .xalign(0.0)
        .build();
    summary.add_css_class("dim-label");
    root.append(&summary);

    let grid = Grid::builder().row_spacing(8).column_spacing(10).build();
    let key = PasswordEntry::builder()
        .text(ssid.wpa_key.as_deref().unwrap_or_default())
        .hexpand(true)
        .build();
    key.set_show_peek_icon(true);

    grid.attach(
        &Label::builder().label("Password").xalign(0.0).build(),
        0,
        0,
        1,
        1,
    );
    grid.attach(&key, 1, 0, 1, 1);

    let qr = DrawingArea::builder()
        .width_request(1)
        .height_request(1)
        .halign(Align::Center)
        .valign(Align::Center)
        .build();
    let qr_button = Button::with_label("Show QR");
    qr_button.set_tooltip_text(Some("Show Wi-Fi QR code"));
    header.append(&qr_button);
    root.append(&grid);

    let options = GtkBox::new(Orientation::Horizontal, 10);
    let broadcast = CheckButton::builder()
        .label("Broadcast")
        .active(ssid.is_broadcast_enabled.unwrap_or(false))
        .build();
    let two = CheckButton::builder()
        .label("2.4 GHz")
        .active(ssid.two_gig_ssid.unwrap_or(false))
        .build();
    let five = CheckButton::builder()
        .label("5 GHz")
        .active(ssid.five_gig_ssid.unwrap_or(false))
        .build();
    let six = CheckButton::builder()
        .label("6 GHz")
        .active(ssid.six_gig_ssid.unwrap_or(false))
        .build();
    let guest = CheckButton::builder()
        .label("Guest")
        .active(ssid.guest.unwrap_or(false))
        .build();
    for option in [&broadcast, &two, &five, &six, &guest] {
        options.append(option);
    }
    root.append(&options);
    let starts_editing = ssid.ssid_name.as_deref().unwrap_or_default().is_empty();
    title_label.set_visible(!starts_editing);
    name.set_visible(starts_editing);
    delete_button.set_visible(starts_editing);
    grid.set_visible(starts_editing);
    options.set_visible(starts_editing);
    summary.set_visible(!starts_editing);
    edit_button.set_label(if starts_editing { "Done" } else { "Edit" });

    let editing_state = Rc::new(Cell::new(starts_editing));
    let pending_delete = Rc::new(Cell::new(false));
    let grid_for_edit = grid.clone();
    let options_for_edit = options.clone();
    let summary_for_edit = summary.clone();
    let title_for_edit = title_label.clone();
    let name_for_edit = name.clone();
    let delete_for_edit = delete_button.clone();
    let editing_for_edit = editing_state.clone();
    edit_button.connect_clicked(move |button| {
        let editing = !grid_for_edit.is_visible();
        editing_for_edit.set(editing);
        title_for_edit.set_visible(!editing);
        name_for_edit.set_visible(editing);
        delete_for_edit.set_visible(editing);
        grid_for_edit.set_visible(editing);
        options_for_edit.set_visible(editing);
        summary_for_edit.set_visible(!editing);
        button.set_label(if editing { "Done" } else { "Edit" });
    });

    let editor = SsidEditor {
        root: root.clone(),
        title_label,
        summary,
        edit_button,
        name,
        key,
        grid,
        options,
        qr,
        qr_button,
        delete_button,
        editing: editing_state,
        pending_delete,
        broadcast,
        two,
        five,
        six,
        guest,
        encryption_mode: Rc::new(RefCell::new(
            ssid.encryption_mode.clone().unwrap_or_default(),
        )),
        encryption_version: Rc::new(RefCell::new(
            ssid.encryption_version.clone().unwrap_or_default(),
        )),
        extra: Rc::new(RefCell::new(ssid.extra.clone())),
    };
    editor.install_qr_redraw();

    (root, editor)
}

impl SsidEditor {
    fn install_qr_redraw(&self) {
        let name = self.name.clone();
        let key = self.key.clone();
        let broadcast = self.broadcast.clone();
        self.qr.set_draw_func(move |_, ctx, width, height| {
            draw_wifi_qr(
                ctx,
                width as f64,
                height as f64,
                &name.text(),
                &key.text(),
                !broadcast.is_active(),
            );
        });

        let qr = self.qr.clone();
        self.name.connect_changed(move |_| qr.queue_draw());
        let qr = self.qr.clone();
        self.key.connect_changed(move |_| qr.queue_draw());
        let qr = self.qr.clone();
        self.broadcast.connect_toggled(move |_| qr.queue_draw());

        let name = self.name.clone();
        let key = self.key.clone();
        let broadcast = self.broadcast.clone();
        let button = self.qr_button.clone();
        self.qr_button.connect_clicked(move |_| {
            show_wifi_qr_window(&button, &name.text(), &key.text(), !broadcast.is_active());
        });
    }

    fn to_config(&self) -> SsidConfig {
        SsidConfig {
            two_gig_ssid: Some(self.two.is_active()),
            five_gig_ssid: Some(self.five.is_active()),
            six_gig_ssid: Some(self.six.is_active()),
            encryption_mode: non_empty(self.encryption_mode.borrow().clone()),
            encryption_version: non_empty(self.encryption_version.borrow().clone()),
            guest: Some(self.guest.is_active()),
            is_broadcast_enabled: Some(self.broadcast.is_active()),
            ssid_name: Some(self.name.text().to_string()),
            wpa_key: Some(self.key.text().to_string()),
            extra: self.extra.borrow().clone(),
        }
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn show_wifi_qr_window(parent: &Button, ssid: &str, key: &str, hidden: bool) {
    let qr = DrawingArea::builder()
        .width_request(320)
        .height_request(320)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    let ssid = ssid.to_string();
    let key = key.to_string();
    qr.set_draw_func(move |_, ctx, width, height| {
        draw_wifi_qr(ctx, width as f64, height as f64, &ssid, &key, hidden);
    });

    let dialog = gtk::Dialog::builder()
        .title("Wi-Fi QR Code")
        .modal(true)
        .resizable(false)
        .build();
    dialog.content_area().append(&qr);
    dialog.add_button("Close", ResponseType::Close);
    dialog.connect_response(|dialog, _| dialog.close());
    if let Some(root) = parent
        .root()
        .and_then(|root| root.downcast::<Window>().ok())
    {
        dialog.set_transient_for(Some(&root));
    }
    dialog.present();
}

fn show_reboot_confirmation(parent: &Button, ui: &Ui) {
    let dialog = gtk::Dialog::builder()
        .title("Reboot Gateway")
        .modal(true)
        .resizable(false)
        .build();
    dialog
        .content_area()
        .append(&Label::new(Some("Reboot the gateway now?")));
    dialog.add_button("Cancel", ResponseType::Cancel);
    dialog.add_button("Reboot Gateway", ResponseType::Accept);
    if let Some(root) = parent
        .root()
        .and_then(|root| root.downcast::<Window>().ok())
    {
        dialog.set_transient_for(Some(&root));
    }
    let ui = ui.clone();
    dialog.connect_response(move |dialog, response| {
        if response == ResponseType::Accept {
            ui.set_status("Rebooting gateway...", false);
            ui.send_command(AppCommand::Reboot);
        }
        dialog.close();
    });
    dialog.present();
}

fn export_diagnostics(parent: &Button, ui: &Ui) {
    let chooser = FileChooserNative::builder()
        .title("Export Diagnostics Bundle")
        .action(FileChooserAction::Save)
        .accept_label("Export")
        .modal(true)
        .build();
    chooser.set_current_name("hintcontrol-diagnostics.json");
    if let Some(root) = parent
        .root()
        .and_then(|root| root.downcast::<Window>().ok())
    {
        chooser.set_transient_for(Some(&root));
    }
    let ui = ui.clone();
    chooser.connect_response(move |chooser, response| {
        if response == ResponseType::Accept {
            if let Some(path) = chooser.file().and_then(|file| file.path()) {
                match diagnostics_json(&ui)
                    .and_then(|json| fs::write(&path, json).map_err(|error| error.to_string()))
                {
                    Ok(()) => ui.set_status("Diagnostics bundle exported", true),
                    Err(error) => {
                        ui.set_status(&format!("Diagnostics export failed: {error}"), false)
                    }
                }
            }
        }
        chooser.destroy();
    });
    chooser.show();
}

fn diagnostics_json(ui: &Ui) -> Result<String, String> {
    let state = ui.state.borrow();
    let mut wifi = state.snapshot.wifi.clone();
    if let Some(wifi) = wifi.as_mut() {
        if let Some(ssids) = wifi.ssids.as_mut() {
            for ssid in ssids {
                ssid.wpa_key = None;
            }
        }
    }
    let now = Instant::now();
    serde_json::to_string_pretty(&serde_json::json!({
        "device": state.snapshot.device_summary,
        "connection": state.snapshot.general_summary,
        "sim": state.snapshot.sim_summary,
        "clients": state.snapshot.clients,
        "wifi": wifi,
        "signal": {
            "five_g": export_signal_history(&ui.five_g_graph.history.borrow(), now),
            "lte": export_signal_history(&ui.lte_graph.history.borrow(), now),
        }
    }))
    .map_err(|error| error.to_string())
}

fn export_signal_history(history: &SignalHistory, now: Instant) -> serde_json::Value {
    serde_json::json!({
        "rsrp": history.rsrp.export_values(now),
        "rsrq": history.rsrq.export_values(now),
        "rssi": history.rssi.export_values(now),
        "sinr": history.sinr.export_values(now),
        "cqi": history.cqi.export_values(now),
    })
}

fn draw_wifi_qr(
    ctx: &cairo::Context,
    width: f64,
    height: f64,
    ssid: &str,
    key: &str,
    hidden: bool,
) {
    ctx.set_source_rgb(1.0, 1.0, 1.0);
    let _ = ctx.paint();

    let payload = wifi_qr_payload(ssid, key, hidden);
    let Ok(code) = QrCode::new(payload.as_bytes()) else {
        return;
    };

    let modules = code.width();
    let quiet_zone = 4usize;
    let cells = modules + quiet_zone * 2;
    let cell_size = (width.min(height) / cells as f64).floor().max(1.0);
    let qr_size = cell_size * cells as f64;
    let left = ((width - qr_size) / 2.0).max(0.0);
    let top = ((height - qr_size) / 2.0).max(0.0);

    ctx.set_source_rgb(0.0, 0.0, 0.0);
    for y in 0..modules {
        for x in 0..modules {
            if code[(x, y)] == qrcode::Color::Dark {
                ctx.rectangle(
                    left + (x + quiet_zone) as f64 * cell_size,
                    top + (y + quiet_zone) as f64 * cell_size,
                    cell_size,
                    cell_size,
                );
            }
        }
    }
    let _ = ctx.fill();
}

fn wifi_qr_payload(ssid: &str, key: &str, hidden: bool) -> String {
    format!(
        "WIFI:T:WPA;S:{};P:{};H:{};;",
        escape_wifi_qr(ssid),
        escape_wifi_qr(key),
        if hidden { "true" } else { "false" }
    )
}

fn escape_wifi_qr(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        if matches!(ch, '\\' | ';' | ',' | ':' | '"') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    #[test]
    fn metric_ring_retains_only_the_fixed_sample_window() {
        let mut ring = MetricRing::default();
        let start = Instant::now();

        for index in 0..(MAX_SAMPLES + 5) {
            ring.push(
                start + Duration::from_secs(index as u64),
                Some(index as i64),
            );
        }

        assert_eq!(ring.values.len(), MAX_SAMPLES);
        assert_eq!(ring.values.front().unwrap().value, 5.0);
        assert_eq!(ring.values.back().unwrap().value, (MAX_SAMPLES + 4) as f64);
    }

    #[test]
    fn metric_ring_iter_visible_filters_samples_outside_time_window() {
        let mut ring = MetricRing::default();
        let now = Instant::now();

        ring.push(now - Duration::from_secs(121), Some(-100));
        ring.push(now - Duration::from_secs(120), Some(-90));
        ring.push(now, Some(-80));

        let values: Vec<_> = ring.iter_visible(now).map(|sample| sample.value).collect();
        assert_eq!(values, vec![-90.0, -80.0]);
    }

    #[test]
    fn wifi_qr_payload_escapes_reserved_characters() {
        assert_eq!(
            wifi_qr_payload("Semi;Colon", r#"p:a,s\s""#, false),
            r#"WIFI:T:WPA;S:Semi\;Colon;P:p\:a\,s\\s\";H:false;;"#
        );
    }

    #[test]
    fn new_ssid_config_reuses_security_metadata_from_template() {
        let template = SsidConfig {
            encryption_mode: Some("WPA".to_string()),
            encryption_version: Some("WPA3".to_string()),
            ..SsidConfig::default()
        };

        let config = new_ssid_config(Some(&template));
        assert_eq!(config.encryption_mode.as_deref(), Some("WPA"));
        assert_eq!(config.encryption_version.as_deref(), Some("WPA3"));
        assert_eq!(config.is_broadcast_enabled, Some(true));
    }
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "
		.login-panel { padding: 24px; }
		.title { font-size: 20px; font-weight: 700; }
		.section-heading { font-weight: 700; }
		.dim-label { color: #555; }
		.monospace { font-family: monospace; }
		.metric-chip { padding: 3px 8px; border: 1px solid #ccc; border-radius: 4px; }
		.overview-card {
			padding: 10px;
			border: 1px solid #d8d8d8;
			border-radius: 4px;
			background: #fff;
		}
		.table-header {
			padding: 0 4px 2px 4px;
			border-bottom: 1px solid #d0d0d0;
		}
		.empty-state {
			padding: 12px;
			background: #f7f7f7;
			border: 1px solid #e0e0e0;
			border-radius: 4px;
		}
		.coverage-bar-good {
			background: #496;
		}
		.coverage-bar-bad {
			background: #ccc;
		}
		.enabled-chip {
			color: #1b5e20;
			background: #eef7ee;
			border-color: #9ccc9c;
		}
		.disabled-chip {
			color: #8a1f1f;
			background: #faeeee;
			border-color: #e0aaaa;
		}
		.quality-poor {
			color: #b00020;
			background: #faeeee;
			border-color: #e0aaaa;
		}
		.quality-fair {
			color: #7a4d00;
			background: #fff6df;
			border-color: #e0c478;
		}
		.quality-good {
			color: #1b5e20;
			background: #eef7ee;
			border-color: #9ccc9c;
		}
		.coverage-bar {
			border-radius: 2px;
			min-width: 9px;
		}
		.muted-metric { color: #777; }
		togglebutton.red { color: #c62828; }
		togglebutton.green { color: #2e7d32; }
		togglebutton.blue { color: #1565c0; }
		togglebutton.orange { color: #ef6c00; }
		togglebutton.purple { color: #7b3fb2; }
		popover.copy-context-menu contents {
			padding: 0;
			min-height: 0;
		}
		popover.copy-context-menu box {
			padding: 0;
			margin: 0;
			min-height: 0;
		}
		popover.copy-context-menu box.copy-context-box {
			padding: 0;
			margin: 0;
			min-height: 0;
		}
		popover.copy-context-menu button.copy-context-button {
			margin: 0;
			padding: 5px 10px;
			min-height: 0;
			border-radius: 3px;
			outline-width: 0;
			transition: none;
		}
		popover.copy-context-menu button.copy-context-button:hover {
			background: #e6e6e6;
		}
		popover.copy-context-menu modelbutton {
			margin: 0;
			padding: 6px 12px;
			min-height: 0;
		}
		label.red { color: #c62828; }
		label.green { color: #2e7d32; }
		label.blue { color: #1565c0; }
		label.orange { color: #ef6c00; }
		label.purple { color: #7b3fb2; }
		",
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
