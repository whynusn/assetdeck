//! 更新检查（D56）：版本比较、GitHub release 清单解析、源顺序回落与结果状态机。
//!
//! 分层依据：网络传输经 [`platform::HttpTextFetcher`] 注入，本模块零网络实现、
//! 零平台 API——纯函数与状态机可全量单测（TDD 第一原则落点）。编排节奏：
//! 启动静默检查（≥24h 节流、失败零打扰）+ 设置面板手动检查（结果面板可见），
//! 命中动作是「弹窗 → 打开发布页」，应用内下载安装明确推迟（D56）。

use std::cmp::Ordering;
use std::path::Path;

use platform::HttpTextFetcher;
use serde::Deserialize;
use serde_json::Value;

/// 默认更新源（D56：GitHub API 主源，与 ci.yml release job 的发布端点同一仓库）。
/// 国内镜像顺序回落清单不写死在代码里——D56 留了「实测哪个可用再定默认」，
/// 在 `update_feeds.toml`（与 settings.toml 同目录）配置后即覆盖本清单。
pub const DEFAULT_FEEDS: &[&str] =
    &["https://api.github.com/repos/whynusn/assetdeck/releases/latest"];

/// 静默检查最小间隔（D56：≥24h）。上次检查无论成败都刷新节流钟——
/// 源持续不可达时每次启动都白打一发，不是用户想要的「检查」。
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// 单源网络超时（毫秒）。检查在后台线程执行，慢只影响结果何时回来，不阻塞 UI。
pub const FETCH_TIMEOUT_MS: u64 = 10_000;

/// release notes 进 UI 的截断上限（字符）。超长 notes 会把弹窗滚动区拖成
/// 万行文档；发布页才是完整变更日志的家。
const NOTES_MAX_CHARS: usize = 4000;

/// 一个发布附件（D70）：release 清单 `assets[]` 里的一项。自更新只按
/// 精确文件名挑（安装器 exe + 校验和清单），其余附件无视。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    pub name: String,
    pub url: String,
}

/// 一次成功解析的发布清单。`version` 保留原始 tag（如 "v0.2.0"），展示直用。
/// `assets` 为空不算坏源（镜像站可能只回填 tag，此时应用内更新不可用、
/// 「打开发布页」仍是出路）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: String,
    pub notes: String,
    pub url: String,
    pub assets: Vec<ReleaseAsset>,
}

/// 一次检查的结局。`Failed` 只在手动检查时对用户可见（静默档记日志即止）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    UpToDate,
    Available(ReleaseInfo),
    Failed(String),
}

/// 版本号解析：接受 `v0.2.0` / `0.2` / `0.2.0-beta`（预发布段忽略，按正式版比）。
/// 主段不是数字返回 None——调用方按「不可判定」处理，绝不误弹更新。
pub fn parse_version(tag: &str) -> Option<[u64; 3]> {
    let core = tag.trim().trim_start_matches(['v', 'V']);
    let core = core.split(['-', '+']).next()?;
    let mut out = [0u64; 3];
    for (index, part) in core.split('.').take(3).enumerate() {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        out[index] = part.parse().ok()?;
    }
    // 首段必须存在且为数字（"" / "beta" 不是版本号）。
    if core.is_empty() || !core.split('.').next()?.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(out)
}

/// 版本比较。任一边不是可解析版本号时返回 Equal（保守：未知不弹更新）。
pub fn compare_version(a: &str, b: &str) -> Ordering {
    match (parse_version(a), parse_version(b)) {
        (Some(x), Some(y)) => x.cmp(&y),
        _ => Ordering::Equal,
    }
}

/// 截断到字符上限（不是字节）——中文 notes 按 4000 字算，不是 1333 字。
fn truncate_chars(text: &str) -> String {
    if text.chars().count() <= NOTES_MAX_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(NOTES_MAX_CHARS).collect();
    out.push('…');
    out
}

