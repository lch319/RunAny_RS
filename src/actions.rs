//! 动作库：内置动作的目录 + 执行器 + AHK 脚本。
//!
//! 设计目标是「以后可以加到成百上千条」：
//!
//! * **目录**（[`BUILTINS`]）只是数据：id / 名字 / 说明 / 分类 / 参数表。
//!   界面（设置 → 动作库）拿它渲染，不用改界面代码就能加动作；
//! * **执行器**（[`execute`]）按 id 分发。参数统一是 `键 -> 字符串`，
//!   缺失或写错的参数按默认值走，不会因为一个空参数就整条失败；
//! * **脚本**（[`run_script`]）把 AHK v1 / v2 代码写到临时文件再用解释器跑，
//!   解释器路径可以在设置里指定，也会自动去注册表和常见目录里找。
//!
//! 加一条新动作 = 在 [`BUILTINS`] 里加一行 + 在 [`execute`] 里加一个分支。

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::i18n::t;

/// 动作分类（设置 → 动作库左侧那一列）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Menu,
    System,
    Window,
    Media,
    Clipboard,
    Input,
    File,
    Net,
    Script,
}

impl Category {
    pub const ALL: [Category; 9] = [
        Category::Menu,
        Category::System,
        Category::Window,
        Category::Media,
        Category::Clipboard,
        Category::Input,
        Category::File,
        Category::Net,
        Category::Script,
    ];

    pub fn label(self) -> &'static str {
        t(match self {
            Category::Menu => "菜单与程序",
            Category::System => "系统",
            Category::Window => "窗口",
            Category::Media => "声音与屏幕",
            Category::Clipboard => "剪贴板",
            Category::Input => "输入与文本",
            Category::File => "文件",
            Category::Net => "网络",
            Category::Script => "脚本",
        })
    }
}

