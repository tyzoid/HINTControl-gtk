use crate::models::*;
use reqwest::blocking::{Client, Response};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, USER_AGENT};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

const BASE_URL: &str = "TMI/v1";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Settings {
    pub gateway_ip: String,
    pub username: String,
    pub password: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            gateway_ip: "192.168.12.1".to_string(),
            username: "admin".to_string(),
            password: None,
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        fs::read_to_string(Self::path())
            .or_else(|_| fs::read_to_string(Self::legacy_path()))
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    fn path() -> PathBuf {
        let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
        PathBuf::from(home).join(".config/hint-control/settings.json")
    }

    fn legacy_path() -> PathBuf {
        let home = std::env::var_os("HOME").unwrap_or_else(|| ".".into());
        PathBuf::from(home).join(".config/hint-control-qt/settings.json")
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Snapshot {
    pub logged_in: bool,
    pub token_present: bool,
    pub gateway_ip: String,
    pub username: String,
    pub device_summary: Vec<Row>,
    pub lte_summary: Vec<Row>,
    pub five_g_summary: Vec<Row>,
    pub lte_metrics: Option<SignalMetrics>,
    pub five_g_metrics: Option<SignalMetrics>,
    pub general_summary: Vec<Row>,
    pub sim_summary: Vec<Row>,
    pub clients: Vec<ClientRow>,
    pub wifi: Option<WifiConfig>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Row {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClientRow {
    pub band: String,
    pub name: String,
    pub ip: String,
    pub mac: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SignalMetrics {
    pub rsrp: Option<i64>,
    pub rsrq: Option<i64>,
    pub rssi: Option<i64>,
    pub sinr: Option<i64>,
    pub cqi: Option<i64>,
}

pub struct GatewayClient {
    settings: Settings,
    client: Client,
    token: Option<String>,
}

impl GatewayClient {
    pub fn new(settings: Settings) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .danger_accept_invalid_certs(false)
            .cookie_store(true)
            .default_headers({
                let mut headers = HeaderMap::new();
                headers.insert(
                    USER_AGENT,
                    HeaderValue::from_static("homeisp/android/2.12.1"),
                );
                headers
            })
            .build()
            .expect("failed to build HTTP client");

        Self {
            settings,
            client,
            token: None,
        }
    }

    pub fn clear_auth(&mut self) {
        self.token = None;
    }

    pub fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let response: AuthResponse = self
            .client
            .post(self.url("auth/login"))
            .json(&serde_json::json!({
                "username": username,
                "password": password,
            }))
            .send()
            .and_then(Response::error_for_status)?
            .json()?;

        self.token = response.auth.and_then(|auth| auth.token);
        if self.token.is_some() {
            Ok(())
        } else {
            Err("login succeeded but the gateway did not return a token".into())
        }
    }

    pub fn refresh_all(&self) -> Result<Snapshot> {
        let main: GatewayInfo = self.get_json("gateway/?get=all")?;
        let cell: Option<CellRoot> = self.get_json("network/telemetry/?get=cell").ok();
        let sim: Option<SimRoot> = self.get_json("network/telemetry/?get=sim").ok();
        let clients: Option<ClientRoot> = self.get_json("network/telemetry/?get=clients").ok();
        let wifi: Option<WifiConfig> = self.get_json("network/configuration/v2?get=ap").ok();

        Ok(Snapshot {
            logged_in: self.token.is_some(),
            token_present: self.token.is_some(),
            gateway_ip: self.settings.gateway_ip.clone(),
            username: self.settings.username.clone(),
            device_summary: device_rows(main.device.as_ref()),
            lte_summary: radio_rows(
                main.signal.as_ref().and_then(|s| s.four_g.as_ref()),
                cell.as_ref()
                    .and_then(|c| c.cell.as_ref())
                    .and_then(|c| c.four_g.as_ref()),
            ),
            five_g_summary: radio_rows(
                main.signal.as_ref().and_then(|s| s.five_g.as_ref()),
                cell.as_ref()
                    .and_then(|c| c.cell.as_ref())
                    .and_then(|c| c.five_g.as_ref()),
            ),
            lte_metrics: signal_metrics(
                main.signal.as_ref().and_then(|s| s.four_g.as_ref()),
                cell.as_ref()
                    .and_then(|c| c.cell.as_ref())
                    .and_then(|c| c.four_g.as_ref()),
            ),
            five_g_metrics: signal_metrics(
                main.signal.as_ref().and_then(|s| s.five_g.as_ref()),
                cell.as_ref()
                    .and_then(|c| c.cell.as_ref())
                    .and_then(|c| c.five_g.as_ref()),
            ),
            general_summary: general_rows(&main),
            sim_summary: sim_rows(sim.as_ref().and_then(|s| s.sim.as_ref())),
            clients: client_rows(clients.as_ref().and_then(|c| c.clients.as_ref())),
            wifi,
            error: None,
        })
    }

    pub fn set_wifi_config(&self, wifi: WifiConfig) -> Result<Snapshot> {
        self.client
            .post(self.url("network/configuration/v2?set=ap"))
            .bearer_auth(self.token.as_deref().unwrap_or(""))
            .json(&wifi)
            .send()
            .and_then(Response::error_for_status)?;

        self.refresh_all()
    }

    pub fn reboot(&self) -> Result<()> {
        self.client
            .post(self.url("gateway/reset?set=reboot"))
            .bearer_auth(self.token.as_deref().unwrap_or(""))
            .send()
            .and_then(Response::error_for_status)?;
        Ok(())
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, endpoint: &str) -> Result<T> {
        let mut request = self.client.get(self.url(endpoint));
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        Ok(request
            .send()
            .and_then(Response::error_for_status)?
            .json()?)
    }

    fn url(&self, endpoint: &str) -> String {
        let address = &self.settings.gateway_ip;
        let port = if address.rsplit_once(':').is_some() {
            ""
        } else {
            ":8080"
        };
        format!("http://{address}{port}/{BASE_URL}/{endpoint}")
    }
}

fn row(label: &str, value: impl ToString) -> Row {
    Row {
        label: label.to_string(),
        value: value.to_string(),
    }
}

fn maybe_row(rows: &mut Vec<Row>, label: &str, value: Option<impl ToString>) {
    if let Some(value) = value {
        rows.push(row(label, value));
    }
}

fn device_rows(device: Option<&Device>) -> Vec<Row> {
    let Some(device) = device else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    maybe_row(
        &mut rows,
        "Name",
        device.friendly_name.as_deref().or(device.name.as_deref()),
    );
    maybe_row(&mut rows, "Manufacturer", device.manufacturer.as_deref());
    maybe_row(&mut rows, "Model", device.model.as_deref());
    maybe_row(&mut rows, "Serial", device.serial.as_deref());
    maybe_row(&mut rows, "Hardware", device.hardware_version.as_deref());
    maybe_row(&mut rows, "Software", device.software_version.as_deref());
    maybe_row(&mut rows, "Update", device.update_state.as_deref());
    rows
}

fn radio_rows(signal: Option<&RadioSignal>, advanced: Option<&serde_json::Value>) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(signal) = signal {
        maybe_row(&mut rows, "Bars", signal.bars);
        maybe_row(
            &mut rows,
            "Bands",
            signal.bands.as_ref().map(|bands| bands.join(", ")),
        );
        maybe_row(
            &mut rows,
            "RSRP",
            signal.rsrp.map(|value| format!("{value} dBm")),
        );
        maybe_row(
            &mut rows,
            "RSRQ",
            signal.rsrq.map(|value| format!("{value} dB")),
        );
        maybe_row(
            &mut rows,
            "RSSI",
            signal.rssi.map(|value| format!("{value} dBm")),
        );
        maybe_row(
            &mut rows,
            "SINR",
            signal.sinr.map(|value| format!("{value} dB")),
        );
        maybe_row(
            &mut rows,
            "CQI",
            signal.cqi.or_else(|| extract_i64_by_key(advanced, "cqi")),
        );
        maybe_row(&mut rows, "CID", signal.cid);
    }

    if let Some(advanced) = advanced {
        maybe_row(
            &mut rows,
            "Bandwidth",
            advanced.get("bandwidth").and_then(|v| v.as_str()),
        );
        maybe_row(
            &mut rows,
            "PCI",
            advanced.get("pci").and_then(|v| v.as_str()),
        );
        maybe_row(
            &mut rows,
            "TAC",
            advanced.get("tac").and_then(|v| v.as_str()),
        );
    }

    rows
}

fn signal_metrics(
    signal: Option<&RadioSignal>,
    advanced: Option<&serde_json::Value>,
) -> Option<SignalMetrics> {
    let cqi = signal
        .and_then(|signal| signal.cqi)
        .or_else(|| extract_i64_by_key(advanced, "cqi"));
    if signal.and_then(|signal| signal.rsrp).is_none()
        && signal.and_then(|signal| signal.rsrq).is_none()
        && signal.and_then(|signal| signal.rssi).is_none()
        && signal.and_then(|signal| signal.sinr).is_none()
        && cqi.is_none()
    {
        return None;
    }

    Some(SignalMetrics {
        rsrp: signal.and_then(|signal| signal.rsrp),
        rsrq: signal.and_then(|signal| signal.rsrq),
        rssi: signal.and_then(|signal| signal.rssi),
        sinr: signal.and_then(|signal| signal.sinr),
        cqi,
    })
}

fn extract_i64_by_key(value: Option<&serde_json::Value>, key: &str) -> Option<i64> {
    let value = value?;
    match value {
        serde_json::Value::Object(map) => {
            for (candidate, value) in map {
                if candidate_matches_key(candidate, key) {
                    if let Some(number) = json_value_as_i64(value) {
                        return Some(number);
                    }
                }
                if let Some(number) = extract_i64_by_key(Some(value), key) {
                    return Some(number);
                }
            }
            None
        }
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| extract_i64_by_key(Some(value), key)),
        _ => None,
    }
}

fn json_value_as_i64(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().map(|value| value.round() as i64))
        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
}

