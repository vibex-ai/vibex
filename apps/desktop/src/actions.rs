use gpui::Action;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct ToggleSidebar;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct TogglePreview;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct ToggleRightRail;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct ToggleComposerMode;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct OpenSettings;

/// Opens the runtime manager panel anchored beside the title bar's runtime
/// button.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct OpenRuntimeManager;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct OpenConversationFind;

/// Opens the command palette, or closes it when it is already showing.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct OpenCommandPalette;

/// Starts a new Agent session.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct NewSession;

/// Opens the mobile device pairing flow.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct PairMobileDevice;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct RetryRuntime;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct SaveActiveFile;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct GoToLineInEditor;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct NavigateBack;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct NavigateForward;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct UndoImageEdit;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct RedoImageEdit;
