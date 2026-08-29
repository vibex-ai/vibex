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

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct OpenConversationFind;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct RetryRuntime;

#[derive(Action, Clone, PartialEq)]
#[action(namespace = vibex, no_json)]
pub struct SaveActiveFile;

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