fn candidate_matches_key(candidate: &str, key: &str) -> bool {
    candidate.eq_ignore_ascii_case(key)
        || candidate
            .to_ascii_lowercase()
            .contains(&key.to_ascii_lowercase())
}

fn general_rows(main: &GatewayInfo) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(generic) = main.signal.as_ref().and_then(|s| s.generic.as_ref()) {
        maybe_row(&mut rows, "APN", generic.apn.as_deref());
        maybe_row(&mut rows, "Registration", generic.registration.as_deref());
        maybe_row(&mut rows, "IPv6", generic.has_ipv6);
        maybe_row(&mut rows, "Roaming", generic.roaming);
    }
    if let Some(time) = &main.time {
        maybe_row(&mut rows, "Uptime", time.uptime.map(format_duration));
    }
    rows
}

fn sim_rows(sim: Option<&SimData>) -> Vec<Row> {
    let Some(sim) = sim else { return Vec::new() };
    let mut rows = Vec::new();
    maybe_row(&mut rows, "ICCID", sim.icc_id.as_deref());
    maybe_row(&mut rows, "IMEI", sim.imei.as_deref());
    maybe_row(&mut rows, "IMSI", sim.imsi.as_deref());
    maybe_row(&mut rows, "MSISDN", sim.msisdn.as_deref());
    maybe_row(&mut rows, "Status", sim.status);
    rows
}

fn client_rows(clients: Option<&ClientBands>) -> Vec<ClientRow> {
    let mut rows = Vec::new();
    let Some(clients) = clients else { return rows };
    append_clients(&mut rows, "2.4 GHz", clients.two_gig.as_deref());
    append_clients(&mut rows, "5 GHz", clients.five_gig.as_deref());
    append_clients(&mut rows, "6 GHz", clients.six_gig.as_deref());
    append_clients(&mut rows, "Ethernet", clients.ethernet.as_deref());
    rows
}

fn append_clients(rows: &mut Vec<ClientRow>, band: &str, clients: Option<&[ClientDevice]>) {
    for client in clients.unwrap_or_default() {
        rows.push(ClientRow {
            band: band.to_string(),
            name: client
                .name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "Unknown".to_string()),
            ip: client.ipv4.clone().unwrap_or_default(),
            mac: client.mac.clone().unwrap_or_default(),
        });
    }
}

fn format_duration(seconds: i64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    format!("{days}d {hours}h {minutes}m")
}
