//! 程序入口：安装设置、注册全局快捷键、装托盘、打开主窗口。
//!
//! 平时是「后台常驻 + 托盘」：启动不显示主窗口、也不弹控制台黑框
//! （release 用 `windows_subsystem = "windows"`），
//! 要唤出菜单就按全局快捷键、点托盘图标，或者在托盘菜单里打开设置。

// release 不要控制台窗口；debug 保留，方便看 `println!` 的日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autofill_runtime;
mod autostart;
mod backup;
mod chrome;
mod column;
mod cursor;
mod everything;
mod hint;
mod host;
mod hotkey;
mod hotstring;
mod i18n;
mod actions;
mod autorule;
mod icon;
mod icon_cache;
mod item_editor;
mod keypad;
mod layout;
mod magic;
mod magic_ui;
mod mediakeys;
mod menu;
mod menu_files;
mod placeholders;
mod popup;
mod progindex;
mod runany_import;
mod rule_editor;
mod runopts;
mod selection;
mod settings;
mod settings_ui;
mod state;
mod text_area;
mod text_input;
mod text_menu;
mod theme;
mod tip_template;
mod tooltip;
mod tray;
#[cfg(windows)]
mod window_border;
mod tree_view;
mod update;
mod undo;
mod when;
mod when_ui;
mod win32;

use std::borrow::Cow;
use std::time::Duration;

use gpui::{
    px, size, App, AppContext, Application, AssetSource, Bounds, KeyBinding, Result, SharedString,
    TitlebarOptions, WindowBounds, WindowOptions,
};

/// 内嵌的线性图标（不依赖外部资源文件，换主题时跟着文字色走）。
const EDIT_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="#ffffff" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"><path d="M10.6 2.2l3.2 3.2-8 8-4.2 1 1-4.2 8-8z"/><path d="M9.2 3.6l3.2 3.2"/><path d="M2 14.3h12"/></svg>"##;

/// 菜单顶部 / 底部「还有更多」的箭头（线性 chevron，用 currentColor 跟着文字色走）。
const ARROW_DOWN_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 4.5L6 8l3.5-3.5"/></svg>"##;
const ARROW_UP_ICON: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="12" height="12" viewBox="0 0 12 12" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 7.5L6 4l3.5 3.5"/></svg>"##;

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "icons/edit.svg" => Some(Cow::Borrowed(EDIT_ICON.as_bytes())),
            "icons/arrow-down.svg" => Some(Cow::Borrowed(ARROW_DOWN_ICON.as_bytes())),
            "icons/arrow-up.svg" => Some(Cow::Borrowed(ARROW_UP_ICON.as_bytes())),
            // 菜单项的图标可以是用户自己的 svg 文件：内置资源里没有就按路径读盘。
            // 这样 `svg().path(r"C:\icons\a.svg")` 也能画出来。
            other => std::fs::read(other).ok().map(Cow::Owned),
        })
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}

