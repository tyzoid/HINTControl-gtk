use crate::backend::{
    client_rows, device_rows, general_rows, radio_rows_from_value, signal_metrics_from_value,
    sim_rows, BootstrapData, ClientRow, GatewayClient, GatewayError, GatewayTimeState, Row,
    Settings, SignalMetrics, Snapshot,
};
use crate::logging;
use crate::models::{CellRoot, GatewayInfo, WifiConfig};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc as tokio_mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataSourceKind {
    Gateway,
    Signal,
    Sim,
    Clients,
    Wifi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum CommandKind {
    Login,
    Logout,
    Refresh,
    SaveWifi,
    Reboot,
    Shutdown,
    SetPollingRate,
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum AppCommand {
    Login {
        gateway_ip: String,
        username: String,
        password: String,
        remember: bool,
    },
    Logout,
    RefreshNow(DataSourceKind),
    RefreshAll,
    SetPollingRate {
        source: DataSourceKind,
        interval: Option<Duration>,
    },
    SaveWifi(Box<WifiConfig>),
    Reboot,
    Shutdown,
}

#[derive(Debug, Clone)]
pub enum DataPayload {
    Gateway(GatewayPayload),
    Signal(SignalPayload),
    Sim(Vec<Row>),
    Clients(Vec<ClientRow>),
    Wifi(Box<WifiConfig>),
}

#[derive(Debug, Clone)]
pub struct GatewayPayload {
    pub device_summary: Vec<Row>,
    pub general_summary: Vec<Row>,
}

#[derive(Debug, Clone)]
pub struct SignalPayload {
    pub lte_summary: Vec<Row>,
    pub five_g_summary: Vec<Row>,
    pub lte_metrics: Option<SignalMetrics>,
    pub five_g_metrics: Option<SignalMetrics>,
}

#[derive(Debug, Clone)]
pub struct InitialData {
    pub settings: Settings,
    pub snapshot: Snapshot,
    pub warning: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum UiEvent {
    DataUpdated {
        source: DataSourceKind,
        generation: u64,
        payload: DataPayload,
    },
    DataError {
        source: DataSourceKind,
        generation: u64,
        error: String,
    },
    LoginSucceeded {
        initial: Box<InitialData>,
    },
    LoggedOut,
    CommandSucceeded {
        command: CommandKind,
        message: String,
    },
    CommandFailed {
        command: CommandKind,
        error: String,
    },
    AuthReauthStarted,
    AuthExpired {
        message: String,
    },
}

#[derive(Clone)]
pub struct AppHandle {
    command_tx: tokio_mpsc::UnboundedSender<AppCommand>,
}

impl AppHandle {
    pub fn send(&self, command: AppCommand) {
        if let Err(error) = self.command_tx.send(command) {
            logging::error(format!("failed to send app command: {error}"));
        }
    }
}

pub struct StartedApp {
    pub handle: AppHandle,
    pub events: mpsc::Receiver<UiEvent>,
    pub settings: Settings,
    pub snapshot: Snapshot,
}

pub fn start(runtime: &tokio::runtime::Runtime) -> StartedApp {
    let settings = Settings::load();
    let snapshot = Snapshot {
        gateway_ip: settings.gateway_ip.clone(),
        username: settings.username.clone(),
        ..Snapshot::default()
    };
    let (command_tx, command_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel();
    let state = AppState::new(settings.clone());
    runtime.spawn(async move {
        Actor::new(state, command_rx, event_tx).run().await;
    });

    StartedApp {
        handle: AppHandle { command_tx },
        events: event_rx,
        settings,
        snapshot,
    }
}

#[derive(Debug, Clone)]
pub struct DataSource<T> {
    pub kind: DataSourceKind,
    cached: Option<T>,
    poll_interval: Option<Duration>,
    next_poll: Instant,
    generation: u64,
}

impl<T> DataSource<T> {
    pub fn new(kind: DataSourceKind) -> Self {
        Self {
            kind,
            cached: None,
            poll_interval: None,
            next_poll: Instant::now(),
            generation: 0,
        }
    }

    pub fn set_poll_interval(&mut self, interval: Option<Duration>) {
        if self.poll_interval == interval {
            return;
        }
        self.poll_interval = interval;
        self.next_poll = Instant::now() + interval.unwrap_or(Duration::from_secs(u64::MAX / 4));
    }

    pub fn update(&mut self, value: T) -> DataUpdate<T>
    where
        T: Clone,
    {
        self.generation += 1;
        self.cached = Some(value.clone());
        if let Some(interval) = self.poll_interval {
            self.advance_next_poll(interval, Instant::now());
        }
        DataUpdate {
            kind: self.kind,
            generation: self.generation,
            value,
        }
    }

    fn advance_next_poll(&mut self, interval: Duration, now: Instant) {
        let mut next_poll = if self.next_poll > now {
            self.next_poll
        } else {
            self.next_poll + interval
        };
        while next_poll <= now {
            next_poll += interval;
        }
        self.next_poll = next_poll;
    }

    fn record_poll_attempt(&mut self) {
        if let Some(interval) = self.poll_interval {
            self.advance_next_poll(interval, Instant::now());
        }
    }

    fn reset_poll_deadline(&mut self) {
        if let Some(interval) = self.poll_interval {
            self.next_poll = Instant::now() + interval;
        }
    }
}

pub struct DataUpdate<T> {
    #[allow(dead_code)]
    pub kind: DataSourceKind,
    pub generation: u64,
    pub value: T,
}

struct Actor {
    state: AppState,
    commands: tokio_mpsc::UnboundedReceiver<AppCommand>,
    events: mpsc::Sender<UiEvent>,
}

impl Actor {
    fn new(
        state: AppState,
        commands: tokio_mpsc::UnboundedReceiver<AppCommand>,
        events: mpsc::Sender<UiEvent>,
    ) -> Self {
        Self {
            state,
            commands,
            events,
        }
    }

    async fn run(mut self) {
        let mut ticker = tokio::time::interval(Duration::from_millis(250));
        loop {
            ticker.tick().await;
            while let Ok(command) = self.commands.try_recv() {
                if matches!(command, AppCommand::Shutdown) {
                    return;
                }
                self.handle_command(command).await;
            }
            for source in self.state.due_sources() {
                self.state.mark_poll_attempt(source);
                self.refresh_source(source).await;
            }
        }
    }

    async fn handle_command(&mut self, command: AppCommand) {
        match command {
            AppCommand::Login {
                gateway_ip,
                username,
                password,
                remember,
            } => match self
                .state
                .login(gateway_ip, username, password, remember)
                .await
            {
                Ok(initial) => self.emit(UiEvent::LoginSucceeded {
                    initial: Box::new(initial),
                }),
                Err(error) => self.emit(UiEvent::CommandFailed {
                    command: CommandKind::Login,
                    error,
                }),
            },
            AppCommand::Logout => {
                self.state.logout();
                self.emit(UiEvent::LoggedOut);
            }
            AppCommand::RefreshNow(source) => self.refresh_source(source).await,
            AppCommand::RefreshAll => self.refresh_all().await,
            AppCommand::SetPollingRate { source, interval } => {
                self.state.set_poll_interval(source, interval);
            }
            AppCommand::SaveWifi(wifi) => match self.state.set_wifi_config(*wifi).await {
                Ok((generation, payload)) => {
                    self.emit(UiEvent::DataUpdated {
                        source: DataSourceKind::Wifi,
                        generation,
                        payload,
                    });
                    self.emit(UiEvent::CommandSucceeded {
                        command: CommandKind::SaveWifi,
                        message: "Wi-Fi saved".to_string(),
                    });
                }
                Err(error) => self.emit(UiEvent::CommandFailed {
                    command: CommandKind::SaveWifi,
                    error,
                }),
            },
            AppCommand::Reboot => match self.state.reboot().await {
                Ok(()) => self.emit(UiEvent::CommandSucceeded {
                    command: CommandKind::Reboot,
                    message: "Reboot requested".to_string(),
                }),
                Err(error) => self.emit(UiEvent::CommandFailed {
                    command: CommandKind::Reboot,
                    error,
                }),
            },
            AppCommand::Shutdown => {}
        }
    }

    async fn refresh_all(&mut self) {
        for source in [
            DataSourceKind::Gateway,
            DataSourceKind::Signal,
            DataSourceKind::Sim,
            DataSourceKind::Clients,
            DataSourceKind::Wifi,
        ] {
            self.refresh_source(source).await;
        }
    }

    async fn refresh_source(&mut self, source: DataSourceKind) {
        match self.state.refresh_source(source).await {
            Ok((generation, payload)) => {
                self.emit(UiEvent::DataUpdated {
                    source,
                    generation,
                    payload,
                });
            }
            Err(error) => {
                if error.to_ascii_lowercase().contains("authentication") {
                    self.emit(UiEvent::AuthExpired {
                        message: error.clone(),
                    });
                }
                let generation = self.state.generation(source);
                self.emit(UiEvent::DataError {
                    source,
                    generation,
                    error,
                });
            }
        }
    }

    fn emit(&self, event: UiEvent) {
        let _ = self.events.send(event);
    }
}

struct AppState {
    settings: Settings,
    client: GatewayClient,
    snapshot: Snapshot,
    gateway_info: Option<GatewayInfo>,
    gateway_time: Option<GatewayTimeState>,
    cached_password: Option<String>,
    reauth_not_before: Option<Instant>,
    gateway: DataSource<GatewayPayload>,
    signal: DataSource<SignalPayload>,
    sim: DataSource<Vec<Row>>,
    clients: DataSource<Vec<ClientRow>>,
    wifi: DataSource<WifiConfig>,
}

impl AppState {
    fn new(settings: Settings) -> Self {
        let client = GatewayClient::new(settings.clone());
        let cached_password = settings.password.clone();
        let snapshot = Snapshot {
            gateway_ip: settings.gateway_ip.clone(),
            username: settings.username.clone(),
            ..Snapshot::default()
        };
        let mut state = Self {
            settings,
            client,
            snapshot,
            gateway_info: None,
            gateway_time: None,
            cached_password,
            reauth_not_before: None,
            gateway: DataSource::new(DataSourceKind::Gateway),
            signal: DataSource::new(DataSourceKind::Signal),
            sim: DataSource::new(DataSourceKind::Sim),
            clients: DataSource::new(DataSourceKind::Clients),
            wifi: DataSource::new(DataSourceKind::Wifi),
        };
        state.set_default_polling();
        state
    }

    fn set_default_polling(&mut self) {
        self.signal.set_poll_interval(Some(Duration::from_secs(5)));
        self.gateway
            .set_poll_interval(Some(Duration::from_secs(5 * 60)));
        self.sim.set_poll_interval(None);
        self.clients
            .set_poll_interval(Some(Duration::from_secs(5 * 60)));
        self.wifi
            .set_poll_interval(Some(Duration::from_secs(10 * 60)));
    }

    async fn login(
        &mut self,
        gateway_ip: String,
        username: String,
        password: String,
        remember: bool,
    ) -> Result<InitialData, String> {
        logging::info(format!("Logging in to {}", gateway_ip));
        self.settings.gateway_ip = gateway_ip;
        self.settings.username = username;
        self.cached_password = Some(password.clone());
        self.reauth_not_before = None;
        self.client = GatewayClient::new(self.settings.clone());
        self.client
            .login(&self.settings.username, &password)
            .await
            .map_err(|error| error.to_string())?;
        let bootstrap = self
            .client
            .bootstrap()
            .await
            .map_err(|error| error.to_string())?;
        self.apply_bootstrap(bootstrap);
        self.reset_poll_deadlines();
        let warning = if remember {
            self.settings.password = Some(password);
            self.settings
                .save()
                .err()
                .map(|error| format!("Logged in, but credentials were not saved: {error}"))
        } else {
            None
        };
        Ok(InitialData {
            settings: self.settings.clone(),
            snapshot: self.snapshot.clone(),
            warning,
        })
    }

    fn logout(&mut self) {
        logging::info("Logging out");
        self.client.clear_auth();
        self.gateway_info = None;
        self.gateway_time = None;
        self.cached_password = None;
        self.reauth_not_before = None;
        self.snapshot.logged_in = false;
        self.snapshot.token_present = false;
    }

    async fn set_wifi_config(&mut self, wifi: WifiConfig) -> Result<(u64, DataPayload), String> {
        logging::info("Saving Wi-Fi settings");
        self.request_with_reauth("Wi-Fi save", |client| {
            let wifi = wifi.clone();
            Box::pin(async move { client.set_wifi_config(wifi).await })
        })
        .await?;
        self.snapshot.wifi = Some(wifi.clone());
        self.snapshot.error = None;
        let update = self.wifi.update(wifi);
        Ok((update.generation, DataPayload::Wifi(Box::new(update.value))))
    }

    async fn reboot(&mut self) -> Result<(), String> {
        logging::info("Rebooting gateway");
        self.request_with_reauth("Gateway reboot", |client| {
            Box::pin(async move { client.reboot().await })
        })
        .await?;
        self.snapshot.error = None;
        Ok(())
    }

    async fn refresh_source(
        &mut self,
        source: DataSourceKind,
    ) -> Result<(u64, DataPayload), String> {
        match source {
            DataSourceKind::Gateway => self.refresh_gateway().await,
            DataSourceKind::Signal => self.refresh_signal().await,
            DataSourceKind::Sim => self.refresh_sim().await,
            DataSourceKind::Clients => self.refresh_clients().await,
            DataSourceKind::Wifi => self.refresh_wifi().await,
        }
    }

    async fn refresh_gateway(&mut self) -> Result<(u64, DataPayload), String> {
        let now = Instant::now();
        let gateway = self
            .request_with_reauth("Gateway refresh", |client| {
                Box::pin(async move { client.gateway_info().await })
            })
            .await?;
        self.gateway_time = gateway
            .time
            .as_ref()
            .map(|time| GatewayTimeState::new(time, now));
        self.gateway_info = Some(gateway.clone());
        let payload = GatewayPayload {
            device_summary: device_rows(gateway.device.as_ref()),
            general_summary: general_rows(
                gateway
                    .signal
                    .as_ref()
                    .and_then(|signal| signal.generic.as_ref()),
                self.gateway_time.as_ref(),
                now,
            ),
        };
        self.snapshot.device_summary = payload.device_summary.clone();
        self.snapshot.general_summary = payload.general_summary.clone();
        self.snapshot.error = None;
        let update = self.gateway.update(payload);
        Ok((update.generation, DataPayload::Gateway(update.value)))
    }

    async fn refresh_signal(&mut self) -> Result<(u64, DataPayload), String> {
        let cell = self
            .request_with_reauth("Signal refresh", |client| {
                Box::pin(async move { client.cell_telemetry().await })
            })
            .await?;
        let payload = signal_payload_from_cell(Some(&cell));
        self.snapshot.lte_summary = payload.lte_summary.clone();
        self.snapshot.five_g_summary = payload.five_g_summary.clone();
        self.snapshot.lte_metrics = payload.lte_metrics.clone();
        self.snapshot.five_g_metrics = payload.five_g_metrics.clone();
        self.snapshot.error = None;
        let update = self.signal.update(payload);
        Ok((update.generation, DataPayload::Signal(update.value)))
    }

    async fn refresh_sim(&mut self) -> Result<(u64, DataPayload), String> {
        let sim = self
            .request_with_reauth("SIM refresh", |client| {
                Box::pin(async move { client.sim_telemetry().await })
            })
            .await?;
        let rows = sim_rows(sim.sim.as_ref());
        self.snapshot.sim_summary = rows.clone();
        self.snapshot.error = None;
        let update = self.sim.update(rows);
        Ok((update.generation, DataPayload::Sim(update.value)))
    }

    async fn refresh_clients(&mut self) -> Result<(u64, DataPayload), String> {
        let clients = self
            .request_with_reauth("Clients refresh", |client| {
                Box::pin(async move { client.clients_telemetry().await })
            })
            .await?;
        let rows = client_rows(clients.clients.as_ref());
        self.snapshot.clients = rows.clone();
        self.snapshot.error = None;
        let update = self.clients.update(rows);
        Ok((update.generation, DataPayload::Clients(update.value)))
    }

    async fn refresh_wifi(&mut self) -> Result<(u64, DataPayload), String> {
        let wifi = self
            .request_with_reauth("Wi-Fi refresh", |client| {
                Box::pin(async move { client.wifi_config().await })
            })
            .await?;
        self.snapshot.wifi = Some(wifi.clone());
        self.snapshot.error = None;
        let update = self.wifi.update(wifi);
        Ok((update.generation, DataPayload::Wifi(Box::new(update.value))))
    }

    fn apply_bootstrap(&mut self, bootstrap: BootstrapData) {
        let now = Instant::now();
        self.gateway_info = Some(bootstrap.gateway.clone());
        self.gateway_time = bootstrap
            .gateway
            .time
            .as_ref()
            .map(|time| GatewayTimeState::new(time, now));
        self.snapshot.logged_in = true;
        self.snapshot.token_present = true;
        self.snapshot.gateway_ip = self.settings.gateway_ip.clone();
        self.snapshot.username = self.settings.username.clone();
        self.snapshot.device_summary = device_rows(bootstrap.gateway.device.as_ref());
        self.snapshot.general_summary = general_rows(
            bootstrap
                .gateway
                .signal
                .as_ref()
                .and_then(|signal| signal.generic.as_ref()),
            self.gateway_time.as_ref(),
            now,
        );
        let signal = signal_payload_from_cell(bootstrap.cell.as_ref());
        self.snapshot.lte_summary = signal.lte_summary.clone();
        self.snapshot.five_g_summary = signal.five_g_summary.clone();
        self.snapshot.lte_metrics = signal.lte_metrics.clone();
        self.snapshot.five_g_metrics = signal.five_g_metrics.clone();
        self.snapshot.sim_summary =
            sim_rows(bootstrap.sim.as_ref().and_then(|sim| sim.sim.as_ref()));
        self.snapshot.clients = client_rows(
            bootstrap
                .clients
                .as_ref()
                .and_then(|clients| clients.clients.as_ref()),
        );
        self.snapshot.wifi = bootstrap.wifi;
        self.snapshot.error = None;
    }

    async fn request_with_reauth<T, F>(&mut self, action: &str, op: F) -> Result<T, String>
    where
        F: for<'a> Fn(
            &'a GatewayClient,
        )
            -> Pin<Box<dyn Future<Output = Result<T, GatewayError>> + Send + 'a>>,
    {
        let now = Instant::now();
        if let Some(until) = self.reauth_not_before {
            if now < until {
                let message = format!(
                    "{action} failed: gateway authentication is still expired; retrying after {} seconds",
                    until.duration_since(now).as_secs().max(1)
                );
                logging::debug(format!("Skipping reauth for {action}; {message}"));
                self.mark_auth_expired(&message);
                return Err(message);
            }
        }

        match op(&self.client).await {
            Ok(result) => {
                self.reauth_not_before = None;
                Ok(result)
            }
            Err(error) if error.status() == Some(reqwest::StatusCode::UNAUTHORIZED) => {
                logging::error(format!("{action} failed with 401: {error}"));
                self.reauthenticate_and_retry(action, op).await
            }
            Err(error) => {
                self.snapshot.error = None;
                Err(error.to_string())
            }
        }
    }

    async fn reauthenticate_and_retry<T, F>(&mut self, action: &str, op: F) -> Result<T, String>
    where
        F: for<'a> Fn(
            &'a GatewayClient,
        )
            -> Pin<Box<dyn Future<Output = Result<T, GatewayError>> + Send + 'a>>,
    {
        let Some(password) = self.cached_password.clone() else {
            self.reauth_not_before = Some(Instant::now() + Duration::from_secs(20));
            let message = format!("{action} failed: no cached credentials are available");
            self.mark_auth_expired(&message);
            return Err(message);
        };

        logging::info("Gateway authentication expired; reauthenticating");
        if let Err(error) = self
            .client
            .login(&self.settings.username, &password)
            .await
            .map_err(|error| error.to_string())
        {
            self.reauth_not_before = Some(Instant::now() + Duration::from_secs(20));
            logging::error(format!("Reauthentication failed: {error}"));
            self.mark_auth_expired(&error);
            return Err(error);
        }

        match op(&self.client).await {
            Ok(result) => {
                self.reauth_not_before = None;
                Ok(result)
            }
            Err(error) if error.status() == Some(reqwest::StatusCode::UNAUTHORIZED) => {
                self.reauth_not_before = Some(Instant::now() + Duration::from_secs(20));
                logging::error(format!(
                    "{action} still failed with 401 after reauthentication: {error}"
                ));
                let message = format!("{action} failed: gateway authentication is still expired");
                self.mark_auth_expired(&message);
                Err(message)
            }
            Err(error) => {
                self.snapshot.error = None;
                Err(error.to_string())
            }
        }
    }

    fn mark_auth_expired(&mut self, message: &str) {
        self.snapshot.error = Some(message.to_string());
        self.snapshot.lte_metrics = None;
        self.snapshot.five_g_metrics = None;
    }

    fn set_poll_interval(&mut self, source: DataSourceKind, interval: Option<Duration>) {
        self.source_mut(source).set_poll_interval(interval);
    }

    fn mark_poll_attempt(&mut self, source: DataSourceKind) {
        self.source_mut(source).record_poll_attempt();
    }

    fn reset_poll_deadlines(&mut self) {
        for source in [
            DataSourceKind::Gateway,
            DataSourceKind::Signal,
            DataSourceKind::Sim,
            DataSourceKind::Clients,
            DataSourceKind::Wifi,
        ] {
            self.source_mut(source).reset_poll_deadline();
        }
    }

    fn due_sources(&self) -> Vec<DataSourceKind> {
        let now = Instant::now();
        self.sources()
            .into_iter()
            .filter_map(|(_, (kind, interval, next_poll))| {
                interval.and_then(|_| (now >= next_poll && self.snapshot.logged_in).then_some(kind))
            })
            .collect()
    }

    fn generation(&self, source: DataSourceKind) -> u64 {
        match source {
            DataSourceKind::Gateway => self.gateway.generation,
            DataSourceKind::Signal => self.signal.generation,
            DataSourceKind::Sim => self.sim.generation,
            DataSourceKind::Clients => self.clients.generation,
            DataSourceKind::Wifi => self.wifi.generation,
        }
    }

    fn sources(&self) -> HashMap<DataSourceKind, (DataSourceKind, Option<Duration>, Instant)> {
        HashMap::from([
            (
                DataSourceKind::Gateway,
                (
                    DataSourceKind::Gateway,
                    self.gateway.poll_interval,
                    self.gateway.next_poll,
                ),
            ),
            (
                DataSourceKind::Signal,
                (
                    DataSourceKind::Signal,
                    self.signal.poll_interval,
                    self.signal.next_poll,
                ),
            ),
            (
                DataSourceKind::Sim,
                (
                    DataSourceKind::Sim,
                    self.sim.poll_interval,
                    self.sim.next_poll,
                ),
            ),
            (
                DataSourceKind::Clients,
                (
                    DataSourceKind::Clients,
                    self.clients.poll_interval,
                    self.clients.next_poll,
                ),
            ),
            (
                DataSourceKind::Wifi,
                (
                    DataSourceKind::Wifi,
                    self.wifi.poll_interval,
                    self.wifi.next_poll,
                ),
            ),
        ])
    }

    fn source_mut(&mut self, source: DataSourceKind) -> &mut dyn PollSource {
        match source {
            DataSourceKind::Gateway => &mut self.gateway,
            DataSourceKind::Signal => &mut self.signal,
            DataSourceKind::Sim => &mut self.sim,
            DataSourceKind::Clients => &mut self.clients,
            DataSourceKind::Wifi => &mut self.wifi,
        }
    }
}

trait PollSource {
    fn set_poll_interval(&mut self, interval: Option<Duration>);
    fn record_poll_attempt(&mut self);
    fn reset_poll_deadline(&mut self);
}

impl<T> PollSource for DataSource<T> {
    fn set_poll_interval(&mut self, interval: Option<Duration>) {
        DataSource::set_poll_interval(self, interval);
    }

    fn record_poll_attempt(&mut self) {
        DataSource::record_poll_attempt(self);
    }

    fn reset_poll_deadline(&mut self) {
        DataSource::reset_poll_deadline(self);
    }
}

fn signal_payload_from_cell(cell: Option<&CellRoot>) -> SignalPayload {
    let four_g = cell
        .and_then(|cell| cell.cell.as_ref())
        .and_then(|cell| cell.four_g.as_ref());
    let five_g = cell
        .and_then(|cell| cell.cell.as_ref())
        .and_then(|cell| cell.five_g.as_ref());
    SignalPayload {
        lte_summary: radio_rows_from_value(four_g),
        five_g_summary: radio_rows_from_value(five_g),
        lte_metrics: signal_metrics_from_value(four_g),
        five_g_metrics: signal_metrics_from_value(five_g),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datasource_updates_generation_and_poll_interval() {
        let mut source = DataSource::new(DataSourceKind::Clients);
        source.set_poll_interval(Some(Duration::from_secs(30)));
        let first = source.update(vec![Row {
            label: "A".to_string(),
            value: "B".to_string(),
        }]);
        assert_eq!(first.generation, 1);
        let second = source.update(Vec::<Row>::new());
        assert_eq!(second.generation, 2);
        assert_eq!(source.poll_interval, Some(Duration::from_secs(30)));
    }

    #[test]
    fn datasource_can_disable_polling() {
        let mut source = DataSource::<Vec<Row>>::new(DataSourceKind::Sim);
        source.set_poll_interval(None);
        assert_eq!(source.poll_interval, None);
    }

    #[test]
    fn datasource_advances_from_scheduled_deadline_without_drift() {
        let mut source = DataSource::<Vec<Row>>::new(DataSourceKind::Signal);
        let interval = Duration::from_secs(5);
        let deadline = Instant::now() - Duration::from_millis(80);
        source.poll_interval = Some(interval);
        source.next_poll = deadline;

        source.update(Vec::new());

        assert!(source.next_poll >= deadline + interval);
        assert!(source.next_poll < Instant::now() + interval);
    }

    #[test]
    fn datasource_keeps_future_deadline_after_early_update() {
        let mut source = DataSource::<Vec<Row>>::new(DataSourceKind::Wifi);
        let deadline = Instant::now() + Duration::from_secs(60);
        source.poll_interval = Some(Duration::from_secs(60));
        source.next_poll = deadline;

        source.update(Vec::new());

        assert_eq!(source.next_poll, deadline);
    }

    #[test]
    fn datasource_records_failed_poll_attempt_without_waiting_for_update() {
        let mut source = DataSource::<Vec<Row>>::new(DataSourceKind::Signal);
        let interval = Duration::from_secs(5);
        let deadline = Instant::now() - Duration::from_millis(50);
        source.poll_interval = Some(interval);
        source.next_poll = deadline;

        source.record_poll_attempt();

        assert!(source.next_poll >= deadline + interval);
        assert!(source.next_poll > Instant::now());
    }

    #[test]
    fn datasource_keeps_deadline_when_poll_interval_is_unchanged() {
        let mut source = DataSource::<Vec<Row>>::new(DataSourceKind::Signal);
        source.set_poll_interval(Some(Duration::from_secs(5)));
        let deadline = source.next_poll;

        source.set_poll_interval(Some(Duration::from_secs(5)));

        assert_eq!(source.next_poll, deadline);
    }

    #[test]
    fn datasource_can_explicitly_reset_deadline_after_login() {
        let mut source = DataSource::<Vec<Row>>::new(DataSourceKind::Signal);
        source.poll_interval = Some(Duration::from_secs(5));
        source.next_poll = Instant::now() - Duration::from_secs(60);

        source.reset_poll_deadline();

        assert!(source.next_poll > Instant::now());
        assert!(source.next_poll <= Instant::now() + Duration::from_secs(5));
    }
}
