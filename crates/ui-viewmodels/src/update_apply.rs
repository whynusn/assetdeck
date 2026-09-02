//! 应用内自更新（D70）：发布附件挑选、校验和解析与「下载→就绪→启动」状态机。
//!
//! 分层依据（同 `update_check`）：网络与哈希经 [`platform::HttpFileDownloader`]
//! 和平台 sha256 注入，本模块零网络、零平台 API——纯函数与状态机全量单测。
//!
//! 统一路径（D70 定盘）：无论安装版还是便携版，下载物都是
//! `assetdeck-installer-<ver>.exe`——其 payload 内嵌 dist.tar.gz，一个文件即
//! 完整新版本。壳层校验通过后 spawn `--silent --install-dir=<exe 目录>
//! --wait-pid=<本进程>` 并退出，安装器等老进程退出后接管解包与重启；
//! 便携目录只多带一个 `--no-shortcuts`。运行中 exe 的文件锁由
//! 「先退出、安装器等信号」的时序消解，不需要 rename 舞步。

use super::update_check::ReleaseAsset;

/// 发布附件命名与 scripts/package.ps1 / ci.yml 对齐：分发产物一律 ASCII 名
/// （中文文件名在 GitHub 资产链路被吞，实测事故见 package.ps1 头注）。
pub const INSTALLER_ASSET_PREFIX: &str = "assetdeck-installer-";
pub const SUMS_ASSET_NAME: &str = "SHA256SUMS.txt";

/// 单源下载超时（毫秒）。与 D56 检查同语义：每相位上限，不是总时长上限；
/// 下载在后台线程执行，慢只影响完成时刻，不阻塞 UI。
pub const DOWNLOAD_TIMEOUT_MS: u64 = 30_000;

/// 下载量异常上限。当前安装包约 30 MB，给数量级余量防对端异常无界吃盘；
/// 真包超限属于发版事故，宁可失败也不静默吞下。
pub const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// 版本 tag → 安装器附件名。tag 形如 "v0.2.0"（CI 强校验 tag == Cargo.toml
/// 版本，产物文件名取无 v 的裸版本，见 package.ps1 `$Version`）。
pub fn installer_asset_name(version_tag: &str) -> String {
    format!(
        "{INSTALLER_ASSET_PREFIX}{}.exe",
        version_tag.trim_start_matches(['v', 'V'])
    )
}

/// 按精确文件名挑附件。清单里出现同前缀旧版本附件也不模糊匹配——
/// 宁可报「缺少安装包」也不能装错文件。
pub fn pick_asset<'a>(assets: &'a [ReleaseAsset], name: &str) -> Option<&'a ReleaseAsset> {
    assets.iter().find(|asset| asset.name == name)
}

/// 解析 sha256sum 标准格式（`<十六进制>␣␣<文件名>`，二进制模式为 `␣*文件名`；
/// package.ps1 生成，`sha256sum -c` 可直接校验的同一格式）。坏行跳过，
/// 哈希统一转小写。
pub fn parse_sha256_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (hash, name) = line.split_once(char::is_whitespace)?;
            let name = name.trim_start_matches([' ', '*']).trim();
            let hash = hash.trim();
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) || name.is_empty()
            {
                return None;
            }
            Some((hash.to_ascii_lowercase(), name.to_string()))
        })
        .collect()
}

/// 摘要比对：两侧都必须是 64 位十六进制（长度守卫防「空对空相等」的假通过），
/// 十六进制大小写不敏感。
pub fn hash_matches(expected: &str, actual: &str) -> bool {
    expected.len() == 64
        && actual.len() == 64
        && expected.bytes().all(|b| b.is_ascii_hexdigit())
        && expected.eq_ignore_ascii_case(actual)
}

/// 自更新应用状态机。弹窗四态 = 初始（可选按钮）/ 下载中（进度条）/ 启动中
/// （不可逆段）/ 失败（可重试）。下载线程经壳层回 UI 线程喂进度与结局；
/// 状态只有 VM 自己能改，壳层只做属性回填（同 UpdateCheckVm 纪律）。
#[derive(Debug, Default)]
pub struct UpdateApplyVm {
    state: ApplyState,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
enum ApplyState {
    #[default]
    Idle,
    Downloading { received: u64, total: u64 },
    Launching,
    Failed(String),
}

impl UpdateApplyVm {
    pub fn new() -> Self {
        UpdateApplyVm { state: ApplyState::Idle }
    }

