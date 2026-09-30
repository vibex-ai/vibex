//! The action risk model and the target-resolution rules.
//!
//! The browser's policy module answers "may this page be reached". Desktop
//! computer use has an almost disjoint risk surface: credentials, destructive
//! clicks, a human who is *also* using the window, and the runtime's own UI. So
//! this module is new rather than a copy — what it does borrow is the shape:
//! assess centrally, degrade explicitly, and never let a model-supplied string
//! decide what was approved.
//!
//! Two rules are load-bearing:
//!
//! 1. **The risk class comes from the runtime's own reading** of the canonical
//!    application identity, the accessibility element and the delivery mode. A
//!    model-supplied `app` string is resolved against the engine's own list and
//!    replaced by the canonical target before anything is acted on. Approving
//!    one identifier and acting on another is the failure this prevents.
//! 2. **The strictest applicable class wins.** A click that is both destructive
//!    and foreground is approved once, never for the session, even though
//!    neither field alone asked for that.

use std::collections::HashSet;

use vibex_core::{
    COMPUTER_CONCURRENT_ACTIVITY_MS, ComputerActionKind, ComputerApplication,
    ComputerApprovalGranularity, ComputerDeliveryMode, ComputerElement, ComputerRiskClass,
    ComputerRiskPolicy,
};

use crate::error::{ComputerError, ComputerResult, codes};

/// The runtime's own reading of an action.
#[derive(Debug, Clone)]
pub struct PolicyRequest {
    pub kind: ComputerActionKind,
    /// The canonical application the action resolved to.
    pub app: ComputerApplication,
    /// The element the action targets, when it targets one.
    pub element: Option<ComputerElement>,
    /// The label read off the element, used for the destructive-word scan.
    pub label: Option<String>,
    pub delivery: ComputerDeliveryMode,
    /// The action enters text, which makes a credential target a write rather
    /// than a read.
    pub writes_text: bool,
    /// Whether the target window is frontmost right now.
    pub target_is_frontmost: bool,
    /// How long ago the human produced input, when the engine knows.
    pub user_activity_age_ms: Option<i64>,
    /// Whether the runtime resolved the target to its own window or process.
    pub self_target: bool,
}

/// The decision, with everything the approval card and the ledger need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyOutcome {
    pub risk: ComputerRiskClass,
    pub policy: ComputerRiskPolicy,
    /// True when the human must answer before the action runs.
    pub requires_approval: bool,
    /// The strictest granularity across every applicable class.
    pub granularity: ComputerApprovalGranularity,
    /// A one-line reason, shown on the card and written to the ledger.
    pub reason: String,
    /// Every applicable class, so the card can list more than one.
    pub classes: Vec<ComputerRiskClass>,
}

impl PolicyOutcome {
    fn from_classes(classes: Vec<ComputerRiskClass>, reason: String) -> Self {
        // Strictest policy first: hard deny beats approval beats allowed.
        let policy = classes
            .iter()
            .map(|class| class.default_policy())
            .min_by_key(|policy| match policy {
                ComputerRiskPolicy::HardDeny => 0,
                ComputerRiskPolicy::RequiresApproval => 1,
                ComputerRiskPolicy::Allowed => 2,
            })
            .unwrap_or(ComputerRiskPolicy::Allowed);
        // Strictest granularity: once beats session beats none.
        let granularity = classes
            .iter()
            .map(|class| class.approval_granularity())
            .min_by_key(|granularity| match granularity {
                ComputerApprovalGranularity::Once => 0,
                ComputerApprovalGranularity::Session => 1,
                ComputerApprovalGranularity::None => 2,
            })
            .unwrap_or(ComputerApprovalGranularity::None);
        let risk = classes
            .iter()
            .copied()
            .min_by_key(risk_severity)
            .unwrap_or(ComputerRiskClass::Ordinary);
        Self {
            risk,
            policy,
            requires_approval: policy == ComputerRiskPolicy::RequiresApproval,
            granularity,
            reason,
            classes,
        }
    }
}

