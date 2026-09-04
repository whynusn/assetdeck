use regex::Regex;

use platform::WindowSnapshot;

use crate::{Profile, ProfileSet, TargetBinding};

// 内嵌的 Profile 含比例锚点(f32)，因此只实现 PartialEq。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTarget {
    pub binding: TargetBinding,
    pub profile: Profile,
    pub score: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchResult {
    Matched(Box<ResolvedTarget>),
    Ambiguous(Vec<ResolvedTarget>),
    None,
}

/// 识别一个前台窗口。未命中内置画像时返回通用 fallback，不把未知 IM 丢掉。
pub fn resolve_snapshot(profiles: &ProfileSet, snapshot: &WindowSnapshot) -> ResolvedTarget {
    resolve_eligible_snapshot(profiles, snapshot).unwrap_or_else(|| {
        let profile = profiles.generic().clone();
        ResolvedTarget {
            binding: binding(&profile, snapshot, true, false),
            profile,
            score: 0,
        }
    })
}

/// 识别可自动写入热目标的前台窗口。
///
/// 通用 fallback 只用于用户显式捕获和确认未知 IM；浏览器、资源管理器等未命中画像的
/// 窗口不得经 fallback 自动成为热目标，否则会破坏粘性锁定。
pub fn resolve_eligible_snapshot(
    profiles: &ProfileSet,
    snapshot: &WindowSnapshot,
) -> Option<ResolvedTarget> {
    let mut best: Option<(&Profile, u16, bool)> = None;
    let mut ambiguous = false;
    for profile in profiles.profiles() {
        let Some((score, session_window)) = match_score(profile, snapshot) else {
            continue;
        };
        match best {
            None => {
                best = Some((profile, score, session_window));
                ambiguous = false;
            }
            Some((_, best_score, _)) if score > best_score => {
                best = Some((profile, score, session_window));
                ambiguous = false;
            }
            Some((_, best_score, _)) if score == best_score => ambiguous = true,
            Some(_) => {}
        }
    }
    if ambiguous {
        return None;
    }
    best.map(|(profile, score, session_window)| ResolvedTarget {
        binding: binding(profile, snapshot, false, session_window),
        profile: profile.clone(),
        score,
    })
}

/// 为指定画像解析当前所有窗口。最高分并列时返回 Ambiguous，禁止静默取第一个。
pub fn resolve_profile_windows(profile: &Profile, windows: &[WindowSnapshot]) -> MatchResult {
    let mut matches = matching_profile_windows(profile, windows);

    let Some(best_score) = matches.iter().map(|candidate| candidate.score).max() else {
        return MatchResult::None;
    };
    matches.retain(|candidate| candidate.score == best_score);
    match matches.len() {
        0 => MatchResult::None,
        1 => MatchResult::Matched(Box::new(matches.remove(0))),
        _ => MatchResult::Ambiguous(matches),
    }
}

/// 返回指定画像命中的全部窗口，供冷目标选择器逐个展示和精确选择。
///
/// 只暴露 `visible` 窗口：真实枚举里同一个 IM 进程会挂几十个隐藏辅助窗口（托盘消息、
/// GDI+ Hook、LoadingWnd、催一催提示等），它们没有输入框，混进 picker 会淹没真实会话窗口。
/// 最小化窗口 `IsWindowVisible` 仍为真，因此不会被这条规则误杀。
pub fn matching_profile_windows(
    profile: &Profile,
    windows: &[WindowSnapshot],
) -> Vec<ResolvedTarget> {
    let mut matches: Vec<ResolvedTarget> = windows
        .iter()
        .filter(|snapshot| snapshot.visible)
        .filter_map(|snapshot| {
            match_score(profile, snapshot).map(|(score, session_window)| ResolvedTarget {
                binding: binding(profile, snapshot, false, session_window),
                profile: profile.clone(),
                score,
            })
        })
        .collect();
    matches.sort_by_key(|candidate| std::cmp::Reverse(candidate.score));
    disambiguate_labels(&mut matches);
    matches
}

fn disambiguate_labels(matches: &mut [ResolvedTarget]) {
    // 先按原始标签统计重数，再统一改写。就地边比较边改写会让第二个重复项
    // 看不到同名对手（第一个已被加后缀），结果只有一半候选带窗口后缀，
    // 用户在 picker 里看到「微信 · 窗口 N」和「微信」两张卡，无法判断谁是谁。
    let duplicated: Vec<bool> = matches
        .iter()
        .map(|candidate| {
            matches
                .iter()
                .filter(|other| other.binding.label == candidate.binding.label)
                .count()
                > 1
        })
        .collect();
    for (candidate, is_duplicated) in matches.iter_mut().zip(duplicated) {
        if !is_duplicated {
            continue;
        }
        if let Some(hwnd) = candidate.binding.hwnd {
            candidate.binding.label = format!("{} · 窗口 {}", candidate.binding.label, hwnd.0);
        }
    }
}

