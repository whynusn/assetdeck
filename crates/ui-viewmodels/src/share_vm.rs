//! 共享状态机 VM（D80 M0）：设备表、在途批次、入口报价确认、角标派生。
//!
//! 纯逻辑零 IO：worker 子进程的生命周期与管道归 app-ui，本模块只吃
//! [`WorkerEvent`]（stdout 行解析产物）、吐 [`ShareAction`]（app-ui 要执行的
//! 副作用：往 worker 写行、开弹窗、喂导入管线、toast）。状态不设第二真源：
//! 角标/进度/确认队列全部由事件流推导。

use std::collections::HashMap;
use std::path::PathBuf;

use uuid::Uuid;

use share::{DeviceEntry, DoneState, SendItem, SendRequest, ShareDirection, WorkerEvent};

use crate::target_bar_vm::TargetNoticeTone;

/// 批次在途阶段（仅活跃期有意义；终结后 phase=None、final 就位）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharePhase {
    /// 发送侧：读文件算 SHA-256。
    Hashing,
    /// 发送侧：握手。
    Connecting,
    /// 发送侧：裸流传输中。
    Sending,
    /// 接收侧：清单已报，等人首连确认。
    AwaitingConfirm,
    /// 接收侧：文件已暂存，等导入收尾。
    Importing,
}

impl SharePhase {
    pub fn label(&self) -> &'static str {
        match self {
            SharePhase::Hashing => "校验中",
            SharePhase::Connecting => "连接中",
            SharePhase::Sending => "传输中",
            SharePhase::AwaitingConfirm => "等待确认",
            SharePhase::Importing => "导入中",
        }
    }
}

/// 批次终局（done 行 / 用户拒绝产物）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareFinal {
    /// 送达（发送侧 DELIVERED / 接收侧导入完成）。
    Delivered,
    /// 对端拒收。
    Rejected(String),
    /// 故障（含超时、链路断）。
    Failed(String),
}

/// 一个在途或刚完结的共享批次。
#[derive(Debug, Clone)]
pub struct ShareBatch {
    pub id: Uuid,
    pub direction: ShareDirection,
    /// 对端显示名（发送=目标设备名；接收=清单里的 sender_name）。
    pub peer_name: String,
    pub phase: Option<SharePhase>,
    pub final_state: Option<ShareFinal>,
    pub items_total: usize,
    pub items_done: usize,
    pub sent_bytes: u64,
    pub total_bytes: u64,
    /// 发送侧：本批覆盖的素材 uuid（角标键）。
    pub asset_uuids: Vec<Uuid>,
    /// 失败/被拒的单项明细（uuid → 原因）。
    pub failed_items: HashMap<Uuid, String>,
}

impl ShareBatch {
    pub fn finished(&self) -> bool {
        self.final_state.is_some()
    }

    /// 多批并发时 UI 摘要行的单批文案（「给小李的机器：传输中 2/3」）。
    pub fn summary(&self) -> String {
        let who = match self.direction {
            ShareDirection::Sent => format!("给{}", self.peer_name),
            ShareDirection::Received => format!("来自{}", self.peer_name),
        };
        match (&self.phase, &self.final_state) {
            (Some(phase), _) => {
                format!(
                    "{who}：{} {}/{}",
                    phase.label(),
                    self.items_done,
                    self.items_total
                )
            }
            (None, Some(ShareFinal::Delivered)) => format!("{who}：已送达"),
            (None, Some(ShareFinal::Rejected(reason))) => format!("{who}：被拒收（{reason}）"),
            (None, Some(ShareFinal::Failed(reason))) => format!("{who}：失败（{reason}）"),
            (None, None) => who,
        }
    }
}

/// 瓦片角标三态（D80 红线：共享状态在角标上可见，不设第二真源）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareBadge {
    /// 在途（校验/连接/传输/等确认/导入中）。
    Busy,
    /// 已送达。
    Done,
    /// 失败或被拒。
    Failed,
}

