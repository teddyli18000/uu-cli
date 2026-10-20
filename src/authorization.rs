use crate::model::SessionInfo;
use std::collections::HashSet;

/// The policy result for one discovered session.
///
/// Fingerprints include device identity, session name, and creation time. A
/// session without a creation time cannot be safely authorized because its name
/// may have been reused after a UU restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDecision {
    Authorized,
    Declined,
    Prompt,
    AutoAuthorizeForPendingRequest,
    Unverifiable,
}

/// Classify a session against the persisted decisions and, when present, the
/// inventory captured before a waiting request told the user to open a terminal.
///
/// A decline is sticky for this exact session instance. It takes precedence if
/// corrupted/legacy state happens to contain both decisions for one fingerprint.
pub fn classify_session(
    session: &SessionInfo,
    authorized_fingerprints: &[String],
    declined_fingerprints: &[String],
    waiting_baseline: Option<&HashSet<String>>,
) -> SessionDecision {
    let Some(fingerprint) = session.authorization_fingerprint() else {
        return SessionDecision::Unverifiable;
    };

    if declined_fingerprints.iter().any(|item| item == &fingerprint) {
        return SessionDecision::Declined;
    }
    if authorized_fingerprints
        .iter()
        .any(|item| item == &fingerprint)
    {
        return SessionDecision::Authorized;
    }

    if waiting_baseline.is_some_and(|baseline| !baseline.contains(&fingerprint)) {
        return SessionDecision::AutoAuthorizeForPendingRequest;
    }

    SessionDecision::Prompt
}

/// Select a newly opened instance for a request that already told the user to
/// open a matching terminal. The caller must first filter to eligible sessions
/// and, for an existing binding, to its bound device. Multiple candidates stay
/// ambiguous; this function never guesses by device or session name.
pub fn select_waiting_session<'a>(
    sessions: impl IntoIterator<Item = &'a SessionInfo>,
    baseline: &HashSet<String>,
) -> WaitSelection<'a> {
    let mut seen = HashSet::new();
    let mut candidates = Vec::new();

    for session in sessions {
        let Some(fingerprint) = session.authorization_fingerprint() else {
            continue;
        };
        if !baseline.contains(&fingerprint) && seen.insert(fingerprint) {
            candidates.push(session);
        }
    }

    match candidates.len() {
        0 => WaitSelection::None,
        1 => WaitSelection::Unique(candidates[0]),
        _ => WaitSelection::Ambiguous(candidates),
    }
}

/// Capture exact identities present before the waiting prompt is shown.
pub fn fingerprint_baseline<'a>(
    sessions: impl IntoIterator<Item = &'a SessionInfo>,
) -> HashSet<String> {
    sessions
        .into_iter()
        .filter_map(SessionInfo::authorization_fingerprint)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitSelection<'a> {
    None,
    Unique(&'a SessionInfo),
    Ambiguous(Vec<&'a SessionInfo>),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str, created_at: Option<&str>) -> SessionInfo {
        SessionInfo {
            device_identity: "device-1".into(),
            device_name: "Car".into(),
            session_name: name.into(),
            shell: Some("/bin/bash".into()),
            state: "running".into(),
            created_at: created_at.map(str::to_owned),
            attachable: Some(true),
        }
    }

    #[test]
    fn authorization_requires_creation_time() {
        let session = session("bash", None);
        assert_eq!(
            classify_session(&session, &[], &[], None),
            SessionDecision::Unverifiable
        );
    }

    #[test]
    fn name_reuse_is_a_new_instance_and_is_not_inherited() {
        let old = session("bash", Some("2026-10-19T10:00:00Z"));
        let replacement = session("bash", Some("2026-10-20T10:00:00Z"));
        let authorized = vec![old.authorization_fingerprint().unwrap()];

        assert_eq!(
            classify_session(&replacement, &authorized, &[], None),
            SessionDecision::Prompt
        );
    }

    #[test]
    fn explicit_decline_is_sticky_for_the_exact_instance() {
        let session = session("bash", Some("2026-10-20T10:00:00Z"));
        let declined = vec![session.authorization_fingerprint().unwrap()];
        let baseline = HashSet::new();

        assert_eq!(
            classify_session(&session, &[], &declined, Some(&baseline)),
            SessionDecision::Declined
        );
    }

    #[test]
    fn a_new_terminal_is_implicitly_authorized_only_for_a_waiting_request() {
        let old = session("bash", Some("old"));
        let fresh = session("bash", Some("new"));
        let baseline = fingerprint_baseline([&old]);

        assert_eq!(
            classify_session(&fresh, &[], &[], Some(&baseline)),
            SessionDecision::AutoAuthorizeForPendingRequest
        );
        assert_eq!(
            classify_session(&fresh, &[], &[], None),
            SessionDecision::Prompt
        );
    }

    #[test]
    fn waiting_selection_never_guesses_when_multiple_devices_appear() {
        let mut other_device = session("bash-2", Some("other"));
        other_device.device_identity = "device-2".into();
        other_device.device_name = "Other".into();
        let one = session("bash-1", Some("one"));
        let baseline = HashSet::new();

        assert!(matches!(
            select_waiting_session([&one, &other_device], &baseline),
            WaitSelection::Ambiguous(candidates) if candidates.len() == 2
        ));
    }

    #[test]
    fn only_one_new_instance_matches_the_waiting_request() {
        let existing = session("old", Some("old"));
        let fresh = session("new", Some("new"));
        let baseline = fingerprint_baseline([&existing]);

        assert!(matches!(
            select_waiting_session([&existing, &fresh], &baseline),
            WaitSelection::Unique(candidate) if candidate.session_name == "new"
        ));
    }

    #[test]
    fn identical_fingerprints_are_counted_once() {
        let one = session("bash", Some("same"));
        let duplicate = one.clone();
        let baseline = HashSet::new();

        assert!(matches!(
            select_waiting_session([&one, &duplicate], &baseline),
            WaitSelection::Unique(_)
        ));
    }
}
