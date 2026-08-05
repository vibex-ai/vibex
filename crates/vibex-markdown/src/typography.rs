use gpui::{App, FontWeight, Global};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CodeTypography {
    weight: u16,
}

impl Default for CodeTypography {
    fn default() -> Self {
        Self { weight: 400 }
    }
}

impl Global for CodeTypography {}

pub fn apply_code_font_weight(weight: u16, cx: &mut App) {
    cx.set_global(CodeTypography {
        weight: weight.clamp(100, 900),
    });
}

pub fn code_font_weight(cx: &App) -> FontWeight {
    FontWeight(
        cx.try_global::<CodeTypography>()
            .copied()
            .unwrap_or_default()
            .weight as f32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    #[gpui::test]
    fn code_font_weight_applies_and_bounds_preferences(cx: &mut TestAppContext) {
        cx.update(|cx| {
            assert_eq!(code_font_weight(cx), FontWeight(400.0));
            apply_code_font_weight(950, cx);
            assert_eq!(code_font_weight(cx), FontWeight(900.0));
        });
    }
}
