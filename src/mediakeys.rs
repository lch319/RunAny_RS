//! 多媒体键 / 浏览器键 / 启动键的清单。
//!
//! 这些键在标准键盘上**没有字符**，按键面板（画键盘那张）点不出来；
//! 但 `SendInput` 直接发虚拟键就能按到 —— 所以「发送按键」里给它们单开一片
//! 点一下就追加的按钮。
//!
//! 键名沿用 AHK 的写法（`Volume_Up` / `Media_Play_Pause` …），
//! 从 AHK 脚本里抄过来的写法能直接用；`win32::virtual_key` 认这些名字。

/// 一片按钮：`(分组标题, [(按钮上写的字, 写进设置的键名), ...])`
pub const GROUPS: &[(&str, &[(&str, &str)])] = &[
    (
        "音量",
        &[
            ("音量 +", "Volume_Up"),
            ("音量 −", "Volume_Down"),
            ("静音", "Volume_Mute"),
        ],
    ),
    (
        "播放",
        &[
            ("播放 / 暂停", "Media_Play_Pause"),
            ("上一首", "Media_Prev"),
            ("下一首", "Media_Next"),
            ("停止", "Media_Stop"),
        ],
    ),
    (
        "浏览器",
        &[
            ("后退", "Browser_Back"),
            ("前进", "Browser_Forward"),
            ("刷新", "Browser_Refresh"),
            ("停止", "Browser_Stop"),
            ("搜索", "Browser_Search"),
            ("收藏夹", "Browser_Favorites"),
            ("主页", "Browser_Home"),
        ],
    ),
    (
        "启动",
        &[
            ("邮件", "Launch_Mail"),
            ("媒体播放器", "Launch_Media"),
            ("我的电脑", "Launch_App1"),
            ("计算器", "Launch_App2"),
        ],
    ),
    (
        "补充",
        &[("睡眠", "Sleep"), ("菜单键", "Apps"), ("截屏", "PrintScreen")],
    ),
];

/// 「动作」里那排常用键（不想展开整片多媒体键时点这个就够）
pub const QUICK: &[(&str, &str)] = &[
    ("音量 +", "Volume_Up"),
    ("音量 −", "Volume_Down"),
    ("静音", "Volume_Mute"),
    ("播放 / 暂停", "Media_Play_Pause"),
    ("上一首", "Media_Prev"),
    ("下一首", "Media_Next"),
];
