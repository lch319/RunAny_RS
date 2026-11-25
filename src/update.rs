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
    // 不要只押一个源：
    //
    // - `api.github.com` 按 IP 限流（未认证 60 次/小时），走代理时出口 IP 共用，
    //   很容易被别人吃光 → 403；
    // - `releases.atom` / `tags.atom` 是 github.com 的普通页面，不吃那个额度。
    //
    // 先试 atom，失败再退回 API。
    for url in [
        format!("https://github.com/{REPO}/releases.atom"),
        format!("https://github.com/{REPO}/tags.atom"),
    ] {
        if let Ok((200, body)) = crate::win32::http_get(&url, 8000) {
            let state = parse_atom(&body);
            if !matches!(state, State::Failed(_)) {
                return state;
            }
        }
    }

    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    match crate::win32::http_get(&url, 8000) {
        Ok((200, body)) => parse(&body),
        Ok((404, _)) => State::Failed(
            "拿不到发布信息：仓库还没公开，或者还没建过 Release".to_string(),
        ),
        Ok((403, _)) => State::Failed("被 GitHub 限流了，过一会儿再试".to_string()),
        Ok((code, _)) => State::Failed(format!("GitHub 返回 HTTP {code}")),
        Err(err) => State::Failed(err),
    }
}

fn parse_atom(body: &str) -> State {
    // `releases.atom` 用 `/releases/tag/v0.2.2`，`tags.atom` 用 `/tags/v0.2.2`
    // （有的页面直接把版本写在 <title> 里）。三种都认，取第一个像版本号的。
    let mut tag = String::new();
    for needle in ["/releases/tag/", "/tags/", "<title>"] {
        let Some(index) = body.find(needle) else {
            continue;
        };
        let rest = &body[index + needle.len()..];
        let found: String = rest
            .trim_start_matches(['v', 'V'])
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '.' || *ch == '-' || *ch == '_')
            .collect();
        if found.chars().any(|ch| ch.is_ascii_digit()) && found.contains('.') {
            tag = found;
            break;
        }
    }
    if tag.is_empty() {
        return State::Failed("更新信息的格式看不懂".to_string());
    }
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
    use super::*;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(is_newer("0.3.0", "0.2.0"));
        assert!(is_newer("v0.2.10", "0.2.9"), "按文本比会错：0.2.10 < 0.2.9");
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.9", "0.2.0"));
        assert!(!is_newer("V0.2.0", "0.2.0"));
    }

    #[test]
    fn atom_feed_gives_the_latest_tag() {
        let body = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom">
  <id>tag:github.com,2008:https://github.com/lch319/RunAny_RS/releases</id>
  <link type="text/html" rel="alternate" href="https://github.com/lch319/RunAny_RS/releases"/>
  <entry>
    <link type="text/html" rel="alternate" href="https://github.com/lch319/RunAny_RS/releases/tag/v0.2.2"/>
    <title>v0.2.2</title>
  </entry>
  <entry>
    <link type="text/html" rel="alternate" href="https://github.com/lch319/RunAny_RS/releases/tag/v0.2.1"/>
  </entry>
</feed>"#;
        // 解析成功即可（版本比不比当前新取决于编译时的版本号，测试里不断言）
        assert!(!matches!(parse_atom(body), State::Failed(_)));
    }

    #[test]
    fn atom_feed_without_a_tag_fails() {
        assert!(matches!(parse_atom("<feed></feed>"), State::Failed(_)));
    }
}
