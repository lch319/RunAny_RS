//! 全局快捷键：弹出菜单那一键，外加每个菜单项 / 子菜单自己的触发键。
//!
//! ## 三类目标
//!
//! | 目标 | 按下去做什么 |
//! | --- | --- |
//! | 主菜单 | 在鼠标处弹出完整菜单（再按一次收起） |
//! | 普通项 | **先抓选中内容**（动作里可能有 `{selection}`），然后直接跑它的动作 |
//! | 子菜单 | 先抓选中内容，然后只弹这一支（根层就它一项，并且自动展开一层） |
//!
//! ## 为什么不用 `RegisterHotKey`
//!
//! 原来这一层是交给系统的 `RegisterHotKey`（`global-hotkey` 这个 crate）。它有三个
//! 用户一眼就能看见的硬伤：
//!
//! 1. **不区分左右**：注册表里只有一颗 `MOD_CONTROL`，`RCtrl+0` 和 `LCtrl+0` 是同一个键；
//! 2. **抢不过别人**：别的程序先注册了 `Win+0`，我们就注册失败，界面上只留一句
//!    「注册失败（被别的软件占了？）」，用户完全没辙；
//! 3. **没有鼠标**：`RegisterHotKey` 只认键盘，鼠标侧键 / 中键根本没入口。
//!
//! 现在改成**底层钩子**（`WH_KEYBOARD_LL` / `WH_MOUSE_LL`，和热字符串共用一条线程）：
//! 钩子在「按键送到目标窗口之前」被调用，所以既能看清左右修饰键，也能把键**吞掉**
//! —— 谁先装钩子谁说话，本程序一律优先，别的程序的热键也就压过去了。
//!
//! 代价是自己要做匹配：把设置里的写法解析成「四族修饰键 + 一个主键」，
//! 每次按键拿 `GetAsyncKeyState` 读一遍真实的修饰键状态再比。这部分全是内存操作，
//! 一次按键几十纳秒，用户感觉不到。
//!
//! ## 「要不要区分左右」是**每一条**自己的事
//!
//! 有人希望弹菜单那一颗左右都能按（顺手），又希望某个项只认右边的 Alt（不和别的软件撞）。
//! 所以这个开关跟着每一条快捷键走（`ItemSetting::hotkey_sided` /
//! `MenuSettings::popup_hotkey_sided`），在**键盘面板**里拨 —— 不是全局一刀切。
//! 关掉之后带 `L` / `R` 前缀的写法在那一条上按「任意一边」算。
//!
//! ## 生效条件（`#If`）
//!
//! 每条菜单项还能配一组条件（`ItemSetting::hotkey_when`，语法见 `when.rs`）：
//! 前台不是那个程序时，**键不吞、也不触发**，原样交给别的程序。
//!
//! ## 什么时候重新编表
//!
//! `settings::update()` 末尾会调 [`sync`]：整表重建（比原来的增量注册简单得多，
//! 因为这里没有「注册」这个有代价的动作）。写错格式的写法会保留上一次能用的那一颗，
//! 并把问题记进 [`problems`] 给界面显示。

use std::sync::Mutex;
use std::time::Duration;

use gpui::App;

use crate::settings::{self, ItemKind};
use crate::win32::{self, MouseKind, RawKey, RawMouse};

/// 「弹出菜单」那两颗全局键在冲突检查里的身份（菜单项用的是自己的 id）。
pub const POPUP_ID: &str = "$popup";
pub const POPUP_NOSELECT_ID: &str = "$noselect";
/// 「一键直达」那一颗（设置页查冲突时不算它自己）
pub const MAGIC_ID: &str = "$magic";

/// 同一颗键的重复事件在这之内只算一次 —— 只是个防抖下限。
///
/// 比系统默认的重复间隔（31ms，控制面板「键盘 → 速度」）**小**，所以按住不放时
/// 的节奏仍由系统决定：故意不用一个大的固定值，那样按着会一格一格地爬。
const REPEAT_FLOOR: Duration = Duration::from_millis(15);
/// 收起菜单到「焦点回到原窗口」大概要这么久，抓取要等它。
const FOCUS_BACK: Duration = Duration::from_millis(140);

/// 一个快捷键绑给了谁。
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// 完整菜单（先抓一次选中内容）
    Popup,
    /// 一键直达：抓内容 → 按规则表匹配 → 命中就执行，没命中（看设置）再弹菜单
    Magic,
    /// 完整菜单，但**不抓**选中内容（`{selection}` 是空的）
    PopupNoSelect,
    Item(String),
    Submenu(String),
}

/// 修饰键的匹配要求。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    /// 写的是 `Ctrl` / `Shift` / `Alt` / `Win`：左右哪一颗都行
    Any,
    Left,
    Right,
}

/// 四族修饰键各自的要求（`None` = 这一族不许按着）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Mods {
    ctrl: Option<Side>,
    shift: Option<Side>,
    alt: Option<Side>,
    win: Option<Side>,
}

/// 主键：键盘上的一个虚拟键，或者鼠标上的一个位置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Vk(u16),
    Mouse(MousePart),
}

/// 能当主键按的鼠标位置（滚轮分上下两个方向）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MousePart {
    Left,
    Right,
    Middle,
    X1,
    X2,
    WheelUp,
    WheelDown,
}

/// 解析好的一条组合键。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combo {
    mods: Mods,
    /// 主键。带 `second` 时它是「要先按住的那颗」
    key: Key,
    /// 要不要 CapsLock 处于**开启**状态（`CapsLock+Q` 这种写法）
    caps: bool,
    /// 双键组合的第二颗（`1+2`：按住 1 再按 2 才触发）
    second: Option<Key>,
}

impl Combo {
    /// 现在的真实按键状态能不能满足这条组合键。
    ///
    /// `physical` 是 [`win32::modifier_state`] 那八个数；`sided == false` 时
    /// 带左右前缀的写法按「任意一边」算。
    fn fits(&self, key: Key, physical: &[bool; 8], sided: bool) -> bool {
        self.fits_with(key, physical, sided, false, &|_| false)
    }