/// 解析 GitHub `releases/latest` 的响应体。`tag_name` 必需且必须是可识别
/// 版本号（解析不出就当坏源，回落下一源）；`body`（notes）与 `html_url`
/// 可选——镜像站可能只回填 tag。
pub fn parse_release_json(text: &str) -> Result<ReleaseInfo, String> {
    let value: Value =
        serde_json::from_str(text).map_err(|error| format!("JSON 解析失败: {error}"))?;
    let tag = value
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or("缺少 tag_name 字段")?;
    if parse_version(tag).is_none() {
        return Err(format!("tag_name 不是可识别的版本号: {tag}"));
    }
    let notes = value
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let url = value
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let assets = value
        .get("assets")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let name = item.get("name")?.as_str()?.trim().to_string();
                    let asset_url = item.get("browser_download_url")?.as_str()?.to_string();
                    if name.is_empty() || asset_url.is_empty() {
                        return None;
                    }
                    Some(ReleaseAsset { name, url: asset_url })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(ReleaseInfo {
        version: tag.to_string(),
        notes: truncate_chars(notes),
        url: url.to_string(),
        assets,
    })
}

/// 顺序回落（D56：不并发竞速——白烧限流配额且镜像数据可能滞后）：
/// 主源成功即采纳（哪怕答案是不更新——主源健康时镜像的滞后数据不可信），
/// 失败或解析不出才落到下一源；全部失败把各源错误拼成一串带回。
pub fn check_update(
    fetcher: &dyn HttpTextFetcher,
    feeds: &[String],
    current: &str,
) -> CheckOutcome {
    let mut errors = Vec::new();
    for url in feeds {
        let outcome = fetcher
            .fetch_text(url, FETCH_TIMEOUT_MS)
            .and_then(|text| parse_release_json(&text).map_err(platform::PlatformError::Network));
        match outcome {
            Err(error) => errors.push(format!("{url}: {error}")),
            Ok(info) => {
                return if compare_version(&info.version, current) == Ordering::Greater {
                    CheckOutcome::Available(info)
                } else {
                    CheckOutcome::UpToDate
                };
            }
        }
    }
    CheckOutcome::Failed(errors.join("；"))
}

/// 更新源清单：`update_feeds.toml`（`feeds = [...]`，可配镜像顺序回落）覆盖
/// 默认；文件缺失或解析失败回落内置默认并留痕——坏配置不该把更新检查整个弄哑。
pub fn load_feeds(config: &Path) -> Vec<String> {
    #[derive(Deserialize)]
    struct FeedConfig {
        #[serde(default)]
        feeds: Vec<String>,
    }

    let default = || DEFAULT_FEEDS.iter().map(|s| s.to_string()).collect();
    let Ok(text) = std::fs::read_to_string(config) else {
        return default();
    };
    match toml::from_str::<FeedConfig>(&text) {
        Ok(config) if !config.feeds.is_empty() => config.feeds,
        Ok(_) => {
            log::warn!("{} 存在但 feeds 为空，回落默认更新源", config.display());
            default()
        }
        Err(error) => {
            log::warn!("{} 解析失败（{error}），回落默认更新源", config.display());
            default()
        }
    }
}

/// 距上次检查的相对文案（「3 分钟前」「5 小时前」「2 天前」）。0 = 从未检查，
/// 由调用方给专门文案，这里不管。
pub fn relative_time(elapsed_secs: u64) -> String {
    match elapsed_secs {
        0..=59 => "刚刚".to_string(),
        60..=5_399 => format!("{} 分钟前", elapsed_secs / 60),
        5_400..=172_799 => format!("{} 小时前", elapsed_secs / 3_600),
        _ => format!("{} 天前", elapsed_secs / 86_400),
    }
}

/// 更新检查状态机。状态只有 VM 自己能改，壳层经 [`Self::finish`] 拿到
/// 「下一步 UI 动作」——弹不弹窗的决策（静默失败不打扰、跳过的版本不弹）
/// 全收在这里，壳层只做属性回填。
#[derive(Debug)]
pub struct UpdateCheckVm {
    state: UpdateCheckState,
    dismissed_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum UpdateCheckState {
    /// 从未检查（或静默检查失败回退到这里——面板不显示错误）。
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Failed(String),
}

/// 检查收尾后壳层要做的 UI 动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateUiAction {
    /// 只刷新面板状态文案与角标。
    StatusOnly,
    /// 状态之外弹出「发现新版本」弹窗。
    OpenDialog,
}

