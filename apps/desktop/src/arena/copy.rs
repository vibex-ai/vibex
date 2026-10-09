use crate::locale::text;

use super::combat::{Arena, ArrowState, Cue, Guardian, Phase};

pub(super) fn title() -> &'static str {
    text("Unbound", "断链", "斷鏈")
}

pub(super) fn guardian_name(guardian: Guardian) -> &'static str {
    match guardian {
        Guardian::Ember => text("The ember eye", "余烬之眼", "餘燼之眼"),
        Guardian::Knot => text("The knot sentinel", "结环守卫", "結環守衛"),
        Guardian::Prism => text("The mirror twins", "镜像双子", "鏡像雙子"),
    }
}

pub(super) fn tactic(guardian: Guardian) -> &'static str {
    match guardian {
        Guardian::Ember => text(
            "Its core opens after each burst. Dodge the embers, then put your arrow through ◆.",
            "星火散尽，核心才会露出。避开弹幕，让箭命中 ◆。",
            "星火散盡，核心才會露出。避開彈幕，讓箭命中 ◆。",
        ),
        Guardian::Knot => text(
            "Bait a charge into the arena wall. Roll aside, then strike the core behind its broken armor.",
            "诱导它冲向边界。翻滚避开冲撞，再射中破甲后露出的核心。",
            "誘導它衝向邊界。翻滾避開衝撞，再射中破甲後露出的核心。",
        ),
        Guardian::Prism => text(
            "Your shot passes through the mirrors. When the real ◆ lights up, hold K to recall the arrow through it.",
            "去箭会穿过镜像。真身的 ◆ 亮起时，按住 K，让回箭穿过核心。",
            "去箭會穿過鏡像。真身的 ◆ 亮起時，按住 K，讓回箭穿過核心。",
        ),
    }
}

pub(super) fn boss_state(arena: &Arena) -> &'static str {
    if arena.phase == Phase::Victory {
        text("◆ Core shattered", "◆ 核心已击破", "◆ 核心已擊破")
    } else if arena.boss.exposed > 0.0 {
        text("◆ Core exposed", "◆ 核心已露出", "◆ 核心已露出")
    } else if arena.boss.charging {
        text("Charging", "冲撞中", "衝撞中")
    } else if arena.boss.windup.is_some() {
        text("Attack incoming", "攻击将至", "攻擊將至")
    } else {
        text("◇ Core sealed", "◇ 核心封闭", "◇ 核心封閉")
    }
}

pub(super) fn arrow_state(arena: &Arena) -> &'static str {
    match arena.arrow.state {
        ArrowState::Ready if arena.player.charge >= 0.22 => {
            text("Release to shoot", "松开射箭", "鬆開射箭")
        }
        ArrowState::Ready if arena.player.charge > 0.0 => text("Drawing…", "蓄力中…", "蓄力中…"),
        ArrowState::Ready => text("Arrow ready", "箭已就绪", "箭已就緒"),
        ArrowState::Flying => text("Arrow in flight", "箭已射出", "箭已射出"),
        ArrowState::Lodged => text("Hold K to recall", "按住 K 召回", "按住 K 召回"),
        ArrowState::Returning => text(
            "Recalling · +16 focus on catch",
            "召回中 · 接箭恢复 16 专注",
            "召回中 · 接箭恢復 16 專注",
        ),
    }
}

pub(super) fn cue(arena: &Arena) -> &'static str {
    match arena.cue {
        Some(Cue::Shielded) => text(
            "Armor deflected the arrow. Wait for the core to open.",
            "箭被护甲挡住了，等待核心露出。",
            "箭被護甲擋住了，等待核心露出。",
        ),
        Some(Cue::ReturnArrow) => text(
            "Only a returning arrow can break the mirror core.",
            "只有回箭才能击穿镜核。",
            "只有回箭才能擊穿鏡核。",
        ),
        Some(Cue::Caught) => text(
            "Arrow caught · focus restored",
            "接住了箭 · 专注已恢复",
            "接住了箭 · 專注已恢復",
        ),
        Some(Cue::FocusEmpty) => text(
            "Stillness needs 60 focus. Catch your arrow to recover focus.",
            "静域需要 60 专注。召回并接住箭可恢复专注。",
            "靜域需要 60 專注。召回並接住箭可恢復專注。",
        ),
        None => tactic(arena.boss.guardian),
    }
}