fn binding(
    profile: &Profile,
    snapshot: &WindowSnapshot,
    fallback: bool,
    session_window: bool,
) -> TargetBinding {
    TargetBinding {
        id: profile.id.clone(),
        hwnd: Some(snapshot.hwnd),
        label: if fallback && !snapshot.title.trim().is_empty() {
            snapshot.title.clone()
        } else if !snapshot.title.trim().is_empty()
            && !snapshot.title.eq_ignore_ascii_case(&profile.label)
        {
            format!("{} · {}", profile.label, snapshot.title.trim())
        } else {
            profile.label.clone()
        },
        fallback,
        minimized: snapshot.minimized,
        visible: snapshot.visible,
        instance_id: format!("{}:{}", snapshot.exe_name, snapshot.process_id),
        session_window,
    }
}

/// 匹配打分，返回 (分数, 标题是否命中)。标题命中即「会话窗口」证据，
/// 随绑定携带，供热目标切换日志区分正常跟随与可疑顶替。
fn match_score(profile: &Profile, snapshot: &WindowSnapshot) -> Option<(u16, bool)> {
    if profile.exe_names.is_empty()
        && profile.class_names.is_empty()
        && profile.title_regexes.is_empty()
    {
        return None;
    }

    let mut score = 0;
    if !profile.exe_names.is_empty() {
        if contains_ci(&profile.exe_names, &snapshot.exe_name) {
            score += 100;
        } else {
            return None;
        }
    }
    let class_hit = class_matches(&profile.class_names, &snapshot.class_name);
    let title_hit = title_matches(&profile.title_regexes, &snapshot.title);
    // 画像一旦声明了窗口特征，就必须至少命中一项：只靠 exe 命中会把同进程的托盘窗口、
    // 隐藏提示框、GDI+ Hook 窗口全部收进候选，而它们根本没有输入框。
    if !(profile.class_names.is_empty() && profile.title_regexes.is_empty())
        && !class_hit
        && !title_hit
    {
        log::debug!(
            "画像拒绝 exe={} class={:?} title={:?} 理由=特征全不中(类名{:?} 标题{:?})",
            snapshot.exe_name,
            snapshot.class_name,
            snapshot.title,
            profile.class_names,
            profile.title_regexes
        );
        return None;
    }
    // 严格档（require_title）：标题命中是会话窗口的身份门槛。Qt 应用所有普通
    // 窗口共享同一个类名，只看类名会把优惠弹窗、活动窗一并放进候选——真机
    // 实证（2026-08-29）它们抢到前台后会静默顶替热目标，下次上框拉起的就是弹窗。
    if profile.require_title && !title_hit {
        log::debug!(
            "画像拒绝 exe={} class={:?} title={:?} 理由=严格档类名命中但标题不中({:?})",
            snapshot.exe_name,
            snapshot.class_name,
            snapshot.title,
            profile.title_regexes
        );
        return None;
    }
    if class_hit {
        score += 40;
    }
    if title_hit {
        score += 20;
    }
    if snapshot.visible {
        score += 5;
    }
    if !snapshot.minimized {
        score += 2;
    }
    if snapshot.rect.has_area() {
        score += 1;
    }
    Some((score, title_hit))
}

/// 窗口类名匹配。等值优先；另外允许「声明名 + `{GUID}`」这种真实存在的变体，
/// 拼多多商家版主窗口的类名就是 `g_wszPDDWindowClass{E77EAED1-...}`。
fn class_matches(declared: &[String], actual: &str) -> bool {
    declared.iter().any(|value| {
        value.eq_ignore_ascii_case(actual)
            || actual
                .get(..value.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(value))
                && actual[value.len()..].starts_with('{')
    })
}

fn title_matches(patterns: &[String], title: &str) -> bool {
    patterns
        .iter()
        .any(|pattern| Regex::new(pattern).is_ok_and(|re| re.is_match(title)))
}

fn contains_ci(values: &[String], actual: &str) -> bool {
    values
        .iter()
        .any(|value| value.eq_ignore_ascii_case(actual))
}

#[cfg(test)]
mod tests {
    use platform::{WindowHandle, WindowRect};

    use crate::load_profiles;

    use super::*;

    fn snapshot(hwnd: isize, exe: &str, minimized: bool) -> WindowSnapshot {
        WindowSnapshot {
            hwnd: WindowHandle(hwnd),
            exe_name: exe.to_string(),
            class_name: "WeChatMainWndForPC".to_string(),
            title: "微信".to_string(),
            visible: true,
            minimized,
            rect: WindowRect {
                left: 0,
                top: 0,
                right: 800,
                bottom: 600,
            },
            process_id: 1,
        }
    }

