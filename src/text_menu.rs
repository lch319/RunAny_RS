//! 输入框的右键菜单（复制 / 剪切 / 粘贴 / 全选 / 清空）。
//!
//! ## 为什么要一个「全局状态 + 窗口根层浮层」
//!
//! 输入框一般只有 24px 高、还带裁剪，菜单画在它自己里面会被裁掉；
//! gpui 又没有「在窗口根上随手弹一个窗口」的 API。所以：
//!
//! 1. `TextInput` 右键时把「哪个窗口、哪个输入框、点在哪儿」记进一个全局实体；
//! 2. 设置窗口 / 编辑窗口（它们才是窗口的根视图）在 `render` 末尾把这一层画出来 ——
//!    和调色板浮层同一套路：先铺一张吃掉点击的遮罩，再把菜单画在遮罩上面。
//!
//! 根视图 `observe` 这个实体，所以右键一点就能重绘出来，不用谁去猜窗口。

use gpui::prelude::*;
use gpui::{
    div, px, AnyElement, App, AppContext, ClickEvent, Context, Entity, Global, IntoElement,
    MouseButton, MouseDownEvent, Pixels, Point, SharedString, Window, WindowId,
};

use crate::text_area::TextArea;
use crate::text_input::TextInput;
use crate::theme;

/// 右键菜单作用于谁。
///
/// 单行输入框（`text_input`）和多行编辑框（`text_area`）暴露的方法名是一样的，
/// 所以菜单项只记「要干什么」，具体落到哪个实体上由这里分发 ——
/// 不然每加一种输入框就得把菜单再抄一遍。
#[derive(Clone)]
pub enum Target {
    Input(Entity<TextInput>),
    Area(Entity<TextArea>),
}

/// 菜单项的动作。
#[derive(Clone, Copy)]
enum Verb {
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    SelectAll,
    Clear,
}

impl Target {
    fn has_selection(&self, cx: &App) -> bool {
        match self {
            Target::Input(entity) => entity.read(cx).has_selection(),
            Target::Area(entity) => entity.read(cx).has_selection(),
        }
    }

    fn can_undo(&self, cx: &App) -> bool {
        match self {
            Target::Input(entity) => entity.read(cx).can_undo(),
            Target::Area(entity) => entity.read(cx).can_undo(),
        }
    }

    fn can_redo(&self, cx: &App) -> bool {
        match self {
            Target::Input(entity) => entity.read(cx).can_redo(),
            Target::Area(entity) => entity.read(cx).can_redo(),
        }
    }

    fn run(&self, verb: Verb, window: &mut Window, cx: &mut App) {
        match self {
            Target::Input(entity) => entity.update(cx, |input, cx| match verb {
                Verb::Undo => input.do_undo(cx),
                Verb::Redo => input.do_redo(cx),
                Verb::Copy => input.do_copy(cx),
                Verb::Cut => input.do_cut(window, cx),
                Verb::Paste => input.do_paste(window, cx),
                Verb::SelectAll => input.do_select_all(cx),
                Verb::Clear => input.do_clear(window, cx),
            }),
            Target::Area(entity) => entity.update(cx, |area, cx| match verb {
                Verb::Undo => area.do_undo(cx),
                Verb::Redo => area.do_redo(cx),
                Verb::Copy => area.do_copy(cx),
                Verb::Cut => area.do_cut(window, cx),
                Verb::Paste => area.do_paste(window, cx),
                Verb::SelectAll => area.do_select_all(cx),
                Verb::Clear => area.do_clear(window, cx),
            }),
        }
    }
}

/// 正在显示的右键菜单。
#[derive(Clone)]
struct OpenMenu {
    window: WindowId,
    input: Target,
    anchor: Point<Pixels>,
}

pub struct TextMenuState {
    open: Option<OpenMenu>,
}

pub struct TextMenuHandle(pub Entity<TextMenuState>);

impl Global for TextMenuHandle {}

/// 启动时调一次（放在设置安装之后）。
pub fn install(cx: &mut App) {
    let entity = cx.new(|_| TextMenuState { open: None });
    cx.set_global(TextMenuHandle(entity));
}

pub fn store(cx: &App) -> Option<Entity<TextMenuState>> {
    cx.try_global::<TextMenuHandle>()
        .map(|handle| handle.0.clone())
}

/// 在 `anchor`（窗口坐标）处弹出右键菜单。
pub fn open(input: Target, window: &Window, anchor: Point<Pixels>, cx: &mut App) {
    let Some(store) = store(cx) else {
        return;
    };
    let window_id = window.window_handle().window_id();
    store.update(cx, |state, cx| {
        state.open = Some(OpenMenu {
            window: window_id,
            input,
            anchor,
        });
        cx.notify();
    });
}