    /// 同上，额外带上 CapsLock 状态和「现在还按着哪些键」。
    ///
    /// 双键组合（`1+2`）判定的是**后按的那颗**：`held` 里要有第一颗。
    fn fits_with(
        &self,
        key: Key,
        physical: &[bool; 8],
        sided: bool,
        caps_on: bool,
        held: &dyn Fn(Key) -> bool,
    ) -> bool {
        // CapsLock 写法要求它此刻是开着的（它是开关量，按不住）
        if self.caps && !caps_on {
            return false;
        }
        match self.second {
            // 双键：这次按下的必须是第二颗，且第一颗还按着
            Some(second) => {
                if second != key || !held(self.key) {
                    return false;
                }
            }
            None => {
                if self.key != key {
                    return false;
                }
            }
        }
        let family = |requirement: Option<Side>, left: bool, right: bool| -> bool {
            match requirement {
                None => !left && !right,
                Some(Side::Any) => left || right,
                Some(Side::Left) => {
                    if sided {
                        left
                    } else {
                        left || right
                    }
                }
                Some(Side::Right) => {
                    if sided {
                        right
                    } else {
                        left || right
                    }
                }
            }
        };
        family(self.mods.ctrl, physical[0], physical[1])
            && family(self.mods.shift, physical[2], physical[3])
            && family(self.mods.alt, physical[4], physical[5])
            && family(self.mods.win, physical[6], physical[7])
    }

    /// 两条快捷键会不会「同时被同一个按键满足」。
    ///
    /// 只看写法相等是不够的：`RAlt+0`（区分左右）和 `Alt+0`（不区分）写法不同，
    /// 但按右 Alt 时两条都成立 —— 它们就是撞了。这里按**实际能命中的集合**比：
    /// 每一族修饰键算出一个允许集合（`{左}` / `{右}` / `{左,右}` / 空 = 必须松着）。
    fn overlaps(&self, other: &Combo, self_sided: bool, other_sided: bool) -> bool {
        if self.key != other.key {
            return false;
        }
        let allowed = |requirement: Option<Side>, sided: bool| -> Vec<Side> {
            match requirement {
                None => Vec::new(),
                Some(Side::Any) => vec![Side::Left, Side::Right],
                Some(Side::Left) => {
                    if sided {
                        vec![Side::Left]
                    } else {
                        vec![Side::Left, Side::Right]
                    }
                }
                Some(Side::Right) => {
                    if sided {
                        vec![Side::Right]
                    } else {
                        vec![Side::Left, Side::Right]
                    }
                }
            }
        };
        let overlaps_one = |a: Option<Side>, b: Option<Side>| -> bool {
            let left = allowed(a, self_sided);
            let right = allowed(b, other_sided);
            match (left.is_empty(), right.is_empty()) {
                // 两边都要求「这一族不按」：互不冲突
                (true, true) => true,
                // 一边要求松着、一边要求按着：不可能同时成立
                (true, false) | (false, true) => false,
                (false, false) => left.iter().any(|side| right.contains(side)),
            }
        };
        overlaps_one(self.mods.ctrl, other.mods.ctrl)
            && overlaps_one(self.mods.shift, other.mods.shift)
            && overlaps_one(self.mods.alt, other.mods.alt)
            && overlaps_one(self.mods.win, other.mods.win)
    }
}

struct Binding {
    id: u32,
    combo: Combo,
    target: Target,
    /// 这一条区分左右修饰键吗（每条自己定，见 `hotkey_sided`）
    sided: bool,
    /// 生效条件（空 = 任何程序里都生效）。钩子里同步判。
    when: crate::when::Conditions,
    /// 有没有写生效条件（冲突判定用：带条件的两条可能永远不同时成立）
    conditional: bool,
    /// 条件的原文（只给冲突判定用：两条条件一模一样才真的会同时响）
    when_text: String,
}

thread_local! {
    /// 上一次同步时遇到的问题（写错格式 / 被多处占用），界面拿它显示
    static PROBLEMS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// 钩子线程要读的那张表（不能用 thread_local：钩子跑在另一条线程上）。
static TABLE: Mutex<Vec<Binding>> = Mutex::new(Vec::new());
/// 钩子线程往界面送「哪个快捷键被按了」
static SENDER: Mutex<Option<async_channel::Sender<u32>>> = Mutex::new(None);

/// 上一次同步时没能生效的快捷键说明（空 = 全部正常）。
pub fn problems() -> Vec<String> {
    PROBLEMS.with(|slot| slot.borrow().clone())
}

/// 现在真正生效的快捷键个数（主菜单那一键也算一个）。
pub fn registered() -> usize {
    TABLE.lock().map(|table| table.len()).unwrap_or(0)
}

/// 文本 → 组合键。认不出来（空串、`Ctrl+`、`Ctrl+M+N`）返回 `None`。
pub fn parse(text: &str) -> Option<Combo> {
    let mut mods = Mods::default();
    let mut key: Option<Key> = None;
    let mut caps = false;
    let mut second: Option<Key> = None;
    for part in text.trim().split('+') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        // CapsLock 是开关量的「修饰键」：`CapsLock+Q`
        let lower = part.to_ascii_lowercase();
        if lower == "capslock" || lower == "caps" || part == "大写" {
            caps = true;
            continue;
        }
        if let Some((slot, side)) = modifier_token(part) {
            let target = match slot {
                Slot::Ctrl => &mut mods.ctrl,
                Slot::Shift => &mut mods.shift,
                Slot::Alt => &mut mods.alt,
                Slot::Win => &mut mods.win,
            };
            // 同一族写了两次（`Ctrl+LCtrl+A`）取更「具体」的那个
            *target = Some(match (*target, side) {
                (Some(Side::Left), _) | (_, Side::Left) => Side::Left,
                (Some(Side::Right), _) | (_, Side::Right) => Side::Right,
                _ => Side::Any,
            });
            continue;
        }
        let parsed = mouse_token(part)
            .map(Key::Mouse)
            .or_else(|| win32::virtual_key(part).map(Key::Vk))?;
        // 第二个主键 = 双键组合（`1+2`：按住 1 再按 2）
        //
        // 只允许**一颗**这样的键：`1+2+3` 这种三键组合手感太差，也容易误触
        if key.is_some() {
            if second.is_some() {
                return None;
            }
            second = Some(parsed);
            continue;
        }
        key = Some(parsed);
    }
    let key = key?;
    // 只有修饰键没有主键（`Ctrl+Alt`）不算
    Some(Combo { mods, key, caps, second })
}

