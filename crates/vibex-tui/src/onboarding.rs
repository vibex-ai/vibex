//! First-run guidance: what to do first, in order.
//!
//! The order is the product decision, and it is fixed here rather than
//! scattered through the welcome screen: connect, choose where the Agent works,
//! start a session, write the first message. Each step's state is *derived*
//! from what the client already knows, so there is no "seen the tour" flag to
//! write, read back, or get out of sync. A step is done because it is done.
//!
//! That also means the guide disappears on its own: once all four steps are
//! complete the welcome screen is just a welcome screen again. Nothing has to
//! be dismissed, and a reader who reinstalls into an empty home gets the guide
//! back, which is exactly when it is useful.

use crate::app::App;
use crate::locale::Strings;

/// One step of the first-run order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OnboardingStep {
    Connect,
    Workspace,
    Session,
    FirstMessage,
}

impl OnboardingStep {
    /// Every step, in the order they should be done.
    pub const ALL: [OnboardingStep; 4] = [
        OnboardingStep::Connect,
        OnboardingStep::Workspace,
        OnboardingStep::Session,
        OnboardingStep::FirstMessage,
    ];

    pub fn label(self, strings: Strings) -> &'static str {
        match self {
            OnboardingStep::Connect => strings.onboarding_connect(),
            OnboardingStep::Workspace => strings.onboarding_workspace(),
            OnboardingStep::Session => strings.onboarding_session(),
            OnboardingStep::FirstMessage => strings.onboarding_first_message(),
        }
    }

    pub fn detail(self, strings: Strings) -> &'static str {
        match self {
            OnboardingStep::Connect => strings.onboarding_connect_detail(),
            OnboardingStep::Workspace => strings.onboarding_workspace_detail(),
            OnboardingStep::Session => strings.onboarding_session_detail(),
            OnboardingStep::FirstMessage => strings.onboarding_first_message_detail(),
        }
    }

    /// The key that advances the step, when a key does.
    pub const fn key(self) -> &'static str {
        match self {
            // Connection is the runtime's job; the client waits for it.
            OnboardingStep::Connect => "",
            OnboardingStep::Workspace => "b",
            OnboardingStep::Session => "n",
            OnboardingStep::FirstMessage => "i",
        }
    }
}

/// A step with the state the welcome screen draws it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardingProgress {
    pub step: OnboardingStep,
    pub done: bool,
    /// The first step that is not done.
    pub current: bool,
}

impl App {
    /// The first-run order with each step's state, in order.
    pub fn onboarding(&self) -> Vec<OnboardingProgress> {
        let done = |step: OnboardingStep| self.onboarding_step_done(step);
        let current = OnboardingStep::ALL.into_iter().find(|step| !done(*step));
        OnboardingStep::ALL
            .into_iter()
            .map(|step| OnboardingProgress {
                step,
                done: done(step),
                current: current == Some(step),
            })
            .collect()
    }

    /// Whether one step is complete.
    pub fn onboarding_step_done(&self, step: OnboardingStep) -> bool {
        let sessions = self
            .agent
            .state
            .sessions
            .value
            .as_deref()
            .unwrap_or_default();
        match step {
            OnboardingStep::Connect => self.live.is_live(),
            OnboardingStep::Workspace => self.workspace_path.is_some() || !sessions.is_empty(),
            OnboardingStep::Session => !sessions.is_empty() || self.active_session().is_some(),
            OnboardingStep::FirstMessage => {
                self.active_session().is_some() && !self.transcript.is_empty()
            }
        }
    }

    /// The step the reader should do next, when there is one.
    pub fn onboarding_current(&self) -> Option<OnboardingStep> {
        OnboardingStep::ALL
            .into_iter()
            .find(|step| !self.onboarding_step_done(*step))
    }

    /// Whether every step is done, so the guide can get out of the way.
    pub fn onboarding_complete(&self) -> bool {
        self.onboarding_current().is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_order_is_the_product_decision() {
        // Pinned because the order is the whole point of the module.
        assert_eq!(
            OnboardingStep::ALL,
            [
                OnboardingStep::Connect,
                OnboardingStep::Workspace,
                OnboardingStep::Session,
                OnboardingStep::FirstMessage,
            ]
        );
        // Every step but connection is advanced by a key the welcome screen
        // can advertise.
        for step in OnboardingStep::ALL {
            if step != OnboardingStep::Connect {
                assert!(!step.key().is_empty(), "{step:?} has no key");
            }
        }
    }

    #[test]
    fn every_step_has_copy_in_every_locale() {
        for locale in [
            crate::locale::Locale::En,
            crate::locale::Locale::ZhCn,
            crate::locale::Locale::ZhTw,
        ] {
            let strings = Strings::for_locale(locale);
            for step in OnboardingStep::ALL {
                assert!(!step.label(strings).is_empty(), "{step:?} label");
                assert!(!step.detail(strings).is_empty(), "{step:?} detail");
            }
        }
    }
}
