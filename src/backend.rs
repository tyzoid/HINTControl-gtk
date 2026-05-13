use crate::logging;
use crate::models::*;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, USER_AGENT};
use reqwest::StatusCode;
use reqwest::{Client, RequestBuilder, Response};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

type Result<T> = std::result::Result<T, GatewayError>;

#[derive(Debug)]
pub enum GatewayError {
    Http {
        method: String,
        endpoint: String,
        status: StatusCode,
    },
    Request {
        method: String,
        endpoint: String,
        message: String,
    },
    Decode {
        method: String,
        endpoint: String,
        message: String,
    },
    Other(String),
}

impl GatewayError {
    pub fn status(&self) -> Option<StatusCode> {
        match self {
            GatewayError::Http { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GatewayError::Http {
                method,
                endpoint,
                status,
            } => write!(f, "HTTP {method} {endpoint} returned {status}"),
            GatewayError::Request {
                method,
                endpoint,
                message,
            } => write!(f, "HTTP {method} {endpoint}: {message}"),
            GatewayError::Decode {
                method,
                endpoint,
                message,
            } => write!(f, "HTTP {method} {endpoint}: {message}"),
            GatewayError::Other(message) => write!(f, "{message}"),
        }
    }
}

impl Error for GatewayError {}

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
            fs::create_dir_all(parent).map_err(|error| GatewayError::Other(error.to_string()))?;
        }
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| GatewayError::Other(error.to_string()))?;
        fs::write(path, bytes).map_err(|error| GatewayError::Other(error.to_string()))?;
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

#[derive(Debug)]
pub struct GatewayTimeState {
    #[allow(dead_code)]
    pub local_time: Option<i64>,
    pub uptime: Option<i64>,
    recorded_at: Instant,
}

impl GatewayTimeState {
    pub fn new(time: &TimeInfo, recorded_at: Instant) -> Self {
        Self {
            local_time: time.local_time,
            uptime: time.uptime,
            recorded_at,
        }
    }

    pub fn current_uptime(&self, now: Instant) -> Option<i64> {
        self.uptime
            .map(|uptime| uptime + now.duration_since(self.recorded_at).as_secs() as i64)
    }

    #[allow(dead_code)]
    pub fn current_local_time(&self, now: Instant) -> Option<i64> {
        self.local_time
            .map(|local_time| local_time + now.duration_since(self.recorded_at).as_secs() as i64)
    }
}

#[derive(Debug)]
pub struct BootstrapData {
    pub gateway: GatewayInfo,
    pub cell: Option<CellRoot>,
    pub sim: Option<SimRoot>,
    pub clients: Option<ClientRoot>,
    pub wifi: Option<WifiConfig>,
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

    pub async fn login(&mut self, username: &str, password: &str) -> Result<()> {
        let endpoint = "auth/login";
        let request = self
            .client
            .post(self.url(endpoint))
            .json(&serde_json::json!({
                "username": username,
                "password": password,
            }));
        let response = self.send_request("POST", endpoint, request).await?;
        let response: AuthResponse = self.parse_json_response("POST", endpoint, response).await?;

        self.token = response.auth.and_then(|auth| auth.token);
        if self.token.is_some() {
            Ok(())
        } else {
            logging::error("login succeeded but the gateway did not return a token");
            Err(GatewayError::Other(
                "login succeeded but the gateway did not return a token".to_string(),
            ))
        }
    }

    pub async fn bootstrap(&self) -> Result<BootstrapData> {
        Ok(BootstrapData {
            gateway: self.gateway_info().await?,
            cell: self.cell_telemetry().await.ok(),
            sim: self.sim_telemetry().await.ok(),
            clients: self.clients_telemetry().await.ok(),
            wifi: self.wifi_config().await.ok(),
        })
    }

    pub async fn gateway_info(&self) -> Result<GatewayInfo> {
        self.get_json("gateway/?get=all").await
    }

    pub async fn cell_telemetry(&self) -> Result<CellRoot> {
        self.get_json("network/telemetry/?get=cell").await
    }

    pub async fn sim_telemetry(&self) -> Result<SimRoot> {
        self.get_json("network/telemetry/?get=sim").await
    }

    pub async fn clients_telemetry(&self) -> Result<ClientRoot> {
        self.get_json("network/telemetry/?get=clients").await
    }

    pub async fn wifi_config(&self) -> Result<WifiConfig> {
        self.get_json("network/configuration/v2?get=ap").await
    }

    pub async fn set_wifi_config(&self, wifi: WifiConfig) -> Result<()> {
        let endpoint = "network/configuration/v2?set=ap";
        let request = self
            .client
            .post(self.url(endpoint))
            .bearer_auth(self.token.as_deref().unwrap_or(""))
            .json(&wifi);
        self.send_request("POST", endpoint, request).await?;
        Ok(())
    }