/// 这个写法能不能用（设置界面用来标红）。
pub fn is_valid(text: &str) -> bool {
    reason(text).is_none()
}

/// 这条写法为什么不能用（能用 = `None`，空串也算能用）。
///
/// 单独一档给「单按鼠标左右键 / 中键」：它技术上是能注册的，但钩子会把
/// **全系统**的点击都吞掉 —— 菜单自己都点不动，只能算写法错误。
pub fn reason(text: &str) -> Option<&'static str> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let Some(combo) = parse(text) else {
        return Some(crate::i18n::either(
            "写法不合法：修饰键 + 主键，或「按住一颗再按另一颗」，例如 Ctrl+Alt+1、CapsLock+Q、1+2",
            "Not a hotkey - modifiers plus one main key, e.g. Ctrl+Alt+1 or XButton2",
        ));
    };
    if matches!(
        combo.key,
        Key::Mouse(MousePart::Left | MousePart::Right | MousePart::Middle)
    ) && combo.mods == Mods::default()
    {
        return Some(crate::i18n::either(
            "左键 / 右键 / 中键要配着 Ctrl、Alt、Shift 或 Win 才能当快捷键：单按会把全系统的点击都吃掉",
            "Left / right / middle click needs a modifier - on its own it would swallow every click",
        ));
    }
    None
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Ctrl,
    Shift,
    Alt,
    Win,
}

/// 修饰键的别名 → （哪一族, 左右）。
fn modifier_token(part: &str) -> Option<(Slot, Side)> {
    let lower = part.trim().to_ascii_lowercase();
    Some(match lower.as_str() {
        "ctrl" | "control" => (Slot::Ctrl, Side::Any),
        "lctrl" | "lcontrol" | "leftctrl" | "leftcontrol" => (Slot::Ctrl, Side::Left),
        "rctrl" | "rcontrol" | "rightctrl" | "rightcontrol" => (Slot::Ctrl, Side::Right),
        "shift" => (Slot::Shift, Side::Any),
        "lshift" | "leftshift" => (Slot::Shift, Side::Left),
        "rshift" | "rightshift" => (Slot::Shift, Side::Right),
        "alt" => (Slot::Alt, Side::Any),
        "lalt" | "leftalt" => (Slot::Alt, Side::Left),
        "ralt" | "rightalt" => (Slot::Alt, Side::Right),
        "win" | "super" | "cmd" | "meta" | "windows" => (Slot::Win, Side::Any),
        "lwin" | "leftwin" => (Slot::Win, Side::Left),
        "rwin" | "rightwin" => (Slot::Win, Side::Right),
        _ => return None,
    })
}

/// 鼠标键的别名（AHK 的叫法优先）。
fn mouse_token(part: &str) -> Option<MousePart> {
    let lower = part.trim().to_ascii_lowercase();
    Some(match lower.as_str() {
        "lbutton" | "leftbutton" | "mouseleft" | "左键" => MousePart::Left,
        "rbutton" | "rightbutton" | "mouseright" | "右键" => MousePart::Right,
        "mbutton" | "middlebutton" | "mousemiddle" | "中键" => MousePart::Middle,
        "xbutton1" | "x1button" | "side1" | "侧键1" => MousePart::X1,
        "xbutton2" | "x2button" | "side2" | "侧键2" => MousePart::X2,
        "wheelup" | "wheelu" | "滚轮上" => MousePart::WheelUp,
        "wheeldown" | "wheeld" | "滚轮下" => MousePart::WheelDown,
        _ => return None,
    })
}

/// 启动时调一次：装钩子、拉起分发任务，然后按当前设置编一遍表。
pub fn install(cx: &mut App) {
    // 记「用户刚才在哪个窗口」：按快捷键时如果前台是我们自己的窗口，
    // 抓取要回到那个窗口去复制（见 win32::capture_target）
    crate::win32::watch_foreground();

    let (tx, rx) = async_channel::bounded::<u32>(32);
    if let Ok(mut slot) = SENDER.lock() {
        *slot = Some(tx);
    }

    let key_ready = win32::set_key_gate(Box::new(|raw: RawKey| on_key(raw)));
    let mouse_ready = win32::set_mouse_gate(Box::new(|raw: RawMouse| on_mouse(raw)));
    if !key_ready {
        PROBLEMS.with(|slot| {
            slot.borrow_mut().push(
                crate::i18n::either(
                    "装不上键盘钩子：全局快捷键用不了（可能被安全软件拦了）",
                    "Could not install the keyboard hook - global hotkeys are unavailable",
                )
                .to_string(),
            )
        });
    }
    if !mouse_ready {
        eprintln!("[popup-menu] 鼠标钩子装不上：鼠标键不能当快捷键");
    }

    // 系统事件是阻塞式通道：钩子里只 try_send，转到 GPUI 这边再慢慢处理
    cx.spawn(async move |cx| {
        // 上一次真正派发的是哪颗键、什么时候
        let mut last_fired: Option<(u32, std::time::Instant)> = None;
        while let Ok(first) = rx.recv().await {
            // 按住不放时，系统会按自己的速率连续发 keydown（就是记事本里按住 a
            // 的节奏），钩子每个都 try_send 过来。这里**不再给每次派发加固定
            // 睡眠**：原来那样睡 150ms，比系统慢太多，而且睡的时候事件还在通道
            // 里堆 —— 表现就是「按着 F1 音量一格格爬，松手了还在爬」。
            //
            // 现在：先把握着的这一段里**同一颗键**的积压折叠成一个，再立刻派发。
            let mut batch = vec![first];
            while let Ok(next) = rx.try_recv() {
                if batch.last() != Some(&next) {
                    batch.push(next);
                }
            }
            for id in batch {
                // 刚发过同一颗键（小于防抖下限）就跳过，避免一次按下算两次
                if let Some((last_id, at)) = last_fired {
                    if last_id == id && at.elapsed() < REPEAT_FLOOR {
                        continue;
                    }
                }
                last_fired = Some((id, std::time::Instant::now()));
                let Some(target) = lookup(id) else { continue };
                // 菜单开着 + 要「执行 / 只弹一支」：先收起来（焦点还给原窗口），
                // 否则抓到的选中内容是空的。主菜单那一键不能这么干 —— 它自己会 toggle。
                let close_first = matches!(
                    target,
                    Target::Item(_) | Target::Submenu(_) | Target::Magic
                );
                if close_first {
                    let was_open = cx.update(|app| crate::popup::is_open(app)).unwrap_or(false);
                    if was_open {
                        let _ = cx.update(|app| crate::popup::close_all(app));
                        cx.background_executor().timer(FOCUS_BACK).await;
                    }
                }
                let _ = cx.update(move |app| dispatch(target, app));
            }
        }
    })
    .detach();

    sync(cx);
    println!(
        "[popup-menu] 全局快捷键已就绪（钩子模式，{} 个）",
        registered()
    );
}

