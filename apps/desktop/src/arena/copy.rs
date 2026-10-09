use super::guardian::Guardian;
use crate::locale::text;
fn title() -> &'static str {
    text("AI Souls", "AI 之魂", "AI 之魂")
}
pub(super) fn entry_label(guardian: Guardian) -> String {
    format!("{} · {}", title(), guardian.agent())
}
pub(super) fn entry_help() -> &'static str {
    text(
        "AI Souls · WASD to move · Hold and release J to shoot · K to recall · Space to roll · P to pause · Esc to return",
        "AI 之魂 · WASD 移动 · 按住 J 蓄力、松开射箭 · K 召回 · 空格翻滚 · P 暂停 · Esc 返回",
        "AI 之魂 · WASD 移動 · 按住 J 蓄力、鬆開射箭 · K 召回 · 空格翻滾 · P 暫停 · Esc 返回",
    )
}
pub(super) fn battle_label(guardian: Guardian, paused: bool) -> String {
    let status = if paused {
        text("Paused", "已暂停", "已暫停")
    } else {
        text("Battle", "战斗中", "戰鬥中")
    };
    format!("{} · {} · {}", title(), guardian.agent(), status)
}