/// 参数的形状。界面按它画控件，执行器按它取默认值。
pub enum ParamKind {
    /// 单行文本
    Text,
    /// 整数（带范围和单位）
    Int {
        min: i32,
        max: i32,
        step: i32,
        unit: &'static str,
    },
    /// 几个里选一个：`(存进配置的值, 显示的名字)`
    Choice(&'static [(&'static str, &'static str)]),
    /// 开关
    Bool,
}

pub struct ParamDef {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub default: &'static str,
    pub hint: &'static str,
}

/// 目录里的一条动作。全是静态数据，编译进二进制，查表是 O(n) 但 n 小且结果可缓存。
pub struct BuiltinDef {
    pub id: &'static str,
    pub name: &'static str,
    pub desc: &'static str,
    pub category: Category,
    pub params: &'static [ParamDef],
}

/// 常用参数表的简写
const NONE: &[ParamDef] = &[];

const ARG_PATH: &[ParamDef] = &[ParamDef {
    key: "path",
    label: "路径",
    kind: ParamKind::Text,
    default: "",
    hint: "支持 {clipboard} 这类占位符；留空就是当前选中的文件 / 文件夹",
}];

/// 原版 RunAny 插件调用：整串 `名字[函数](参数)` 原样填进来
const ARG_PLUGIN_CALL: &[ParamDef] = &[ParamDef {
    key: "call",
    label: "插件调用",
    kind: ParamKind::Text,
    default: "",
    hint: "例如 XiaoYao_plus[hotkeyStatus](%\"clash.exe\"%)，原版 RunAny.ini 里的写法直接粘过来",
}];

const ARG_TEXT: &[ParamDef] = &[ParamDef {
    key: "text",
    label: "文本",
    kind: ParamKind::Text,
    default: "",
    hint: "支持占位符，例如 {clipboard} / {date} / {time}",
}];

/// 动作库目录。
///
/// 顺序就是界面上的顺序：同分类里越靠前越常用。
pub static BUILTINS: &[BuiltinDef] = &[
    // ---------------- 菜单与程序 ----------------
    BuiltinDef {
        id: "open-settings",
        name: "打开设置窗口",
        desc: "把本软件的设置窗口叫到前面",
        category: Category::Menu,
        params: NONE,
    },
    BuiltinDef {
        id: "open-config-dir",
        name: "打开配置目录",
        desc: "在资源管理器里打开 settings.json 所在目录",
        category: Category::Menu,
        params: NONE,
    },
    BuiltinDef {
        id: "about",
        name: "打开关于",
        desc: "快捷键说明 / 版本 / 配置路径",
        category: Category::Menu,
        params: NONE,
    },
    BuiltinDef {
        id: "reload-menu",
        name: "重新加载菜单",
        desc: "改完配置立刻生效（不重启进程）",
        category: Category::Menu,
        params: NONE,
    },
    BuiltinDef {
        id: "open-menu",
        name: "弹出完整菜单",
        desc: "把完整菜单（不做选中内容筛选）弹出来",
        category: Category::Menu,
        params: NONE,
    },
    BuiltinDef {
        id: "restart",
        name: "重启软件",
        desc: "退出后重新拉起自己",
        category: Category::Menu,
        params: NONE,
    },
    BuiltinDef {
        id: "quit",
        name: "退出程序",
        desc: "连托盘图标一起退出",
        category: Category::Menu,
        params: NONE,
    },
    BuiltinDef {
        id: "close-menu",
        name: "只关闭菜单",
        desc: "收起菜单，什么都不做",
        category: Category::Menu,
        params: NONE,
    },
    BuiltinDef {
        id: "copy-selection",
        name: "复制选中内容",
        desc: "把弹出菜单前抓到的选中内容写回剪贴板",
        category: Category::Menu,
        params: NONE,
    },
    // ---------------- 系统 ----------------
    BuiltinDef {
        id: "sys.lock",
        name: "锁定屏幕",
        desc: "等同于 Win+L",
        category: Category::System,
        params: NONE,
    },
    BuiltinDef {
        id: "sys.power",
        name: "电源操作",
        desc: "关机 / 重启 / 休眠 / 注销",
        category: Category::System,
        params: &[ParamDef {
            key: "action",
            label: "做什么",
            kind: ParamKind::Choice(&[
                ("shutdown", "关机"),
                ("restart", "重启"),
                ("hibernate", "休眠"),
                ("logoff", "注销"),
                ("sleep", "睡眠"),
            ]),
            default: "shutdown",
            hint: "关机和重启会立刻执行，不会弹确认框",
        },
        ParamDef {
            key: "force",
            label: "强制",
            kind: ParamKind::Bool,
            default: "0",
            hint: "开着就先结束没有响应的程序",
        }],
    },
    BuiltinDef {
        id: "sys.monitor-off",
        name: "关闭显示器",
        desc: "只关屏，不解锁也不睡眠",
        category: Category::System,
        params: NONE,
    },
    BuiltinDef {
        id: "sys.recycle-bin",
        name: "清空回收站",
        desc: "删除所有回收站内容（不弹确认）",
        category: Category::System,
        params: NONE,
    },
    BuiltinDef {
        id: "sys.task-manager",
        name: "任务管理器",
        desc: "打开 taskmgr.exe",
        category: Category::System,
        params: NONE,
    },
    BuiltinDef {
        id: "sys.settings",
        name: "Windows 设置",
        desc: "打开「设置」应用",
        category: Category::System,
        params: NONE,
    },
    BuiltinDef {
        id: "sys.control-panel",
        name: "控制面板",
        desc: "打开经典控制面板",
        category: Category::System,
        params: NONE,
    },
    BuiltinDef {
        id: "sys.device-manager",
        name: "设备管理器",
        desc: "打开 devmgmt.msc",
        category: Category::System,
        params: NONE,
    },
    BuiltinDef {
        id: "sys.services",
        name: "服务",
        desc: "打开 services.msc",
        category: Category::System,
        params: NONE,
    },
    BuiltinDef {
        id: "sys.registry",
        name: "注册表编辑器",
        desc: "打开 regedit",
        category: Category::System,
        params: NONE,
    },
    BuiltinDef {
        id: "sys.show-desktop",
        name: "显示桌面 / 最小化全部",
        desc: "等同于 Win+D",
        category: Category::System,
        params: NONE,
    },
    // ---------------- 声音与屏幕 ----------------
    BuiltinDef {
        id: "sys.volume",
        name: "调节音量",
        desc: "音量加 / 减 / 静音（走系统媒体键）",
        category: Category::Media,
        params: &[
            ParamDef {
                key: "action",
                label: "做什么",
                kind: ParamKind::Choice(&[
                    ("up", "调大"),
                    ("down", "调小"),
                    ("mute", "静音开关"),
                ]),
                default: "mute",
                hint: "「静音开关」是按一下切换",
            },
            ParamDef {
                key: "steps",
                label: "按几下",
                kind: ParamKind::Int {
                    min: 1,
                    max: 50,
                    step: 1,
                    unit: "下",
                },
                default: "2",
                hint: "每下 2%，按 25 下就是从 0 到满",
            },
        ],
    },
    BuiltinDef {
        id: "sys.media",
        name: "媒体控制",
        desc: "播放 / 暂停、上一首、下一首、停止",
        category: Category::Media,
        params: &[ParamDef {
            key: "action",
            label: "做什么",
            kind: ParamKind::Choice(&[
                ("play-pause", "播放 / 暂停"),
                ("next", "下一首"),
                ("prev", "上一首"),
                ("stop", "停止"),
            ]),
            default: "play-pause",
            hint: "对当前在放歌的程序生效（浏览器也认）",
        }],
    },
    BuiltinDef {
        id: "sys.brightness",
        name: "调节屏幕亮度",
        desc: "设置内屏亮度（0-100），走 WMI",
        category: Category::Media,
        params: &[ParamDef {
            key: "level",
            label: "亮度",
            kind: ParamKind::Int {
                min: 0,
                max: 100,
                step: 5,
                unit: "%",
            },
            default: "60",
            hint: "多数笔记本有效；外接显示器一般无效（那要用 DDC/CI）",
        }],
    },
    // ---------------- 窗口 ----------------
    BuiltinDef {
        id: "win.topmost",
        name: "窗口置顶开关",
        desc: "把前台窗口切到「总在最前」或取消，可以顺手加一圈彩色边框",
        category: Category::Window,
        params: &[
            ParamDef {
                key: "mode",
                label: "模式",
                kind: ParamKind::Choice(&[
                    ("toggle", "自动切换"),
                    ("on", "置顶"),
                    ("off", "取消置顶"),
                ]),
                default: "toggle",
                hint: "「自动切换」＝现在是置顶就取消，不是就置顶",
            },
            ParamDef {
                key: "border",
                label: "加外边框",
                kind: ParamKind::Bool,
                default: "0",
                hint: "置顶时给窗口描一圈彩色边（Windows 11 支持；Win10 上颜色可能不生效）",
            },
            ParamDef {
                key: "border_width",
                label: "边框粗细",
                kind: ParamKind::Int {
                    min: 1,
                    max: 12,
                    step: 1,
                    unit: "px",
                },
                default: "2",
                hint: "Win11 22H2 以上才认；老系统上会被忽略",
            },
            ParamDef {
                key: "border_color",
                label: "边框颜色",
                kind: ParamKind::Text,
                default: "#FF3B30",
                hint: "写 #RRGGBB，比如 #FF3B30 红、#34C759 绿、#0A84FF 蓝",
            },
        ],
    },
    BuiltinDef {
        id: "win.minimize",
        name: "最小化窗口",
        desc: "最小化前台窗口",
        category: Category::Window,
        params: NONE,
    },
    BuiltinDef {
        id: "win.maximize",
        name: "最大化 / 还原",
        desc: "前台窗口在最大化和普通之间切换",
        category: Category::Window,
        params: NONE,
    },
    BuiltinDef {
        id: "win.close",
        name: "关闭窗口",
        desc: "给前台窗口发 Alt+F4",
        category: Category::Window,
        params: NONE,
    },
    BuiltinDef {
        id: "win.left-half",
        name: "靠左半屏",
        desc: "前台窗口铺满左半边工作区",
        category: Category::Window,
        params: NONE,
    },
    BuiltinDef {
        id: "win.right-half",
        name: "靠右半屏",
        desc: "前台窗口铺满右半边工作区",
        category: Category::Window,
        params: NONE,
    },
    BuiltinDef {
        id: "win.center",
        name: "窗口居中",
        desc: "把前台窗口挪到当前屏幕中间（大小不变）",
        category: Category::Window,
        params: NONE,
    },
    BuiltinDef {
        id: "win.switch",
        name: "切换到下一个窗口",
        desc: "等同于 Alt+Tab",
        category: Category::Window,
        params: NONE,
    },
    // ---------------- 剪贴板 ----------------
    BuiltinDef {
        id: "clip.copy",
        name: "复制",
        desc: "给前台窗口发 Ctrl+C",
        category: Category::Clipboard,
        params: NONE,
    },
    BuiltinDef {
        id: "clip.cut",
        name: "剪切",
        desc: "给前台窗口发 Ctrl+X",
        category: Category::Clipboard,
        params: NONE,
    },
    BuiltinDef {
        id: "clip.paste",
        name: "粘贴",
        desc: "给前台窗口发 Ctrl+V",
        category: Category::Clipboard,
        params: NONE,
    },
    BuiltinDef {
        id: "clip.clear",
        name: "清空剪贴板",
        desc: "把剪贴板内容清掉",
        category: Category::Clipboard,
        params: NONE,
    },
    BuiltinDef {
        id: "clip.transform",
        name: "剪贴板文字变形",
        desc: "转大写 / 小写 / 去首尾空白 / 去空行，结果写回剪贴板",
        category: Category::Clipboard,
        params: &[ParamDef {
            key: "action",
            label: "怎么变",
            kind: ParamKind::Choice(&[
                ("upper", "全部大写"),
                ("lower", "全部小写"),
                ("trim", "去掉首尾空白"),
                ("squash", "去掉空行"),
                ("reverse", "反转顺序（按行）"),
            ]),
            default: "upper",
            hint: "只处理文本；图片 / 文件不动",
        }],
    },
    BuiltinDef {
        id: "clip.save",
        name: "剪贴板存成文件",
        desc: "把剪贴板文本写到一个 .txt",
        category: Category::Clipboard,
        params: &[ParamDef {
            key: "path",
            label: "存到",
            kind: ParamKind::Text,
            default: "{desktop}\\剪贴板.txt",
            hint: "支持占位符；同名文件会被覆盖",
        }],
    },
    // ---------------- 输入与文本 ----------------
    BuiltinDef {
        id: "input.datetime",
        name: "插入日期时间",
        desc: "把当前时间按格式打出去",
        category: Category::Input,
        params: &[ParamDef {
            key: "format",
            label: "格式",
            kind: ParamKind::Text,
            default: "yyyy-MM-dd HH:mm:ss",
            hint: "yyyy 年 MM 月 dd 日 HH 时 mm 分 ss 秒，可自由组合",
        }],
    },
    BuiltinDef {
        id: "runany-plugin",
        name: "执行原版插件功能",
        desc: "调用原版 RunAny 的插件函数（从 RunAny.ini 导入的插件菜单项会落到这一条）；功能还没接上，调用先原样存着",
        category: Category::Script,
        params: ARG_PLUGIN_CALL,
    },
    BuiltinDef {
        id: "input.text",
        name: "插入一段文本",
        desc: "直接当键盘输入打出去（不碰剪贴板）",
        category: Category::Input,
        params: ARG_TEXT,
    },
    BuiltinDef {
        id: "input.uuid",
        name: "插入 UUID",
        desc: "生成一个随机 UUID（v4 形状）并打出去",
        category: Category::Input,
        params: NONE,
    },
    BuiltinDef {
        id: "input.password",
        name: "生成随机密码",
        desc: "生成后直接打出去",
        category: Category::Input,
        params: &[
            ParamDef {
                key: "length",
                label: "长度",
                kind: ParamKind::Int {
                    min: 4,
                    max: 64,
                    step: 1,
                    unit: "位",
                },
                default: "16",
                hint: "用系统 CSPRNG 生成",
            },
            ParamDef {
                key: "charset",
                label: "字符集",
                kind: ParamKind::Choice(&[
                    ("alnum", "字母 + 数字"),
                    ("alpha", "只有字母"),
                    ("symbol", "字母 + 数字 + 符号"),
                    ("digit", "只有数字"),
                ]),
                default: "alnum",
                hint: "符号集不包含引号和反斜杠，避免命令行转义麻烦",
            },
        ],
    },
    BuiltinDef {
        id: "input.run-script",
        name: "运行 AHK 脚本",
        desc: "执行一段 AutoHotkey v1 / v2 代码（脚本内容在「动作」里填）",
        category: Category::Script,
        params: NONE,
    },
    // ---------------- 文件 ----------------
    BuiltinDef {
        id: "file.open",
        name: "打开路径",
        desc: "用默认程序打开文件或文件夹",
        category: Category::File,
        params: ARG_PATH,
    },
    BuiltinDef {
        id: "file.reveal",
        name: "在资源管理器中显示",
        desc: "打开所在文件夹并选中这个文件",
        category: Category::File,
        params: ARG_PATH,
    },
    BuiltinDef {
        id: "file.copy-path",
        name: "复制路径",
        desc: "把路径写进剪贴板",
        category: Category::File,
        params: ARG_PATH,
    },
    // ---------------- 网络 ----------------
    BuiltinDef {
        id: "net.ip",
        name: "复制本机 IP",
        desc: "把当前 IPv4 地址写进剪贴板",
        category: Category::Net,
        params: NONE,
    },
    BuiltinDef {
        id: "net.open-url",
        name: "打开网址",
        desc: "用默认浏览器打开一个网址",
        category: Category::Net,
        params: &[ParamDef {
            key: "url",
            label: "网址",
            kind: ParamKind::Text,
            default: "https://",
            hint: "不写 https:// 也能开",
        }],
    },
];

/// 按 id 找一条动作。
pub fn find(id: &str) -> Option<&'static BuiltinDef> {
    BUILTINS.iter().find(|def| def.id == id.trim())
}

