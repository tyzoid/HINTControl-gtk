mod backend;
mod models;

use backend::{GatewayClient, Row, Settings, SignalMetrics, Snapshot};
use gtk::cairo;
use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, CheckButton, DrawingArea, Entry,
    Grid, Label, ListBox, ListBoxRow, Orientation, PasswordEntry, ScrolledWindow, Stack,
    ToggleButton, Window,
};
use models::{SsidConfig, WifiConfig};
use qrcode::QrCode;
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::f64;
use std::rc::Rc;
use std::time::{Duration, Instant};

const APP_ID: &str = "dev.zwander.hintcontrol.gtk";
const HISTORY_SECONDS: f64 = 120.0;
const MAX_SAMPLES: usize = 32;

struct AppState {
    settings: Settings,
    client: GatewayClient,
    snapshot: Snapshot,
}

impl AppState {
    fn load() -> Self {
        let settings = Settings::load();
        let client = GatewayClient::new(settings.clone());
        let snapshot = Snapshot {
            gateway_ip: settings.gateway_ip.clone(),
            username: settings.username.clone(),
            ..Snapshot::default()
        };
        Self {
            settings,
            client,
            snapshot,
        }
    }

    fn login(
        &mut self,
        gateway_ip: String,
        username: String,
        password: String,
        remember: bool,
    ) -> Result<(), String> {
        self.settings.gateway_ip = gateway_ip;
        self.settings.username = username;
        self.client = GatewayClient::new(self.settings.clone());
        self.client
            .login(&self.settings.username, &password)
            .and_then(|_| self.client.refresh_all())
            .map(|snapshot| {
                if remember {
                    self.settings.password = Some(password);
                    let _ = self.settings.save();
                }
                self.snapshot = snapshot;
            })
            .map_err(|error| error.to_string())
    }

    fn refresh(&mut self) -> Result<(), String> {
        self.client
            .refresh_all()
            .map(|snapshot| {
                self.snapshot = preserve_optional_snapshot_data(snapshot, &self.snapshot);
            })
            .map_err(|error| error.to_string())
    }

    fn set_wifi_config(&mut self, wifi: WifiConfig) -> Result<(), String> {
        self.client
            .set_wifi_config(wifi)
            .map(|snapshot| {
                self.snapshot = preserve_optional_snapshot_data(snapshot, &self.snapshot);
            })
            .map_err(|error| error.to_string())
    }

    fn reboot(&self) -> Result<(), String> {
        self.client.reboot().map_err(|error| error.to_string())
    }

    fn logout(&mut self) {
        self.client.clear_auth();
        self.snapshot.logged_in = false;
        self.snapshot.token_present = false;
    }
}

fn preserve_optional_snapshot_data(mut next: Snapshot, previous: &Snapshot) -> Snapshot {
    if next.sim_summary.is_empty() && !previous.sim_summary.is_empty() {
        next.sim_summary = previous.sim_summary.clone();
    }
    if next.clients.is_empty() && !previous.clients.is_empty() {
        next.clients = previous.clients.clone();
    }
    if next.wifi.is_none() {
        next.wifi = previous.wifi.clone();
    }
    next
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
    rsrp: CheckButton,
    rsrq: CheckButton,
    rssi: CheckButton,
    sinr: CheckButton,
    cqi: CheckButton,
    history: Rc<RefCell<SignalHistory>>,
}