/// VM 要 app-ui 执行的副作用。字符串 `Send` 是原样写进 worker stdin 的行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareAction {
    /// 向 worker stdin 写一行。
    Send(String),
    /// 弹首连确认框（入口报价到达）。
    OpenOfferDialog,
    /// 关确认框（裁决完成/超时撤单）。
    CloseOfferDialog,
    /// 暂存完成 → 走 D66 归类弹窗导入流（完成后 app-ui 回调 import_completed）。
    ImportStaged {
        batch: Uuid,
        paths: Vec<PathBuf>,
    },
    Notice(TargetNoticeTone, String),
    /// 这些素材的角标状态变了（app-ui 定向更新瓦片，不整表重建）。
    BadgesChanged(Vec<Uuid>),
    /// 设备表/进度/角标有变化，UI 拉取刷新。
    Refresh,
}

/// 入口报价（待确认的来件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOffer {
    pub batch_id: Uuid,
    pub sender_name: String,
    pub items_total: usize,
    pub total_bytes: u64,
}

pub struct ShareVm {
    /// READY 回报的本机 endpoint id（z32）。
    self_id: Option<String>,
    self_name: Option<String>,
    devices: Vec<DeviceEntry>,
    scanning: bool,
    picker_selected: Option<usize>,
    /// 设备选择弹窗是否打开（决定 DEVICE 到齐前展示扫描态文案）。
    picker_open: bool,
    /// 待确认来件队列（并发推送按先到先弹）。
    offers: Vec<PendingOffer>,
    batches: Vec<ShareBatch>,
    /// 角标真源：资产 uuid → 最近共享状态。批次的在途/终态每次迁移都
    /// 写这里（含 Busy），badge() 查询 O(1)；批次被 evict 不回滚角标
    /// （角标语义 = 「最近一次共享结果」，生命周期长于批次记录）。
    badge_overrides: HashMap<Uuid, ShareBadge>,
    /// import_completed 攒下的待写行（导入回调不在事件循环里，app-ui 下轮取走）。
    pending_writes: Vec<String>,
    /// 已完结批次保留最近这么多条供摘要行/记录回放（超出静默丢弃旧的）。
    final_keep: usize,
}

/// 已完结批次的保留上限（够看最近几次；再往后的历史属于记录面，M1 落库）。
const FINAL_KEEP: usize = 8;

impl Default for ShareVm {
    fn default() -> Self {
        Self::new()
    }
}

impl ShareVm {
    pub fn new() -> Self {
        Self {
            self_id: None,
            self_name: None,
            devices: Vec::new(),
            scanning: false,
            picker_selected: None,
            picker_open: false,
            offers: Vec::new(),
            batches: Vec::new(),
            badge_overrides: HashMap::new(),
            pending_writes: Vec::new(),
            final_keep: FINAL_KEEP,
        }
    }

    pub fn self_id(&self) -> Option<&str> {
        self.self_id.as_deref()
    }

    pub fn self_name(&self) -> Option<&str> {
        self.self_name.as_deref()
    }