/// 按当前设置重建整张表。设置一变就调（`settings::update` 末尾）。
pub fn sync(cx: &mut App) {
    // 1) 期望的绑定：主菜单两颗 + 一键直达一颗 + 每个开了「触发快捷键」的条目
    let mut wanted: Vec<Wanted> = Vec::new();
    let popup = settings::read(|settings| settings.popup_hotkey.trim().to_string());
    let popup = if popup.is_empty() {
        settings::DEFAULT_POPUP_HOTKEY.to_string()
    } else {
        popup
    };
    let popup_sided = settings::read(|settings| settings.popup_hotkey_sided);
    wanted.push(Wanted {
        text: popup,
        target: Target::Popup,
        sided: popup_sided,
        when: String::new(),
    });
    // 「不抓选中内容」的那一颗：留空 = 不注册
    let (noselect, noselect_sided) = settings::read(|settings| {
        (
            settings.popup_hotkey_noselect.trim().to_string(),
            settings.popup_hotkey_noselect_sided,
        )
    });
    if !noselect.is_empty() {
        wanted.push(Wanted {
            text: noselect,
            target: Target::PopupNoSelect,
            sided: noselect_sided,
            when: String::new(),
        });
    }
    // 一键直达：自己配了键才单独注册。**留空 = 跟弹出菜单共用那一颗**
    // （按下先问规则，一条不中再照常弹菜单），所以这里不建绑定 —— 共用同一颗时
    // 再建一条就是「一个键两条绑定」，后建的那条永远轮不到。
    let magic = settings::read(|settings| settings.magic.clone());
    if magic.enabled && !magic.hotkey.trim().is_empty() {
        wanted.push(Wanted {
            text: magic.hotkey.trim().to_string(),
            target: Target::Magic,
            sided: magic.sided,
            when: String::new(),
        });
    }
    settings::read(|settings| collect_hotkeys(&settings.items, &mut wanted));

    // 2) 解析 + 去重
    let mut problems = Vec::new();
    let mut next: Vec<Binding> = Vec::new();
    for item in wanted {
        let text = item.text;
        // 写错了 / 单按鼠标左右键：保留上一次能用的那一颗，别把用户原本能用的快捷键弄丢
        let parsed = match (reason(&text), parse(&text)) {
            (Some(problem), _) => {
                keep_old(&text, item.target.clone(), &mut next);
                problems.push(problem_line(&text, problem));
                continue;
            }
            (None, Some(combo)) => combo,
            (None, None) => {
                keep_old(&text, item.target.clone(), &mut next);
                problems.push(problem_line(
                    &text,
                    crate::i18n::either("不是合法的快捷键", "not a valid hotkey"),
                ));
                continue;
            }
        };
        let combo = parsed;
        // 生效条件写错：这条不生效（但也别把用户原本能用的绑弄丢）
        let when = crate::when::compile(&item.when);
        let conditional = !item.when.trim().is_empty();
        if let Some(problem) = when.problems().first() {
            problems.push(if crate::i18n::is_english() {
                format!(
                    "{text} has a broken condition ({}), so it never fires",
                    problem
                )
            } else {
                format!("{text} 的生效条件写错了（{problem}），这条不会触发")
            });
        }
        // 两条「会不会撞」不看写法定成一样，而是看**实际能不能同时命中**
        // （`RAlt+0` 和 `Alt+0` 写法不同，但按右 Alt 时两条都满足）
        //
        // 带生效条件的（局部快捷键）只有在**两边条件一模一样**时才真会同时响：
        // 一个只在 QQ 里、一个只在微信里，静态判不出来，不该吓唬用户。
        if let Some(clash) = next
            .iter()
            .find(|binding| binding.combo.overlaps(&combo, binding.sided, item.sided))
        {
            if !conditional || !clash.conditional || same_scope(&item.when, &clash.when_text) {
                problems.push(if crate::i18n::is_english() {
                    format!(
                        "{text} is already used by \"{}\" - only the first one works",
                        describe(&clash.target)
                    )
                } else {
                    format!(
                        "{text} 已经被「{}」占用了，同一条只有先设置的那个生效",
                        describe(&clash.target)
                    )
                });
                continue;
            }
        }
        next.push(Binding {
            id: next.len() as u32 + 1,
            combo,
            target: item.target,
            sided: item.sided,
            when,
            conditional,
            when_text: item.when.clone(),
        });
    }

    if std::env::var_os("GPUI_MENU_DEBUG_HOTKEY").is_some() {
        for binding in &next {
            eprintln!(
                "[hotkey] 绑定 #{} {:?} -> {:?}",
                binding.id, binding.combo, binding.target
            );
        }
    }
    let count = next.len();
    if let Ok(mut table) = TABLE.lock() {
        *table = next;
    }
    // 问题只在「跟上次不一样」时打一遍日志，免得每次改设置都刷屏
    let before = PROBLEMS.with(|slot| slot.borrow().clone());
    if before != problems {
        for problem in &problems {
            eprintln!("[popup-menu] 快捷键：{problem}");
        }
    }
    PROBLEMS.with(|slot| *slot.borrow_mut() = problems);
    let _ = (cx, count);
}