impl SignalGraph {
    fn new(title: &str, disconnected_text: &str) -> (GtkBox, Self) {
        let root = GtkBox::new(Orientation::Vertical, 6);
        root.append(&Label::builder().label(title).xalign(0.0).build());

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

        let toggles = GtkBox::new(Orientation::Horizontal, 10);
        let rsrp = metric_toggle("RSRP", "red");
        let rsrq = metric_toggle("RSRQ", "green");
        let rssi = metric_toggle("RSSI", "blue");
        let sinr = metric_toggle("SiNR", "orange");
        let cqi = metric_toggle("CQI", "purple");
        toggles.append(&rsrp);
        toggles.append(&rsrq);
        toggles.append(&rssi);
        toggles.append(&sinr);
        toggles.append(&cqi);

        root.append(&area);
        root.append(&warning);
        root.append(&toggles);

        let graph = Self {
            area,
            warning,
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
            toggle.connect_toggled(move |_| area.queue_draw());
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

#[derive(Clone)]
struct Ui {
    state: Rc<RefCell<AppState>>,
    stack: Stack,
    status: Label,
    gateway_ip: Entry,
    username: Entry,
    password: PasswordEntry,
    remember: CheckButton,
    nav_signal: ToggleButton,
    nav_details: ToggleButton,
    nav_clients: ToggleButton,
    nav_wifi: ToggleButton,
    content_stack: Stack,
    five_g_graph: SignalGraph,
    lte_graph: SignalGraph,
    device_list: ListBox,
    general_list: ListBox,
    sim_list: ListBox,
    clients_list: ListBox,
    two_radio: CheckButton,
    five_radio: CheckButton,
    six_radio: CheckButton,
    ssid_list: ListBox,
    ssid_editors: Rc<RefCell<Vec<SsidEditor>>>,
    add_ssid: Button,
}

impl Ui {
    fn refresh(&self, show_status: bool) {
        if show_status {
            self.status.set_text("Refreshing...");
        }

        let result = self.state.borrow_mut().refresh();
        match result {
            Ok(()) => {
                if show_status {
                    self.status.set_text("Refreshed");
                }
                self.apply_state(true);
            }
            Err(error) => {
                self.status.set_text(&error);
            }
        }
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

        fill_rows(&self.device_list, &snapshot.device_summary);
        fill_rows(&self.general_list, &snapshot.general_summary);
        fill_rows(&self.sim_list, &snapshot.sim_summary);
        fill_clients(&self.clients_list, &snapshot.clients);
        apply_wifi(
            &self.two_radio,
            &self.five_radio,
            &self.six_radio,
            &self.ssid_list,
            &self.ssid_editors,
            snapshot,
        );

        if snapshot.logged_in && record_signal_sample {
            let now = Instant::now();
            self.five_g_graph
                .push(now, snapshot.five_g_metrics.as_ref());
            self.lte_graph.push(now, snapshot.lte_metrics.as_ref());
        }
    }
}

fn main() {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &Application) {
    install_css();

    let state = Rc::new(RefCell::new(AppState::load()));
    let window = ApplicationWindow::builder()
        .application(app)
        .title("HINT Control")
        .default_width(980)
        .default_height(680)
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

    let status = Label::builder().xalign(0.0).label("Ready").build();
    status.add_css_class("dim-label");
    root.append(&status);

    let login = build_login_page();
    stack.add_named(&login.root, Some("login"));

    let details = build_details_page();
    stack.add_named(&details.root, Some("details"));

    window.set_child(Some(&root));

    let ui = Ui {
        state,
        stack,
        status,
        gateway_ip: login.gateway_ip,
        username: login.username,
        password: login.password,
        remember: login.remember,
        nav_signal: details.nav_signal,
        nav_details: details.nav_details,
        nav_clients: details.nav_clients,
        nav_wifi: details.nav_wifi,
        content_stack: details.content_stack,
        five_g_graph: details.five_g_graph,
        lte_graph: details.lte_graph,
        device_list: details.device_list,
        general_list: details.general_list,
        sim_list: details.sim_list,
        clients_list: details.clients_list,
        two_radio: details.two_radio,
        five_radio: details.five_radio,
        six_radio: details.six_radio,
        ssid_list: details.ssid_list,
        ssid_editors: details.ssid_editors,
        add_ssid: details.add_ssid,
    };

    wire_actions(
        &ui,
        login.login_button,
        details.refresh,
        details.save_wifi,
        details.reboot,
        details.logout,
    );
    wire_navigation(&ui);
    ui.apply_state(false);

    let ui_for_timer = ui.clone();
    glib::timeout_add_local(Duration::from_secs(5), move || {
        if ui_for_timer.state.borrow().snapshot.logged_in {
            ui_for_timer.refresh(false);
        }
        glib::ControlFlow::Continue
    });

    window.present();
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

    let gateway_ip = Entry::builder().text("192.168.12.1").hexpand(true).build();
    let username = Entry::builder().text("admin").hexpand(true).build();
    let password = PasswordEntry::builder().hexpand(true).build();
    let remember = CheckButton::builder()
        .label("Remember credentials")
        .active(true)
        .build();
    let login_button = Button::with_label("Log In");
    login_button.add_css_class("suggested-action");

    panel.attach(
        &Label::builder()
            .label("Gateway address")
            .xalign(0.0)
            .build(),
        0,
        0,
        1,
        1,
    );
    panel.attach(&gateway_ip, 1, 0, 1, 1);
    panel.attach(
        &Label::builder().label("Username").xalign(0.0).build(),
        0,
        1,
        1,
        1,
    );
    panel.attach(&username, 1, 1, 1, 1);
    panel.attach(
        &Label::builder().label("Password").xalign(0.0).build(),
        0,
        2,
        1,
        1,
    );
    panel.attach(&password, 1, 2, 1, 1);
    panel.attach(&remember, 1, 3, 1, 1);
    panel.attach(&login_button, 1, 4, 1, 1);

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
    nav_signal: ToggleButton,
    nav_details: ToggleButton,
    nav_clients: ToggleButton,
    nav_wifi: ToggleButton,
    content_stack: Stack,
    five_g_graph: SignalGraph,
    lte_graph: SignalGraph,
    device_list: ListBox,
    general_list: ListBox,
    sim_list: ListBox,
    clients_list: ListBox,
    two_radio: CheckButton,
    five_radio: CheckButton,
    six_radio: CheckButton,
    ssid_list: ListBox,
    ssid_editors: Rc<RefCell<Vec<SsidEditor>>>,
    add_ssid: Button,
    refresh: Button,
    save_wifi: Button,
    reboot: Button,
    logout: Button,
}

fn build_details_page() -> DetailsPage {
    let root = GtkBox::new(Orientation::Vertical, 8);
    let nav = GtkBox::new(Orientation::Horizontal, 0);
    nav.add_css_class("linked");

    let nav_signal = ToggleButton::with_label("Signal");
    let nav_details = ToggleButton::with_label("Device Details");
    let nav_clients = ToggleButton::with_label("Clients");
    let nav_wifi = ToggleButton::with_label("Wi-Fi");
    nav_signal.set_active(true);
    for button in [&nav_signal, &nav_clients, &nav_wifi, &nav_details] {
        nav.append(button);
    }
    root.append(&nav);

    let content_stack = Stack::new();
    content_stack.set_vexpand(true);
    root.append(&content_stack);

    let (signal_page, five_g_graph, lte_graph) = build_signal_page();
    content_stack.add_named(&signal_page, Some("signal"));

    let (details_page, device_list, general_list, sim_list) = build_device_details_page();
    content_stack.add_named(&details_page, Some("details"));

    let clients_list = ListBox::new();
    let clients_scroll = scrolled(&clients_list);
    content_stack.add_named(&clients_scroll, Some("clients"));

    let (wifi_page, two_radio, five_radio, six_radio, ssid_list, ssid_editors, add_ssid, save_wifi) =
        build_wifi_page();
    content_stack.add_named(&wifi_page, Some("wifi"));

    let actions = GtkBox::new(Orientation::Horizontal, 8);
    let refresh = Button::with_label("Refresh");
    let reboot = Button::with_label("Reboot Gateway");
    let logout = Button::with_label("Log Out");
    actions.append(&refresh);
    let spacer = GtkBox::new(Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    actions.append(&spacer);
    actions.append(&reboot);
    actions.append(&logout);
    root.append(&actions);

    DetailsPage {
        root,
        nav_signal,
        nav_details,
        nav_clients,
        nav_wifi,
        content_stack,
        five_g_graph,
        lte_graph,
        device_list,
        general_list,
        sim_list,
        clients_list,
        two_radio,
        five_radio,
        six_radio,
        ssid_list,
        ssid_editors,
        add_ssid,
        refresh,
        save_wifi,
        reboot,
        logout,
    }
}

fn build_signal_page() -> (GtkBox, SignalGraph, SignalGraph) {
    let root = GtkBox::new(Orientation::Vertical, 10);
    let (five_g_box, five_g_graph) = SignalGraph::new("5G", "5G network not connected");
    let (lte_box, lte_graph) = SignalGraph::new("LTE", "LTE network not connected");
    root.append(&five_g_box);
    root.append(&lte_box);
    (root, five_g_graph, lte_graph)
}

fn build_device_details_page() -> (ScrolledWindow, ListBox, ListBox, ListBox) {
    let root = GtkBox::new(Orientation::Vertical, 10);
    let device = section_list("Device", &root);
    let general = section_list("General", &root);
    let sim = section_list("SIM", &root);
    (scrolled(&root), device, general, sim)
}

#[derive(Clone)]
struct SsidEditor {
    name: Entry,
    key: PasswordEntry,
    qr: DrawingArea,
    qr_button: Button,
    delete_button: Button,
    broadcast: CheckButton,
    two: CheckButton,
    five: CheckButton,
    six: CheckButton,
    guest: CheckButton,
    encryption_mode: String,
    encryption_version: String,
}

fn build_wifi_page() -> (
    ScrolledWindow,
    CheckButton,
    CheckButton,
    CheckButton,
    ListBox,
    Rc<RefCell<Vec<SsidEditor>>>,
    Button,
    Button,
) {
    let root = GtkBox::new(Orientation::Vertical, 10);
    root.set_margin_bottom(8);

    root.append(
        &Label::builder()
            .label("Radio Bands Enabled")
            .xalign(0.0)
            .build(),
    );
    let radio_options = GtkBox::new(Orientation::Horizontal, 12);
    let two = CheckButton::with_label("2.4 GHz");
    let five = CheckButton::with_label("5 GHz");
    let six = CheckButton::with_label("6 GHz");
    radio_options.append(&two);
    radio_options.append(&five);
    radio_options.append(&six);
    root.append(&radio_options);

    root.append(&Label::builder().label("SSIDs").xalign(0.0).build());
    let ssid_list = ListBox::new();
    ssid_list.add_css_class("boxed-list");
    root.append(&ssid_list);

    let add = Button::with_label("Add SSID");
    root.append(&add);

    let save = Button::with_label("Save Wi-Fi");
    root.append(&save);
    (
        scrolled(&root),
        two,
        five,
        six,
        ssid_list,
        Rc::new(RefCell::new(Vec::new())),
        add,
        save,
    )
}

fn wire_actions(
    ui: &Ui,
    login: Button,
    refresh: Button,
    save_wifi: Button,
    reboot: Button,
    logout: Button,
) {
    let ui_login = ui.clone();
    login.connect_clicked(move |_| {
        ui_login.status.set_text("Logging in...");
        let result = ui_login.state.borrow_mut().login(
            ui_login.gateway_ip.text().to_string(),
            ui_login.username.text().to_string(),
            ui_login.password.text().to_string(),
            ui_login.remember.is_active(),
        );
        match result {
            Ok(()) => {
                ui_login.status.set_text("Logged in");
                ui_login.apply_state(true);
            }
            Err(error) => ui_login.status.set_text(&error),
        }
    });

    let ui_refresh = ui.clone();
    refresh.connect_clicked(move |_| ui_refresh.refresh(true));

    let ui_add_ssid = ui.clone();
    ui.add_ssid.connect_clicked(move |_| {
        let mut ssids = current_ssid_configs(&ui_add_ssid.ssid_editors);
        ssids.push(new_ssid_config(ssids.first()));
        fill_ssids(&ui_add_ssid.ssid_list, &ui_add_ssid.ssid_editors, &ssids);
    });

    let ui_save = ui.clone();
    save_wifi.connect_clicked(move |_| {
        let result = build_wifi_config_from_ui(&ui_save)
            .and_then(|wifi| ui_save.state.borrow_mut().set_wifi_config(wifi));
        match result {
            Ok(()) => {
                ui_save.status.set_text("Wi-Fi saved");
                ui_save.apply_state(true);
            }
            Err(error) => ui_save.status.set_text(&error),
        }
    });

    let ui_reboot = ui.clone();
    reboot.connect_clicked(move |_| match ui_reboot.state.borrow().reboot() {
        Ok(()) => ui_reboot.status.set_text("Reboot requested"),
        Err(error) => ui_reboot.status.set_text(&error),
    });

    let ui_logout = ui.clone();
    logout.connect_clicked(move |_| {
        ui_logout.state.borrow_mut().logout();
        ui_logout.status.set_text("Logged out");
        ui_logout.apply_state(false);
    });
}

fn wire_navigation(ui: &Ui) {
    let buttons = [
        (ui.nav_signal.clone(), "signal"),
        (ui.nav_clients.clone(), "clients"),
        (ui.nav_wifi.clone(), "wifi"),
        (ui.nav_details.clone(), "details"),
    ];

    let changing = Rc::new(Cell::new(false));
    for (button, page) in buttons {
        let ui_nav = ui.clone();
        let changing = changing.clone();
        button.connect_clicked(move |_| {
            if changing.get() {
                return;
            }
            changing.set(true);
            ui_nav.nav_signal.set_active(page == "signal");
            ui_nav.nav_details.set_active(page == "details");
            ui_nav.nav_clients.set_active(page == "clients");
            ui_nav.nav_wifi.set_active(page == "wifi");
            ui_nav.content_stack.set_visible_child_name(page);
            changing.set(false);
        });
    }
}

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

    for index in 1..4 {
        let y = plot_top + plot_height * index as f64 / 4.0;
        ctx.move_to(plot_left, y);
        ctx.line_to(plot_right, y);
        let _ = ctx.stroke();
    }
    for seconds in [30.0, 60.0, 90.0] {
        let x = plot_right - seconds / HISTORY_SECONDS * plot_width;
        ctx.move_to(x, plot_top);
        ctx.line_to(x, plot_bottom);
        let _ = ctx.stroke();
    }

    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    collect_bounds(&history.rsrp, show_rsrp, now, &mut min, &mut max);
    collect_bounds(&history.rsrq, show_rsrq, now, &mut min, &mut max);
    collect_bounds(&history.rssi, show_rssi, now, &mut min, &mut max);
    collect_bounds(&history.sinr, show_sinr, now, &mut min, &mut max);
    collect_bounds(&history.cqi, show_cqi, now, &mut min, &mut max);
    if !min.is_finite() || !max.is_finite() {
        return;
    }
    let padding = ((max - min) * 0.15).max(4.0);
    min -= padding;
    max += padding;
    if (max - min).abs() < 0.01 {
        min -= 1.0;
        max += 1.0;
    }

    draw_metric(
        ctx,
        &history.rsrp,
        show_rsrp,
        now,
        (0.78, 0.16, 0.16),
        min,
        max,
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
        min,
        max,
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
        min,
        max,
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
        min,
        max,
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
        min,
        max,
        plot_left,
        plot_top,
        plot_width,
        plot_height,
    );

    draw_current_value(
        ctx,
        "RSRP",
        &history.rsrp,
        show_rsrp,
        now,
        (0.78, 0.16, 0.16),
        min,
        max,
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
        min,
        max,
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
        min,
        max,
        plot_right,
        plot_top,
        plot_height,
    );
    draw_current_value(
        ctx,
        "SiNR",
        &history.sinr,
        show_sinr,
        now,
        (0.94, 0.42, 0.0),
        min,
        max,
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
        min,
        max,
        plot_right,
        plot_top,
        plot_height,
    );

    ctx.set_source_rgb(0.35, 0.35, 0.35);
    ctx.move_to(4.0, plot_top + 12.0);
    let _ = ctx.show_text(&format!("{max:.0}"));
    ctx.move_to(4.0, plot_bottom - 4.0);
    let _ = ctx.show_text(&format!("{min:.0}"));
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
    for sample in ring.iter_visible(now) {
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

fn collect_bounds(ring: &MetricRing, visible: bool, now: Instant, min: &mut f64, max: &mut f64) {
    if !visible {
        return;
    }
    for sample in ring.iter_visible(now) {
        if sample.value.is_finite() {
            *min = min.min(sample.value);
            *max = max.max(sample.value);
        }
    }
}

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
        current_value_label("SiNR", &history.sinr, show_sinr, now),
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

fn metric_toggle(label: &str, css_class: &str) -> CheckButton {
    let toggle = CheckButton::builder().label(label).active(true).build();
    toggle.add_css_class(css_class);
    toggle
}

fn section_list(title: &str, root: &GtkBox) -> ListBox {
    root.append(&Label::builder().label(title).xalign(0.0).build());
    let list = ListBox::new();
    list.add_css_class("boxed-list");
    root.append(&list);
    list
}

fn scrolled<W: IsA<gtk::Widget>>(child: &W) -> ScrolledWindow {
    ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(child)
        .build()
}

fn fill_rows(list: &ListBox, rows: &[Row]) {
    clear_list(list);
    for row in rows {
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
        let value = Label::builder()
            .label(&row.value)
            .xalign(1.0)
            .selectable(true)
            .build();
        line.append(&label);
        line.append(&value);
        list.append(&line);
    }
}

fn fill_clients(list: &ListBox, clients: &[backend::ClientRow]) {
    clear_list(list);
    for client in clients {
        let line = GtkBox::new(Orientation::Horizontal, 12);
        line.set_margin_top(6);
        line.set_margin_bottom(6);
        line.set_margin_start(8);
        line.set_margin_end(8);
        line.append(
            &Label::builder()
                .label(&client.band)
                .xalign(0.0)
                .width_chars(10)
                .build(),
        );
        line.append(
            &Label::builder()
                .label(&client.name)
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        line.append(
            &Label::builder()
                .label(&client.ip)
                .xalign(0.0)
                .width_chars(16)
                .build(),
        );
        line.append(
            &Label::builder()
                .label(&client.mac)
                .xalign(0.0)
                .width_chars(18)
                .build(),
        );
        list.append(&line);
    }
}

fn clear_list(list: &ListBox) {
    while let Some(row) = list
        .first_child()
        .and_then(|child| child.downcast::<ListBoxRow>().ok())
    {
        list.remove(&row);
    }
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
            .map(SsidEditor::to_config)
            .collect(),
    );
    Ok(wifi)
}

fn apply_wifi(
    two: &CheckButton,
    five: &CheckButton,
    six: &CheckButton,
    ssid_list: &ListBox,
    ssid_editors: &Rc<RefCell<Vec<SsidEditor>>>,
    snapshot: &Snapshot,
) {
    let Some(wifi) = &snapshot.wifi else {
        clear_list(ssid_list);
        ssid_editors.borrow_mut().clear();
        return;
    };
    if let Some(band) = &wifi.two_gig {
        two.set_active(band.is_radio_enabled.unwrap_or(false));
    }
    if let Some(band) = &wifi.five_gig {
        five.set_active(band.is_radio_enabled.unwrap_or(false));
    }
    if let Some(band) = &wifi.six_gig {
        six.set_active(band.is_radio_enabled.unwrap_or(false));
        six.set_visible(true);
    } else {
        six.set_visible(false);
    }

    fill_ssids(
        ssid_list,
        ssid_editors,
        wifi.ssids.as_deref().unwrap_or(&[]),
    );
}

fn fill_ssids(list: &ListBox, editors: &Rc<RefCell<Vec<SsidEditor>>>, ssids: &[SsidConfig]) {
    clear_list(list);
    let mut new_editors = Vec::new();
    for (index, ssid) in ssids.iter().enumerate() {
        let (row, editor) = ssid_editor_row(index, ssid);
        list.append(&row);
        new_editors.push(editor);
    }
    *editors.borrow_mut() = new_editors;
    wire_ssid_delete_buttons(list, editors);
}

fn wire_ssid_delete_buttons(list: &ListBox, editors: &Rc<RefCell<Vec<SsidEditor>>>) {
    let delete_buttons: Vec<_> = editors
        .borrow()
        .iter()
        .map(|editor| editor.delete_button.clone())
        .collect();
    for (index, button) in delete_buttons.into_iter().enumerate() {
        let list = list.clone();
        let editors = editors.clone();
        button.connect_clicked(move |_| {
            let mut ssids = current_ssid_configs(&editors);
            if index < ssids.len() {
                ssids.remove(index);
                fill_ssids(&list, &editors, &ssids);
            }
        });
    }
}

fn current_ssid_configs(editors: &Rc<RefCell<Vec<SsidEditor>>>) -> Vec<SsidConfig> {
    editors.borrow().iter().map(SsidEditor::to_config).collect()
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
    }
}

fn ssid_editor_row(index: usize, ssid: &SsidConfig) -> (GtkBox, SsidEditor) {
    let root = GtkBox::new(Orientation::Vertical, 8);
    root.set_margin_top(8);
    root.set_margin_bottom(8);
    root.set_margin_start(8);
    root.set_margin_end(8);

    let title = ssid
        .ssid_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(|name| format!("SSID {}: {name}", index + 1))
        .unwrap_or_else(|| format!("SSID {}", index + 1));
    let header = GtkBox::new(Orientation::Horizontal, 8);
    header.append(
        &Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .build(),
    );
    let delete_button = Button::with_label("Delete");
    delete_button.add_css_class("destructive-action");
    header.append(&delete_button);
    root.append(&header);

    let grid = Grid::builder().row_spacing(8).column_spacing(10).build();
    let name = Entry::builder()
        .text(ssid.ssid_name.as_deref().unwrap_or_default())
        .hexpand(true)
        .build();
    let key = PasswordEntry::builder()
        .text(ssid.wpa_key.as_deref().unwrap_or_default())
        .hexpand(true)
        .build();

    grid.attach(
        &Label::builder().label("Name").xalign(0.0).build(),
        0,
        0,
        1,
        1,
    );
    grid.attach(&name, 1, 0, 1, 1);
    grid.attach(
        &Label::builder().label("Password").xalign(0.0).build(),
        0,
        1,
        1,
        1,
    );
    grid.attach(&key, 1, 1, 1, 1);

    let qr = DrawingArea::builder()
        .width_request(42)
        .height_request(42)
        .halign(Align::Center)
        .valign(Align::Center)
        .build();
    let qr_button = Button::new();
    qr_button.set_child(Some(&qr));
    qr_button.set_tooltip_text(Some("Show Wi-Fi QR code"));
    grid.attach(&qr_button, 2, 0, 1, 2);
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

    let editor = SsidEditor {
        name,
        key,
        qr,
        qr_button,
        delete_button,
        broadcast,
        two,
        five,
        six,
        guest,
        encryption_mode: ssid.encryption_mode.clone().unwrap_or_default(),
        encryption_version: ssid.encryption_version.clone().unwrap_or_default(),
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
            encryption_mode: non_empty(self.encryption_mode.clone()),
            encryption_version: non_empty(self.encryption_version.clone()),
            guest: Some(self.guest.is_active()),
            is_broadcast_enabled: Some(self.broadcast.is_active()),
            ssid_name: Some(self.name.text().to_string()),
            wpa_key: Some(self.key.text().to_string()),
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

    let window = Window::builder()
        .title("Wi-Fi QR Code")
        .modal(true)
        .resizable(false)
        .child(&qr)
        .build();
    if let Some(root) = parent
        .root()
        .and_then(|root| root.downcast::<Window>().ok())
    {
        window.set_transient_for(Some(&root));
    }
    window.present();
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

    #[test]
    fn refresh_preserves_optional_sections_when_next_snapshot_is_blank() {
        let previous = Snapshot {
            sim_summary: vec![Row {
                label: "SIM".to_string(),
                value: "Ready".to_string(),
            }],
            clients: vec![backend::ClientRow {
                band: "5 GHz".to_string(),
                name: "Laptop".to_string(),
                ip: "192.168.12.10".to_string(),
                mac: "00:11:22:33:44:55".to_string(),
            }],
            wifi: Some(WifiConfig::default()),
            ..Snapshot::default()
        };

        let next = preserve_optional_snapshot_data(Snapshot::default(), &previous);

        assert_eq!(next.sim_summary.len(), 1);
        assert_eq!(next.clients.len(), 1);
        assert!(next.wifi.is_some());
    }
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(
        "
        .login-panel { padding: 18px; }
        .dim-label { color: #555; }
        checkbutton.red { color: #c62828; }
        checkbutton.green { color: #2e7d32; }
        checkbutton.blue { color: #1565c0; }
        checkbutton.orange { color: #ef6c00; }
        checkbutton.purple { color: #7b3fb2; }
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