fn main() {
    // 单实例：再开一次 exe = **重启**（把老的关掉再接管）。
    // 不做这个的话托盘里会挂两个图标、热键互相抢，用户以为「没生效」。
    {
        let killed = win32::kill_other_instances();
        if killed > 0 {
            println!("[popup-menu] 已在运行的 {killed} 个实例已退出（单实例启动）");
            // 稍等一下：让旧实例把托盘图标 / 钩子撤干净，否则新实例可能抢不到
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    Application::new().with_assets(Assets).run(|cx: &mut App| {
        // 主窗口要显示的「最近执行」
        let state = cx.new(|_| host::AppState::default());
        cx.set_global(host::AppStateHandle(state.clone()));

        // 设置要在菜单之前装好：窗口池预热时就要按菜单项算层数
        settings::install(cx);

        // 「运行程序只填文件名」怎么变成完整路径：**全交给 Everything**。
        //
        // 这一层现在只做三件事：问 Everything、按设置里的规则挑一份、把结果缓存一会儿。
        // 自己扫盘的索引（不管是全盘那份还是「几百个 exe 的小索引」）都删了 ——
        // 找不到装在 D 盘的程序、文件挪了位置它也不知道，还得维护缓存过期与上限。
        // Everything 本来就在读 NTFS 的 MFT + USN 日志给整台机器建索引，
        // 我们只是借结果，自己这边零开销（见 `progindex` / `everything` 模块）。
        progindex::start();

        // 菜单图标的磁盘缓存（`Icon/`）：启动后在后台把当前这套菜单里
        // 「运行程序」的图标都抠一遍存成 png。
        //
        // 为什么不在呼出菜单的时候现抠：菜单项只写了程序名（`WeChat.exe`）时，
        // 得先走 PATH → 本地索引 → Everything 查路径，一条几十毫秒，一千多项
        // 就是好几秒 —— 用户体感就是「呼出要等半天」。这里趁启动的空档做完，
        // 之后每次呼出只是「查表 + 量一下文件在不在」。
        {
            let (items, auto_icons) =
                settings::read(|settings| (settings.items.clone(), settings.auto_icons));
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .spawn(async move {
                        icon_cache::warm(&items, auto_icons);
                    })
                    .await;
                // 补齐之后让设置页重画一次（菜单每次呼出都会重建，不用管）
                let _ = cx.update(settings_ui::repaint_trees);
            })
            .detach();
        }

        // 清掉老版本留下的全盘索引缓存（可能几十 MB），不再使用
        everything::cleanup_legacy_index();

        // Everything 没在运行时随本程序一起启动（设置里勾了才动）。
        // 放在启动这一段做：等第一次查文件名时它已经在建索引了。
        if everything::autostart_if_needed() {
            println!("[everything] 已随本程序启动 Everything");
        }

        // 子菜单的「自动生成」：启动后跑一遍**开了「仅开启软件时」的那些**。
        //
        // 手动模式等用户点「立即生成」；实时模式在每次呼出菜单那一刻重建
        // （见 `popup::open_with`），启动时不必碰。
        //
        // Everything 是刚拉起来的（上面那一步），索引可能还在建，第一次查多半是空的；
        // 所以查不到就隔几秒再试，最多三轮。查询本身走后台线程（`autofill_plan`
        // 不碰 App/线程本地状态），启动那一下不会被卡住。
        {
            let jobs = settings::autofill_jobs_where(&|spec| {
                spec.mode == settings::AutoFillMode::OnStart
            });
            if !jobs.is_empty() {
                cx.spawn(async move |cx| {
                    for attempt in 0..3 {
                        if attempt > 0 {
                            cx.background_executor()
                                .timer(std::time::Duration::from_secs(5))
                                .await;
                        }
                        let todo = jobs.clone();
                        let plan = cx
                            .background_executor()
                            .spawn(async move { settings::autofill_plan(&todo) })
                            .await;
                        if plan.entries.is_empty() {
                            continue;
                        }
                        // 启动时模式 = 重建，但**不写配置**：结果只挂在运行时缓存里
                        // （写配置会让 settings.json 每次启动都变一遍，见 autofill_runtime）
                        let n = settings::install_runtime_plan(&plan);
                        if n == 0 {
                            continue;
                        }
                        println!("[autofill] 启动时生成了 {n} 项（仅开启软件时的那几支，不写配置）");
                        let _ = cx.update(settings_ui::repaint_trees);
                        return;
                    }
                })
                .detach();
            }
        }

        // 调试：`GPUI_MENU_DEBUG_EVERYTHING=notepad.exe` 直接打一次 Everything 查询结果
        if let Ok(name) = std::env::var("GPUI_MENU_DEBUG_EVERYTHING") {
            let name = name.trim().to_string();
            if !name.is_empty() {
                let hits = everything::lookup(&name, 10);
                let status = everything::status();
                println!(
                    "[everything] query={name:?} backend={:?} source={} last_ms={} err={:?}",
                    status.backend, status.source, status.last_ms, status.last_error
                );
                for hit in &hits {
                    println!("[everything]   {} (mtime={})", hit.path, hit.mtime);
                }
                println!("[everything] 共 {} 条", hits.len());
            }
        }

        // 输入框右键菜单的状态（设置窗口和编辑窗口的根层都会观察它）
        text_menu::install(cx);
        // 「键盘」面板：点着键往快捷键 / 发送按键里填，Win 键和左右修饰键都认
        keypad::install(cx);
        // 「生效条件」窗口（AHK 的 #If）：这条快捷键只在哪些窗口里生效
        when_ui::install(cx);
        // 「一键直达」的规则窗口：类型下拉、多行表达式、直接新建直达项
        magic_ui::install(cx);

        // 设置窗口里那些输入框的按键（gpui 的自带示例也是这么绑的）
        bind_text_input_keys(cx);

        // 「没点确定就关窗 = 不保存」的兜底。
        //
        // 两个窗口右上角的 ✕ 是我们自己的按钮，点击回调里直接回滚；
        // 但 Alt+F4、任务栏右键「关闭窗口」走系统 WM_CLOSE，gpui 的 Windows 后端
        // 没有「即将关闭」的钩子，只能在窗口真的没了之后检查一遍：
        // 哪个窗口不在了、而配置还停在没应用的状态，就退回打开时的快照。
        let sub = cx.on_window_closed(|cx| {
            if !settings_ui::window_alive(cx) {
                settings_ui::revert_if_dirty(cx);
            }
            if !item_editor::window_alive(cx) {
                // 编辑窗口没了，「生效条件」那扇也别自己飘着
                when_ui::close(cx);
                item_editor::revert_if_dirty(cx);
            }
        });
        // 这个订阅要活到进程结束（掉了就等于没注册）
        std::mem::forget(sub);

        // 预热弹出菜单的窗口池（后台逐帧建，避免启动卡顿）
        popup::init(cx);
        // 悬停提示的窗口也提前建好（藏起来），第一次悬停才不用等建窗口
        tooltip::install(cx);

        // 主窗口（演示面板）
        //
        // 平时**不显示**：有了托盘之后，程序该安安静静待在后台。
        // 它还是要建出来 —— GPUI 在最后一个窗口关掉时会退出应用，而弹出菜单的窗口池
        // 是异步逐个建的，靠它先把进程撑住。`show: false` 就是「建好但藏起来」。
        let show_main = std::env::var_os("GPUI_MENU_DEBUG_MAIN").is_some();
        let bounds = Bounds::centered(None, size(px(480.0), px(340.0)), cx);
        let before = win32::zed_window_handles();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("RunAny_RS".into()),
                    ..Default::default()
                }),
                show: show_main,
                ..Default::default()
            },
            |_window, cx| cx.new(|cx| host::HostView::new(state, cx)),
        )
        .expect("创建主窗口失败");
        // 这个窗口平时**一直藏着**，只是为了把进程撑住。GPUI 建窗时用的是
        // `CW_USEDEFAULT`，真实窗口是「整块屏幕」那么大 —— 一张 1424×744 的交换链
        // 光缓冲就 8 MB 上下，白白常驻着。藏着的窗口没人看，直接收成 480×340。
        if !show_main {
            if let Some(hwnd) = win32::zed_window_handles()
                .into_iter()
                .find(|candidate| !before.contains(candidate))
            {
                win32::resize_window(hwnd, 480.0, 340.0, cursor::scale_factor(cx));
            }
        }

        // 托盘图标：左键弹菜单、右键弹托盘菜单
        tray::install(cx);

        if show_main {
            cx.activate(true);
        }

        // 调试用：`GPUI_MENU_DEBUG_SETTINGS=1` 直接把设置窗口摆出来
        // （这台机器上模拟点击不可靠，看版式时省得手动点）
        if std::env::var_os("GPUI_MENU_DEBUG_SETTINGS").is_some() {
            settings_ui::open(cx);
        }
        // 调试用：`GPUI_MENU_DEBUG_EDITOR=<条目 id>` 直接摆出编辑窗口
        if let Some(id) = std::env::var("GPUI_MENU_DEBUG_EDITOR")
            .ok()
            .filter(|v| !v.is_empty())
        {
            item_editor::open(&id, cx);
        }
        // 调试用：`GPUI_MENU_DEBUG_WHEN=<条目 id>` 直接摆出「生效条件」窗口
        // （不经过编辑窗口，好把「窗口本身」和「从 render 里开窗」分开看）
        if let Some(id) = std::env::var("GPUI_MENU_DEBUG_WHEN")
            .ok()
            .filter(|v| !v.is_empty())
        {
            when_ui::open(&id, cx);
        }
        // 调试用：`GPUI_MENU_DEBUG_MENU=1` 启动后自己把菜单弹出来（配合
        // `GPUI_MENU_DEBUG_SELECTION=...` 检查「菜单第一项 = 选中内容」）
        if std::env::var_os("GPUI_MENU_DEBUG_MENU").is_some() {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(900))
                    .await;
                let _ = cx.update(|app| popup::open(app));
            })
            .detach();
        }
        // 调试用：`GPUI_MENU_DEBUG_OPEN=工具,截图` 菜单弹出后按名称展开到那一层
        if let Ok(labels) = std::env::var("GPUI_MENU_DEBUG_OPEN") {
            let labels: Vec<String> = labels
                .split(',')
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .collect();
            if !labels.is_empty() {
                cx.spawn(async move |cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(1500))
                        .await;
                    let _ = cx.update(|app| popup::open_path(app, &labels));
                })
                .detach();
            }
        }

        // 调试用：`GPUI_MENU_DEBUG_TRAY=1` 直接弹托盘右键菜单
        if std::env::var_os("GPUI_MENU_DEBUG_TRAY").is_some() {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(900))
                    .await;
                let _ = cx.update(|app| popup::open_tray(app));
            })
            .detach();
        }
        // 调试用：`GPUI_MENU_DEBUG_CTX=<条目 id>` 直接弹那一项的右键菜单
        if let Some(id) = std::env::var("GPUI_MENU_DEBUG_CTX")
            .ok()
            .filter(|value| !value.is_empty())
        {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(900))
                    .await;
                let _ = cx.update(|app| popup::open_item_context(app, &id));
            })
            .detach();
        }

        // 调试用：`GPUI_MENU_DEBUG_SUB=<子菜单 id>` 走一遍「按子菜单快捷键」那条路
        if let Some(id) = std::env::var("GPUI_MENU_DEBUG_SUB")
            .ok()
            .filter(|value| !value.is_empty())
        {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(900))
                    .await;
                let _ = cx.update(|app| popup::open_submenu(app, &id));
            })
            .detach();
        }

        // 调试用：`GPUI_MENU_DEBUG_SHOWALL=1` 菜单弹出后直接点一下「显示全部菜单」
        if std::env::var_os("GPUI_MENU_DEBUG_SHOWALL").is_some() {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(1600))
                    .await;
                let _ = cx.update(|app| popup::show_all_now(app));
            })
            .detach();
        }

        // 调试用：`GPUI_MENU_DEBUG_RUN=<条目 id>` 直接跑那一项的动作
        if let Some(id) = std::env::var("GPUI_MENU_DEBUG_RUN")
            .ok()
            .filter(|value| !value.is_empty())
        {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(1200))
                    .await;
                let _ = cx.update(|app| popup::run_item_now(app, &id));
            })
            .detach();
        }

        // 调试用：`GPUI_MENU_DEBUG_ICON=<动作值>` 看看这一条能不能解析出程序
        // （只填文件名时图标就靠它），比如 `GPUI_MENU_DEBUG_ICON=AutoHotkey.exe`
        if let Some(value) = std::env::var("GPUI_MENU_DEBUG_ICON")
            .ok()
            .filter(|value| !value.is_empty())
        {
            eprintln!(
                "[icon] program_of({value:?}) = {:?}",
                crate::icon::program_of(&value)
            );
        }

        // 调试用：`GPUI_MENU_DEBUG_RESOLVE=<文件名>` 打一下索引查到的候选和最终补出来的命令
        if let Some(name) = std::env::var("GPUI_MENU_DEBUG_RESOLVE")
            .ok()
            .filter(|value| !value.is_empty())
        {
            let status = everything::status();
            eprintln!(
                "[progindex] 裸文件名交给 Everything（后端 {:?} / {}）",
                status.backend, status.source
            );
            for candidate in progindex::candidates(&name) {
                eprintln!(
                    "[progindex]   {}  mtime={}  v={}",
                    candidate.path, candidate.mtime, candidate.version
                );
            }
            eprintln!(
                "[progindex] resolve({name:?}) = {:?}",
                progindex::resolve(&name)
            );
            eprintln!(
                "[progindex] command = {:?}",
                progindex::resolve_command(&format!("{name} --flag"))
            );
        }

        // 调试用：`GPUI_MENU_DEBUG_HOTSTRING=huiz` 打一下热字符串规则和提示内容
        if let Some(text) = std::env::var("GPUI_MENU_DEBUG_HOTSTRING")
            .ok()
            .filter(|value| !value.is_empty())
        {
            hotstring::debug_match(&text);
        }

        // 调试用：`GPUI_MENU_DEBUG_TIP=<条目 id>` 不等鼠标，直接把提示摆出来
        if let Some(id) = std::env::var("GPUI_MENU_DEBUG_TIP")
            .ok()
            .filter(|value| !value.is_empty())
        {
            cx.spawn(async move |cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(1600))
                    .await;
                let _ = cx.update(|app| tooltip::show_now(app, &id));
            })
            .detach();
        }

        // 全局快捷键：主菜单那一键 + 每个条目自己的触发键
        // （设完在「设置 → 热键」里改，改完当场重新注册）
        hotkey::install(cx);

        // 热字符串：在别的程序里敲一串字就执行菜单项（AHK 的 hotstring）。
        // 钩子只在真的配了短语时才装 —— 一条都没配就零开销。
        hotstring::install(cx);
        // 提示小卡片（敲到第 N 个字符时在鼠标旁边冒出来）
        hint::install(cx);

        // 自动规则：启动后按「轮次 × 间隔」检测环境条件，命中就执行菜单项
        autorule::start(cx);

        // 调试：`GPUI_MENU_DEBUG_RULE_EDIT=1` 进来就打开第一条规则的编辑窗口
        if std::env::var_os("GPUI_MENU_DEBUG_RULE_EDIT").is_some() {
            if let Some(id) = settings::read(|settings| {
                settings.auto_rules.first().map(|rule| rule.id.clone())
            }) {
                rule_editor::open(&id, cx);
            }
        }

        // 启动那一波（设置解析、programs.json、图标、两个预热窗口）干完以后，
        // 把工作集还给系统：不开菜单的时候，这程序就是托盘里挂着的一个东西，
        // 常驻三、五兆比常驻四十七兆体面得多。
        //
        // 时间点很关键：太早会把**正在初始化**的驱动页赶出去（实测 600ms 时裁，
        // 下一次弹菜单多花 700ms）；等预热全部落地（提示卡 400ms、小卡片 700ms）
        // 之后再裁，第一次弹菜单只多 10ms 上下。
        cx.spawn(async move |cx| {
            cx.background_executor()
                .timer(Duration::from_millis(6000))
                .await;
            let _ = cx.update(|_app| win32::trim_working_set());
        })
        .detach();
    });
}

