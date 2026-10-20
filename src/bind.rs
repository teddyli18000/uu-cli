use crate::inventory::{is_live_session, InventorySnapshot};
use crate::model::{DeviceInfo, SessionInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindCandidate {
    pub device_identity: String,
    pub device_name: String,
    pub open_sessions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindDecision {
    /// No live Terminal exists. The caller should tell the user that the next
    /// Terminal they open will be used for this request, then wait.
    WaitForTerminal { online_devices: usize },
    /// Exactly one remote device currently has one or more live Terminals.
    /// Device selection needs no extra prompt; authorization / remote-root
    /// confirmation can proceed in the same short interaction.
    OneDevice(BindCandidate),
    /// More than one remote device has live Terminals. A human must pick the
    /// friendly device once; raw device IDs remain internal.
    ChooseDevice(Vec<BindCandidate>),
}

pub fn decide_binding(snapshot: &InventorySnapshot) -> BindDecision {
    let mut candidates = snapshot
        .devices
        .iter()
        .filter_map(|inventory| {
            if !inventory.device.is_online {
                return None;
            }
            let open_sessions = inventory
                .sessions
                .iter()
                .filter(|session| is_live_session(session))
                .count();
            (open_sessions > 0).then(|| BindCandidate {
                device_identity: inventory.device.device_identity.clone(),
                device_name: friendly_device_name(&inventory.device),
                open_sessions,
            })
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|a, b| {
        a.device_name
            .to_lowercase()
            .cmp(&b.device_name.to_lowercase())
            .then_with(|| a.device_identity.cmp(&b.device_identity))
    });

    match candidates.len() {
        0 => BindDecision::WaitForTerminal {
            online_devices: snapshot
                .devices
                .iter()
                .filter(|item| item.device.is_online)
                .count(),
        },
        1 => BindDecision::OneDevice(candidates.remove(0)),
        _ => BindDecision::ChooseDevice(candidates),
    }
}

/// When a request is already waiting for a Terminal on a known device, opening
/// a new matching live Terminal is the user's explicit authorization for that
/// request. It must not trigger a second redundant "use this terminal?" prompt.
pub fn satisfies_waiting_request<'a>(
    expected_device: Option<&str>,
    newly_seen: &'a [SessionInfo],
) -> Option<&'a SessionInfo> {
    match expected_device {
        Some(device_identity) => newly_seen
            .iter()
            .filter(|session| is_live_session(session))
            .find(|session| session.device_identity == device_identity),
        None => {
            let mut live = newly_seen.iter().filter(|session| is_live_session(session));
            let first = live.next()?;
            if live.any(|other| other.device_identity != first.device_identity) {
                None
            } else {
                Some(first)
            }
        }
    }
}

fn friendly_device_name(device: &DeviceInfo) -> String {
    let name = device.device_name.trim();
    if name.is_empty() {
        "未命名设备".into()
    } else {
        name.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::{DeviceInventory, InventorySnapshot};

    fn device(id: &str, name: &str, online: bool) -> DeviceInfo {
        DeviceInfo {
            device_identity: id.into(),
            device_name: name.into(),
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

    fn inventory(device: DeviceInfo, sessions: Vec<SessionInfo>) -> DeviceInventory {
        DeviceInventory {
            device,
            sessions,
            session_error: None,
            anomalies: Vec::new(),
        }
    }

    #[test]
    fn no_terminal_means_wait_not_guess() {
        let snapshot = InventorySnapshot {
            devices: vec![inventory(device("a", "Car", true), Vec::new())],
        };
        assert_eq!(
            decide_binding(&snapshot),
            BindDecision::WaitForTerminal { online_devices: 1 }
        );
    }

    #[test]
    fn exactly_one_device_with_terminals_needs_no_device_prompt() {
        let snapshot = InventorySnapshot {
            devices: vec![
                inventory(device("a", "Car A", true), vec![session("a", "bash-1")]),
                inventory(device("b", "Car B", true), Vec::new()),
            ],
        };
        assert_eq!(
            decide_binding(&snapshot),
            BindDecision::OneDevice(BindCandidate {
                device_identity: "a".into(),
                device_name: "Car A".into(),
                open_sessions: 1,
            })
        );
    }

    #[test]
    fn busy_terminal_is_still_claimable_during_bind() {
        let mut busy = session("a", "bash-1");
        busy.attachable = Some(false);
        let snapshot = InventorySnapshot {
            devices: vec![inventory(device("a", "Car A", true), vec![busy])],
        };
        assert!(matches!(
            decide_binding(&snapshot),
            BindDecision::OneDevice(BindCandidate {
                open_sessions: 1,
                ..
            })
        ));
    }

    #[test]
    fn detached_terminal_is_still_claimable_during_bind() {
        let mut detached = session("a", "bash-1");
        detached.state = "detached".into();
        let snapshot = InventorySnapshot {
            devices: vec![inventory(device("a", "Car A", true), vec![detached])],
        };
        assert!(matches!(
            decide_binding(&snapshot),
            BindDecision::OneDevice(BindCandidate {
                open_sessions: 1,
                ..
            })
        ));
    }

    #[test]
    fn multiple_devices_with_terminals_require_one_human_choice() {
        let snapshot = InventorySnapshot {
            devices: vec![
                inventory(device("b", "Car B", true), vec![session("b", "bash-2")]),
                inventory(device("a", "Car A", true), vec![session("a", "bash-1")]),
            ],
        };
        let BindDecision::ChooseDevice(choices) = decide_binding(&snapshot) else {
            panic!("expected device choice")
        };
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].device_name, "Car A");
        assert_eq!(choices[1].device_name, "Car B");
    }

    #[test]
    fn waiting_for_known_device_accepts_matching_new_terminal_without_second_prompt() {
        let new = vec![session("b", "bash-b"), session("a", "bash-a")];
        let chosen = satisfies_waiting_request(Some("a"), &new).unwrap();
        assert_eq!(chosen.session_name, "bash-a");
    }

    #[test]
    fn unknown_device_wait_does_not_guess_when_multiple_devices_arrive() {
        let new = vec![session("a", "bash-a"), session("b", "bash-b")];
        assert_eq!(satisfies_waiting_request(None, &new), None);
    }

    #[test]
    fn unknown_device_wait_can_accept_multiple_terminals_from_same_device() {
        let new = vec![session("a", "bash-1"), session("a", "bash-2")];
        let chosen = satisfies_waiting_request(None, &new).unwrap();
        assert_eq!(chosen.device_identity, "a");
    }
}
