use std::{collections::BTreeSet, rc::Rc};

use gpui::{App, SharedString, Window};
use gpui_component::{WindowExt as _, button::ButtonVariant, dialog::DialogButtonProps};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FoundationPrimitive {
    Notification,
    Dialog,
    Drawer,
    Menu,
    ContextMenu,
    Popover,
    Tooltip,
    Select,
    InputForm,
    LoadingErrorRetry,
    DestructiveConfirmation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutsideClickBehavior {
    ConfigurableDismiss,
    Dismiss,
    Ignored,
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimitiveContract {
    pub primitive: FoundationPrimitive,
    pub backend: &'static str,
    pub keyboard_navigation: bool,
    pub focus_trap: bool,
    pub escape_dismiss: bool,
    pub outside_click: OutsideClickBehavior,
    pub return_focus: bool,
    pub disabled_state: bool,
    pub overlay: bool,
    pub accessible_name_and_state: bool,
    pub explicit_confirmation: bool,
}

pub const FOUNDATION_PRIMITIVE_CONTRACTS: [PrimitiveContract; 11] = [
    PrimitiveContract {
        primitive: FoundationPrimitive::Notification,
        backend: "gpui_component::notification::Notification",
        keyboard_navigation: true,
        focus_trap: false,
        escape_dismiss: false,
        outside_click: OutsideClickBehavior::NotApplicable,
        return_focus: false,
        disabled_state: true,
        overlay: true,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::Dialog,
        backend: "gpui_component::dialog::Dialog",
        keyboard_navigation: true,
        focus_trap: true,
        escape_dismiss: true,
        outside_click: OutsideClickBehavior::ConfigurableDismiss,
        return_focus: true,
        disabled_state: true,
        overlay: true,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::Drawer,
        backend: "gpui_component::sheet::Sheet",
        keyboard_navigation: true,
        focus_trap: true,
        escape_dismiss: true,
        outside_click: OutsideClickBehavior::ConfigurableDismiss,
        return_focus: true,
        disabled_state: true,
        overlay: true,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::Menu,
        backend: "gpui_component::menu::PopupMenu",
        keyboard_navigation: true,
        focus_trap: false,
        escape_dismiss: true,
        outside_click: OutsideClickBehavior::Dismiss,
        return_focus: true,
        disabled_state: true,
        overlay: true,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::ContextMenu,
        backend: "gpui_component::menu::ContextMenu",
        keyboard_navigation: true,
        focus_trap: false,
        escape_dismiss: true,
        outside_click: OutsideClickBehavior::Dismiss,
        return_focus: true,
        disabled_state: true,
        overlay: true,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::Popover,
        backend: "gpui_component::popover::Popover",
        keyboard_navigation: true,
        focus_trap: false,
        escape_dismiss: true,
        outside_click: OutsideClickBehavior::ConfigurableDismiss,
        return_focus: true,
        disabled_state: true,
        overlay: true,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::Tooltip,
        backend: "gpui_component::tooltip::Tooltip",
        keyboard_navigation: false,
        focus_trap: false,
        escape_dismiss: false,
        outside_click: OutsideClickBehavior::NotApplicable,
        return_focus: false,
        disabled_state: false,
        overlay: true,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::Select,
        backend: "gpui_component::select::Select",
        keyboard_navigation: true,
        focus_trap: false,
        escape_dismiss: true,
        outside_click: OutsideClickBehavior::Dismiss,
        return_focus: true,
        disabled_state: true,
        overlay: true,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::InputForm,
        backend: "gpui_component::input::Input",
        keyboard_navigation: true,
        focus_trap: false,
        escape_dismiss: false,
        outside_click: OutsideClickBehavior::NotApplicable,
        return_focus: false,
        disabled_state: true,
        overlay: false,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::LoadingErrorRetry,
        backend: "vibex_desktop_model::QueryState + gpui_component::Notification",
        keyboard_navigation: true,
        focus_trap: false,
        escape_dismiss: false,
        outside_click: OutsideClickBehavior::NotApplicable,
        return_focus: false,
        disabled_state: true,
        overlay: false,
        accessible_name_and_state: true,
        explicit_confirmation: false,
    },
    PrimitiveContract {
        primitive: FoundationPrimitive::DestructiveConfirmation,
        backend: "gpui_component::dialog::AlertDialog",
        keyboard_navigation: true,
        focus_trap: true,
        escape_dismiss: true,
        outside_click: OutsideClickBehavior::Ignored,
        return_focus: true,
        disabled_state: true,
        overlay: true,
        accessible_name_and_state: true,
        explicit_confirmation: true,
    },
];

pub fn foundation_primitive_contracts_valid() -> bool {
    validate_foundation_primitive_contracts().is_ok()
}

pub fn validate_foundation_primitive_contracts() -> Result<(), &'static str> {
    let kinds = FOUNDATION_PRIMITIVE_CONTRACTS
        .iter()
        .map(|contract| contract.primitive)
        .collect::<BTreeSet<_>>();
    if kinds.len() != FOUNDATION_PRIMITIVE_CONTRACTS.len() {
        return Err("foundation primitive kinds must be unique");
    }
    if FOUNDATION_PRIMITIVE_CONTRACTS
        .iter()
        .any(|contract| !contract.accessible_name_and_state)
    {
        return Err("every foundation primitive requires an accessible name and state");
    }
    for primitive in [
        FoundationPrimitive::Dialog,
        FoundationPrimitive::Drawer,
        FoundationPrimitive::DestructiveConfirmation,
    ] {
        let contract = contract_for(primitive);
        if !contract.focus_trap
            || !contract.escape_dismiss
            || !contract.return_focus
            || !contract.overlay
        {
            return Err("modal primitives require trap, Escape, overlay, and focus return");
        }
    }
    for primitive in [
        FoundationPrimitive::Menu,
        FoundationPrimitive::ContextMenu,
        FoundationPrimitive::Popover,
        FoundationPrimitive::Select,
    ] {
        let contract = contract_for(primitive);
        if !contract.keyboard_navigation || !contract.escape_dismiss || !contract.return_focus {
            return Err("popup primitives require keyboard, Escape, and focus return");
        }
    }
    let destructive = contract_for(FoundationPrimitive::DestructiveConfirmation);
    if destructive.outside_click != OutsideClickBehavior::Ignored
        || !destructive.explicit_confirmation
    {
        return Err("destructive confirmation must ignore outside clicks and require confirmation");
    }
    Ok(())
}

pub fn open_destructive_confirmation(
    window: &mut Window,
    cx: &mut App,
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    confirm_label: impl Into<SharedString>,
    on_confirm: impl Fn(&mut Window, &mut App) + 'static,
) {
    let title = title.into();
    let description = description.into();
    let confirm_label = confirm_label.into();
    let on_confirm = Rc::new(on_confirm);
    window.open_alert_dialog(cx, move |dialog, _, _| {
        let on_confirm = on_confirm.clone();
        dialog
            .title(title.clone())
            .description(description.clone())
            .button_props(
                DialogButtonProps::default()
                    .ok_text(confirm_label.clone())
                    .ok_variant(ButtonVariant::Danger)
                    .cancel_text("Cancel")
                    .show_cancel(true),
            )
            .overlay_closable(false)
            .keyboard(true)
            .on_ok(move |_, window, cx| {
                on_confirm(window, cx);
                true
            })
    });
}

fn contract_for(primitive: FoundationPrimitive) -> &'static PrimitiveContract {
    FOUNDATION_PRIMITIVE_CONTRACTS
        .iter()
        .find(|contract| contract.primitive == primitive)
        .expect("foundation primitive contract is complete")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_primitive_matrix_has_radix_equivalent_product_semantics() {
        validate_foundation_primitive_contracts().unwrap();
        assert_eq!(FOUNDATION_PRIMITIVE_CONTRACTS.len(), 11);
    }
}