impl UpdateCheckVm {
    pub fn new(dismissed_version: String) -> Self {
        UpdateCheckVm {
            state: UpdateCheckState::Idle,
            dismissed_version,
        }
    }

    /// 进入检查态。重复调用无害（按钮禁用是壳层的职责，这里只兜底状态）。
    pub fn begin_check(&mut self) {
        self.state = UpdateCheckState::Checking;
    }

    pub fn is_checking(&self) -> bool {
        self.state == UpdateCheckState::Checking
    }

    /// 检查收尾：更新状态并裁决 UI 动作。
    ///
    /// - 静默档失败 → 状态回 Idle（不把网络故障泼到面板上），仅 StatusOnly；
    /// - 命中可用更新 → 弹窗；但静默档里用户已「跳过此版本」的不弹（手动
    ///   档照样弹——用户亲手点「检查更新」就是要看结果）。
    pub fn finish(&mut self, outcome: CheckOutcome, silent: bool) -> UpdateUiAction {
        self.state = match outcome {
            CheckOutcome::UpToDate => UpdateCheckState::UpToDate,
            CheckOutcome::Available(release) => UpdateCheckState::Available(release),
            CheckOutcome::Failed(message) => {
                if silent {
                    UpdateCheckState::Idle
                } else {
                    UpdateCheckState::Failed(message)
                }
            }
        };
        match &self.state {
            UpdateCheckState::Available(release)
                if silent && release.version == self.dismissed_version =>
            {
                UpdateUiAction::StatusOnly
            }
            UpdateCheckState::Available(_) => UpdateUiAction::OpenDialog,
            _ => UpdateUiAction::StatusOnly,
        }
    }

    /// 「跳过此版本」：只对命中更新有意义。返回需要持久化的版本号
    /// （调用方写 settings），无更新时返回 None。
    pub fn skip_version(&mut self) -> Option<String> {
        let version = match &self.state {
            UpdateCheckState::Available(release) => release.version.clone(),
            _ => return None,
        };
        self.dismissed_version = version.clone();
        Some(version)
    }

    /// 当前命中的可用更新（弹窗内容与角标的唯一来源）。
    pub fn available(&self) -> Option<&ReleaseInfo> {
        match &self.state {
            UpdateCheckState::Available(release) => Some(release),
            _ => None,
        }
    }

    /// 齿轮角标可见性：命中更新且不是已跳过的版本。
    pub fn badge_visible(&self) -> bool {
        match self.available() {
            Some(release) => release.version != self.dismissed_version,
            None => false,
        }
    }

    /// 面板状态文案。`last_check_unix` / `now_unix` 只用于 Idle 态的
    /// 「上次检查」读数。
    pub fn status_text(&self, last_check_unix: u64, now_unix: u64) -> String {
        match &self.state {
            UpdateCheckState::Checking => "正在检查更新…".to_string(),
            UpdateCheckState::Available(release) => {
                if release.version == self.dismissed_version {
                    format!("发现新版本 {}（已选择跳过）", release.version)
                } else {
                    format!("发现新版本 {}", release.version)
                }
            }
            UpdateCheckState::UpToDate => "已是最新版本".to_string(),
            UpdateCheckState::Failed(message) => format!("检查失败：{message}"),
            UpdateCheckState::Idle => {
                if last_check_unix == 0 {
                    "尚未检查过更新".to_string()
                } else {
                    let elapsed = now_unix.saturating_sub(last_check_unix);
                    format!("上次检查：{}", relative_time(elapsed))
                }
            }
        }
    }

    /// 面板状态文案是否错误级（着 danger 色）。静默失败回 Idle，不在此列。
    pub fn status_is_error(&self) -> bool {
        matches!(self.state, UpdateCheckState::Failed(_))
    }
}