/// 一条「想要注册的快捷键」（还没解析）。
struct Wanted {
    text: String,
    target: Target,
    sided: bool,
    when: String,
}

/// 这一条没生效：把上一次能用的那颗原样接回去（别因为改坏一行就全没了）。
fn keep_old(text: &str, target: Target, next: &mut Vec<Binding>) {
    let _ = text;
    let old = TABLE.lock().ok().and_then(|table| {
        table
            .iter()
            .find(|binding| binding.target == target)
            .map(|binding| Binding {
                id: 0, // 下面按顺序重排
                combo: binding.combo,
                target: binding.target.clone(),
                sided: binding.sided,
                when: crate::when::Conditions::default(),
                conditional: false,
                when_text: String::new(),
            })
    });
    if let Some(binding) = old {
        next.push(Binding {
            id: next.len() as u32 + 1,
            ..binding
        });
    }
}

/// `「X」用不了：原因` / `"X" cannot be used: reason`
fn problem_line(text: &str, problem: &str) -> String {
    if crate::i18n::is_english() {
        format!("\"{text}\" cannot be used - {problem}")
    } else {
        format!("「{text}」用不了：{problem}")
    }
}

/// 树里所有「配了触发快捷键」的条目（禁用的、分隔线不算）。
///
/// 每个条目有**两颗**键：全局的 `hotkey` 和「只在某些窗口里生效」的
/// `hotkey_local`。两颗都各自占一个坑，互不影响。
fn collect_hotkeys(items: &[settings::ItemSetting], out: &mut Vec<Wanted>) {
    for item in items {
        if item.enabled && item.kind != ItemKind::Separator {
            let target = || {
                if item.is_submenu() {
                    Target::Submenu(item.id.clone())
                } else {
                    Target::Item(item.id.clone())
                }
            };
            let text = item.hotkey.trim();
            if !text.is_empty() {
                out.push(Wanted {
                    text: text.to_string(),
                    target: target(),
                    sided: item.hotkey_sided,
                    when: String::new(),
                });
            }
            let local = item.hotkey_local.trim();
            if !local.is_empty() {
                out.push(Wanted {
                    text: local.to_string(),
                    target: target(),
                    sided: item.hotkey_local_sided,
                    when: item.hotkey_local_when.clone(),
                });
            }
        }
        collect_hotkeys(&item.children, out);
    }
}

fn lookup(id: u32) -> Option<Target> {
    TABLE
        .lock()
        .ok()?
        .iter()
        .find(|binding| binding.id == id)
        .map(|binding| binding.target.clone())
}

/// 钩子那一关：这个按键是不是哪个快捷键要的？是就吞掉。
///
/// 只看「按下」：抬起留给系统自己派发（吞掉的话有些程序会以为键卡住了）。
///
/// 顺序：先按组合键筛，再问**生效条件** —— 条件不满足时**不吞键**，
/// 那个组合在这个程序里还是它本来的意思（这就是 `#If` 的语义）。
/// 此刻还按着的键（双键组合 `1+2` 靠它判断「1 是不是还按着」）
static HELD: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<u16>>> =
    std::sync::OnceLock::new();

fn held_now() -> std::collections::HashSet<u16> {
    HELD.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .map(|set| set.clone())
        .unwrap_or_default()
}

fn on_key(raw: RawKey) -> bool {
    // 抬起也要记：双键组合要知道第一颗还有没有按着
    if !raw.down {
        if let Ok(mut set) = HELD
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
            .lock()
        {
            set.remove(&raw.vk);
        }
        return false;
    }
    if let Ok(mut set) = HELD
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
    {
        set.insert(raw.vk);
    }
    // CapsLock 自己永远不触发（它只是开关键）
    const VK_CAPITAL: u16 = 0x14;
    if raw.vk == VK_CAPITAL {
        return false;
    }
    // 修饰键本身不触发（`Ctrl+Alt+M` 里的 Ctrl 不能算「按下了 M」）
    if is_modifier_vk(raw.vk) {
        return false;
    }
    let physical = win32::modifier_state();
    let Ok(table) = TABLE.lock() else {
        return false;
    };
    let debug = std::env::var_os("GPUI_MENU_DEBUG_HOTKEY").is_some();
    if debug {
        eprintln!(
            "[hotkey] vk={:#04x} mods={:?} table={}",
            raw.vk,
            physical,
            table.len()
        );
    }
    let caps_on = win32::caps_lock_on();
    let held = held_now();
    for binding in table.iter() {
        let is_held = |key: Key| match key {
            Key::Vk(vk) => held.contains(&vk),
            Key::Mouse(_) => false,
        };
        if !binding
            .combo
            .fits_with(Key::Vk(raw.vk), &physical, binding.sided, caps_on, &is_held)
        {
            continue;
        }
        // 只有条件里写了 `mouse.` 才去取鼠标下那扇窗口：钩子里每按一个键都要判，
        // 多取一份现场就多一次开进程句柄
        if !binding
            .when
            .matches(&win32::input_context(binding.when.needs_mouse()))
        {
            if debug {
                eprintln!("[hotkey] #{} 组合对上了，但生效条件不满足", binding.id);
            }
            continue;
        }
        if debug {
            eprintln!("[hotkey] 命中 #{} -> {:?}", binding.id, binding.target);
        }
        send(binding.id);
        return true;
    }
    false
}

