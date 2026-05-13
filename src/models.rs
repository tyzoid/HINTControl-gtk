use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

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
    pub wifi: Option<Vec<ClientDevice>>,
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
    #[serde(rename = "2.4ghz", skip_serializing_if = "Option::is_none")]
    pub two_gig: Option<BandConfig>,
    #[serde(rename = "5.0ghz", skip_serializing_if = "Option::is_none")]
    pub five_gig: Option<BandConfig>,
    #[serde(rename = "6.0ghz", skip_serializing_if = "Option::is_none")]
    pub six_gig: Option<BandConfig>,
    #[serde(rename = "bandSteering", skip_serializing_if = "Option::is_none")]
    pub band_steering: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssids: Option<Vec<SsidConfig>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct BandConfig {
    #[serde(rename = "airtimeFairness", skip_serializing_if = "Option::is_none")]
    pub airtime_fairness: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(rename = "channelBandwidth", skip_serializing_if = "Option::is_none")]
    pub channel_bandwidth: Option<String>,
    #[serde(rename = "isMUMIMOEnabled", skip_serializing_if = "Option::is_none")]
    pub is_mu_mimo_enabled: Option<bool>,
    #[serde(rename = "isRadioEnabled", skip_serializing_if = "Option::is_none")]
    pub is_radio_enabled: Option<bool>,
    #[serde(rename = "isWMMEnabled", skip_serializing_if = "Option::is_none")]
    pub is_wmm_enabled: Option<bool>,
    #[serde(rename = "maxClients", skip_serializing_if = "Option::is_none")]
    pub max_clients: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(rename = "transmissionPower", skip_serializing_if = "Option::is_none")]
    pub transmission_power: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Default, PartialEq)]
pub struct SsidConfig {
    #[serde(rename = "2.4ghzSsid", skip_serializing_if = "Option::is_none")]
    pub two_gig_ssid: Option<bool>,
    #[serde(rename = "5.0ghzSsid", skip_serializing_if = "Option::is_none")]
    pub five_gig_ssid: Option<bool>,
    #[serde(rename = "6.0ghzSsid", skip_serializing_if = "Option::is_none")]
    pub six_gig_ssid: Option<bool>,
    #[serde(rename = "encryptionMode", skip_serializing_if = "Option::is_none")]
    pub encryption_mode: Option<String>,
    #[serde(rename = "encryptionVersion", skip_serializing_if = "Option::is_none")]
    pub encryption_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub guest: Option<bool>,
    #[serde(rename = "isBroadcastEnabled", skip_serializing_if = "Option::is_none")]
    pub is_broadcast_enabled: Option<bool>,
    #[serde(rename = "ssidName", skip_serializing_if = "Option::is_none")]
    pub ssid_name: Option<String>,
    #[serde(rename = "wpaKey", skip_serializing_if = "Option::is_none")]
    pub wpa_key: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_config_serialization_omits_none_fields() {
        let config = WifiConfig {
            two_gig: Some(BandConfig {
                is_radio_enabled: Some(true),
                ..BandConfig::default()
            }),
            ..WifiConfig::default()
        };

        let value = serde_json::to_value(config).unwrap();
        assert_eq!(value["2.4ghz"]["isRadioEnabled"], true);
        assert!(value["2.4ghz"].get("channel").is_none());
        assert!(value.get("5.0ghz").is_none());
        assert!(value.get("ssids").is_none());
    }

    #[test]
    fn wifi_config_round_trips_unknown_fields() {
        let json = serde_json::json!({
            "2.4ghz": {
                "isRadioEnabled": true,
                "vendorBandField": "kept"
            },
            "ssids": [{
                "ssidName": "Main",
                "wpaKey": "secret",
                "vendorSsidField": 42
            }],
            "vendorRootField": { "nested": true }
        });

        let mut config: WifiConfig = serde_json::from_value(json).unwrap();
        config.two_gig.as_mut().unwrap().is_radio_enabled = Some(false);
        config.ssids.as_mut().unwrap()[0].ssid_name = Some("Renamed".to_string());

        let value = serde_json::to_value(config).unwrap();
        assert_eq!(value["2.4ghz"]["isRadioEnabled"], false);
        assert_eq!(value["2.4ghz"]["vendorBandField"], "kept");
        assert_eq!(value["ssids"][0]["ssidName"], "Renamed");
        assert_eq!(value["ssids"][0]["vendorSsidField"], 42);
        assert_eq!(value["vendorRootField"]["nested"], true);
    }
}