/// 文本输入框的编辑键。
fn bind_text_input_keys(cx: &mut App) {
    use text_input::{
        Backspace, Copy, Cut, Delete, End, Home, Left, Paste, Redo, Right, SelectAll, SelectLeft,
        SelectRight, ShowCharacterPalette, Undo,
    };

    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, None),
        KeyBinding::new("delete", Delete, None),
        KeyBinding::new("left", Left, None),
        KeyBinding::new("right", Right, None),
        KeyBinding::new("shift-left", SelectLeft, None),
        KeyBinding::new("shift-right", SelectRight, None),
        KeyBinding::new("ctrl-a", SelectAll, None),
        KeyBinding::new("ctrl-v", Paste, None),
        KeyBinding::new("ctrl-c", Copy, None),
        KeyBinding::new("ctrl-x", Cut, None),
        // 撤销 / 重做：Ctrl+Z、Ctrl+Y，另外 Ctrl+Shift+Z 也算重做（很多人习惯这个）
        KeyBinding::new("ctrl-z", Undo, None),
        KeyBinding::new("ctrl-y", Redo, None),
        KeyBinding::new("ctrl-shift-z", Redo, None),
        KeyBinding::new("home", Home, None),
        KeyBinding::new("end", End, None),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, None),
    ]);
}