/// 鼠标那一关：侧键 / 中键 / 左右键 / 滚轮上下能不能当快捷键。
fn on_mouse(raw: RawMouse) -> bool {
    let part = match (raw.button, raw.wheel.signum()) {
        (Some(MouseKind::Left), _) => MousePart::Left,
        (Some(MouseKind::Right), _) => MousePart::Right,
        (Some(MouseKind::Middle), _) => MousePart::Middle,
        (Some(MouseKind::X1), _) => MousePart::X1,
        (Some(MouseKind::X2), _) => MousePart::X2,
        (None, 1) => MousePart::WheelUp,
        (None, -1) => MousePart::WheelDown,
        _ => return false,
    };
    let physical = win32::modifier_state();
    let Ok(table) = TABLE.lock() else {
        return false;
    };
    let debug = std::env::var_os("GPUI_MENU_DEBUG_HOTKEY").is_some();
    if debug {
        eprintln!(
            "[hotkey] mouse={part:?} mods={:?} table={}",
            physical,
            table.len()
        );
    }
    for binding in table.iter() {
        if !binding.combo.fits(Key::Mouse(part), &physical, binding.sided) {
            continue;
        }
        // 只有条件里写了 `mouse.` 才去取鼠标下那扇窗口：钩子里每按一个键都要判，
        // 多取一份现场就多一次开进程句柄
        if !binding
            .when
            .matches(&win32::input_context(binding.when.needs_mouse()))
        {
            if debug {
                eprintln!("[hotkey] #{} 组合对上了，但生效条件不满足", binding.id);
            }
            continue;
        }
        if debug {
            eprintln!("[hotkey] 命中 #{} -> {:?}", binding.id, binding.target);
        }
        send(binding.id);
        return true;
    }
    false
}

/// 八颗修饰键的虚拟键码（钩子里要跳过它们）。
fn is_modifier_vk(vk: u16) -> bool {
    matches!(vk, 0xA0..=0xA5 | 0x5B | 0x5C)
}

fn send(id: u32) {
    if let Ok(slot) = SENDER.lock() {
        if let Some(tx) = slot.as_ref() {
            // 界面那头忙不过来就丢掉：连按没人要 30 次
            let _ = tx.try_send(id);
        }
    }
}

/// 目标的可读名字（冲突提示里要说「被谁占了」）。
fn describe(target: &Target) -> String {
    match target {
        Target::Popup => crate::i18n::t("弹出菜单（抓选中内容）").to_string(),
        Target::PopupNoSelect => crate::i18n::t("弹出菜单（不抓选中内容）").to_string(),
        Target::Magic => crate::i18n::t("一键直达").to_string(),
        Target::Item(id) | Target::Submenu(id) => settings::read(|settings| {
            settings
                .find_path(id)
                .and_then(|path| settings.node(&path))
                .map(|item| {
                    if item.label.trim().is_empty() {
                        crate::i18n::t("未命名的菜单项").to_string()
                    } else {
                        item.label.clone()
                    }
                })
                .unwrap_or_else(|| crate::i18n::t("已删除的菜单项").to_string())
        }),
    }
}

/// 一处「配了快捷键」的地方。
struct Owner {
    text: String,
    who: String,
    /// 菜单项 / 弹菜单两颗自己的 id（`None` = 一键直达）
    id: Option<String>,
    sided: bool,
    /// 这颗键的生效条件（空串 = 到处都生效）。
    ///
    /// 只有**局部键**才有值。判断「撞没撞」时要拿它比一比：两颗 `Ctrl+1`
    /// 一个限定在 QQ、一个限定在 Chrome，本来就不会同时响应，不该报冲突。
    when: String,
}

/// 两段生效条件算不算「同一片地盘」。
///
/// - 两边都空（都到处生效）→ 是
/// - 一边空一边有条件 → 是（到处生效那颗会盖住有条件那颗）
/// - 两边都有 → 规范化后逐条相等才算（行顺序无所谓：行与行之间本来就是「或」）
fn same_scope(a: &str, b: &str) -> bool {
    fn norm(text: &str) -> Vec<String> {
        let mut lines: Vec<String> = text
            .lines()
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        lines.sort();
        lines
    }
    let (a, b) = (norm(a), norm(b));
    if a.is_empty() || b.is_empty() {
        return true;
    }
    a == b
}

/// 现在配了快捷键的所有地方。
fn owners() -> Vec<Owner> {
    let mut out: Vec<Owner> = Vec::new();
    let (popup, noselect, popup_sided, noselect_sided, magic) = settings::read(|settings| {
        (
            settings.popup_hotkey.trim().to_string(),
            settings.popup_hotkey_noselect.trim().to_string(),
            settings.popup_hotkey_sided,
            settings.popup_hotkey_noselect_sided,
            settings.magic.clone(),
        )
    });
    let popup = if popup.is_empty() {
        settings::DEFAULT_POPUP_HOTKEY.to_string()
    } else {
        popup
    };
    out.push(Owner {
        text: popup,
        who: crate::i18n::t("弹出菜单（抓选中内容）").to_string(),
        id: Some(POPUP_ID.to_string()),
        sided: popup_sided,
        when: String::new(),
    });
    if !noselect.is_empty() {
        out.push(Owner {
            text: noselect,
            who: crate::i18n::t("弹出菜单（不抓选中内容）").to_string(),
            id: Some(POPUP_NOSELECT_ID.to_string()),
            sided: noselect_sided,
            when: String::new(),
        });
    }
    if magic.enabled && !magic.hotkey.trim().is_empty() {
        out.push(Owner {
            text: magic.hotkey.trim().to_string(),
            who: crate::i18n::t("一键直达").to_string(),
            id: Some(MAGIC_ID.to_string()),
            sided: magic.sided,
            when: String::new(),
        });
    }
    fn walk(items: &[settings::ItemSetting], out: &mut Vec<Owner>) {
        for item in items {
            let text = item.hotkey.trim();
            let usable = item.enabled && item.kind != ItemKind::Separator;
            if usable && (!text.is_empty() || !item.hotkey_local.trim().is_empty()) {
                let who = if item.label.trim().is_empty() {
                    crate::i18n::t("未命名的菜单项").to_string()
                } else {
                    item.label.clone()
                };
                if !text.is_empty() {
                    out.push(Owner {
                        text: text.to_string(),
                        who: who.clone(),
                        id: Some(item.id.clone()),
                        sided: item.hotkey_sided,
                        when: String::new(),
                    });
                }
                // 局部键单独占一行：界面上要能说清「撞的是那颗局部的」
                let local = item.hotkey_local.trim();
                if !local.is_empty() {
                    out.push(Owner {
                        text: local.to_string(),
                        who: if crate::i18n::is_english() {
                            format!("{who} (local)")
                        } else {
                            format!("{who}（局部）")
                        },
                        id: Some(item.id.clone()),
                        sided: item.hotkey_local_sided,
                        when: item.hotkey_local_when.clone(),
                    });
                }
            }
            walk(&item.children, out);
        }
    }
    settings::read(|settings| walk(&settings.items, &mut out));
    out
}