    /// 真实枚举里的辅助窗口：exe 命中画像，但类名/标题都不是会话窗口。
    fn helper_snapshot(hwnd: isize, exe: &str, class: &str, title: &str) -> WindowSnapshot {
        WindowSnapshot {
            class_name: class.to_string(),
            title: title.to_string(),
            ..snapshot(hwnd, exe, false)
        }
    }

    fn profiles() -> ProfileSet {
        load_profiles(
            r#"
[[profiles]]
id = "wechat"
label = "微信"
exe_names = ["WeChat.exe"]
class_names = ["WeChatMainWndForPC"]
"#,
            None,
        )
        .unwrap()
    }

    #[test]
    fn resolve_two_wechat_windows_returns_ambiguous() {
        let set = profiles();
        let result = resolve_profile_windows(
            set.get(&"wechat".into()).unwrap(),
            &[
                snapshot(1, "WeChat.exe", false),
                snapshot(2, "WeChat.exe", false),
            ],
        );
        assert!(matches!(result, MatchResult::Ambiguous(items) if items.len() == 2));
    }

    #[test]
    fn minimized_window_still_matches_but_marked() {
        let set = profiles();
        let resolved = resolve_snapshot(&set, &snapshot(1, "wechat.EXE", true));
        assert_eq!(resolved.binding.id.as_str(), "wechat");
        assert!(resolved.binding.minimized);
    }

    #[test]
    fn generic_fallback_sets_fallback_flag() {
        let set = profiles();
        let resolved = resolve_snapshot(&set, &snapshot(1, "Telegram.exe", false));
        assert_eq!(resolved.profile.id.as_str(), "generic_im");
        assert!(resolved.binding.fallback);
    }

    #[test]
    fn unknown_foreground_is_not_eligible_for_hot_target() {
        let set = profiles();
        assert!(resolve_eligible_snapshot(&set, &snapshot(1, "chrome.exe", false)).is_none());
    }

    #[test]
    fn overlapping_profiles_do_not_auto_choose_by_iteration_order() {
        let set = load_profiles(
            r#"
[[profiles]]
id = "wechat-primary"
label = "微信主账号"
exe_names = ["WeChat.exe"]

[[profiles]]
id = "wechat-secondary"
label = "微信副账号"
exe_names = ["WeChat.exe"]
"#,
            None,
        )
        .unwrap();

        assert!(resolve_eligible_snapshot(&set, &snapshot(1, "WeChat.exe", false)).is_none());
    }