/// 这条 id 认不认识（旧的「内置动作」枚举也算）。
pub fn is_known(id: &str) -> bool {
    find(id).is_some() || crate::settings::BuiltinAction::from_key(id).is_some()
}

/// 默认参数（加进菜单时先填一套，用户再改）。
pub fn defaults(def: &BuiltinDef) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for param in def.params {
        if !param.default.is_empty() {
            map.insert(param.key.to_string(), param.default.to_string());
        }
    }
    map
}

/// 一个参数值在界面上怎么显示（`10` + 单位 `下`、枚举值换成中文、空文本写「（空）」）。
pub fn display_value(param: &ParamDef, value: &str) -> String {
    let value = if value.is_empty() {
        param.default
    } else {
        value
    };
    match &param.kind {
        ParamKind::Text => {
            if value.is_empty() {
                t("（空）").to_string()
            } else {
                value.to_string()
            }
        }
        ParamKind::Int { unit, .. } => {
            if unit.is_empty() {
                value.to_string()
            } else {
                format!("{value}{unit}")
            }
        }
        ParamKind::Choice(options) => options
            .iter()
            .find(|(key, _)| *key == value)
            .map(|(_, label)| t(label).to_string())
            .unwrap_or_else(|| value.to_string()),
        ParamKind::Bool => {
            if matches!(value, "true" | "1" | "on" | "yes") {
                t("开").to_string()
            } else {
                t("关").to_string()
            }
        }
    }
}