/// Orders risk classes from most to least severe.
fn risk_severity(class: &ComputerRiskClass) -> u8 {
    match class {
        ComputerRiskClass::CredentialTarget => 0,
        ComputerRiskClass::SelfTarget => 1,
        ComputerRiskClass::DestructiveClick => 2,
        ComputerRiskClass::FileDeletionOrShare => 3,
        ComputerRiskClass::ClipboardRead => 4,
        ComputerRiskClass::ForegroundEscalation => 5,
        ComputerRiskClass::KillApp => 6,
        ComputerRiskClass::ConcurrentUserActivity => 7,
        ComputerRiskClass::ClipboardWrite => 8,
        ComputerRiskClass::LaunchApp => 9,
        ComputerRiskClass::Ordinary => 10,
    }
}

/// Assesses one action against the risk model.
pub fn assess(request: &PolicyRequest) -> PolicyOutcome {
    let mut classes = Vec::new();
    let mut reasons = Vec::new();
    let identity = request.app.canonical_identity();

    if request.self_target {
        classes.push(ComputerRiskClass::SelfTarget);
        reasons.push("the target is Vibex's own window".to_string());
    }

    if vibex_core::is_credential_application(&identity) {
        classes.push(ComputerRiskClass::CredentialTarget);
        reasons.push(format!(
            "{} is a credential store",
            request.app.display_name
        ));
    }
    if request
        .element
        .as_ref()
        .is_some_and(|element| element.secure || vibex_core::is_secure_field_role(&element.role))
    {
        classes.push(ComputerRiskClass::CredentialTarget);
        reasons.push("the target is a secure text field".to_string());
    }

    match request.kind {
        ComputerActionKind::ClipboardRead => {
            classes.push(ComputerRiskClass::ClipboardRead);
            reasons.push("reading the clipboard can expose a password".to_string());
        }
        ComputerActionKind::ClipboardWrite => {
            classes.push(ComputerRiskClass::ClipboardWrite);
            reasons.push("this replaces what the user has copied".to_string());
        }
        ComputerActionKind::LaunchApp => {
            classes.push(ComputerRiskClass::LaunchApp);
            reasons.push(format!("start {} on the desktop", request.app.display_name));
        }
        ComputerActionKind::KillApp => {
            classes.push(ComputerRiskClass::KillApp);
            reasons.push(format!("quit {}", request.app.display_name));
        }
        ComputerActionKind::Click => {
            let label = request
                .label
                .clone()
                .or_else(|| request.element.as_ref().map(|element| element.name.clone()))
                .unwrap_or_default();
            if vibex_core::is_destructive_action_label(&label) {
                classes.push(ComputerRiskClass::DestructiveClick);
                reasons.push(format!("the control is labelled “{label}”"));
            }
            if let Some(element) = &request.element
                && is_file_deletion_control(&element.role, &element.name)
            {
                classes.push(ComputerRiskClass::FileDeletionOrShare);
                reasons.push("this deletes or externalises a file".to_string());
            }
        }
        ComputerActionKind::TypeText | ComputerActionKind::SetValue
            if vibex_core::is_credential_application(&identity) =>
        {
            // Already covered above; kept explicit so a future edit that drops
            // the identity check does not silently allow typed credentials.
            classes.push(ComputerRiskClass::CredentialTarget);
        }
        _ => {}
    }

    if request.delivery == ComputerDeliveryMode::Foreground {
        classes.push(ComputerRiskClass::ForegroundEscalation);
        reasons.push("this briefly takes over the foreground".to_string());
    }

    if request.target_is_frontmost
        && request
            .user_activity_age_ms
            .is_some_and(|age| age < COMPUTER_CONCURRENT_ACTIVITY_MS)
    {
        classes.push(ComputerRiskClass::ConcurrentUserActivity);
        reasons.push("the user appears to be using this window right now".to_string());
    }

    if classes.is_empty() {
        classes.push(ComputerRiskClass::Ordinary);
        reasons.push("an ordinary desktop action".to_string());
    }
    let reason = reasons.join("; ");
    PolicyOutcome::from_classes(classes, reason)
}

