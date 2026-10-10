use super::guardian::Guardian;
use crate::locale::text;
fn title() -> &'static str {
    text("AI Souls", "AI 之魂", "AI 之魂")
}
pub(super) fn entry_label(guardian: Guardian) -> String {
    format!("{} · {}", title(), guardian.agent())
}
pub(super) fn entry_help(guardian: Guardian) -> String {
    let controls = text(
        "WASD / arrows move · Hold J / left mouse, release to shoot · K / right mouse recall · Space roll · P pause · Esc return",
        "WASD / 方向键移动 · J / 鼠标左键蓄力，松开射箭 · K / 右键召回 · 空格翻滚 · P 暂停 · Esc 返回",
        "WASD / 方向鍵移動 · J / 滑鼠左鍵蓄力，鬆開射箭 · K / 右鍵召回 · 空格翻滾 · P 暫停 · Esc 返回",
    );
    let hint = match guardian {
        Guardian::Claude => text(
            "Sever two glowing tendons, then strike the exposed underside.",
            "射断两处发光的触手关节，再击中腹部核心。",
            "射斷兩處發光的觸手關節，再擊中腹部核心。",
        ),
        Guardian::Codex => text(
            "Lure the rush into stone; the impact unfolds the central cube.",
            "引导冲撞击中石柱或场地边缘，待金环展开后射击中央方核。",
            "引導衝撞擊中石柱或場地邊緣，待金環展開後射擊中央方核。",
        ),
        Guardian::Pi => text(
            "Lure each fist onto its ground seal to unlock the square aperture.",
            "引导左右拳分别砸中对应地印，解锁方孔中的核心。",
            "引導左右拳分別砸中對應地印，解鎖方孔中的核心。",
        ),
        Guardian::OpenCode => text(
            "Shoot into the inhale, then hold recall to pull the frame apart.",
            "吸气时射入箭矢，持续召回扯开外框，再射核心。",
            "吸氣時射入箭矢，持續召回扯開外框，再射核心。",
        ),
        Guardian::DeepSeek => text(
            "Lure breaches away from the islands; shoot the tail crystal from dry ground.",
            "把跃击引离浮岛，站在干地射击尾部晶体；水中只能召回。",
            "把躍擊引離浮島，站在乾地射擊尾部晶體；水中只能召回。",
        ),
        Guardian::Copilot => text(
            "Intercept the bracketed energy orb to overload the shield and reveal the reactor.",
            "射中带四角光标的能量弹，令护盾过载后击中胸口反应堆。",
            "射中帶四角光標的能量彈，令護盾過載後擊中胸口反應爐。",
        ),
    };
    format!("{} · {}\n{}", entry_label(guardian), hint, controls)
}
pub(super) fn battle_label(guardian: Guardian, paused: bool) -> String {
    let status = if paused {
        text("Paused", "已暂停", "已暫停")
    } else {
        text("Battle", "战斗中", "戰鬥中")
    };
    format!("{} · {} · {}", title(), guardian.agent(), status)
}