/// 搜索：名字、说明、id、分类名都能命中（空格分隔的多个词是「且」）。
pub fn search(query: &str) -> Vec<&'static BuiltinDef> {
    let words: Vec<String> = query
        .split_whitespace()
        .map(|word| word.to_lowercase())
        .collect();
    BUILTINS
        .iter()
        .filter(|def| {
            if words.is_empty() {
                return true;
            }
            let haystack = format!(
                "{} {} {} {}",
                t(def.name),
                t(def.desc),
                def.id,
                def.category.label()
            )
            .to_lowercase();
            words.iter().all(|word| haystack.contains(word))
        })
        .collect()
}

/// 一条动作的参数（带默认值兜底）。
pub struct Values<'a>(&'a BTreeMap<String, String>);

impl<'a> Values<'a> {
    pub fn new(map: &'a BTreeMap<String, String>) -> Self {
        Self(map)
    }

    pub fn text(&self, key: &str, default: &str) -> String {
        self.0
            .get(key)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    pub fn int(&self, key: &str, default: i32) -> i32 {
        self.0
            .get(key)
            .and_then(|value| value.trim().parse::<i32>().ok())
            .unwrap_or(default)
    }

    pub fn flag(&self, key: &str, default: bool) -> bool {
        match self.0.get(key).map(|value| value.trim()) {
            Some("1" | "true" | "on" | "yes") => true,
            Some("0" | "false" | "off" | "no") => false,
            _ => default,
        }
    }
}

/// 执行一条内置动作。
///
/// 旧的 9 个「内置动作」枚举（`open-settings` / `quit` …）也走这里，
/// 所以老配置不用迁移。
pub fn execute(id: &str, params: &BTreeMap<String, String>) -> Result<(), String> {
    let values = Values::new(params);
    let id = id.trim();

    // 原版 RunAny 插件：**功能还没实现**。
    //
    // 调用文本（`名字[函数](参数)`）原样存在参数 `call` 里，一个字都不改。
    // 等摸清插件的调用约定（怎么加载插件 DLL、函数怎么传参、返回值怎么用），
    // 在这一支里接上即可 —— 界面、导入、存盘都不用再动。
    //
    // 现在只打一行日志：用户点了没反应时，至少能在控制台看到它确实被调到了。
    if id == "runany-plugin" {
        let call = values.text("call", "");
        if call.trim().is_empty() {
            return Err(crate::i18n::t("插件调用是空的：把原版那一串填进参数里").to_string());
        }
        eprintln!("[popup-menu] 原版插件调用还没接上，先跳过：{call}");
        return Ok(());
    }

    // 「打开设置 / 退出程序」这几条要操作本进程（需要 `App`），所以在
    // `popup::execute_step` 里就被拦下来了，根本走不到这儿。
    match id {
        // ---- 系统 ----
        "sys.lock" => shell("rundll32.exe user32.dll,LockWorkStation"),
        "sys.power" => {
            let force = if values.flag("force", false) { " /f" } else { "" };
            match values.text("action", "shutdown").as_str() {
                "restart" => shell(&format!("shutdown /r /t 0{force}")),
                "hibernate" => shell("shutdown /h"),
                "logoff" => shell(&format!("shutdown /l{force}")),
                "sleep" => shell("rundll32.exe powrprof.dll,SetSuspendState 0,1,0"),
                _ => shell(&format!("shutdown /s /t 0{force}")),
            }
        }
        "sys.monitor-off" => {
            #[cfg(windows)]
            {
                crate::win32::monitor_off();
                Ok(())
            }
            #[cfg(not(windows))]
            {
                Err("仅 Windows".into())
            }
        }
        "sys.recycle-bin" => {
            #[cfg(windows)]
            {
                crate::win32::empty_recycle_bin()
            }
            #[cfg(not(windows))]
            {
                Err("仅 Windows".into())
            }
        }
        "sys.task-manager" => shell("start \"\" taskmgr.exe"),
        "sys.settings" => shell("start \"\" ms-settings:"),
        "sys.control-panel" => shell("start \"\" control.exe"),
        "sys.device-manager" => shell("start \"\" devmgmt.msc"),
        "sys.services" => shell("start \"\" services.msc"),
        "sys.registry" => shell("start \"\" regedit.exe"),
        "sys.show-desktop" => combo(&[Key::Win, Key::Char('D')]),

        // ---- 声音与屏幕 ----
        "sys.volume" => {
            let times = values.int("steps", 2).clamp(1, 50) as usize;
            match values.text("action", "mute").as_str() {
                "up" => repeat_key(0xAF, times),
                "down" => repeat_key(0xAE, times),
                _ => repeat_key(0xAD, 1),
            }
        }
        "sys.media" => {
            let vk = match values.text("action", "play-pause").as_str() {
                "next" => 0xB0,
                "prev" => 0xB1,
                "stop" => 0xB2,
                _ => 0xB3,
            };
            repeat_key(vk, 1)
        }
        "sys.brightness" => {
            let level = values.int("level", 60).clamp(0, 100);
            let script = format!(
                "(Get-CimInstance -Namespace root/WMI -ClassName WmiMonitorBrightnessMethods).WmiSetBrightness(1,{level})"
            );
            shell(&format!(
                "powershell -NoProfile -WindowStyle Hidden -Command \"{script}\""
            ))
        }

        // ---- 窗口 ----
        "win.topmost" => {
            #[cfg(windows)]
            {
                let hwnd = crate::win32::foreground_window();
                let mode = values.text("mode", "toggle");
                let topmost = crate::win32::set_topmost(hwnd, &mode)?;
                if values.flag("border", false) && topmost {
                    let thickness = values.int("border_width", 2).clamp(1, 12) as u32;
                    let color = crate::win32::parse_hex_color(&values.text("border_color", "#FF3B30"));
                    crate::win32::set_window_border(hwnd, color, thickness);
                } else if !topmost {
                    // 取消置顶时把边框一起收掉，不然会留在窗口上
                    crate::win32::set_window_border(hwnd, None, 0);
                }
                Ok(())
            }
            #[cfg(not(windows))]
            {
                Err("仅 Windows".into())
            }
        }
        "win.minimize" => {
            #[cfg(windows)]
            {
                crate::win32::minimize_window(crate::win32::foreground_window());
                Ok(())
            }
            #[cfg(not(windows))]
            {
                Err("仅 Windows".into())
            }
        }
        "win.maximize" => {
            #[cfg(windows)]
            {
                let hwnd = crate::win32::foreground_window();
                crate::win32::toggle_maximize(hwnd)
            }
            #[cfg(not(windows))]
            {
                Err("仅 Windows".into())
            }
        }
        "win.close" => combo(&[Key::Alt, Key::Func(0x73)]), // Alt+F4
        "win.left-half" => half_screen(true),
        "win.right-half" => half_screen(false),
        "win.center" => center_window(),
        "win.switch" => combo(&[Key::Alt, Key::Tab]),

        // ---- 剪贴板 ----
        "clip.copy" => combo(&[Key::Ctrl, Key::Char('C')]),
        "clip.cut" => combo(&[Key::Ctrl, Key::Char('X')]),
        "clip.paste" => combo(&[Key::Ctrl, Key::Char('V')]),
        "clip.clear" => {
            crate::win32::write_clipboard_text("");
            Ok(())
        }
        "clip.transform" => {
            let Some(text) = crate::win32::read_clipboard_text() else {
                return Err("剪贴板里没有文字".into());
            };
            let out = match values.text("action", "upper").as_str() {
                "lower" => text.to_lowercase(),
                "trim" => text.trim().to_string(),
                "squash" => text
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join("\r\n"),
                "reverse" => text
                    .lines()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\r\n"),
                _ => text.to_uppercase(),
            };
            crate::win32::write_clipboard_text(&out);
            Ok(())
        }
        "clip.save" => {
            let Some(text) = crate::win32::read_clipboard_text() else {
                return Err("剪贴板里没有文字".into());
            };
            let path = expand(&values.text("path", "{desktop}\\剪贴板.txt"));
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&path, text).map_err(|err| format!("写不了 {path}：{err}"))
        }

        // ---- 输入与文本 ----
        "input.datetime" => {
            let format = values.text("format", "yyyy-MM-dd HH:mm:ss");
            crate::win32::send_text(&format_time(&format))
        }
        "input.text" => {
            let text = expand(&values.text("text", ""));
            if text.is_empty() {
                return Ok(());
            }
            crate::win32::send_text(&text)
        }
        "input.uuid" => crate::win32::send_text(&uuid()),
        "input.password" => {
            let length = values.int("length", 16).clamp(4, 64) as usize;
            let charset = match values.text("charset", "alnum").as_str() {
                "alpha" => "abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ",
                "digit" => "23456789",
                "symbol" => {
                    "abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789!@#$%^&*()-_=+[]{}:,.?"
                }
                _ => "abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789",
            };
            crate::win32::send_text(&random_string(length, charset))
        }

        // ---- 文件 ----
        "file.open" => {
            let path = expand(&values.text("path", ""));
            let path = if path.is_empty() {
                crate::selection::first_path().unwrap_or_default()
            } else {
                path
            };
            if path.is_empty() {
                return Err("没有选中任何文件".into());
            }
            shell(&format!("start \"\" \"{path}\""))
        }
        "file.reveal" => {
            let path = expand(&values.text("path", ""));
            let path = if path.is_empty() {
                crate::selection::first_path().unwrap_or_default()
            } else {
                path
            };
            if path.is_empty() {
                return Err("没有选中任何文件".into());
            }
            shell(&format!("explorer /select,\"{path}\""))
        }
        "file.copy-path" => {
            let path = expand(&values.text("path", ""));
            let path = if path.is_empty() {
                crate::selection::first_path().unwrap_or_default()
            } else {
                path
            };
            crate::win32::write_clipboard_text(&path);
            Ok(())
        }

        // ---- 网络 ----
        "net.ip" => shell(
            "powershell -NoProfile -WindowStyle Hidden -Command \
             \"Get-NetIPAddress -AddressFamily IPv4 | Where-Object { $_.IPAddress -ne '127.0.0.1' } | \
             Select-Object -First 1 -ExpandProperty IPAddress | Set-Clipboard\"",
        ),
        "net.open-url" => {
            let url = values.text("url", "");
            if url.is_empty() {
                return Err("网址是空的".into());
            }
            let url = if url.contains("://") {
                url
            } else {
                format!("https://{url}")
            };
            shell(&format!("start \"\" \"{url}\""))
        }

        // ---- 脚本 ----
        "input.run-script" => Err("这条只是目录里的说明，真正执行请用「运行脚本」类型的动作".into()),

        other => Err(format!("不认识的动作 {other:?}")),
    }
}