/// File-manager and share-sheet controls whose label names a file operation.
fn is_file_deletion_control(role: &str, name: &str) -> bool {
    let role = role.to_ascii_lowercase();
    let name = name.to_ascii_lowercase();
    let destructive = [
        "delete",
        "move to trash",
        "empty trash",
        "删除",
        "移到废纸篓",
        "清空",
    ];
    let externalising = ["upload", "share", "attach", "上传", "分享", "作为附件"];
    let file_context = role.contains("file")
        || role.contains("outline")
        || name.contains("file")
        || name.contains("folder")
        || name.contains("文件")
        || name.contains("文件夹");
    (file_context
        && destructive
            .iter()
            .chain(externalising.iter())
            .any(|word| name.contains(word)))
        || destructive.iter().any(|word| name.contains(word))
            && name.split_whitespace().count() <= 3
}

/// Refuses an action the policy hard-denies, with the class in the code.
pub fn enforce(outcome: &PolicyOutcome, app: &ComputerApplication) -> ComputerResult<()> {
    match outcome.risk {
        ComputerRiskClass::CredentialTarget => Err(ComputerError::permission(
            codes::CREDENTIAL_TARGET,
            format!(
                "Vibex never types into or reads from {}. Use the password manager yourself.",
                app.display_name
            ),
        )),
        ComputerRiskClass::SelfTarget => Err(ComputerError::permission(
            codes::SELF_TARGET,
            "The Agent may not operate Vibex's own window.",
        )),
        _ if outcome.policy == ComputerRiskPolicy::HardDeny => Err(ComputerError::permission(
            codes::HARD_DENIED,
            format!("`{}` is refused by policy", outcome.risk.as_str()),
        )),
        _ => Ok(()),
    }
}

/// Resolves a model-supplied application selector against the engine's own list.
///
/// The comparison is exact on the canonical id first, then on an unambiguous
/// display name. An ambiguous name is an error rather than a coin flip:
/// approving "Mail" and acting on a different Mail is precisely the failure the
/// canonical-target rule exists to prevent.
pub fn resolve_target(
    apps: &[ComputerApplication],
    selector: &str,
) -> ComputerResult<ComputerApplication> {
    let selector = selector.trim();
    if selector.is_empty() {
        return Err(ComputerError::validation(
            codes::UNKNOWN_APP,
            "`app` is required",
        ));
    }
    if let Some(app) = apps.iter().find(|app| app.app_id == selector) {
        return Ok(app.clone());
    }
    if let Some(app) = apps.iter().find(|app| {
        app.bundle_id
            .as_deref()
            .is_some_and(|bundle| bundle == selector)
    }) {
        return Ok(app.clone());
    }
    let lowered = selector.to_ascii_lowercase();
    let mut matches: Vec<&ComputerApplication> = apps
        .iter()
        .filter(|app| app.display_name.to_ascii_lowercase() == lowered)
        .collect();
    if matches.is_empty() {
        matches = apps
            .iter()
            .filter(|app| {
                app.executable_path
                    .as_deref()
                    .is_some_and(|path| path == selector)
            })
            .collect();
    }
    match matches.len() {
        0 => Err(ComputerError::validation(
            codes::UNKNOWN_APP,
            format!("no application matches `{selector}`"),
        )
        .with_recovery_hint("Call computer_list_apps and use the app id it reports.")),
        1 => Ok(matches[0].clone()),
        _ => Err(ComputerError::validation(
            codes::UNKNOWN_APP,
            format!("`{selector}` matches more than one application"),
        )
        .with_recovery_hint("Use the application id rather than its display name.")),
    }
}

/// Session-scoped approvals for the classes that may be remembered.
///
/// A one-off approval never enters this store: it travels with the retry that
/// carries it, exactly like the browser's origin approval. The store only ever
/// holds [`ComputerApprovalGranularity::Session`] classes, and
/// [`ComputerRiskClass::can_be_remembered`] is what decides that.
#[derive(Debug, Default)]
pub struct GrantStore {
    grants: HashSet<(ComputerRiskClass, String)>,
}