    pub fn devices(&self) -> &[DeviceEntry] {
        &self.devices
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    pub fn picker_selected(&self) -> Option<usize> {
        self.picker_selected
    }

    pub fn current_offer(&self) -> Option<&PendingOffer> {
        self.offers.first()
    }

    pub fn batches(&self) -> &[ShareBatch] {
        &self.batches
    }

    // ===== 用户动作入口 =====

    /// 打开设备选择弹窗并触发一轮扫描。
    pub fn open_picker(&mut self) -> Vec<ShareAction> {
        self.picker_open = true;
        self.picker_selected = if self.devices.is_empty() {
            None
        } else {
            self.picker_selected.filter(|i| *i < self.devices.len())
        };
        self.start_scan()
    }

    pub fn close_picker(&mut self) {
        self.picker_open = false;
    }

    pub fn is_picker_open(&self) -> bool {
        self.picker_open
    }

    pub fn select_device(&mut self, index: usize) {
        if index < self.devices.len() {
            self.picker_selected = Some(index);
        }
    }

    /// 弹窗内手动刷新。
    pub fn start_scan(&mut self) -> Vec<ShareAction> {
        self.scanning = true;
        vec![ShareAction::Send("SCAN".into()), ShareAction::Refresh]
    }

    /// 右键/多选入口确认：按选中设备发起推送。
    pub fn start_share(&mut self, items: Vec<SendItem>) -> Vec<ShareAction> {
        let Some(index) = self.picker_selected else {
            return vec![ShareAction::Notice(
                TargetNoticeTone::Warning,
                "提示：先在设备列表里选一个设备".into(),
            )];
        };
        let Some(device) = self.devices.get(index).cloned() else {
            return Vec::new();
        };
        let (Some(self_name), Some(_self_id)) = (self.self_name.clone(), self.self_id.clone())
        else {
            return vec![ShareAction::Notice(
                TargetNoticeTone::Error,
                "共享通道尚未就绪，稍后再试".into(),
            )];
        };
        let batch_id = Uuid::new_v4();
        let total_bytes = items.iter().map(|i| i.size_bytes).sum();
        let asset_uuids: Vec<Uuid> = items.iter().map(|i| i.asset_uuid).collect();
        let request = SendRequest {
            batch_id,
            sender_name: self_name,
            receiver_id: device.id.clone(),
            receiver_addrs: device.addrs.clone(),
            items: items.clone(),
        };
        let line = match serde_json::to_string(&request) {
            Ok(json) => format!("SEND\t{json}"),
            Err(e) => {
                return vec![ShareAction::Notice(
                    TargetNoticeTone::Error,
                    format!("共享请求构造失败：{e}"),
                )]
            }
        };
        self.batches.push(ShareBatch {
            id: batch_id,
            direction: ShareDirection::Sent,
            peer_name: device.name.clone(),
            phase: Some(SharePhase::Hashing),
            final_state: None,
            items_total: request.items.len(),
            items_done: 0,
            sent_bytes: 0,
            total_bytes,
            asset_uuids,
            failed_items: HashMap::new(),
        });
        self.picker_open = false;
        let badge_changed =
            self.apply_badges(items.iter().map(|i| (i.asset_uuid, ShareBadge::Busy)));
        let mut actions = vec![ShareAction::Send(line)];
        if !badge_changed.is_empty() {
            actions.push(ShareAction::BadgesChanged(badge_changed));
        }
        actions.push(ShareAction::Refresh);
        actions
    }

    /// 首连确认「接收」。
    pub fn offer_accept(&mut self) -> Vec<ShareAction> {
        let Some(offer) = self.offers.first().cloned() else {
            return Vec::new();
        };
        if let Some(batch) = self.find_batch_mut(offer.batch_id) {
            batch.phase = Some(SharePhase::Importing);
        }
        self.offers.remove(0);
        let mut actions = vec![ShareAction::Send(format!("ACCEPT\t{}", offer.batch_id))];
        if self.offers.is_empty() {
            actions.push(ShareAction::CloseOfferDialog);
        }
        actions.push(ShareAction::Refresh);
        actions
    }

    /// 首连确认「拒绝」。
    pub fn offer_reject(&mut self, reason: &str) -> Vec<ShareAction> {
        let Some(offer) = self.offers.first().cloned() else {
            return Vec::new();
        };
        if let Some(batch) = self.find_batch_mut(offer.batch_id) {
            batch.phase = None;
            batch.final_state = Some(ShareFinal::Rejected(reason.to_string()));
        }
        self.offers.remove(0);
        let mut actions = vec![ShareAction::Send(format!(
            "REJECT\t{}\t{reason}",
            offer.batch_id
        ))];
        if self.offers.is_empty() {
            actions.push(ShareAction::CloseOfferDialog);
        }
        actions.push(ShareAction::Refresh);
        actions
    }

    /// 导入管线收尾回执（ImportFlow.do_import 的 post_phase1 回调调用）。
    pub fn import_completed(&mut self, batch: Uuid, ok: bool, reason: &str) {
        let line = if ok {
            format!("ACK\t{batch}")
        } else {
            format!("NACK\t{batch}\t{reason}")
        };
        // 行发出即可；最终状态以 worker 的 done 行为准（此处不改批次状态，
        // 避免 UI 与 worker 两真源打架）。
        let _ = batch;
        self.pending_writes.push(line);
    }

    /// import_completed 攒下的待写行（app-ui 每轮刷新时取走写管道）。
    /// 之所以不塞进 handle_event 返回值：调用点在导入回调，不在事件循环里。
    pub fn take_pending_writes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_writes)
    }

    // ===== 事件流 =====

    /// 消费一行 worker 输出，产出副作用。
    pub fn handle_line(&mut self, line: &str) -> Vec<ShareAction> {
        let event = share::parse_worker_line(line);
        self.handle_event(event)
    }

    pub fn handle_event(&mut self, event: WorkerEvent) -> Vec<ShareAction> {
        match event {
            WorkerEvent::Ready { id_z32, name } => {
                self.self_id = Some(id_z32);
                self.self_name = Some(name);
                vec![]
            }
            WorkerEvent::Device(entry) => {
                self.merge_device(entry);
                vec![ShareAction::Refresh]
            }
            WorkerEvent::ScanDone => self.scan_finished(),
            WorkerEvent::Incoming(manifest) => {
                let total_bytes = manifest.items.iter().map(|i| i.size_bytes).sum();
                let offer = PendingOffer {
                    batch_id: manifest.batch_id,
                    sender_name: manifest.sender_name.clone(),
                    items_total: manifest.items.len(),
                    total_bytes,
                };
                self.batches.push(ShareBatch {
                    id: manifest.batch_id,
                    direction: ShareDirection::Received,
                    peer_name: manifest.sender_name.clone(),
                    phase: Some(SharePhase::AwaitingConfirm),
                    final_state: None,
                    items_total: manifest.items.len(),
                    items_done: 0,
                    sent_bytes: 0,
                    total_bytes,
                    asset_uuids: Vec::new(),
                    failed_items: HashMap::new(),
                });
                let first = self.offers.is_empty();
                self.offers.push(offer);
                let mut actions = Vec::new();
                if first {
                    actions.push(ShareAction::OpenOfferDialog);
                }
                actions.push(ShareAction::Refresh);
                actions
            }
            WorkerEvent::Staged { batch_id, paths } => {
                if let Some(batch) = self.find_batch_mut(batch_id) {
                    batch.phase = Some(SharePhase::Importing);
                }
                vec![
                    ShareAction::ImportStaged {
                        batch: batch_id,
                        paths,
                    },
                    ShareAction::Refresh,
                ]
            }
            WorkerEvent::Progress {
                batch_id,
                sent,
                total,
            } => {
                if let Some(batch) = self.find_batch_mut(batch_id) {
                    batch.phase = Some(SharePhase::Sending);
                    batch.sent_bytes = sent;
                    if total > 0 {
                        batch.total_bytes = total;
                    }
                }
                vec![ShareAction::Refresh]
            }
            WorkerEvent::Item {
                batch_id,
                uuid,
                ok,
                detail,
                ..
            } => {
                let mut actions = Vec::new();
                if let Some(batch) = self.find_batch_mut(batch_id) {
                    if ok {
                        batch.items_done += 1;
                    } else {
                        batch.failed_items.insert(uuid, detail.clone());
                        actions.push(ShareAction::Notice(
                            TargetNoticeTone::Warning,
                            format!("警示：1 项传输失败：{detail}"),
                        ));
                    }
                }
                if !ok {
                    // 单项失败即翻 Failed：等 done 再改会让「失败」窗口期漏报。
                    let changed = self.apply_badges([(uuid, ShareBadge::Failed)]);
                    if !changed.is_empty() {
                        actions.push(ShareAction::BadgesChanged(changed));
                    }
                }
                actions.push(ShareAction::Refresh);
                actions
            }
            WorkerEvent::Done {
                batch_id,
                state,
                detail: _,
            } => {
                let mut actions = Vec::new();
                let (final_state, tone) = match state {
                    DoneState::Delivered | DoneState::Transferred => {
                        (ShareFinal::Delivered, TargetNoticeTone::Success)
                    }
                    DoneState::Rejected(reason) => {
                        (ShareFinal::Rejected(reason), TargetNoticeTone::Warning)
                    }
                    DoneState::Failed(reason) => {
                        (ShareFinal::Failed(reason), TargetNoticeTone::Error)
                    }
                };
                let delivered = matches!(final_state, ShareFinal::Delivered);
                // 发送批才有本地素材 uuid（接收批 asset_uuids 为空，无角标可翻）。
                // 送达时单项失败的不洗成 Done（保留 Failed）；整体失败/被拒则全翻。
                let badge_updates = match self.find_batch_mut(batch_id) {
                    Some(batch) => {
                        batch.phase = None;
                        batch.final_state = Some(final_state.clone());
                        let target = if delivered {
                            ShareBadge::Done
                        } else {
                            ShareBadge::Failed
                        };
                        batch
                            .asset_uuids
                            .iter()
                            .filter(|u| !(delivered && batch.failed_items.contains_key(u)))
                            .map(|u| (*u, target))
                            .collect::<Vec<_>>()
                    }
                    None => Vec::new(),
                };
                let badge_changed = self.apply_badges(badge_updates);
                let summary = match self.find_batch(batch_id) {
                    Some(batch) => batch.summary(),
                    None => match &final_state {
                        ShareFinal::Delivered => "共享批次：已送达".to_string(),
                        ShareFinal::Rejected(r) | ShareFinal::Failed(r) => {
                            format!("共享批次：{r}")
                        }
                    },
                };
                // 送达静默（成功不该抢焦点）；拒收/失败才 toast。
                if !delivered {
                    actions.push(ShareAction::Notice(tone, summary));
                }
                if !badge_changed.is_empty() {
                    actions.push(ShareAction::BadgesChanged(badge_changed));
                }
                self.evict_final();
                actions.push(ShareAction::Refresh);
                actions
            }
            WorkerEvent::Notice(text) => {
                let tone = if text.starts_with("警示") {
                    TargetNoticeTone::Warning
                } else {
                    TargetNoticeTone::Success
                };
                vec![ShareAction::Notice(tone, text)]
            }
            WorkerEvent::Unknown(raw) => {
                if raw.is_empty() {
                    vec![]
                } else {
                    // 协议漂移只警示一次原文，绝不当命令处理。
                    vec![ShareAction::Notice(
                        TargetNoticeTone::Warning,
                        format!("警示：收到未知共享通道消息（{raw}）"),
                    )]
                }
            }
        }
    }

    /// 扫描窗口结束（app-ui 在发 SCAN 后的窗口回调里调用）。
    pub fn scan_finished(&mut self) -> Vec<ShareAction> {
        self.scanning = false;
        vec![ShareAction::Refresh]
    }

    /// 多批并发摘要行（多选栏/弹窗底部提示）：活跃批次全部列出。
    pub fn active_summary(&self) -> Option<String> {
        let active: Vec<String> = self
            .batches
            .iter()
            .filter(|b| !b.finished())
            .map(ShareBatch::summary)
            .collect();
        (!active.is_empty()).then(|| active.join("；"))
    }

    /// 素材角标：O(1) 查覆盖表（真源在 badge_overrides）。
    pub fn badge(&self, asset: Uuid) -> Option<ShareBadge> {
        self.badge_overrides.get(&asset).copied()
    }

    // ===== 内部 =====

    fn find_batch(&self, id: Uuid) -> Option<&ShareBatch> {
        self.batches.iter().find(|b| b.id == id)
    }

    fn find_batch_mut(&mut self, id: Uuid) -> Option<&mut ShareBatch> {
        self.batches.iter_mut().find(|b| b.id == id)
    }

    /// 写角标覆盖表，返回真正翻转了的 uuid（app-ui 定向刷新瓦片的依据；
    /// 值没变的 uuid 不进列表，省一次 set_row_data）。
    fn apply_badges<I>(&mut self, updates: I) -> Vec<Uuid>
    where
        I: IntoIterator<Item = (Uuid, ShareBadge)>,
    {
        let mut changed = Vec::new();
        for (asset, badge) in updates {
            if self.badge_overrides.get(&asset) != Some(&badge) {
                changed.push(asset);
            }
            self.badge_overrides.insert(asset, badge);
        }
        changed
    }

    /// 设备表合并：同 id 原位更新（名称/地址刷新、顺序稳定）；新 id 追加。
    fn merge_device(&mut self, entry: DeviceEntry) {
        if self.self_id.as_deref() == Some(entry.id.as_str()) {
            return; // 自家回声（正常情况下 worker 已过滤，双保险）。
        }
        match self.devices.iter().position(|d| d.id == entry.id) {
            Some(pos) => self.devices[pos] = entry,
            None => self.devices.push(entry),
        }
    }

    /// 完结批次超出保留上限时丢弃最旧。
    fn evict_final(&mut self) {
        let mut finished: Vec<usize> = (0..self.batches.len())
            .filter(|i| self.batches[*i].finished())
            .collect();
        if finished.len() <= self.final_keep {
            return;
        }
        let drop_count = finished.len() - self.final_keep;
        // 丢弃最旧（插入序即时间序）。
        finished.truncate(drop_count);
        finished.sort_unstable_by(|a, b| b.cmp(a)); // 倒序删，下标不失效
        for index in finished {
            self.batches.remove(index);
        }
    }
}

