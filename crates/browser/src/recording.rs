//! Operation recording and Playwright export.
//!
//! Recording is an explicit, user-started mode. The audit ledger stays redacted
//! at all times; the recorder is the one place that keeps raw form values, and
//! only until the user exports or discards them.
//!
//! Exported tests locate elements by **role and accessible name**, never by CSS
//! selector or coordinates. A `ref` is valid for one generation only, so the
//! recorder has to capture the role and name at record time or the export is
//! worthless.

use vibex_core::{BrowserActionKind, BrowserRecordingStep};

/// Largest number of steps kept in a recording.
pub const MAX_RECORDING_STEPS: usize = 500;

/// Escapes a string for a single-quoted TypeScript literal.
fn escape_single_quoted(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Renders a `getByRole(...)` locator, falling back to `getByText` when the
/// element had no usable role.
fn locator(step: &BrowserRecordingStep) -> Option<String> {
    let name = step.name.as_deref().unwrap_or_default();
    match step.role.as_deref() {
        Some(role) if !role.is_empty() && !name.is_empty() => Some(format!(
            "page.getByRole('{}', {{ name: '{}' }})",
            escape_single_quoted(role),
            escape_single_quoted(name)
        )),
        _ if !name.is_empty() => Some(format!("page.getByText('{}')", escape_single_quoted(name))),
        _ => None,
    }
}

/// Converts one recorded step into Playwright source.
///
/// Returns `None` when the step cannot be expressed stably (for example a click
/// on an element that had neither role nor name).
pub fn step_to_playwright(step: &BrowserRecordingStep) -> Option<String> {
    match step.kind {
        BrowserActionKind::Navigate => Some(format!(
            "await page.goto('{}');",
            escape_single_quoted(step.url.as_deref().unwrap_or("about:blank"))
        )),
        BrowserActionKind::Click => {
            locator(step).map(|locator| format!("await {locator}.click();"))
        }
        BrowserActionKind::Fill => {
            let locator = locator(step)?;
            let value = escape_single_quoted(step.value.as_deref().unwrap_or_default());
            Some(format!("await {locator}.fill('{value}');"))
        }
        BrowserActionKind::Press => Some(format!(
            "await page.keyboard.press('{}');",
            escape_single_quoted(step.key.as_deref().unwrap_or("Enter"))
        )),
        BrowserActionKind::Hover => {
            locator(step).map(|locator| format!("await {locator}.hover();"))
        }
        BrowserActionKind::SelectOption => {
            let locator = locator(step)?;
            let value = escape_single_quoted(step.value.as_deref().unwrap_or_default());
            Some(format!("await {locator}.selectOption('{value}');"))
        }
        BrowserActionKind::Scroll => Some(format!(
            "await page.mouse.wheel(0, {});",
            step.delta_y.unwrap_or(0)
        )),
        BrowserActionKind::WaitFor => {
            let condition = step.condition.as_deref().unwrap_or_default();
            if let Some(url) = condition.strip_prefix("url:") {
                return Some(format!(
                    "await page.waitForURL('{}');",
                    escape_single_quoted(url.trim())
                ));
            }
            if let Some(selector) = condition.strip_prefix("selector:") {
                return Some(format!(
                    "await page.waitForSelector('{}');",
                    escape_single_quoted(selector.trim())
                ));
            }
            if let Some(text) = condition.strip_prefix("text:") {
                return Some(format!(
                    "await page.getByText('{}').waitFor();",
                    escape_single_quoted(text.trim())
                ));
            }
            None
        }
        // Raw JavaScript is functionally correct but unreadable; the export
        // marks it explicitly so a reviewer knows to clean it up.
        BrowserActionKind::Evaluate => step.script.as_deref().map(|script| {
            format!(
                "// NOTE: recorded browser_evaluate — consider replacing with a semantic locator.\nawait page.evaluate(() => {{ {script} }});"
            )
        }),
        _ => None,
    }
}

/// Renders a full Playwright test module for a recording.
pub fn export_playwright(test_name: &str, steps: &[BrowserRecordingStep]) -> String {
    let mut body = String::new();
    let mut skipped = 0usize;
    for step in steps.iter().take(MAX_RECORDING_STEPS) {
        match step_to_playwright(step) {
            Some(line) => {
                for line in line.lines() {
                    body.push_str("    ");
                    body.push_str(line);
                    body.push('\n');
                }
            }
            None => skipped += 1,
        }
    }
    if skipped > 0 {
        body.push_str(&format!(
            "    // {skipped} recorded action(s) could not be exported with a stable locator and were skipped.\n"
        ));
    }
    let safe_name = test_name.replace('\'', "\\'");
    format!(
        "// Generated by Vibex from a recorded browser session.\n\
         // Locators use role and accessible name, so they survive markup changes.\n\
         import {{ test, expect }} from '@playwright/test';\n\n\
         test('{safe_name}', async ({{ page }}) => {{\n{body}}});\n"
    )
}

/// Strips values that must never reach the audit ledger.
///
/// The ledger records *that* a field was filled, never what was typed into it.
pub fn redacted_step_summary(step: &BrowserRecordingStep) -> String {
    match step.kind {
        BrowserActionKind::Fill => match step.name.as_deref() {
            Some(name) if !name.is_empty() => format!("filled `{name}` (value redacted)"),
            _ => "filled a field (value redacted)".to_string(),
        },
        BrowserActionKind::Navigate => match step.url.as_deref() {
            Some(url) => format!("navigated to {}", vibex_core::redact_url_for_ledger(url)),
            None => "navigated".to_string(),
        },
        BrowserActionKind::Click => match step.name.as_deref() {
            Some(name) if !name.is_empty() => format!("clicked `{name}`"),
            _ => "clicked an element".to_string(),
        },
        other => other.as_str().replace('_', " "),
    }
}

/// In-memory recording buffer. Values live here and nowhere else.
#[derive(Debug, Default)]
pub struct BrowserRecorder {
    active: bool,
    steps: Vec<BrowserRecordingStep>,
}

impl BrowserRecorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&mut self) {
        self.active = true;
        self.steps.clear();
    }

    pub fn stop(&mut self) -> Vec<BrowserRecordingStep> {
        self.active = false;
        std::mem::take(&mut self.steps)
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    pub fn record(&mut self, step: BrowserRecordingStep) {
        if !self.active {
            return;
        }
        self.steps.push(step);
        if self.steps.len() > MAX_RECORDING_STEPS {
            self.steps.remove(0);
        }
    }

    pub fn steps(&self) -> &[BrowserRecordingStep] {
        &self.steps
    }

    /// Discards recorded values without exporting.
    pub fn discard(&mut self) {
        self.active = false;
        self.steps.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(role: &str, name: &str) -> BrowserRecordingStep {
        BrowserRecordingStep {
            kind: BrowserActionKind::Click,
            role: Some(role.to_string()),
            name: Some(name.to_string()),
            url: None,
            value: None,
            key: None,
            delta_y: None,
            condition: None,
            script: None,
        }
    }

    #[test]
    fn clicks_export_as_role_locators() {
        let line = step_to_playwright(&click("button", "Submit")).unwrap();
        assert_eq!(
            line,
            "await page.getByRole('button', { name: 'Submit' }).click();"
        );
    }

    #[test]
    fn clicks_without_a_role_fall_back_to_text() {
        let mut step = click("", "Sign in");
        step.role = None;
        let line = step_to_playwright(&step).unwrap();
        assert_eq!(line, "await page.getByText('Sign in').click();");
    }

    #[test]
    fn a_click_with_no_locator_at_all_is_not_exported() {
        let mut step = click("", "");
        step.role = None;
        step.name = None;
        assert!(step_to_playwright(&step).is_none());
    }

    #[test]
    fn fill_values_are_escaped() {
        let step = BrowserRecordingStep {
            kind: BrowserActionKind::Fill,
            role: Some("textbox".to_string()),
            name: Some("Email".to_string()),
            value: Some("o'brien\\x\nline".to_string()),
            url: None,
            key: None,
            delta_y: None,
            condition: None,
            script: None,
        };
        let line = step_to_playwright(&step).unwrap();
        assert!(line.contains("o\\'brien\\\\x\\nline"));
    }

    #[test]
    fn navigation_press_scroll_and_select_are_exported() {
        let navigate = BrowserRecordingStep {
            kind: BrowserActionKind::Navigate,
            url: Some("http://localhost:5173/".to_string()),
            role: None,
            name: None,
            value: None,
            key: None,
            delta_y: None,
            condition: None,
            script: None,
        };
        assert_eq!(
            step_to_playwright(&navigate).unwrap(),
            "await page.goto('http://localhost:5173/');"
        );

        let press = BrowserRecordingStep {
            kind: BrowserActionKind::Press,
            key: Some("Enter".to_string()),
            role: None,
            name: None,
            url: None,
            value: None,
            delta_y: None,
            condition: None,
            script: None,
        };
        assert_eq!(
            step_to_playwright(&press).unwrap(),
            "await page.keyboard.press('Enter');"
        );

        let scroll = BrowserRecordingStep {
            kind: BrowserActionKind::Scroll,
            delta_y: Some(-400),
            role: None,
            name: None,
            url: None,
            value: None,
            key: None,
            condition: None,
            script: None,
        };
        assert_eq!(
            step_to_playwright(&scroll).unwrap(),
            "await page.mouse.wheel(0, -400);"
        );

        let select = BrowserRecordingStep {
            kind: BrowserActionKind::SelectOption,
            role: Some("combobox".to_string()),
            name: Some("Country".to_string()),
            value: Some("JP".to_string()),
            url: None,
            key: None,
            delta_y: None,
            condition: None,
            script: None,
        };
        assert_eq!(
            step_to_playwright(&select).unwrap(),
            "await page.getByRole('combobox', { name: 'Country' }).selectOption('JP');"
        );
    }

    #[test]
    fn wait_for_conditions_map_to_the_right_playwright_call() {
        let wait = |condition: &str| BrowserRecordingStep {
            kind: BrowserActionKind::WaitFor,
            condition: Some(condition.to_string()),
            role: None,
            name: None,
            url: None,
            value: None,
            key: None,
            delta_y: None,
            script: None,
        };
        assert_eq!(
            step_to_playwright(&wait("url:https://a.test/done")).unwrap(),
            "await page.waitForURL('https://a.test/done');"
        );
        assert_eq!(
            step_to_playwright(&wait("selector:#ready")).unwrap(),
            "await page.waitForSelector('#ready');"
        );
        assert_eq!(
            step_to_playwright(&wait("text:Loaded")).unwrap(),
            "await page.getByText('Loaded').waitFor();"
        );
        assert!(step_to_playwright(&wait("nonsense")).is_none());
    }

    #[test]
    fn evaluate_is_exported_with_a_warning_comment() {
        let step = BrowserRecordingStep {
            kind: BrowserActionKind::Evaluate,
            script: Some("document.title".to_string()),
            role: None,
            name: None,
            url: None,
            value: None,
            key: None,
            delta_y: None,
            condition: None,
        };
        let line = step_to_playwright(&step).unwrap();
        assert!(line.starts_with("// NOTE:"));
        assert!(line.contains("await page.evaluate"));
    }

    #[test]
    fn export_reports_steps_it_had_to_skip() {
        let mut unlocatable = click("", "");
        unlocatable.role = None;
        unlocatable.name = None;
        let source = export_playwright("login flow", &[click("button", "Sign in"), unlocatable]);
        assert!(source.contains("test('login flow'"));
        assert!(source.contains("could not be exported"));
        assert!(source.contains("getByRole('button', { name: 'Sign in' })"));
    }

    #[test]
    fn ledger_summaries_never_contain_filled_values() {
        let step = BrowserRecordingStep {
            kind: BrowserActionKind::Fill,
            role: Some("textbox".to_string()),
            name: Some("Password".to_string()),
            value: Some("hunter2".to_string()),
            url: None,
            key: None,
            delta_y: None,
            condition: None,
            script: None,
        };
        let summary = redacted_step_summary(&step);
        assert!(!summary.contains("hunter2"));
        assert!(summary.contains("Password"));
        assert!(summary.contains("redacted"));
    }

    #[test]
    fn navigate_summaries_drop_query_strings() {
        let step = BrowserRecordingStep {
            kind: BrowserActionKind::Navigate,
            url: Some("https://example.com/a?token=secret".to_string()),
            role: None,
            name: None,
            value: None,
            key: None,
            delta_y: None,
            condition: None,
            script: None,
        };
        let summary = redacted_step_summary(&step);
        assert!(!summary.contains("secret"));
        assert!(summary.contains("https://example.com/a"));
    }

    #[test]
    fn recorder_ignores_steps_while_stopped_and_caps_the_buffer() {
        let mut recorder = BrowserRecorder::new();
        recorder.record(click("button", "Ignored"));
        assert!(recorder.steps().is_empty());

        recorder.start();
        recorder.record(click("button", "One"));
        assert_eq!(recorder.steps().len(), 1);
        let steps = recorder.stop();
        assert_eq!(steps.len(), 1);
        assert!(!recorder.is_active());
        assert!(recorder.steps().is_empty());
    }

    #[test]
    fn recorder_discard_clears_values() {
        let mut recorder = BrowserRecorder::new();
        recorder.start();
        recorder.record(click("button", "One"));
        recorder.discard();
        assert!(recorder.steps().is_empty());
        assert!(!recorder.is_active());
    }
}