    /// 进入下载态。从任何状态（失败重试、取消后重下）都允许。
    pub fn begin_download(&mut self) {
        self.state = ApplyState::Downloading { received: 0, total: 0 };
    }

    /// 下载进度回填。仅在下载态生效——取消后迟到的进度不得把状态拉回下载。
    pub fn set_progress(&mut self, received: u64, total: u64) {
        if let ApplyState::Downloading { received: r, total: t } = &mut self.state {
            *r = received;
            *t = total;
        }
    }

    /// 下载完成且校验通过（校验判定在 [`hash_matches`] 侧），进入不可逆的
    /// 启动段：壳层接下来 spawn 安装器并退出本进程。
    pub fn mark_launching(&mut self) {
        if matches!(self.state, ApplyState::Downloading { .. }) {
            self.state = ApplyState::Launching;
        }
    }

    /// 失败登记。Idle 态吞掉——那是「用户已取消」后迟到的失败，不该把
    /// 弹窗重新拽回错误态。
    pub fn mark_failed(&mut self, message: String) {
        if matches!(
            self.state,
            ApplyState::Downloading { .. } | ApplyState::Launching
        ) {
            self.state = ApplyState::Failed(message);
        }
    }

    /// 回 Idle（用户取消 / 重试前清理）。
    pub fn reset(&mut self) {
        self.state = ApplyState::Idle;
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.state, ApplyState::Downloading { .. } | ApplyState::Launching)
    }

    pub fn is_downloading(&self) -> bool {
        matches!(self.state, ApplyState::Downloading { .. })
    }

    pub fn is_launching(&self) -> bool {
        self.state == ApplyState::Launching
    }

    pub fn failure(&self) -> Option<&str> {
        match &self.state {
            ApplyState::Failed(message) => Some(message),
            _ => None,
        }
    }

    /// 进度条比例 0.0..=1.0。total 未知（对端未报 Content-Length）给 0.0，
    /// UI 侧按不确定态呈现；received 超过 total（代理多算等）钳到 1.0。
    pub fn progress_ratio(&self) -> f32 {
        match self.state {
            ApplyState::Downloading { received, total } if total > 0 => {
                (received as f32 / total as f32).clamp(0.0, 1.0)
            }
            _ => 0.0,
        }
    }

