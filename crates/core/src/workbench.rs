use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchPanel {
    Agent,
    Files,
    Git,
    Terminal,
    Providers,
    Details,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkbenchTabKind {
    Agent,
    Scheduled,
    Automation,
    Editor,
    GitDiff,
    Terminal,
    Providers,
    Preview,
}