/// 用 `cmd /c` 跑一条命令（不闪黑窗口）。
pub fn shell(command: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .arg("/C")
            .raw_arg(command)
            .creation_flags(0x0800_0000)
            .spawn()
            .map(|_| ())
            .map_err(|err| err.to_string())
    }
    #[cfg(not(windows))]
    {
        std::process::Command::new("sh")
            .args(["-c", command])
            .spawn()
            .map(|_| ())
            .map_err(|err| err.to_string())
    }
}

// ---------------------------------------------------------------- 键盘

#[derive(Clone, Copy)]
enum Key {
    Ctrl,
    Alt,
    Win,
    Tab,
    Char(char),
    Func(u16),
}

/// 按一个组合键（修饰键依次按下，最后一个是主键）。
fn combo(keys: &[Key]) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            keybd_event, KEYEVENTF_KEYUP, VK_CONTROL, VK_LWIN, VK_MENU,
        };
        let vk_of = |key: Key| -> u16 {
            match key {
                Key::Ctrl => VK_CONTROL as u16,
                Key::Alt => VK_MENU as u16,
                Key::Win => VK_LWIN as u16,
                Key::Tab => 0x09,
                Key::Func(vk) => vk,
                Key::Char(ch) => ch.to_ascii_uppercase() as u16,
            }
        };
        unsafe {
            for key in keys {
                keybd_event(vk_of(*key) as u8, 0, 0, 0);
                std::thread::sleep(std::time::Duration::from_millis(12));
            }
            for key in keys.iter().rev() {
                keybd_event(vk_of(*key) as u8, 0, KEYEVENTF_KEYUP, 0);
                std::thread::sleep(std::time::Duration::from_millis(12));
            }
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = keys;
        Err("仅 Windows".into())
    }
}

/// 连按 `times` 次同一个虚拟键（音量、媒体键都是这种）。
fn repeat_key(vk: u16, times: usize) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            keybd_event, KEYEVENTF_KEYUP,
        };
        for _ in 0..times.max(1) {
            unsafe {
                keybd_event(vk as u8, 0, 0, 0);
                std::thread::sleep(std::time::Duration::from_millis(8));
                keybd_event(vk as u8, 0, KEYEVENTF_KEYUP, 0);
            }
            std::thread::sleep(std::time::Duration::from_millis(18));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (vk, times);
        Err("仅 Windows".into())
    }
}