pub fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform::PlatformError;
    use std::cell::RefCell;

    /// 测试替身：按序回放预设响应，记录调用过的 URL（断言回落顺序）。
    /// trait 是 `&self`，两者都走 RefCell 内可变性。
    struct MockFetcher {
        responses: RefCell<Vec<platform::Result<String>>>,
        calls: RefCell<Vec<String>>,
    }

    impl MockFetcher {
        fn ok(responses: &[&str]) -> Self {
            MockFetcher {
                responses: RefCell::new(responses.iter().map(|s| Ok(s.to_string())).collect()),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn fail_then_ok(failures: usize, ok: &str) -> Self {
            let mut responses: Vec<platform::Result<String>> = (0..failures)
                .map(|_| Err(PlatformError::Network("连接失败".into())))
                .collect();
            responses.push(Ok(ok.to_string()));
            MockFetcher {
                responses: RefCell::new(responses),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl HttpTextFetcher for MockFetcher {
        fn fetch_text(&self, url: &str, _timeout_ms: u64) -> platform::Result<String> {
            self.calls.borrow_mut().push(url.to_string());
            self.responses.borrow_mut().remove(0)
        }
    }

    fn release_json(tag: &str) -> String {
        format!(
            r#"{{"tag_name":"{tag}","name":"{tag}","body":"修复若干问题","html_url":"https://github.com/x/y/releases/tag/{tag}"}}"#
        )
    }

    #[test]
    fn parse_version_accepts_v_prefix_two_segments_and_prerelease() {
        assert_eq!(parse_version("v0.2.0"), Some([0, 2, 0]));
        assert_eq!(parse_version("0.2"), Some([0, 2, 0]));
        assert_eq!(parse_version("V1.2.3-beta.1"), Some([1, 2, 3]));
        assert_eq!(parse_version("1.2.3+build.7"), Some([1, 2, 3]));
    }

    #[test]
    fn parse_version_rejects_non_numeric_and_empty() {
        assert_eq!(parse_version("beta"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("v"), None);
        assert_eq!(parse_version("0.x.1"), None);
    }

    #[test]
    fn compare_version_orders_numerically_and_unknown_is_equal() {
        use Ordering::*;
        assert_eq!(compare_version("0.2.0", "0.1.9"), Greater);
        assert_eq!(compare_version("v0.1.0", "0.1.0"), Equal);
        assert_eq!(compare_version("0.1.0", "0.2"), Less);
        assert_eq!(compare_version("0.10.0", "0.9.0"), Greater); // 字典序陷阱
        assert_eq!(compare_version("dev-build", "0.1.0"), Equal);
    }

    #[test]
    fn parse_release_json_extracts_tag_notes_and_url() {
        let info = parse_release_json(&release_json("v0.2.0")).unwrap();
        assert_eq!(info.version, "v0.2.0");
        assert_eq!(info.notes, "修复若干问题");
        assert_eq!(info.url, "https://github.com/x/y/releases/tag/v0.2.0");
    }

    #[test]
    fn parse_release_json_rejects_missing_or_bad_tag() {
        assert!(parse_release_json(r#"{"name":"no tag"}"#).is_err());
        assert!(parse_release_json(r#"{"tag_name":"not-a-version"}"#).is_err());
        assert!(parse_release_json("不是 JSON").is_err());
    }

    #[test]
    fn parse_release_json_extracts_release_assets() {
        let text = r#"{
            "tag_name": "v0.2.0",
            "assets": [
                {"name": "assetdeck-portable-0.2.0.zip", "browser_download_url": "https://dl/a.zip"},
                {"name": "assetdeck-installer-0.2.0.exe", "browser_download_url": "https://dl/a.exe"},
                {"name": "SHA256SUMS.txt", "browser_download_url": "https://dl/sums"},
                {"name": "缺 URL 的残缺项"},
                {"name": " ", "browser_download_url": "https://dl/blank-name"}
            ]
        }"#;
        let info = parse_release_json(text).unwrap();
        assert_eq!(info.assets.len(), 3);
        assert_eq!(info.assets[0].name, "assetdeck-portable-0.2.0.zip");
        assert_eq!(info.assets[2].url, "https://dl/sums");
    }

    #[test]
    fn parse_release_json_without_assets_is_not_a_bad_source() {
        // 镜像站可能只回填 tag；assets 缺失/为空只是「应用内更新不可用」，
        // 不是坏源——源回落不应被触发。
        let info = parse_release_json(&release_json("v0.2.0")).unwrap();
        assert!(info.assets.is_empty());
        let empty = parse_release_json(r#"{"tag_name":"v0.2.0","assets":[]}"#).unwrap();
        assert!(empty.assets.is_empty());
    }

    #[test]
    fn parse_release_json_truncates_long_notes() {
        let long = "很".repeat(NOTES_MAX_CHARS + 100);
        let text = format!(r#"{{"tag_name":"v1.0.0","body":"{long}"}}"#);
        let info = parse_release_json(&text).unwrap();
        assert_eq!(info.notes.chars().count() - 1, NOTES_MAX_CHARS); // 截断 + 省略号
        assert!(info.notes.ends_with('…'));
    }

    #[test]
    fn check_update_falls_back_to_mirror_in_order() {
        let fetcher = MockFetcher::fail_then_ok(1, &release_json("v0.2.0"));
        let feeds = vec![
            "https://primary/api".to_string(),
            "https://mirror/api".to_string(),
        ];
        let outcome = check_update(&fetcher, &feeds, "0.1.0");
        assert_eq!(
            outcome,
            CheckOutcome::Available(ReleaseInfo {
                version: "v0.2.0".into(),
                notes: "修复若干问题".into(),
                url: "https://github.com/x/y/releases/tag/v0.2.0".into(),
                assets: Vec::new(),
            })
        );
        assert_eq!(
            fetcher.calls.borrow().clone(),
            vec![
                "https://primary/api".to_string(),
                "https://mirror/api".to_string()
            ]
        );
    }

    #[test]
    fn check_update_primary_answer_wins_even_when_up_to_date() {
        // 主源健康回答「不更新」时终止，不落到镜像（镜像可能滞后）。
        let fetcher = MockFetcher::ok(&[&release_json("v0.1.0"), &release_json("v9.9.9")]);
        let feeds = vec![
            "https://primary/api".to_string(),
            "https://mirror/api".to_string(),
        ];
        assert_eq!(
            check_update(&fetcher, &feeds, "0.1.0"),
            CheckOutcome::UpToDate
        );
        assert_eq!(fetcher.calls.borrow().len(), 1);
    }

    #[test]
    fn check_update_all_failed_joins_errors() {
        let fetcher = MockFetcher {
            responses: RefCell::new(vec![
                Err(PlatformError::Network("HTTP 404（primary）".into())),
                Err(PlatformError::Network("连接失败".into())),
            ]),
            calls: RefCell::new(Vec::new()),
        };
        let feeds = vec![
            "https://primary/api".to_string(),
            "https://mirror/api".to_string(),
        ];
        let outcome = check_update(&fetcher, &feeds, "0.1.0");
        let CheckOutcome::Failed(message) = outcome else {
            panic!("应当 Failed");
        };
        assert!(message.contains("primary"));
        assert!(message.contains("mirror"));
    }

    #[test]
    fn load_feeds_missing_file_falls_back_to_default() {
        let feeds = load_feeds(Path::new("Z:/不存在的目录/update_feeds.toml"));
        assert_eq!(
            feeds,
            DEFAULT_FEEDS
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn load_feeds_broken_toml_falls_back_to_default() {
        let dir = std::env::temp_dir().join("assetdeck-update-check-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.toml");
        std::fs::write(&path, "feeds = 不是数组").unwrap();
        assert_eq!(load_feeds(&path).len(), DEFAULT_FEEDS.len());
    }

    #[test]
    fn load_feeds_config_overrides_default() {
        let dir = std::env::temp_dir().join("assetdeck-update-check-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("override.toml");
        std::fs::write(
            &path,
            "feeds = [\"https://mirror-a/api\", \"https://mirror-b/api\"]",
        )
        .unwrap();
        let feeds = load_feeds(&path);
        assert_eq!(
            feeds,
            vec![
                "https://mirror-a/api".to_string(),
                "https://mirror-b/api".to_string()
            ]
        );
    }

    #[test]
    fn relative_time_buckets() {
        assert_eq!(relative_time(30), "刚刚");
        assert_eq!(relative_time(120), "2 分钟前");
        assert_eq!(relative_time(7_200), "2 小时前");
        assert_eq!(relative_time(3 * 86_400), "3 天前");
    }

    #[test]
    fn vm_silent_failure_is_invisible_in_panel() {
        let mut vm = UpdateCheckVm::new(String::new());
        vm.begin_check();
        assert!(vm.is_checking());
        assert_eq!(
            vm.finish(CheckOutcome::Failed("网络错误".into()), true),
            UpdateUiAction::StatusOnly
        );
        assert_eq!(vm.status_text(0, 0), "尚未检查过更新");
        assert!(!vm.status_is_error());
    }

    #[test]
    fn vm_manual_failure_shows_error() {
        let mut vm = UpdateCheckVm::new(String::new());
        vm.begin_check();
        vm.finish(CheckOutcome::Failed("源不可达".into()), false);
        assert!(vm.status_text(0, 0).contains("源不可达"));
        assert!(vm.status_is_error());
    }

    #[test]
    fn vm_available_opens_dialog_and_badge() {
        let mut vm = UpdateCheckVm::new(String::new());
        vm.begin_check();
        assert_eq!(
            vm.finish(
                CheckOutcome::Available(ReleaseInfo {
                    version: "v0.2.0".into(),
                    notes: String::new(),
                    url: "https://example.com".into(),
                    assets: Vec::new(),
                }),
                true
            ),
            UpdateUiAction::OpenDialog
        );
        assert!(vm.badge_visible());
        assert_eq!(vm.status_text(0, 0), "发现新版本 v0.2.0");
    }

    #[test]
    fn vm_skipped_version_silences_dialog_and_badge_but_keeps_status() {
        let mut vm = UpdateCheckVm::new(String::new());
        vm.begin_check();
        vm.finish(
            CheckOutcome::Available(ReleaseInfo {
                version: "v0.2.0".into(),
                notes: String::new(),
                url: String::new(),
                assets: Vec::new(),
            }),
            true,
        );
        assert_eq!(vm.skip_version().as_deref(), Some("v0.2.0"));
        // 下一轮静默检查命中同一版本：不弹窗、无角标，但状态行仍可见。
        vm.begin_check();
        assert_eq!(
            vm.finish(
                CheckOutcome::Available(ReleaseInfo {
                    version: "v0.2.0".into(),
                    notes: String::new(),
                    url: String::new(),
                    assets: Vec::new(),
                }),
                true
            ),
            UpdateUiAction::StatusOnly
        );
        assert!(!vm.badge_visible());
        assert_eq!(vm.status_text(0, 0), "发现新版本 v0.2.0（已选择跳过）");
    }

    #[test]
    fn vm_manual_check_shows_skipped_version_dialog_anyway() {
        let mut vm = UpdateCheckVm::new("v0.2.0".to_string());
        vm.begin_check();
        assert_eq!(
            vm.finish(
                CheckOutcome::Available(ReleaseInfo {
                    version: "v0.2.0".into(),
                    notes: String::new(),
                    url: String::new(),
                    assets: Vec::new(),
                }),
                false
            ),
            UpdateUiAction::OpenDialog
        );
    }

    #[test]
    fn vm_skip_without_update_is_none() {
        let mut vm = UpdateCheckVm::new(String::new());
        assert_eq!(vm.skip_version(), None);
        vm.begin_check();
        vm.finish(CheckOutcome::UpToDate, true);
        assert_eq!(vm.skip_version(), None);
    }

    #[test]
    fn vm_idle_shows_relative_last_check() {
        let vm = UpdateCheckVm::new(String::new());
        let now = 10_000_000u64;
        assert_eq!(
            vm.status_text(now - 7_200, now),
            format!("上次检查：{}", relative_time(7_200))
        );
    }
}
