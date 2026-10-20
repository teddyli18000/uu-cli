use crate::model::{DeviceInfo, SessionInfo};
use crate::uuyc::OfficialCli;
use anyhow::Result;

pub trait InventoryProvider {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>>;
    fn list_sessions(&self, device_identity: &str) -> Result<Vec<SessionInfo>>;
}

impl InventoryProvider for OfficialCli {
    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        OfficialCli::list_devices(self)
    }

    fn list_sessions(&self, device_identity: &str) -> Result<Vec<SessionInfo>> {
        OfficialCli::list_sessions(self, device_identity)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInventory {
    pub device: DeviceInfo,
    pub sessions: Vec<SessionInfo>,
    pub session_error: Option<String>,
    pub anomalies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InventorySnapshot {
    pub devices: Vec<DeviceInventory>,
}

impl InventorySnapshot {
    pub fn eligible_sessions(&self) -> Vec<&SessionInfo> {
        self.devices
            .iter()
            .flat_map(|device| device.sessions.iter())
            .filter(|session| is_eligible_session(session))
            .collect()
    }

    pub fn eligible_sessions_for(&self, device_identity: &str) -> Vec<&SessionInfo> {
        self.devices
            .iter()
            .filter(|device| device.device.device_identity == device_identity)
            .flat_map(|device| device.sessions.iter())
            .filter(|session| is_eligible_session(session))
            .collect()
    }

    pub fn live_sessions_for(&self, device_identity: &str) -> Vec<&SessionInfo> {
        self.devices
            .iter()
            .filter(|device| device.device.device_identity == device_identity)
            .flat_map(|device| device.sessions.iter())
            .filter(|session| is_live_session(session))
            .collect()
    }
}

/// Discover devices first, then query terminal sessions per online device.
///
/// A failure on one device does not erase the rest of the inventory. A session
/// whose embedded device identity disagrees with the device queried is rejected
/// from the worker candidates and recorded as an anomaly rather than trusted.
pub fn discover_inventory(provider: &impl InventoryProvider) -> Result<InventorySnapshot> {
    let devices = provider.list_devices()?;
    let mut discovered = Vec::with_capacity(devices.len());

    for device in devices {
        let mut inventory = DeviceInventory {
            device: device.clone(),
            sessions: Vec::new(),
            session_error: None,
            anomalies: Vec::new(),
        };

        if !device.is_online {
            discovered.push(inventory);
            continue;
        }

        match provider.list_sessions(&device.device_identity) {
            Ok(sessions) => {
                for session in sessions {
                    if session.device_identity != device.device_identity {
                        inventory.anomalies.push(format!(
                            "session {} reported device {} while querying {}",
                            session.session_name, session.device_identity, device.device_identity
                        ));
                        continue;
                    }
                    inventory.sessions.push(session);
                }
            }
            Err(error) => inventory.session_error = Some(error.to_string()),
        }
        discovered.push(inventory);
    }

    Ok(InventorySnapshot {
        devices: discovered,
    })
}

/// A live shell may be either currently connected (`running`) or retained by the
/// remote after its controller detached (`detached`). Both are valid instances to
/// authorize. `exited`/unknown states are never reused.
pub fn is_live_session(session: &SessionInfo) -> bool {
    session.state.eq_ignore_ascii_case("running")
        || session.state.eq_ignore_ascii_case("detached")
}

/// A worker is immediately schedulable only when it is live and the official CLI
/// does not explicitly mark it non-attachable. A live non-attachable session is
/// treated as busy capacity, not as a missing/replaced authorization.
pub fn is_eligible_session(session: &SessionInfo) -> bool {
    is_live_session(session) && session.attachable != Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{anyhow, Result};
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeProvider {
        devices: Vec<DeviceInfo>,
        sessions: HashMap<String, Result<Vec<SessionInfo>, String>>,
    }

    impl InventoryProvider for FakeProvider {
        fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
            Ok(self.devices.clone())
        }

        fn list_sessions(&self, device_identity: &str) -> Result<Vec<SessionInfo>> {
            match self.sessions.get(device_identity) {
                Some(Ok(sessions)) => Ok(sessions.clone()),
                Some(Err(error)) => Err(anyhow!(error.clone())),
                None => Ok(Vec::new()),
            }
        }
    }

    fn device(id: &str, online: bool) -> DeviceInfo {
        DeviceInfo {
            device_identity: id.into(),
            device_name: format!("device-{id}"),
            is_online: online,
            platform: Some("linux".into()),
        }
    }

    fn session(device: &str, name: &str) -> SessionInfo {
        SessionInfo {
            device_identity: device.into(),
            device_name: format!("device-{device}"),
            session_name: name.into(),
            shell: Some("/bin/bash".into()),
            state: "running".into(),
            created_at: Some("2026-10-20T00:00:00Z".into()),
            attachable: Some(true),
        }
    }

    #[test]
    fn offline_device_is_not_probed_for_sessions() {
        let provider = FakeProvider {
            devices: vec![device("a", false)],
            ..Default::default()
        };
        let snapshot = discover_inventory(&provider).unwrap();
        assert!(snapshot.devices[0].sessions.is_empty());
        assert_eq!(snapshot.devices[0].session_error, None);
    }

    #[test]
    fn one_device_failure_does_not_hide_other_devices() {
        let mut provider = FakeProvider {
            devices: vec![device("a", true), device("b", true)],
            ..Default::default()
        };
        provider
            .sessions
            .insert("a".into(), Err("terminal service unavailable".into()));
        provider
            .sessions
            .insert("b".into(), Ok(vec![session("b", "bash-1")]));

        let snapshot = discover_inventory(&provider).unwrap();
        assert!(snapshot.devices[0].session_error.is_some());
        assert_eq!(snapshot.eligible_sessions_for("b").len(), 1);
    }

    #[test]
    fn mismatched_device_session_is_rejected() {
        let mut provider = FakeProvider {
            devices: vec![device("a", true)],
            ..Default::default()
        };
        provider
            .sessions
            .insert("a".into(), Ok(vec![session("b", "bash-1")]));

        let snapshot = discover_inventory(&provider).unwrap();
        assert!(snapshot.devices[0].sessions.is_empty());
        assert_eq!(snapshot.devices[0].anomalies.len(), 1);
    }

    #[test]
    fn running_and_detached_sessions_are_live() {
        let running = session("a", "run");
        let mut detached = session("a", "detached");
        detached.state = "detached".into();
        let mut exited = session("a", "exited");
        exited.state = "exited".into();
        assert!(is_live_session(&running));
        assert!(is_live_session(&detached));
        assert!(!is_live_session(&exited));
    }

    #[test]
    fn live_non_attachable_session_is_busy_not_eligible() {
        let mut busy = session("a", "busy");
        busy.attachable = Some(false);
        assert!(is_live_session(&busy));
        assert!(!is_eligible_session(&busy));
    }

    #[test]
    fn detached_attachable_session_is_eligible() {
        let mut detached = session("a", "detached");
        detached.state = "detached".into();
        assert!(is_eligible_session(&detached));
        detached.attachable = None;
        assert!(is_eligible_session(&detached));
    }
}