/// 人话字节数（弹窗摘要/进度行共用）。
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

impl ShareVm {
    /// 入口报价确认弹窗正文（「来自小李的机器：3 项 · 45.2 MB」）。
    pub fn offer_text(&self) -> Option<String> {
        let offer = self.offers.first()?;
        Some(format!(
            "来自 {}：{} 项 · {}",
            offer.sender_name,
            offer.items_total,
            human_size(offer.total_bytes)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::AssetKind;
    use share::{ManifestItem, TransferManifest};
    use std::path::PathBuf;

    fn item(uuid: Uuid) -> SendItem {
        SendItem {
            asset_uuid: uuid,
            file_name: "a.png".into(),
            kind: AssetKind::Image,
            size_bytes: 1024,
            category_hint: Some("壁纸".into()),
            path: PathBuf::from(r"C:\lib\a.png"),
        }
    }

    fn manifest(batch: Uuid, sender: &str, items: usize) -> TransferManifest {
        TransferManifest {
            batch_id: batch,
            sender_name: sender.into(),
            items: (0..items)
                .map(|_| ManifestItem {
                    asset_uuid: Uuid::new_v4(),
                    file_name: "x.png".into(),
                    kind: AssetKind::Image,
                    size_bytes: 500_000,
                    sha256_hex: "0".repeat(64),
                    category_hint: None,
                })
                .collect(),
        }
    }

    fn ready_vm() -> ShareVm {
        let mut vm = ShareVm::new();
        vm.handle_line("READY\tz32z32z32z32z32z32z32z32z32z32z32z32z32z32z32z32\t本机");
        vm
    }

    fn device_line() -> String {
        format!("DEVICE\t{}\t小李的机器\t192.168.1.8:4433", "d".repeat(52))
    }

    #[test]
    fn ready_and_device_merge_keeps_order_stable() {
        let mut vm = ready_vm();
        assert_eq!(vm.self_name(), Some("本机"));
        vm.handle_line(&device_line());
        let renamed = format!("DEVICE\t{}\t改名的小李\t192.168.1.9:4433", "d".repeat(52));
        vm.handle_line(&renamed);
        let other = format!("DEVICE\t{}\t小王\t10.0.0.2:1", "e".repeat(52));
        vm.handle_line(&other);
        let ids: Vec<&str> = vm.devices().iter().map(|d| &d.id[..]).collect();
        assert_eq!(ids.len(), 2, "同 id 必须原位合并而非追加");
        assert_eq!(vm.devices()[0].name, "改名的小李");
        assert_eq!(vm.devices()[0].addrs[0], "192.168.1.9:4433");
        assert_eq!(vm.devices()[1].name, "小王");
    }

    #[test]
    fn share_flow_emits_send_and_tracks_badges() {
        let mut vm = ready_vm();
        vm.handle_line(&device_line());
        vm.open_picker();
        vm.select_device(0);
        let asset = Uuid::new_v4();
        let actions = vm.start_share(vec![item(asset)]);
        assert!(matches!(&actions[0], ShareAction::Send(l) if l.starts_with("SEND\t")));
        assert!(actions.iter().any(
            |a| matches!(a, ShareAction::BadgesChanged(ids) if ids.len() == 1 && ids[0] == asset)
        ));
        assert_eq!(vm.badge(asset), Some(ShareBadge::Busy));
        let batch = vm.batches()[0].id;
        // 进度不改角标态，只推进摘要。
        vm.handle_line(&format!("PROGRESS\t{batch}\t1024\t1024"));
        assert_eq!(vm.badge(asset), Some(ShareBadge::Busy));
        let actions = vm.handle_line(&format!("done\t{batch}\tdelivered\t"));
        assert_eq!(vm.badge(asset), Some(ShareBadge::Done));
        assert!(actions.iter().any(
            |a| matches!(a, ShareAction::BadgesChanged(ids) if ids.len() == 1 && ids[0] == asset)
        ));
        assert_eq!(vm.active_summary(), None);
    }

    #[test]
    fn item_failure_flips_badge_and_warns() {
        let mut vm = ready_vm();
        vm.handle_line(&device_line());
        vm.open_picker();
        vm.select_device(0);
        let asset = Uuid::new_v4();
        vm.start_share(vec![item(asset)]);
        let batch = vm.batches()[0].id;
        let actions = vm.handle_line(&format!("ITEM\t{batch}\tsend\t{asset}\tfailed\t磁盘满"));
        assert!(actions.iter().any(|a| matches!(a, ShareAction::Notice(TargetNoticeTone::Warning, t) if t.contains("磁盘满"))));
        assert!(actions.iter().any(
            |a| matches!(a, ShareAction::BadgesChanged(ids) if ids.len() == 1 && ids[0] == asset)
        ));
        assert_eq!(vm.badge(asset), Some(ShareBadge::Failed));
    }

    #[test]
    fn incoming_offer_confirm_cycle() {
        let mut vm = ready_vm();
        let batch = Uuid::new_v4();
        let json = serde_json::to_string(&manifest(batch, "小李的机器", 3)).unwrap();
        let actions = vm.handle_line(&format!("INCOMING\t{json}"));
        assert!(actions.contains(&ShareAction::OpenOfferDialog));
        assert_eq!(
            vm.offer_text().as_deref(),
            Some("来自 小李的机器：3 项 · 1.4 MB")
        );
        let actions = vm.offer_accept();
        assert!(actions.contains(&ShareAction::Send(format!("ACCEPT\t{batch}"))));
        assert!(actions.contains(&ShareAction::CloseOfferDialog));
        assert_eq!(vm.current_offer(), None);
    }

    #[test]
    fn concurrent_offers_queue_and_reject_leaves_batch_failed_not_badge() {
        let mut vm = ready_vm();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        vm.handle_line(&format!(
            "INCOMING\t{}",
            serde_json::to_string(&manifest(a, "甲", 1)).unwrap()
        ));
        vm.handle_line(&format!(
            "INCOMING\t{}",
            serde_json::to_string(&manifest(b, "乙", 2)).unwrap()
        ));
        // 第二份不重复弹窗，只在队列里排队。
        assert_eq!(vm.current_offer().unwrap().sender_name, "甲");
        let actions = vm.offer_reject("不想要");
        assert!(actions.contains(&ShareAction::Send(format!("REJECT\t{a}\t不想要"))));
        // 队列还有乙 → 弹窗不关。
        assert!(!actions.contains(&ShareAction::CloseOfferDialog));
        assert_eq!(vm.current_offer().unwrap().sender_name, "乙");
    }

    #[test]
    fn staged_then_import_completed_writes_ack() {
        let mut vm = ready_vm();
        let batch = Uuid::new_v4();
        let json = serde_json::to_string(&manifest(batch, "小李", 1)).unwrap();
        vm.handle_line(&format!("INCOMING\t{json}"));
        vm.offer_accept();
        let actions = vm.handle_line(&format!("STAGED\t{batch}\t[\"C:\\\\inbox\\\\a.png\"]"));
        match &actions[0] {
            ShareAction::ImportStaged { batch: b, paths } => {
                assert_eq!(*b, batch);
                assert_eq!(paths[0], PathBuf::from(r"C:\inbox\a.png"));
            }
            other => panic!("{other:?}"),
        }
        vm.import_completed(batch, true, "");
        assert_eq!(vm.take_pending_writes(), vec![format!("ACK\t{batch}")]);
        vm.import_completed(batch, false, "用户取消归类");
        assert_eq!(
            vm.take_pending_writes(),
            vec![format!("NACK\t{batch}\t用户取消归类")]
        );
    }

    #[test]
    fn final_batches_evict_oldest_first() {
        let mut vm = ready_vm();
        vm.handle_line(&device_line());
        vm.open_picker();
        vm.select_device(0);
        let mut last_asset = Uuid::new_v4();
        for _ in 0..(FINAL_KEEP + 3) {
            last_asset = Uuid::new_v4();
            vm.start_share(vec![item(last_asset)]);
            let batch = vm.batches().last().unwrap().id;
            vm.handle_line(&format!("done\t{batch}\tdelivered\t"));
        }
        assert_eq!(
            vm.batches().iter().filter(|b| b.finished()).count(),
            FINAL_KEEP
        );
        // 角标生命周期长于批次记录：旧批次被 evict 不得回滚角标。
        assert_eq!(vm.badge(last_asset), Some(ShareBadge::Done));
    }

    #[test]
    fn scan_lifecycle_and_notice_routing() {
        let mut vm = ready_vm();
        let actions = vm.start_scan();
        assert!(vm.is_scanning());
        assert!(actions.contains(&ShareAction::Send("SCAN".into())));
        // SCAN_DONE 事件收口扫描态（不靠定时器）。
        let actions = vm.handle_line("SCAN_DONE");
        assert!(!vm.is_scanning());
        assert!(actions.contains(&ShareAction::Refresh));
        let actions = vm.handle_line("NOTICE\t警示：mDNS 服务不可用");
        assert!(matches!(
            actions[0],
            ShareAction::Notice(TargetNoticeTone::Warning, _)
        ));
    }

    #[test]
    fn unknown_line_warns_but_ready_gate_blocks_share() {
        let mut vm = ShareVm::new(); // 没 READY
        let actions = vm.handle_line("GIBBERISH");
        assert!(matches!(
            actions[0],
            ShareAction::Notice(TargetNoticeTone::Warning, _)
        ));
        vm.handle_line(&device_line());
        vm.open_picker();
        vm.select_device(0);
        let actions = vm.start_share(vec![item(Uuid::new_v4())]);
        // 未就绪 → 明确报错，不发 SEND。
        assert!(!matches!(&actions[0], ShareAction::Send(_)));
        assert!(matches!(
            &actions[0],
            ShareAction::Notice(TargetNoticeTone::Error, _)
        ));
    }

    #[test]
    fn human_size_formatting() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(1536 * 1024), "1.5 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