impl GrantStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(risk: ComputerRiskClass, target: &ComputerApplication) -> (ComputerRiskClass, String) {
        (risk, target.canonical_identity())
    }

    /// True when this class and target were approved for the session.
    pub fn is_granted(&self, risk: ComputerRiskClass, target: &ComputerApplication) -> bool {
        risk.can_be_remembered() && self.grants.contains(&Self::key(risk, target))
    }

    /// Remembers a session approval. Refuses classes that may not be remembered.
    pub fn remember(&mut self, risk: ComputerRiskClass, target: &ComputerApplication) -> bool {
        if !risk.can_be_remembered() {
            return false;
        }
        self.grants.insert(Self::key(risk, target))
    }

    pub fn clear(&mut self) {
        self.grants.clear();
    }

    pub fn len(&self) -> usize {
        self.grants.len()
    }

    pub fn is_empty(&self) -> bool {
        self.grants.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{ComputerRect, ComputerRiskClass};

    fn app(app_id: &str, display_name: &str) -> ComputerApplication {
        ComputerApplication {
            app_id: app_id.to_string(),
            display_name: display_name.to_string(),
            executable_path: Some(format!("/usr/bin/{app_id}")),
            bundle_id: Some(app_id.to_string()),
            running: true,
            pid: Some(1),
            windows: Vec::new(),
        }
    }

    fn element(role: &str, name: &str) -> ComputerElement {
        ComputerElement {
            reference: "c1-3".to_string(),
            role: role.to_string(),
            name: name.to_string(),
            value: None,
            editable: false,
            secure: false,
            disabled: false,
            bounds: Some(ComputerRect {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            }),
        }
    }

    fn request(kind: ComputerActionKind) -> PolicyRequest {
        PolicyRequest {
            kind,
            app: app("com.example.notes", "Notes"),
            element: None,
            label: None,
            delivery: ComputerDeliveryMode::Background,
            writes_text: false,
            target_is_frontmost: false,
            user_activity_age_ms: Some(60_000),
            self_target: false,
        }
    }

    #[test]
    fn an_ordinary_click_needs_no_approval() {
        let mut request = request(ComputerActionKind::Click);
        request.element = Some(element("AXButton", "Save"));
        let outcome = assess(&request);
        assert_eq!(outcome.risk, ComputerRiskClass::Ordinary);
        assert!(!outcome.requires_approval);
    }

    #[test]
    fn a_destructive_label_is_approved_once_and_never_remembered() {
        let mut request = request(ComputerActionKind::Click);
        request.element = Some(element("AXButton", "Send"));
        let outcome = assess(&request);
        assert_eq!(outcome.risk, ComputerRiskClass::DestructiveClick);
        assert!(outcome.requires_approval);
        assert_eq!(outcome.granularity, ComputerApprovalGranularity::Once);
        assert!(!outcome.risk.can_be_remembered());
    }

    #[test]
    fn a_credential_application_is_hard_denied_with_no_card() {
        let mut request = request(ComputerActionKind::TypeText);
        request.app = app("com.1password.1password", "1Password");
        request.writes_text = true;
        let outcome = assess(&request);
        assert_eq!(outcome.risk, ComputerRiskClass::CredentialTarget);
        assert_eq!(outcome.policy, ComputerRiskPolicy::HardDeny);
        assert!(!outcome.requires_approval);
        let error = enforce(&outcome, &request.app).unwrap_err();
        assert_eq!(error.code, codes::CREDENTIAL_TARGET);
        assert!(!error.is_approval_required());
    }

    #[test]
    fn a_secure_field_is_hard_denied_even_in_an_ordinary_application() {
        let mut request = request(ComputerActionKind::SetValue);
        request.element = Some(element("AXSecureTextField", "Password"));
        let outcome = assess(&request);
        assert_eq!(outcome.risk, ComputerRiskClass::CredentialTarget);
        assert_eq!(outcome.policy, ComputerRiskPolicy::HardDeny);
    }

    #[test]
    fn the_runtime_never_drives_its_own_window() {
        let mut request = request(ComputerActionKind::Click);
        request.self_target = true;
        request.element = Some(element("AXButton", "Save"));
        let outcome = assess(&request);
        assert_eq!(outcome.risk, ComputerRiskClass::SelfTarget);
        let error = enforce(&outcome, &request.app).unwrap_err();
        assert_eq!(error.code, codes::SELF_TARGET);
    }

    #[test]
    fn foreground_and_destructive_together_stay_one_off() {
        let mut request = request(ComputerActionKind::Click);
        request.element = Some(element("AXButton", "Pay now"));
        request.delivery = ComputerDeliveryMode::Foreground;
        let outcome = assess(&request);
        assert!(
            outcome
                .classes
                .contains(&ComputerRiskClass::DestructiveClick)
        );
        assert!(
            outcome
                .classes
                .contains(&ComputerRiskClass::ForegroundEscalation)
        );
        assert_eq!(outcome.granularity, ComputerApprovalGranularity::Once);
        assert!(outcome.requires_approval);
    }

    #[test]
    fn a_human_typing_in_the_target_window_pauses_the_action() {
        let mut request = request(ComputerActionKind::Click);
        request.element = Some(element("AXButton", "Save"));
        request.target_is_frontmost = true;
        request.user_activity_age_ms = Some(100);
        let outcome = assess(&request);
        assert_eq!(outcome.risk, ComputerRiskClass::ConcurrentUserActivity);
        assert!(outcome.requires_approval);
        assert_eq!(outcome.granularity, ComputerApprovalGranularity::Once);
        assert!(outcome.reason.contains("right now"));
    }

    #[test]
    fn stale_activity_does_not_count_as_concurrent() {
        let mut request = request(ComputerActionKind::Click);
        request.element = Some(element("AXButton", "Save"));
        request.target_is_frontmost = true;
        request.user_activity_age_ms = Some(COMPUTER_CONCURRENT_ACTIVITY_MS + 1);
        assert_eq!(assess(&request).risk, ComputerRiskClass::Ordinary);
    }

    #[test]
    fn launching_may_be_remembered_but_quitting_may_not() {
        let launch = assess(&request(ComputerActionKind::LaunchApp));
        assert_eq!(launch.granularity, ComputerApprovalGranularity::Session);
        assert!(launch.risk.can_be_remembered());
        let kill = assess(&request(ComputerActionKind::KillApp));
        assert_eq!(kill.granularity, ComputerApprovalGranularity::Once);
        assert!(!kill.risk.can_be_remembered());
    }

    #[test]
    fn ambiguous_targets_are_refused_rather_than_guessed() {
        let apps = vec![
            app("com.example.mail", "Mail"),
            app("com.other.mail", "Mail"),
            app("com.example.notes", "Notes"),
        ];
        assert_eq!(
            resolve_target(&apps, "com.example.mail").unwrap().app_id,
            "com.example.mail"
        );
        assert_eq!(
            resolve_target(&apps, "Notes").unwrap().app_id,
            "com.example.notes"
        );
        assert!(resolve_target(&apps, "Mail").is_err());
        assert!(resolve_target(&apps, "nope").is_err());
        assert!(resolve_target(&apps, "  ").is_err());
    }

    #[test]
    fn the_grant_store_only_remembers_what_may_be_remembered() {
        let target = app("com.example.notes", "Notes");
        let mut store = GrantStore::new();
        assert!(!store.is_granted(ComputerRiskClass::LaunchApp, &target));
        assert!(store.remember(ComputerRiskClass::LaunchApp, &target));
        assert!(store.is_granted(ComputerRiskClass::LaunchApp, &target));
        // A different target is a different decision.
        let other = app("com.example.mail", "Mail");
        assert!(!store.is_granted(ComputerRiskClass::LaunchApp, &other));
        // Destructive classes can never be remembered.
        assert!(!store.remember(ComputerRiskClass::DestructiveClick, &target));
        assert!(!store.is_granted(ComputerRiskClass::DestructiveClick, &target));
        assert!(!store.remember(ComputerRiskClass::ForegroundEscalation, &target));
        store.clear();
        assert!(store.is_empty());
    }

    #[test]
    fn a_file_deletion_control_is_its_own_class() {
        let mut request = request(ComputerActionKind::Click);
        request.element = Some(element("AXRow", "Delete file"));
        let outcome = assess(&request);
        assert!(
            outcome
                .classes
                .contains(&ComputerRiskClass::FileDeletionOrShare)
        );
    }
}
