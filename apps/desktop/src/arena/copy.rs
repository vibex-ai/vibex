use crate::locale::text;

use super::combat::{Arena, ArrowState, Attack, BossState, Cue, Guardian, MIN_CHARGE, Phase};

pub(super) fn title() -> &'static str {
    text("AI Souls", "AI 之魂", "AI 之魂")
}

pub(super) fn guardian_name(guardian: Guardian) -> &'static str {
    match guardian {
        Guardian::Claude => text("The clay colossus", "陶土巨像", "陶土巨像"),
        Guardian::Codex => text("The living knot", "生息之环", "生息之環"),
        Guardian::Pi => text("The staff keeper", "持杖者", "持杖者"),
        Guardian::OpenCode => text("The hollow sentinel", "空心哨兵", "空心哨兵"),
        Guardian::DeepSeek => text("The tide caller", "唤潮巨鲸", "喚潮巨鯨"),
        Guardian::Copilot => text("The winged watcher", "翼巡者", "翼巡者"),
    }
}

pub(super) fn tactic(guardian: Guardian) -> &'static str {
    match guardian {
        Guardian::Claude => text(
            "Bait the two fists, then dodge the double slam. Its chest opens as the arms recover.",
            "引开左右拳，再避开双拳重砸。收回手臂时，胸口会露出破绽。",
            "引開左右拳，再避開雙拳重砸。收回手臂時，胸口會露出破綻。",
        ),
        Guardian::Codex => text(
            "Lure its charge into a wall, roll aside, then shoot the core exposed behind the knot.",
            "诱导它撞墙，翻滚避开冲撞，再射中环结背后的核心。",
            "誘導它撞牆，翻滾避開衝撞，再射中環結背後的核心。",
        ),
        Guardian::Pi => text(
            "Step out of the staff's spell. Shoot through the mantle, then recall through its exposed seal.",
            "避开法杖的咒印。先让箭穿过披风，再用回箭击穿施法后露出的封印。",
            "避開法杖的咒印。先讓箭穿過披風，再用回箭擊穿施法後露出的封印。",
        ),
        Guardian::OpenCode => text(
            "Dodge the stomp and the following beam. The shutter opens after the sweep.",
            "避开重踏和随后的扫射。光束扫过后，舱门会短暂打开。",
            "避開重踏和隨後的掃射。光束掃過後，艙門會短暫打開。",
        ),
        Guardian::DeepSeek => text(
            "Keep moving through three surges. The whale reveals its heart when the third surge ends.",
            "连续避开三次突进。第三次潮涌停下时，巨鲸会露出心核。",
            "連續避開三次突進。第三次潮湧停下時，巨鯨會露出心核。",
        ),
        Guardian::Copilot => text(
            "Slip between its paired volleys. Bait the dive and strike while its wings are grounded.",
            "穿过双翼齐射的间隙，引开俯冲，趁落地收翼时出箭。",
            "穿過雙翼齊射的間隙，引開俯衝，趁落地收翼時出箭。",
        ),
    }
}

pub(super) fn boss_state(arena: &Arena) -> &'static str {
    if arena.phase == Phase::Awakening {
        return text("Awakening", "正在苏醒", "正在甦醒");
    }
    if arena.phase == Phase::Victory {
        return text("Core shattered", "核心已击破", "核心已擊破");
    }
    if arena.boss.exposed > 0.0 {
        return text("Core exposed", "核心已露出", "核心已露出");
    }
    match arena.boss.state {
        BossState::Windup if arena.boss.progress() >= 0.60 => {
            text("Aim locked · dodge", "目标锁定 · 闪避", "目標鎖定 · 閃避")
        }
        BossState::Windup => text("Tracking", "正在瞄准", "正在瞄準"),
        BossState::Striking | BossState::Rushing => text("Attack", "攻击中", "攻擊中"),
        _ => text("Core sealed", "核心封闭", "核心封閉"),
    }
}

pub(super) fn arrow_state(arena: &Arena) -> &'static str {
    match arena.arrow.state {
        ArrowState::Ready if arena.player.charge >= MIN_CHARGE => {
            text("Release to shoot", "松开射箭", "鬆開射箭")
        }
        ArrowState::Ready if arena.player.charge > 0.0 => text("Drawing…", "蓄力中…", "蓄力中…"),
        ArrowState::Ready => text(
            "One arrow · one opening",
            "一支箭 · 一瞬破绽",
            "一支箭 · 一瞬破綻",
        ),
        ArrowState::Flying => text("Arrow in flight", "箭已射出", "箭已射出"),
        ArrowState::Lodged => text("Hold K to recall", "按住 K 召回", "按住 K 召回"),
        ArrowState::Returning => text("Recalling", "召回中", "召回中"),
    }
}

pub(super) fn cue(arena: &Arena) -> &'static str {
    if arena.phase == Phase::Awakening {
        return text(
            "One hit is fatal. Watch the guardian before committing your arrow.",
            "一击即倒。观察守卫的动作，再选择出箭时机。",
            "一擊即倒。觀察守衛的動作，再選擇出箭時機。",
        );
    }
    match arena.cue {
        Some(Cue::Shielded) => text(
            "Armor deflected the arrow. Recover it and wait for an opening.",
            "箭被护甲挡住了。先收回箭，再等待破绽。",
            "箭被護甲擋住了。先收回箭，再等待破綻。",
        ),
        Some(Cue::ReturnArrow) => text(
            "The seal yields to a returning arrow.",
            "用回箭穿过封印。",
            "用回箭穿過封印。",
        ),
        Some(Cue::Caught) => text("Arrow caught", "箭已收回", "箭已收回"),
        None if arena.boss.state == BossState::Windup => match arena.boss.attack {
            Attack::LeftFist => text("Left fist rising", "左拳抬起", "左拳抬起"),
            Attack::RightFist => text("Right fist rising", "右拳抬起", "右拳抬起"),
            Attack::Clap => text(
                "Both fists · an opening follows",
                "双拳重砸 · 破绽即将出现",
                "雙拳重砸 · 破綻即將出現",
            ),
            Attack::Rush => text("The knot winds back", "环结正在蓄势", "環結正在蓄勢"),
            Attack::Leap | Attack::Dive => text(
                "Move out of the landing mark",
                "离开落点标记",
                "離開落點標記",
            ),
            Attack::Cross => text(
                "Leave both arms of the spell",
                "离开咒印的横纵两条路径",
                "離開咒印的橫縱兩條路徑",
            ),
            Attack::Pulse => text(
                "Roll through the expanding ring",
                "翻滚穿过扩散的冲击环",
                "翻滾穿過擴散的衝擊環",
            ),
            Attack::Stomp => text(
                "The sentinel lifts its arms",
                "哨兵抬起双臂",
                "哨兵抬起雙臂",
            ),
            Attack::Sweep => text(
                "Step across the beam's path",
                "横向离开光束路径",
                "橫向離開光束路徑",
            ),
            Attack::Dash => text("Another surge is coming", "潮涌即将袭来", "潮湧即將襲來"),
            Attack::Volley => text("The wings take aim", "双翼正在瞄准", "雙翼正在瞄準"),
        },
        None => tactic(arena.boss.guardian),
    }
}