#[cfg(windows)]
fn half_screen(left: bool) -> Result<(), String> {
    let hwnd = crate::win32::foreground_window();
    if hwnd == 0 {
        return Err("没有前台窗口".into());
    }
    let Some((x, y, w, h)) = crate::win32::window_work_area(hwnd) else {
        return Err("读不到工作区".into());
    };
    let half = w / 2;
    let target = if left { x } else { x + half };
    crate::win32::set_window_bounds(hwnd, target, y, half, h);
    Ok(())
}

#[cfg(not(windows))]
fn half_screen(_left: bool) -> Result<(), String> {
    Err("仅 Windows".into())
}

#[cfg(windows)]
fn center_window() -> Result<(), String> {
    let hwnd = crate::win32::foreground_window();
    if hwnd == 0 {
        return Err("没有前台窗口".into());
    }
    let Some((wx, wy, ww, wh)) = crate::win32::window_work_area(hwnd) else {
        return Err("读不到工作区".into());
    };
    let Some((_, _, width, height)) = crate::win32::window_rect(hwnd) else {
        return Err("读不到窗口大小".into());
    };
    let x = wx + (ww - width) / 2;
    let y = wy + (wh - height) / 2;
    crate::win32::set_window_bounds(hwnd, x, y, width, height);
    Ok(())
}

#[cfg(not(windows))]
fn center_window() -> Result<(), String> {
    Err("仅 Windows".into())
}

// ---------------------------------------------------------------- 文本小工具

/// 展开 `{desktop}` / `{date}` / `{time}` 这类小占位符（`placeholders::expand` 之外的两个）。
fn expand(text: &str) -> String {
    let text = crate::placeholders::expand(text);
    let now = crate::win32::local_time();
    text.replace("{desktop}", &desktop_dir())
        .replace(
            "{date}",
            &format!("{:04}-{:02}-{:02}", now.year, now.month, now.day),
        )
        .replace(
            "{time}",
            &format!("{:02}:{:02}:{:02}", now.hour, now.minute, now.second),
        )
}

fn desktop_dir() -> String {
    std::env::var("USERPROFILE")
        .map(|home| format!("{home}\\Desktop"))
        .unwrap_or_default()
}

/// `yyyy-MM-dd HH:mm:ss` 这种格式（AHK 的写法，不认识的原样输出）。
pub fn format_time(format: &str) -> String {
    let now = crate::win32::local_time();
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(ch) = chars.next() {
        let mut run = 1;
        while chars.peek() == Some(&ch) {
            chars.next();
            run += 1;
        }
        let piece = match (ch, run) {
            ('y', _) => format!("{:04}", now.year),
            ('M', _) => format!("{:02}", now.month),
            ('d', _) => format!("{:02}", now.day),
            ('H', _) => format!("{:02}", now.hour),
            ('h', _) => format!("{:02}", if now.hour % 12 == 0 { 12 } else { now.hour % 12 }),
            ('m', _) => format!("{:02}", now.minute),
            ('s', _) => format!("{:02}", now.second),
            _ => ch.to_string().repeat(run),
        };
        out.push_str(&piece);
    }
    out
}

/// 系统 CSPRNG 取随机字节；拿不到就退回时间（只影响密码强度，不影响功能）。
fn random_bytes(len: usize) -> Vec<u8> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        let mut buffer = vec![0u8; len];
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buffer.as_mut_ptr(),
                len as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        if status == 0 {
            return buffer;
        }
    }
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15)
        ^ (&len as *const usize as u64);
    (0..len)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 24) as u8
        })
        .collect()
}

fn random_string(len: usize, charset: &str) -> String {
    let alphabet: Vec<char> = charset.chars().collect();
    let bytes = random_bytes(len.max(1));
    bytes
        .iter()
        .map(|byte| alphabet[*byte as usize % alphabet.len()])
        .collect()
}

/// 随机 UUID（v4 形状，够用即可）。
pub fn uuid() -> String {
    let mut bytes = random_bytes(16);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: Vec<String> = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        hex[0..4].concat(),
        hex[4..6].concat(),
        hex[6..8].concat(),
        hex[8..10].concat(),
        hex[10..16].concat()
    )
}

// ---------------------------------------------------------------- AHK 脚本

/// 脚本解释器用哪个。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptLang {
    #[default]
    V1,
    V2,
}

impl ScriptLang {
    pub const ALL: [ScriptLang; 2] = [ScriptLang::V1, ScriptLang::V2];

    pub fn key(self) -> &'static str {
        match self {
            ScriptLang::V1 => "v1",
            ScriptLang::V2 => "v2",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key.trim().to_ascii_lowercase().as_str() {
            "v2" | "ahk2" | "2" => ScriptLang::V2,
            _ => ScriptLang::V1,
        }
    }

    /// 插件目录名：`Plugins\ahkv1` / `Plugins\ahkv2`
    pub fn dir_name(self) -> &'static str {
        match self {
            ScriptLang::V1 => "ahkv1",
            ScriptLang::V2 => "ahkv2",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ScriptLang::V1 => "AutoHotkey v1",
            ScriptLang::V2 => "AutoHotkey v2",
        }
    }
}

/// 程序目录下的插件文件夹：`Plugins\ahkv1` / `Plugins\ahkv2`。
///
/// 把解释器和脚本丢进这两个文件夹就行：
///
/// ```text
/// RunAny_RS.exe
/// Plugins\ahkv1\AutoHotkeyU64.exe
/// Plugins\ahkv1\我的脚本.ahk
/// Plugins\ahkv2\AutoHotkey64.exe
/// ```
///
/// 好处是整个文件夹拷走就能搬家，也不用在设置里挨个写路径。
pub fn plugin_dir(lang: ScriptLang) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.join("Plugins").join(lang.dir_name());
    Some(dir)
}

/// 把某个文件夹里的文件按后缀收一遍（最多往下钻 `depth` 层，跳过隐藏目录）。
fn list_files(dir: &std::path::Path, ext: &str, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            if depth > 0 {
                list_files(&path, ext, depth - 1, out);
            }
            continue;
        }
        let matches = path
            .extension()
            .map(|value| value.to_string_lossy().eq_ignore_ascii_case(ext))
            .unwrap_or(false);
        if matches {
            out.push(path);
        }
    }
}

/// `Plugins\ahkv1|ahkv2` 里放着的解释器（有多个时优先名字里带 AutoHotkey 的）。
pub fn plugin_interpreter(lang: ScriptLang) -> Option<PathBuf> {
    let dir = plugin_dir(lang)?;
    let mut exes = Vec::new();
    list_files(&dir, "exe", 1, &mut exes);
    exes.sort();
    let preferred = exes
        .iter()
        .find(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().to_ascii_lowercase().contains("autohotkey"))
                .unwrap_or(false)
        })
        .cloned();
    preferred.or_else(|| exes.into_iter().next())
}