/// 这个写法被谁占用了？（`exclude_id` 是正在编辑的那一项，不算它自己）
///
/// `sided` 是**这一条自己**的「区分左右」设置 —— 判断标准是「按同一个键会不会
/// 同时命中两条」，不是写法字符串相等（见 [`Combo::overlaps`]）。
///
/// `when` 是**这一条自己**的生效条件（全局键传空串）。两颗键就算写法一样，
/// 各自限定在互不相干的窗口里时也不会同时响应，那不叫撞（见 [`same_scope`]）。
pub fn taken_by(text: &str, exclude_id: Option<&str>, sided: bool, when: &str) -> Option<String> {
    let combo = parse(text)?;
    for owner in owners() {
        if owner.id.as_deref() == exclude_id {
            continue;
        }
        let Some(other) = parse(&owner.text) else {
            continue;
        };
        if !combo.overlaps(&other, sided, owner.sided) {
            continue;
        }
        if !same_scope(when, &owner.when) {
            continue;
        }
        return Some(owner.who);
    }
    None
}

/// 所有「一个键被多处占用」的情况：`(写法, 占用者列表)`。
pub fn conflicts() -> Vec<(String, Vec<String>)> {
    let all = owners();
    let parsed: Vec<(Combo, String, String, bool, String)> = all
        .into_iter()
        .filter_map(|owner| {
            let combo = parse(&owner.text)?;
            Some((combo, owner.text, owner.who, owner.sided, owner.when))
        })
        .collect();
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for (index, (combo, text, who, sided, when)) in parsed.iter().enumerate() {
        let others: Vec<String> = parsed
            .iter()
            .enumerate()
            .filter(|(other_index, (other, _, _, other_sided, other_when))| {
                *other_index != index
                    && combo.overlaps(other, *sided, *other_sided)
                    && same_scope(when, other_when)
            })
            .map(|(_, (_, _, name, _, _))| name.clone())
            .collect();
        if others.is_empty() {
            continue;
        }
        let mut names = vec![who.clone()];
        names.extend(others);
        match out.iter_mut().find(|(seen, _)| seen == text) {
            Some((_, list)) => {
                for name in names {
                    if !list.contains(&name) {
                        list.push(name);
                    }
                }
            }
            None => out.push((text.clone(), names)),
        }
    }
    out
}

/// 真的做事：主菜单 / 执行一个条目 / 只弹一支 / 一键直达。
fn dispatch(target: Target, cx: &mut App) {
    match target {
        Target::Popup => {
            if crate::magic::shares_popup_hotkey() {
                // 一键直达没单独配键：和弹菜单共用这一颗 —— 先问规则，
                // 一条都不中再照常弹菜单（用户按的本来就是这个键）。
                // 菜单开着时仍然是「收起」，别让它去跑规则。
                if crate::popup::is_open(cx) {
                    crate::popup::close_all(cx);
                } else if !crate::settings::foreground_blocked() {
                    run_magic(cx, true);
                }
            } else {
                crate::popup::toggle(cx)
            }
        }
        Target::PopupNoSelect => crate::popup::toggle_no_select(cx),
        Target::Magic => run_magic(cx, false),
        Target::Item(id) => {
            // 从别的程序直接按快捷键时，菜单没弹过，选中内容要在这儿自己抓。
            //
            // 但**不是每一项都要**：只有「运行程序」这种会接上选中内容的、或者动作用了
            // `{selection}` 的才值得发一次 Ctrl+C。发送按键 / 发送文本 / 打开网址这些
            // 抓了也没人用 —— 而抓一次要等剪贴板（最长半秒），连按几下就叠起来，
            // 用户看到的就是「连续按会卡住」。
            if settings::item_needs_selection(&id) {
                crate::selection::capture();
            } else {
                crate::selection::clear();
            }
            if let Some((label, actions)) = settings::item_actions(&id) {
                crate::popup::finish(id.into(), label.into(), actions, cx);
            }
        }
        // 子菜单：抓完内容只弹这一支（open_submenu 内部会抓）
        Target::Submenu(id) => crate::popup::open_submenu(cx, &id),
    }
}