    pub async fn reboot(&self) -> Result<()> {
        let endpoint = "gateway/reset?set=reboot";
        let request = self
            .client
            .post(self.url(endpoint))
            .bearer_auth(self.token.as_deref().unwrap_or(""));
        self.send_request("POST", endpoint, request).await?;
        Ok(())
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, endpoint: &str) -> Result<T> {
        let mut request = self.client.get(self.url(endpoint));
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = self.send_request("GET", endpoint, request).await?;
        self.parse_json_response("GET", endpoint, response).await
    }

    async fn send_request(
        &self,
        method: &str,
        endpoint: &str,
        request: RequestBuilder,
    ) -> Result<Response> {
        logging::http_request(method, endpoint);
        let response = request.send().await.map_err(|error| {
            logging::http_error(method, endpoint, format!("request failed: {error}"));
            GatewayError::Request {
                method: method.to_string(),
                endpoint: endpoint.to_string(),
                message: error.to_string(),
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            if logging::level() == logging::LogLevel::Trace {
                match response.text().await {
                    Ok(body) => logging::http_trace_response(method, endpoint, &body),
                    Err(error) => logging::http_error(
                        method,
                        endpoint,
                        format!("failed to read error response body: {error}"),
                    ),
                }
            }
            logging::http_error(
                method,
                endpoint,
                format!("response returned status {}", status.as_u16()),
            );
            return Err(GatewayError::Http {
                method: method.to_string(),
                endpoint: endpoint.to_string(),
                status,
            });
        }
        logging::http_success(method, endpoint, status);
        Ok(response)
    }

    async fn parse_json_response<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        endpoint: &str,
        response: Response,
    ) -> Result<T> {
        let body = response.text().await.map_err(|error| {
            logging::http_error(
                method,
                endpoint,
                format!("failed to read response body: {error}"),
            );
            GatewayError::Decode {
                method: method.to_string(),
                endpoint: endpoint.to_string(),
                message: error.to_string(),
            }
        })?;
        if logging::level() == logging::LogLevel::Trace {
            logging::http_trace_response(method, endpoint, &body);
        }
        serde_json::from_str(&body).map_err(|error| {
            logging::http_error(method, endpoint, format!("failed to decode JSON: {error}"));
            GatewayError::Decode {
                method: method.to_string(),
                endpoint: endpoint.to_string(),
                message: error.to_string(),
            }
        })
    }

    fn url(&self, endpoint: &str) -> String {
        let authority = gateway_authority(&self.settings.gateway_ip);
        format!("http://{authority}/{BASE_URL}/{endpoint}")
    }
}

fn gateway_authority(address: &str) -> String {
    if address.starts_with('[') {
        return if address.rsplit_once("]:").is_some() {
            address.to_string()
        } else {
            format!("{address}:8080")
        };
    }
    if address.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
        return format!("[{address}]:8080");
    }
    if address.rsplit_once(':').is_some() {
        address.to_string()
    } else {
        format!("{address}:8080")
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

pub fn device_rows(device: Option<&Device>) -> Vec<Row> {
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

pub fn radio_rows_from_value(advanced: Option<&serde_json::Value>) -> Vec<Row> {
    let mut rows = Vec::new();
    maybe_row(
        &mut rows,
        "Bars",
        extract_i64_by_key(advanced, "bars")
            .or_else(|| extract_f64_by_key(advanced, "bars").map(|value| value.round() as i64)),
    );
    maybe_row(
        &mut rows,
        "Bands",
        extract_string_list_by_key(advanced, "bands").map(|bands| bands.join(", ")),
    );
    maybe_row(
        &mut rows,
        "RSRP",
        extract_i64_by_key(advanced, "rsrp").map(|value| format!("{value} dBm")),
    );
    maybe_row(
        &mut rows,
        "RSRQ",
        extract_i64_by_key(advanced, "rsrq").map(|value| format!("{value} dB")),
    );
    maybe_row(
        &mut rows,
        "RSSI",
        extract_i64_by_key(advanced, "rssi").map(|value| format!("{value} dBm")),
    );
    maybe_row(
        &mut rows,
        "SINR",
        extract_i64_by_key(advanced, "sinr").map(|value| format!("{value} dB")),
    );
    maybe_row(&mut rows, "CQI", extract_i64_by_key(advanced, "cqi"));
    maybe_row(&mut rows, "CID", extract_i64_by_key(advanced, "cid"));
    maybe_row(
        &mut rows,
        "Bandwidth",
        extract_string_by_key(advanced, "bandwidth"),
    );
    maybe_row(&mut rows, "PCI", extract_string_by_key(advanced, "pci"));
    maybe_row(&mut rows, "TAC", extract_string_by_key(advanced, "tac"));

    rows
}

pub fn signal_metrics_from_value(advanced: Option<&serde_json::Value>) -> Option<SignalMetrics> {
    let rsrp = extract_i64_by_key(advanced, "rsrp");
    let rsrq = extract_i64_by_key(advanced, "rsrq");
    let rssi = extract_i64_by_key(advanced, "rssi");
    let sinr = extract_i64_by_key(advanced, "sinr");
    let cqi = extract_i64_by_key(advanced, "cqi");
    if rsrp.is_none() && rsrq.is_none() && rssi.is_none() && sinr.is_none() && cqi.is_none() {
        return None;
    }

    Some(SignalMetrics {
        rsrp,
        rsrq,
        rssi,
        sinr,
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

fn extract_f64_by_key(value: Option<&serde_json::Value>, key: &str) -> Option<f64> {
    let value = value?;
    match value {
        serde_json::Value::Object(map) => {
            for (candidate, value) in map {
                if candidate_matches_key(candidate, key) {
                    if let Some(number) = value.as_f64() {
                        return Some(number);
                    }
                }
                if let Some(number) = extract_f64_by_key(Some(value), key) {
                    return Some(number);
                }
            }
            None
        }
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| extract_f64_by_key(Some(value), key)),
        _ => None,
    }
}

fn extract_string_by_key(value: Option<&serde_json::Value>, key: &str) -> Option<String> {
    let value = value?;
    match value {
        serde_json::Value::Object(map) => {
            for (candidate, value) in map {
                if candidate_matches_key(candidate, key) {
                    if let Some(text) = value.as_str() {
                        return Some(text.to_string());
                    }
                }
                if let Some(text) = extract_string_by_key(Some(value), key) {
                    return Some(text);
                }
            }
            None
        }
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| extract_string_by_key(Some(value), key)),
        _ => None,
    }
}

fn extract_string_list_by_key(value: Option<&serde_json::Value>, key: &str) -> Option<Vec<String>> {
    let value = value?;
    match value {
        serde_json::Value::Object(map) => {
            for (candidate, value) in map {
                if candidate_matches_key(candidate, key) {
                    if let Some(values) = value.as_array() {
                        let mut strings = Vec::new();
                        for item in values {
                            if let Some(text) = item.as_str() {
                                strings.push(text.to_string());
                            }
                        }
                        if !strings.is_empty() {
                            return Some(strings);
                        }
                    }
                }
                if let Some(values) = extract_string_list_by_key(Some(value), key) {
                    return Some(values);
                }
            }
            None
        }
        serde_json::Value::Array(values) => values
            .iter()
            .find_map(|value| extract_string_list_by_key(Some(value), key)),
        _ => None,
    }
}

fn candidate_matches_key(candidate: &str, key: &str) -> bool {
    candidate.eq_ignore_ascii_case(key)
        || candidate
            .to_ascii_lowercase()
            .contains(&key.to_ascii_lowercase())
}

pub fn general_rows(
    generic: Option<&GenericSignal>,
    time_state: Option<&GatewayTimeState>,
    now: Instant,
) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(generic) = generic {
        maybe_row(&mut rows, "APN", generic.apn.as_deref());
        maybe_row(&mut rows, "Registration", generic.registration.as_deref());
        maybe_row(&mut rows, "IPv6", generic.has_ipv6);
        maybe_row(&mut rows, "Roaming", generic.roaming);
    }
    if let Some(time_state) = time_state {
        maybe_row(
            &mut rows,
            "Uptime",
            time_state.current_uptime(now).map(format_duration),
        );
    }
    rows
}

pub fn sim_rows(sim: Option<&SimData>) -> Vec<Row> {
    let Some(sim) = sim else { return Vec::new() };
    let mut rows = Vec::new();
    maybe_row(&mut rows, "ICCID", sim.icc_id.as_deref());
    maybe_row(&mut rows, "IMEI", sim.imei.as_deref());
    maybe_row(&mut rows, "IMSI", sim.imsi.as_deref());
    maybe_row(&mut rows, "MSISDN", sim.msisdn.as_deref());
    maybe_row(&mut rows, "Status", sim.status);
    rows
}

pub fn client_rows(clients: Option<&ClientBands>) -> Vec<ClientRow> {
    let mut rows = Vec::new();
    let Some(clients) = clients else { return rows };
    append_clients(&mut rows, "2.4 GHz", clients.two_gig.as_deref());
    append_clients(&mut rows, "5 GHz", clients.five_gig.as_deref());
    append_clients(&mut rows, "6 GHz", clients.six_gig.as_deref());
    append_clients(&mut rows, "Wi-Fi", clients.wifi.as_deref());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_authority_adds_default_port_to_hosts_and_ipv4() {
        assert_eq!(gateway_authority("192.168.12.1"), "192.168.12.1:8080");
        assert_eq!(gateway_authority("gateway.local"), "gateway.local:8080");
    }

    #[test]
    fn gateway_authority_preserves_explicit_ports() {
        assert_eq!(gateway_authority("192.168.12.1:8081"), "192.168.12.1:8081");
        assert_eq!(
            gateway_authority("gateway.local:8081"),
            "gateway.local:8081"
        );
    }

    #[test]
    fn gateway_authority_brackets_ipv6_literals() {
        assert_eq!(gateway_authority("fd00::1"), "[fd00::1]:8080");
        assert_eq!(gateway_authority("[fd00::1]"), "[fd00::1]:8080");
        assert_eq!(gateway_authority("[fd00::1]:8081"), "[fd00::1]:8081");
    }
}