/// `Plugins\ahkv1|ahkv2` 里的 .ahk 脚本（按路径排序，界面上列出来用）。
pub fn plugin_scripts(lang: ScriptLang) -> Vec<PathBuf> {
    let Some(dir) = plugin_dir(lang) else {
        return Vec::new();
    };
    let mut scripts = Vec::new();
    list_files(&dir, "ahk", 2, &mut scripts);
    scripts.sort();
    scripts
}

/// 把一个「脚本引用」变成真实路径：
///
/// * 已经是存在的 `.ahk` 文件 → 直接认；
/// * 只写了文件名（或不带分隔符的相对名）→ 去 `Plugins\ahkv1` / `ahkv2` 里找。
///
/// 返回值里的第二项是「这个文件属于哪个版本」—— 在 `Plugins\ahkv2` 下的就是 v2，
/// 省得用户为每个脚本再选一次解释器。
pub fn resolve_script(name: &str) -> Option<(PathBuf, Option<ScriptLang>)> {
    let raw = name.trim().trim_matches('"').trim();
    if raw.is_empty() {
        return None;
    }
    let direct = PathBuf::from(raw);
    if direct.is_file() {
        let lang = ScriptLang::ALL
            .into_iter()
            .find(|lang| plugin_dir(*lang).is_some_and(|dir| starts_with_path(&direct, &dir)));
        return Some((direct, lang));
    }
    // 只写了文件名：到两个插件目录里按「路径结尾」比对（不分大小写）
    let tail = format!("\\{}", raw.replace('/', "\\").to_ascii_lowercase());
    for lang in ScriptLang::ALL {
        for script in plugin_scripts(lang) {
            let full = script.to_string_lossy().replace('/', "\\").to_ascii_lowercase();
            if full == raw.to_ascii_lowercase() || full.ends_with(&tail) {
                return Some((script, Some(lang)));
            }
        }
    }
    None
}

fn starts_with_path(path: &std::path::Path, dir: &std::path::Path) -> bool {
    let path = path.to_string_lossy().to_ascii_lowercase();
    let dir = dir.to_string_lossy().to_ascii_lowercase();
    path.starts_with(&dir)
}

/// 找 AutoHotkey 解释器：设置里指定的 > `Plugins\ahkv1|ahkv2` > 程序目录 > 注册表 > 常见安装路径。
pub fn ahk_path(lang: ScriptLang) -> Option<PathBuf> {
    use ScriptLang::{V1, V2};
    let configured = crate::settings::read(|settings| match lang {
        V1 => settings.ahk_v1.clone(),
        V2 => settings.ahk_v2.clone(),
    });
    let configured = configured.trim().to_string();
    if !configured.is_empty() {
        let path = PathBuf::from(&configured);
        if path.is_file() {
            return Some(path);
        }
    }

    // 跟着程序走的插件目录：放进去就认，优先级排在程序目录旁边那些 exe 前面
    if let Some(path) = plugin_interpreter(lang) {
        return Some(path);
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
        .unwrap_or_default();
    let mut candidates: Vec<PathBuf> = Vec::new();
    for name in ["AutoHotkey.exe", "AutoHotkey64.exe", "AutoHotkey32.exe"] {
        candidates.push(exe_dir.join(name));
    }
    // 注册表：.ahk 的打开方式里就是解释器路径
    if let Some(path) = crate::win32::ahk_from_registry() {
        candidates.push(PathBuf::from(path));
    }
    let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| "C:\\Program Files".into());
    let program_files_x86 =
        std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| "C:\\Program Files (x86)".into());
    for root in [&program_files, &program_files_x86] {
        for rel in [
            "AutoHotkey\\v2\\AutoHotkey64.exe",
            "AutoHotkey\\v2\\AutoHotkey32.exe",
            "AutoHotkey\\AutoHotkey64.exe",
            "AutoHotkey\\AutoHotkeyU64.exe",
            "AutoHotkey\\AutoHotkeyU32.exe",
            "AutoHotkey\\AutoHotkeyA32.exe",
        ] {
            candidates.push(PathBuf::from(format!("{root}\\{rel}")));
        }
    }

    // AutoHotkey.exe（v2 安装包会同时放一个 launcher）到底是哪一版不好判断，
    // 所以按「路径里带 v2 / 64 / U64 / A32」这些特征挑，再按文件名兜底。
    let wants_v2 = lang == V2;
    let mut fallback: Option<PathBuf> = None;
    for path in candidates {
        if !path.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let text = path.to_string_lossy().to_ascii_lowercase();
        let looks_v2 = text.contains("\\v2") || name == "autohotkey64.exe" || name == "autohotkey32.exe";
        let looks_v1 = name.contains("u64") || name.contains("u32") || name.contains("a32");
        if (wants_v2 && looks_v2) || (!wants_v2 && looks_v1) {
            return Some(path);
        }
        // 只有 v1 接受「认不出是哪一版」的兜底：v1 的 launcher 就叫 AutoHotkey.exe，
        // 而 v2 装上以后这个位置也可能是 v2 的启动器 —— 拿它跑 v2 语法会直接报错，
        // 不如老老实实说「没找到」让用户指定路径。
        if !wants_v2 {
            fallback.get_or_insert(path);
        }
    }
    fallback
}