/// 一键直达：抓一次选中内容 → 按规则表从上往下问 → 第一条命中就执行。
///
/// 抓到的内容已经在 `selection::current()` 里了，所以命中子菜单时走
/// `open_submenu_keeping_selection`，**不再复制第二次**（那要多等半秒）。
///
/// `shared` = 这次是按「弹出菜单」那一颗进来的（一键直达没配自己的键）：
/// 那时候一条不中都**必须**弹菜单 —— 用户按的是「弹菜单」的键，什么都不做
/// 等于这颗键坏了。
fn run_magic(cx: &mut App, shared: bool) {
    let magic = settings::read(|settings| settings.magic.clone());
    let text = crate::selection::capture()
        .map(|selection| selection.placeholder_text())
        .unwrap_or_default();
    let hit = crate::magic::first_hit(&magic.rules, &text).cloned();
    if std::env::var_os("GPUI_MENU_DEBUG_MAGIC").is_some() {
        eprintln!(
            "[magic] 内容 {} 个字 -> {}",
            text.chars().count(),
            match &hit {
                Some(rule) => format!("命中 {}", rule.summary()),
                None => "没命中".to_string(),
            }
        );
    }
    let fallback = shared || magic.fallback_menu;
    match hit {
        Some(rule) => {
            let id = rule.target.clone();
            if crate::magic::target_is_submenu(&id) {
                crate::popup::open_submenu_keeping_selection(cx, &id);
            } else if let Some((label, actions)) = settings::item_actions(&id) {
                crate::popup::finish(id.into(), label.into(), actions, cx);
            } else if fallback {
                // 规则绑的那一项被删了：当没命中处理，别让按键变成「没反应」
                crate::popup::open_keeping_selection(cx);
            }
        }
        None => {
            if fallback {
                crate::popup::open_keeping_selection(cx);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn physical(
        lctrl: bool,
        rctrl: bool,
        lshift: bool,
        rshift: bool,
        lalt: bool,
        ralt: bool,
        lwin: bool,
        rwin: bool,
    ) -> [bool; 8] {
        [lctrl, rctrl, lshift, rshift, lalt, ralt, lwin, rwin]
    }

    const NONE: [bool; 8] = [false; 8];

    #[test]
    fn parses_modifiers_and_the_main_key() {
        let combo = parse("Ctrl+Alt+M").expect("认得出来");
        assert_eq!(combo.key, Key::Vk(0x4D));
        assert_eq!(combo.mods.ctrl, Some(Side::Any));
        assert_eq!(combo.mods.alt, Some(Side::Any));

        // 小键盘 / 功能键 / 鼠标都认
        assert_eq!(parse("Num7").unwrap().key, Key::Vk(0x67));
        assert_eq!(parse("F13").unwrap().key, Key::Vk(0x7C));
        assert_eq!(parse("Ctrl+LButton").unwrap().key, Key::Mouse(MousePart::Left));
        assert_eq!(parse("WheelUp").unwrap().key, Key::Mouse(MousePart::WheelUp));
    }

    #[test]
    fn rejects_things_that_are_not_one_combo() {
        assert!(parse("").is_none());
        assert!(parse("Ctrl+Alt").is_none(), "只有修饰键不算");
        assert!(parse("1+2+3").is_none(), "三颗主键不支持（最多两颗）");
        assert!(parse("Ctrl+M+N").is_some(), "修饰键 + 两颗键也算双键组合");
        assert!(parse("1+2").is_some(), "两颗主键 = 双键组合");
        assert!(parse("Ctrl+").is_none());
    }

    #[test]
    fn caps_lock_and_chords_parse() {
        let caps = parse("CapsLock+Q").expect("CapsLock 当修饰键");
        assert!(caps.caps);
        assert_eq!(caps.key, Key::Vk(0x51));
        assert!(caps.second.is_none());

        let chord = parse("1+2").expect("双键组合");
        assert_eq!(chord.key, Key::Vk(0x31));
        assert_eq!(chord.second, Some(Key::Vk(0x32)));
        assert!(!chord.caps);
    }

    #[test]
    fn left_and_right_modifiers_are_told_apart() {
        let combo = parse("RAlt+0").expect("认得出来");
        // 右边那颗 Alt 按着 → 命中
        assert!(combo.fits(Key::Vk(0x30), &physical(false, false, false, false, false, true, false, false), true));
        // 左边那颗按着 → 不命中（这正是 RegisterHotKey 做不到的）
        assert!(!combo.fits(Key::Vk(0x30), &physical(false, false, false, false, true, false, false, false), true));
    }

    #[test]
    fn the_sided_switch_makes_both_hands_equal() {
        let combo = parse("RAlt+0").expect("认得出来");
        assert!(combo.fits(
            Key::Vk(0x30),
            &physical(false, false, false, false, true, false, false, false),
            false,
        ));
    }

    #[test]
    fn extra_modifiers_do_not_fire() {
        // 只配了 Ctrl+M：按 Ctrl+Shift+M 不该触发（和 RegisterHotKey 一样严格）
        let combo = parse("Ctrl+M").expect("认得出来");
        assert!(combo.fits(Key::Vk(0x4D), &physical(true, false, false, false, false, false, false, false), true));
        assert!(!combo.fits(Key::Vk(0x4D), &physical(true, false, false, true, false, false, false, false), true));
        // 什么修饰键都不按：`M` 也算，只要组合键里没要求修饰键
        assert!(parse("M").unwrap().fits(Key::Vk(0x4D), &NONE, true));
    }

    #[test]
    fn win_keys_can_be_hotkeys() {
        let combo = parse("RWin+0").expect("认得出来");
        assert!(combo.fits(Key::Vk(0x30), &physical(false, false, false, false, false, false, false, true), true));
        assert!(!combo.fits(Key::Vk(0x30), &physical(false, false, false, false, false, false, true, false), true));
    }

    #[test]
    fn a_sided_binding_and_a_loose_one_collide() {
        let left = parse("LCtrl+A").unwrap();
        let loose = parse("Ctrl+A").unwrap();
        assert!(
            left.overlaps(&loose, true, false),
            "「区分左右」的 LCtrl+A 和「不分左右」的 Ctrl+A 会撞车"
        );
        let right = parse("RCtrl+A").unwrap();
        assert!(
            !left.overlaps(&right, true, true),
            "左右分开的两条互不干扰"
        );
        assert!(
            left.overlaps(&right, true, false),
            "两条都不分左右时，同一族修饰键算同一条"
        );
    }

    #[test]
    fn modifier_vks_are_skipped_in_the_hook() {
        for vk in [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0x5B, 0x5C] {
            assert!(is_modifier_vk(vk), "{vk:#x} 是修饰键");
        }
        assert!(!is_modifier_vk(0x41));
    }

    #[test]
    fn side_buttons_and_the_wheel_work_on_their_own() {
        // 侧键 / 滚轮单按不干扰别的东西，可以单独当快捷键
        for text in ["XButton1", "XButton2", "WheelUp", "WheelDown"] {
            assert!(is_valid(text), "{text} 应该能单独用");
            assert!(reason(text).is_none(), "{text} 不该报错");
        }
        // 左右键 / 中键配了修饰键就能用
        for text in ["Ctrl+LButton", "Alt+RButton", "Shift+MButton"] {
            assert!(is_valid(text), "{text} 应该能用");
        }
    }

    #[test]
    fn a_bare_click_is_refused_because_it_would_eat_every_click() {
        for text in ["LButton", "RButton", "MButton", "lbutton"] {
            assert!(!is_valid(text), "{text} 单按会把全系统的点击吞掉，得拦下来");
            assert!(reason(text).is_some(), "{text} 要有说得出口的理由");
        }
        // 空串 = 没配，不算错
        assert!(is_valid(""));
    }

    #[test]
    fn reason_names_the_problem() {
        assert!(reason("Ctrl+Alt").is_some(), "只有修饰键要说清楚");
        assert!(reason("1+2+3").is_some(), "三颗主键要说清楚");
        assert!(reason("1+2").is_none(), "双键组合是合法的");
        assert!(reason("Ctrl+Alt+M").is_none());
    }
}
