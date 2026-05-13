use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct AuthResponse {
    pub auth: Option<AuthData>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct AuthData {
    pub token: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct GatewayInfo {
    pub device: Option<Device>,
    pub signal: Option<Signal>,
    pub time: Option<TimeInfo>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct Device {
    #[serde(rename = "friendlyName")]
    pub friendly_name: Option<String>,
    #[serde(rename = "hardwareVersion")]
    pub hardware_version: Option<String>,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub name: Option<String>,
    pub serial: Option<String>,
    #[serde(rename = "softwareVersion")]
    pub software_version: Option<String>,
    #[serde(rename = "updateState")]
    pub update_state: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct Signal {
    #[serde(rename = "4g")]
    pub four_g: Option<RadioSignal>,
    #[serde(rename = "5g")]
    pub five_g: Option<RadioSignal>,
    pub generic: Option<GenericSignal>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct RadioSignal {
    pub bands: Option<Vec<String>>,
    pub bars: Option<f64>,
    pub cid: Option<i64>,
    #[serde(rename = "eNBID")]
    pub enbid: Option<i64>,
    #[serde(rename = "gNBID")]
    pub gnbid: Option<i64>,
    pub rsrp: Option<i64>,
    pub rsrq: Option<i64>,
    pub rssi: Option<i64>,
    pub sinr: Option<i64>,
    pub cqi: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct GenericSignal {
    pub apn: Option<String>,
    #[serde(rename = "hasIPv6")]
    pub has_ipv6: Option<bool>,
    pub registration: Option<String>,
    pub roaming: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct TimeInfo {
    #[serde(rename = "localTime")]
    pub local_time: Option<i64>,
    #[serde(rename = "upTime")]
    pub uptime: Option<i64>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct CellRoot {
    pub cell: Option<CellData>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct CellData {
    #[serde(rename = "4g")]
    pub four_g: Option<Value>,
    #[serde(rename = "5g")]
    pub five_g: Option<Value>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct SimRoot {
    pub sim: Option<SimData>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct SimData {
    #[serde(rename = "iccId")]
    pub icc_id: Option<String>,
    pub imei: Option<String>,
    pub imsi: Option<String>,
    pub msisdn: Option<String>,
    pub status: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ClientRoot {
    pub clients: Option<ClientBands>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ClientBands {
    #[serde(rename = "2.4ghz")]
    pub two_gig: Option<Vec<ClientDevice>>,
    #[serde(rename = "5.0ghz")]
    pub five_gig: Option<Vec<ClientDevice>>,
    #[serde(rename = "6.0ghz")]
    pub six_gig: Option<Vec<ClientDevice>>,
    pub ethernet: Option<Vec<ClientDevice>>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ClientDevice {
    pub connected: Option<bool>,
    pub ipv4: Option<String>,
    pub ipv6: Option<Vec<String>>,
    pub mac: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct WifiConfig {
    #[serde(rename = "2.4ghz")]
    pub two_gig: Option<BandConfig>,
    #[serde(rename = "5.0ghz")]
    pub five_gig: Option<BandConfig>,
    #[serde(rename = "6.0ghz")]
    pub six_gig: Option<BandConfig>,
    #[serde(rename = "bandSteering")]
    pub band_steering: Option<Value>,
    pub ssids: Option<Vec<SsidConfig>>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct BandConfig {
    #[serde(rename = "airtimeFairness")]
    pub airtime_fairness: Option<bool>,
    pub channel: Option<String>,
    #[serde(rename = "channelBandwidth")]
    pub channel_bandwidth: Option<String>,
    #[serde(rename = "isMUMIMOEnabled")]
    pub is_mu_mimo_enabled: Option<bool>,
    #[serde(rename = "isRadioEnabled")]
    pub is_radio_enabled: Option<bool>,
    #[serde(rename = "isWMMEnabled")]
    pub is_wmm_enabled: Option<bool>,
    #[serde(rename = "maxClients")]
    pub max_clients: Option<i64>,
    pub mode: Option<String>,
    #[serde(rename = "transmissionPower")]
    pub transmission_power: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct SsidConfig {
    #[serde(rename = "2.4ghzSsid")]
    pub two_gig_ssid: Option<bool>,
    #[serde(rename = "5.0ghzSsid")]
    pub five_gig_ssid: Option<bool>,
    #[serde(rename = "6.0ghzSsid")]
    pub six_gig_ssid: Option<bool>,
    #[serde(rename = "encryptionMode")]
    pub encryption_mode: Option<String>,
    #[serde(rename = "encryptionVersion")]
    pub encryption_version: Option<String>,
    pub guest: Option<bool>,
    #[serde(rename = "isBroadcastEnabled")]
    pub is_broadcast_enabled: Option<bool>,
    #[serde(rename = "ssidName")]
    pub ssid_name: Option<String>,
    #[serde(rename = "wpaKey")]
    pub wpa_key: Option<String>,
}