/// 跑一段 AHK 代码：写到临时文件再交给解释器。
///
/// 写临时文件（而不是 `AutoHotkey.exe /script` 那种内联方式）是因为 AHK 需要
/// 一个真实路径来做 `A_ScriptDir`，而且用户脚本里可能带中文 —— 走文件最稳。
pub fn run_script(code: &str, lang: ScriptLang) -> Result<(), String> {
    let code = code.trim();
    if code.is_empty() {
        return Err("脚本是空的".into());
    }
    // 一行、而且以 `.ahk` 结尾 → 这不是代码，是一个脚本文件：
    // 直接让解释器去跑那个文件（`A_ScriptDir` 也就指向脚本自己那儿）。
    // 路径可以只写文件名：`Plugins\ahkv1` / `Plugins\ahkv2` 里有的就自动找到。
    if !code.contains('\n') && code.to_ascii_lowercase().ends_with(".ahk") {
        return match resolve_script(code) {
            Some((path, file_lang)) => run_script_file(&path, file_lang.unwrap_or(lang)),
            None => Err(format!(
                "找不到脚本「{code}」—— 可以直接写完整路径，或者把 .ahk 放进 Plugins\\ahkv1 / Plugins\\ahkv2 里只写文件名"
            )),
        };
    }
    let Some(interpreter) = ahk_path(lang) else {
        return Err(format!(
            "没找到 {}。装一个，或者在 设置 → 动作库 → 脚本解释器 里指定 AutoHotkey.exe 的路径",
            lang.label()
        ));
    };
    let dir = std::env::temp_dir().join("RunAny_RS").join("scripts");
    std::fs::create_dir_all(&dir).map_err(|err| format!("建不了临时目录：{err}"))?;
    // 文件名带哈希：同一段脚本重复执行不会堆出一堆文件
    let hash = code
        .bytes()
        .fold(0u64, |acc, byte| acc.wrapping_mul(1099511628211).wrapping_add(byte as u64));
    let path = dir.join(format!("{hash:016x}.ahk"));
    let body = if code.starts_with("#") {
        code.to_string()
    } else {
        // 关掉「脚本自己弹托盘图标」这类副作用：跑完就退
        format!("#NoTrayIcon\n#SingleInstance Off\n{code}\n")
    };
    std::fs::write(&path, body).map_err(|err| format!("写不了脚本文件：{err}"))?;

    spawn_interpreter(&interpreter, &path)
}

/// 直接跑一个 `.ahk` 文件（自己的 `A_ScriptDir` 就是它所在目录）。
pub fn run_script_file(script: &std::path::Path, lang: ScriptLang) -> Result<(), String> {
    if !script.is_file() {
        return Err(format!("脚本不在了：{}", script.display()));
    }
    let Some(interpreter) = ahk_path(lang) else {
        return Err(format!(
            "没找到 {}。装一个，或者把解释器放进程序目录的 Plugins\\ahkv1 / Plugins\\ahkv2 里",
            lang.label()
        ));
    };
    spawn_interpreter(&interpreter, script)
}

fn spawn_interpreter(interpreter: &std::path::Path, script: &std::path::Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new(interpreter)
            .arg(script)
            .creation_flags(0x0800_0000)
            .spawn()
            .map(|_| ())
            .map_err(|err| format!("启动 {} 失败：{err}", interpreter.display()))
    }
    #[cfg(not(windows))]
    {
        let _ = (interpreter, script);
        Err("仅 Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_a_unique_id_and_a_name() {
        let mut seen = std::collections::HashSet::new();
        for def in BUILTINS {
            assert!(seen.insert(def.id), "id 重复了：{}", def.id);
            assert!(!def.name.is_empty(), "{} 没名字", def.id);
            assert!(!def.desc.is_empty(), "{} 没说明", def.id);
            for param in def.params {
                assert!(
                    !param.key.is_empty() && !param.label.is_empty(),
                    "{} 的参数缺 key / label",
                    def.id
                );
            }
        }
    }

    #[test]
    fn search_finds_actions_by_name_and_category() {
        let hits = search("音量");
        assert!(hits.iter().any(|def| def.id == "sys.volume"));
        let hits = search("窗口");
        assert!(hits.iter().any(|def| def.id == "win.topmost"));
        // 多个词是「且」
        assert!(search("窗口 置顶").iter().any(|def| def.id == "win.topmost"));
        assert!(search("窗口 置顶 不存在").is_empty());
    }

    #[test]
    fn defaults_fill_every_param_with_a_default() {
        let def = find("sys.volume").unwrap();
        let values = defaults(def);
        assert_eq!(values.get("action").map(String::as_str), Some("mute"));
        assert_eq!(values.get("steps").map(String::as_str), Some("2"));
    }

    /// `Plugins\ahkv1` 里的脚本能按文件名找到，并且认出它属于 v1。
    #[test]
    fn a_script_in_the_plugins_folder_is_found_by_name() {
        let Some(dir) = plugin_dir(ScriptLang::V1) else {
            return;
        };
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join("单元测试脚本.ahk");
        if std::fs::write(&path, "MsgBox 1").is_err() {
            return;
        }
        let found = resolve_script("单元测试脚本.ahk").expect("插件目录里的脚本应该找得到");
        assert_eq!(found.0, path);
        assert_eq!(found.1, Some(ScriptLang::V1));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// 一行 `.ahk` 当成脚本文件跑：找不到就直说，别当代码写进临时文件。
    #[test]
    fn a_missing_ahk_file_says_it_is_missing() {
        let err = run_script("绝对不会存在的脚本.ahk", ScriptLang::V1).unwrap_err();
        assert!(err.contains("找不到脚本"), "{err}");
    }

    #[test]
    fn time_format_follows_the_ahk_style_placeholders() {
        let text = format_time("yyyy/MM/dd HH:mm:ss");
        assert_eq!(text.len(), 19, "{text}");
        assert_eq!(&text[4..5], "/");
        assert_eq!(&text[7..8], "/");
        assert_eq!(&text[10..11], " ");
    }

    #[test]
    fn a_random_password_has_the_requested_length_and_alphabet() {
        let password = random_string(24, "abc");
        assert_eq!(password.chars().count(), 24);
        assert!(password.chars().all(|ch| "abc".contains(ch)));
    }

    #[test]
    fn a_uuid_looks_like_a_uuid() {
        let text = uuid();
        assert_eq!(text.len(), 36, "{text}");
        let groups: Vec<&str> = text.split('-').collect();
        assert_eq!(groups.iter().map(|g| g.len()).collect::<Vec<_>>(), vec![8, 4, 4, 4, 12]);
        assert!(text.chars().all(|ch| ch == '-' || ch.is_ascii_hexdigit()));
    }

    /// 真跑一次 AHK（这台机器上装了 v1）。要 AHK，所以默认忽略。
    #[test]
    #[ignore]
    fn a_script_actually_runs_through_autohotkey() {
        let marker = std::env::temp_dir().join("runany-ahk-test.txt");
        let _ = std::fs::remove_file(&marker);
        let code = format!(
            "#NoTrayIcon\nFileAppend, ok, {}\nExitApp\n",
            marker.display()
        );
        let lang = ScriptLang::V1;
        let interpreter = ahk_path(lang);
        println!("interpreter = {interpreter:?}");
        run_script(&code, lang).expect("脚本没跑起来");
        for _ in 0..40 {
            if marker.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let text = std::fs::read_to_string(&marker).unwrap_or_default();
        assert_eq!(text.trim(), "ok", "脚本没写出标记文件");
        let _ = std::fs::remove_file(&marker);
    }

    #[test]
    fn old_builtin_keys_still_resolve() {
        assert!(is_known("open-settings"));
        assert!(is_known("quit"));
        assert!(!is_known("nope"));
    }
}