pub fn close(cx: &mut App) {
    let Some(store) = store(cx) else {
        return;
    };
    store.update(cx, |state, cx| {
        if state.open.take().is_some() {
            cx.notify();
        }
    });
}

/// 有没有任何一个窗口开着这个菜单（Esc 关它用）。
pub fn any_open(cx: &App) -> bool {
    store(cx)
        .map(|store| store.read(cx).open.is_some())
        .unwrap_or(false)
}

/// 在窗口根层画遮罩 + 菜单（只有本窗口开着菜单时才返回东西）。
pub fn layer<T: 'static>(window: &Window, cx: &mut Context<T>) -> Vec<AnyElement> {
    let Some(store) = store(cx) else {
        return Vec::new();
    };
    let Some(menu) = store
        .read(cx)
        .open
        .clone()
        .filter(|menu| menu.window == window.window_handle().window_id())
    else {
        return Vec::new();
    };

    // 点在别处 = 关掉（和系统右键菜单一样，这一下不穿透到下面的界面）
    let backdrop = div()
        .id("text-menu-backdrop")
        .occlude()
        .absolute()
        .left_0()
        .top_0()
        .size_full()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_this, _: &MouseDownEvent, _window, cx| {
                close(cx);
            }),
        )
        .into_any_element();

    let has_selection = menu.input.has_selection(cx);
    let can_undo = menu.input.can_undo(cx);
    let can_redo = menu.input.can_redo(cx);
    let viewport = window.viewport_size();
    let width = 132.0_f32;
    // 27 = 7 条（撤销 / 重做 / 复制 / 剪切 / 粘贴 / 全选 / 清空）+ 一条分隔线
    let height = 7.0 * 26.0 + 8.0 + 9.0;
    let left = f32::from(menu.anchor.x)
        .min(f32::from(viewport.width) - width - 4.0)
        .max(4.0);
    let top = f32::from(menu.anchor.y)
        .min(f32::from(viewport.height) - height - 4.0)
        .max(4.0);

    // 「撤销 / 重做」放在最上面：键盘上就是这个顺序（Ctrl+Z 改坏了马上想退回来），
    // 和系统里那些编辑框的右键菜单也一致。
    let items: Vec<(&'static str, &'static str, bool, Verb)> = vec![
        (
            crate::i18n::either("撤销", "Undo"),
            "Ctrl+Z",
            can_undo,
            Verb::Undo,
        ),
        (
            crate::i18n::either("重做", "Redo"),
            "Ctrl+Y",
            can_redo,
            Verb::Redo,
        ),
        (crate::i18n::t("复制"), "Ctrl+C", has_selection, Verb::Copy),
        (crate::i18n::t("剪切"), "Ctrl+X", has_selection, Verb::Cut),
        (crate::i18n::t("粘贴"), "Ctrl+V", true, Verb::Paste),
        (crate::i18n::t("全选"), "Ctrl+A", true, Verb::SelectAll),
        (crate::i18n::t("清空"), "", true, Verb::Clear),
    ];

    let mut panel = div()
        .id("text-menu-panel")
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(width))
        .py(px(4.))
        .flex()
        .flex_col()
        .rounded(px(8.))
        .bg(theme::surface_elevated())
        .border_1()
        .border_color(theme::border_strong())
        .shadow_md()
        .font_family(theme::font_family());

    for (index, (label, shortcut, enabled, verb)) in items.into_iter().enumerate() {
        // 撤销 / 重做 和编辑操作之间拉一条线
        if index == 2 {
            panel = panel.child(
                div()
                    .mx(px(8.))
                    .my(px(4.))
                    .h(px(1.))
                    .bg(theme::separator()),
            );
        }
        let input = menu.input.clone();
        let mut row = div()
            .id(SharedString::from(format!("text-menu-{label}")))
            .h(px(26.))
            .mx(px(4.))
            .px(px(8.))
            .flex()
            .items_center()
            .gap(px(8.))
            .rounded(px(5.))
            .text_size(px(11.5));
        if enabled {
            row = row
                .text_color(theme::text_primary())
                .cursor_pointer()
                .hover(|style| style.bg(theme::surface_hover()))
                .on_click(cx.listener(move |_this, _: &ClickEvent, window, cx| {
                    input.run(verb, window, cx);
                    close(cx);
                    window.refresh();
                }));
        } else {
            row = row.text_color(theme::ui_text_tertiary());
        }
        row = row.child(div().flex_1().child(label));
        if !shortcut.is_empty() {
            row = row.child(
                div()
                    .flex_none()
                    .text_size(px(9.5))
                    .text_color(theme::ui_text_tertiary())
                    .child(shortcut),
            );
        }
        panel = panel.child(row);
    }

    vec![backdrop, panel.into_any_element()]
}