    #[test]
    fn cold_picker_returns_every_matching_window() {
        let set = profiles();
        let matches = matching_profile_windows(
            set.get(&"wechat".into()).unwrap(),
            &[
                snapshot(1, "WeChat.exe", false),
                snapshot(2, "WeChat.exe", true),
            ],
        );
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].binding.hwnd, Some(WindowHandle(1)));
        assert_eq!(matches[1].binding.hwnd, Some(WindowHandle(2)));
        assert_ne!(matches[0].binding.label, matches[1].binding.label);
    }

    /// 同名候选必须**每一个**都带窗口后缀。只给后来者加后缀会让 picker 出现
    /// 「微信」和「微信 · 窗口 N」并列，用户无法判断前者是哪个窗口。
    #[test]
    fn every_duplicate_label_candidate_carries_window_suffix() {
        let set = profiles();
        let matches = matching_profile_windows(
            set.get(&"wechat".into()).unwrap(),
            &[
                snapshot(11, "WeChat.exe", false),
                snapshot(22, "WeChat.exe", false),
                snapshot(33, "WeChat.exe", false),
            ],
        );
        assert_eq!(matches.len(), 3);
        for candidate in &matches {
            let hwnd = candidate.binding.hwnd.unwrap();
            assert!(
                candidate
                    .binding
                    .label
                    .ends_with(&format!("窗口 {}", hwnd.0)),
                "候选 {:?} 缺少窗口后缀",
                candidate.binding.label
            );
        }
    }

    #[test]
    fn same_process_helper_windows_are_not_candidates() {
        let set = profiles();
        let profile = set.get(&"wechat".into()).unwrap();
        // 取自真实枚举（2026-08-23，PID 17940/7308）的同进程辅助窗口。
        for (class, title) in [
            (
                "Qt51514WxTrayIconMessageWindowClass",
                "WxTrayIconMessageWindow",
            ),
            ("Chrome_SystemMessageWindow", ""),
            ("DisplayICC_SystemMessageWindow", ""),
            ("GDI+ Hook Window Class", "GDI+ Window (Weixin.exe)"),
            ("IME", "Default IME"),
        ] {
            assert!(
                match_score(profile, &helper_snapshot(9, "WeChat.exe", class, title)).is_none(),
                "辅助窗口不应成为候选: {class}"
            );
        }
    }

    #[test]
    fn hidden_windows_are_excluded_from_cold_picker() {
        let set = profiles();
        let mut hidden = snapshot(2, "WeChat.exe", false);
        hidden.visible = false;
        let matches = matching_profile_windows(
            set.get(&"wechat".into()).unwrap(),
            &[snapshot(1, "WeChat.exe", false), hidden],
        );
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].binding.hwnd, Some(WindowHandle(1)));
    }

    #[test]
    fn minimized_window_survives_visibility_filter() {
        let set = profiles();
        let matches = matching_profile_windows(
            set.get(&"wechat".into()).unwrap(),
            &[snapshot(1, "WeChat.exe", true)],
        );
        assert_eq!(matches.len(), 1);
        assert!(matches[0].binding.minimized);
    }

    #[test]
    fn guid_suffixed_class_name_still_matches() {
        let set = load_profiles(
            r#"
[[profiles]]
id = "pdd"
label = "拼多多商家版"
exe_names = ["PddWorkbench.exe"]
class_names = ["g_wszPDDWindowClass"]
"#,
            None,
        )
        .unwrap();
        let profile = set.get(&"pdd".into()).unwrap();
        let real = helper_snapshot(
            5,
            "PddWorkbench.exe",
            "g_wszPDDWindowClass{E77EAED1-21E4-4F21-AE4C-50A6AE1E47A4}",
            "拼多多工作台",
        );
        assert!(match_score(profile, &real).is_some());
        let other = helper_snapshot(6, "PddWorkbench.exe", "g_wszPDDWindowClassOther", "x");
        assert!(match_score(profile, &other).is_none());
    }

    #[test]
    fn title_only_hit_is_enough_when_class_differs() {
        let set = load_profiles(
            r#"
[[profiles]]
id = "qianniu"
label = "千牛"
exe_names = ["AliWorkbench.exe"]
class_names = ["Qt5152QWindowIcon"]
title_regexes = ["接待中心$"]
"#,
            None,
        )
        .unwrap();
        let profile = set.get(&"qianniu".into()).unwrap();
        let renamed_class =
            helper_snapshot(7, "AliWorkbench.exe", "Qt5153QWindowIcon", "tb1-接待中心");
        assert!(match_score(profile, &renamed_class).is_some());
        let floating_bar =
            helper_snapshot(8, "AliWorkbench.exe", "Qt5152QWindowToolSaveBits", "悬浮条");
        assert!(match_score(profile, &floating_bar).is_none());
    }

    /// 严格档（require_title）：类名是整个应用共享的（Qt 按运行时版本命名），
    /// 只看类名会把优惠弹窗一并放进候选——真机实证弹窗抢前台后会静默顶替热目标。
    #[test]
    fn require_title_rejects_class_only_popup_and_accepts_session_variants() {
        let set = load_profiles(
            r#"
[[profiles]]
id = "qianniu"
label = "千牛"
exe_names = ["AliWorkbench.exe"]
class_names = ["Qt5152QWindowIcon"]
title_regexes = ["接待(中心|台)$", "千牛工作台"]
require_title = true
"#,
            None,
        )
        .unwrap();
        let profile = set.get(&"qianniu".into()).unwrap();

        // 优惠弹窗：类名命中、标题不合会话特征 → 不匹配、不得自动成为热目标。
        let popup = helper_snapshot(1, "AliWorkbench.exe", "Qt5152QWindowIcon", "限时特惠");
        assert!(
            match_score(profile, &popup).is_none(),
            "严格档下类名命中不足以匹配"
        );
        assert!(
            resolve_eligible_snapshot(&set, &popup).is_none(),
            "弹窗不得自动成为热目标"
        );

        // 会话窗口及其用户级标题变体照常命中，且绑定携带会话窗证据。
        for (index, title) in ["tb1-接待中心", "易软坊-接待台", "tb940472610424-千牛工作台"]
            .into_iter()
            .enumerate()
        {
            let session = helper_snapshot(
                (index + 2) as isize,
                "AliWorkbench.exe",
                "Qt5152QWindowIcon",
                title,
            );
            let (score, session_window) = match_score(profile, &session).expect("会话窗口应命中");
            assert!(score >= 140);
            assert!(session_window, "标题命中必须标记会话窗: {title}");
            assert!(resolve_eligible_snapshot(&set, &session).is_some());
        }
    }
}