    /// 下载进度文案（None = 不在下载态）。MB 按 1024² 计，一位小数。
    pub fn progress_text(&self) -> Option<String> {
        match self.state {
            ApplyState::Downloading { received, total } => {
                let received_mb = received as f64 / (1024.0 * 1024.0);
                if total > 0 {
                    let total_mb = total as f64 / (1024.0 * 1024.0);
                    let percent = self.progress_ratio() * 100.0;
                    Some(format!(
                        "已下载 {received_mb:.1} MB / {total_mb:.1} MB（{percent:.0}%）"
                    ))
                } else {
                    Some(format!("已下载 {received_mb:.1} MB"))
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update_check::ReleaseAsset;

    fn asset(name: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_string(),
            url: format!("https://dl/{name}"),
        }
    }

    #[test]
    fn installer_asset_name_strips_v_prefix() {
        assert_eq!(
            installer_asset_name("v0.2.0"),
            "assetdeck-installer-0.2.0.exe"
        );
        assert_eq!(installer_asset_name("0.2.0"), "assetdeck-installer-0.2.0.exe");
        assert_eq!(installer_asset_name("V1.0"), "assetdeck-installer-1.0.exe");
    }

    #[test]
    fn pick_asset_matches_exact_name_only() {
        let assets = vec![
            asset("assetdeck-installer-0.1.0.exe"),
            asset("assetdeck-installer-0.2.0.exe"),
            asset(SUMS_ASSET_NAME),
        ];
        assert_eq!(
            pick_asset(&assets, "assetdeck-installer-0.2.0.exe").map(|a| a.url.as_str()),
            Some("https://dl/assetdeck-installer-0.2.0.exe")
        );
        assert!(pick_asset(&assets, "assetdeck-installer-9.9.9.exe").is_none());
        assert!(pick_asset(&[], "assetdeck-installer-0.2.0.exe").is_none());
    }

    #[test]
    fn parse_sha256_sums_handles_standard_variants() {
        let text = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  assetdeck-portable-0.2.0.zip\r\n\
                    BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD *assetdeck-installer-0.2.0.exe\n\
                    这不是一行合法记录\n\
                    short  name.exe\n";
        let sums = parse_sha256_sums(text);
        assert_eq!(sums.len(), 2);
        assert_eq!(
            sums[0],
            (
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string(),
                "assetdeck-portable-0.2.0.zip".to_string()
            )
        );
        // 大写哈希转小写；`*` 二进制标记剥离。
        assert_eq!(
            sums[1].0,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sums[1].1, "assetdeck-installer-0.2.0.exe");
    }

    #[test]
    fn hash_matches_is_case_insensitive_and_length_guarded() {
        let lower = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let upper = lower.to_ascii_uppercase();
        assert!(hash_matches(lower, &upper));
        assert!(hash_matches(&upper, lower));
        assert!(!hash_matches(lower, "different"));
        // 长度守卫：空串与空串不得视为相等。
        assert!(!hash_matches("", ""));
        assert!(!hash_matches("abc", "abc"));
    }

    #[test]
    fn vm_progress_accumulates_only_while_downloading() {
        let mut vm = UpdateApplyVm::new();
        vm.set_progress(1, 10); // Idle 态的进度是迟到噪声，吞掉
        assert_eq!(vm.progress_ratio(), 0.0);

        vm.begin_download();
        vm.set_progress(3_000_000, 10_000_000);
        assert_eq!(vm.progress_ratio(), 0.3);
        assert_eq!(
            vm.progress_text().as_deref(),
            Some("已下载 2.9 MB / 9.5 MB（30%）")
        );
        assert!(vm.is_busy() && vm.is_downloading() && !vm.is_launching());

        vm.reset();
        assert!(!vm.is_busy());
        assert_eq!(vm.progress_text(), None);
    }

    #[test]
    fn vm_progress_handles_unknown_total_and_overrun() {
        let mut vm = UpdateApplyVm::new();
        vm.begin_download();
        vm.set_progress(5 * 1024 * 1024, 0);
        assert_eq!(vm.progress_ratio(), 0.0); // total 未知 = 不确定态
        assert_eq!(vm.progress_text().as_deref(), Some("已下载 5.0 MB"));

        vm.set_progress(20 * 1024 * 1024, 10 * 1024 * 1024);
        assert_eq!(vm.progress_ratio(), 1.0); // 超收钳满
    }

    #[test]
    fn vm_launching_is_terminal_for_progress_but_allows_failure() {
        let mut vm = UpdateApplyVm::new();
        vm.begin_download();
        vm.set_progress(1, 1);
        vm.mark_launching();
        assert!(vm.is_busy() && vm.is_launching() && !vm.is_downloading());
        assert_eq!(vm.progress_text(), None); // 启动段不再显示进度

        // spawn 失败可以从启动段回落到失败态。
        vm.mark_failed("无法启动安装器".into());
        assert_eq!(vm.failure(), Some("无法启动安装器"));
        assert!(!vm.is_busy());
    }

    #[test]
    fn vm_late_failure_after_cancel_is_swallowed() {
        let mut vm = UpdateApplyVm::new();
        vm.begin_download();
        vm.reset(); // 用户取消：状态回 Idle
        vm.mark_failed("下载已取消".into()); // 下载线程迟到的失败
        assert_eq!(vm.failure(), None);
        assert!(!vm.is_busy());
    }

    #[test]
    fn vm_retry_from_failure_starts_fresh_download() {
        let mut vm = UpdateApplyVm::new();
        vm.begin_download();
        vm.mark_failed("网络断开".into());
        vm.begin_download(); // 重试
        assert!(vm.is_downloading());
        assert_eq!(vm.failure(), None);
        assert_eq!(vm.progress_ratio(), 0.0);
    }
}
