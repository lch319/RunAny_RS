//! 检查更新：问 GitHub 的 `releases/latest`，和当前版本比一比。
//!
//! 没有引第三方 HTTP 库 —— 走系统的 WinHTTP（见 [`crate::win32::http_get`]），
//! 这样 exe 体积几乎不增加，也不会多出一棵依赖树。
//!
//! 状态放在一个全局 `Mutex` 里，界面每帧读一次；真正的网络请求在后台线程跑，
//! 点「检查更新」不会把界面卡住。

use std::sync::Mutex;

/// 仓库（改这里就能指向 fork）。
const REPO: &str = "lch319/RunAny_RS";

/// 当前版本（编译时写进 exe）。
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// 还没查过
    Idle,
    Checking,
    UpToDate,
    Newer { version: String, url: String },
    Failed(String),
}

static STATE: Mutex<State> = Mutex::new(State::Idle);

pub fn state() -> State {
    STATE.lock().map(|state| state.clone()).unwrap_or(State::Idle)
}

fn set(next: State) {
    if let Ok(mut state) = STATE.lock() {
        *state = next;
    }
}

/// 在后台查一次。`force` = 用户主动点的（正在查的时候再点就不理了）。
pub fn check(cx: &mut gpui::App, force: bool) {
    if !force && !matches!(state(), State::Idle) {
        return;
    }
    if matches!(state(), State::Checking) {
        return;
    }
    set(State::Checking);
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async { fetch() })
            .await;
        let _ = cx.update(|cx| {
            // 发现新版本：先弹一个确认框问要不要下载（只有用户主动点「检测更新」
            // 时才问，后台静默检查不打扰）
            if let State::Newer { version, url } = &result {
                if force {
                    crate::confirm::ask(
                        crate::confirm::Ask::DownloadUpdate {
                            version: version.clone(),
                            url: url.clone(),
                        },
                        cx,
                    );
                }
            }
            set(result);
            crate::settings_ui::repaint_trees(cx);
        });
    })
    .detach();
}

/// 真的去网上问（在后台线程里跑）。
fn fetch() -> State {
    // 用 `releases/latest.atom` 而不是 `api.github.com`：
    //
    // API 是按 IP 限流的（未认证 60 次/小时），用代理时整条线路的 IP 是共用的，
    // 别人问几次就把额度吃光了 —— 用户看到的「一直被限流」就是这么来的。
    // atom 是 github.com 的普通页面，跟着网页走，没有那个额度。
    let url = format!("https://github.com/{REPO}/releases/latest.atom");
    match crate::win32::http_get(&url, 8000) {
        Ok((200, body)) => parse_atom(&body),
        Ok((404, _)) => State::Failed(
            "拿不到发布信息：仓库还没公开，或者还没建过 Release".to_string(),
        ),
        Ok((code, _)) => State::Failed(format!("GitHub 返回 HTTP {code}")),
        Err(err) => State::Failed(err),
    }
}

/// 从 atom feed 里挑最新那条的 tag。
///
/// feed 里每条 release 都带一个 `.../releases/tag/v0.2.2` 的链接，取第一条就行
/// （GitHub 的 atom 是按时间倒序的）。
fn parse_atom(body: &str) -> State {
    let Some(index) = body.find("/releases/tag/") else {
        return State::Failed("更新信息的格式看不懂".to_string());
    };
    let rest = &body[index + "/releases/tag/".len()..];
    let tag: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '.' || *ch == '-' || *ch == '_')
        .collect();
    let latest = tag.trim_start_matches(['v', 'V']).to_string();
    if latest.is_empty() {
        return State::Failed("发布信息里没有版本号".to_string());
    }
    if is_newer(&latest, current()) {
        State::Newer {
            version: tag,
            url: format!("https://github.com/{REPO}/releases/latest"),
        }
    } else {
        State::UpToDate
    }
}

fn parse(body: &str) -> State {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return State::Failed("更新信息的格式看不懂".to_string());
    };
    let tag = json
        .get("tag_name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let url = json
        .get("html_url")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let latest = tag.trim_start_matches(['v', 'V']).to_string();
    if latest.is_empty() {
        return State::Failed("发布信息里没有版本号".to_string());
    }
    if is_newer(&latest, current()) {
        State::Newer {
            version: tag,
            url: if url.is_empty() {
                format!("https://github.com/{REPO}/releases")
            } else {
                url
            },
        }
    } else {
        State::UpToDate
    }
}

/// `latest` 比 `current` 新吗（按 `.` 分段比数字，认 `1.2.10 > 1.2.9`）。
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |text: &str| -> Vec<u64> {
        text.trim_start_matches(['v', 'V'])
            .split(['.', '-', '+'])
            .map(|part| part.trim().parse::<u64>().unwrap_or(0))
            .collect()
    };
    let a = parse(latest);
    let b = parse(current);
    for index in 0..a.len().max(b.len()) {
        let left = *a.get(index).unwrap_or(&0);
        let right = *b.get(index).unwrap_or(&0);
        if left != right {
            return left > right;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("v0.2.10", "0.2.9"), "按文本比会错：0.2.10 < 0.2.9");
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("V0.2.0", "0.2.0"));
    }
}
