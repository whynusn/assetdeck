//! 共享状态机 VM（D80 M0）：设备表、在途批次、入口报价确认、角标派生。
//!
//! 纯逻辑零 IO：worker 子进程的生命周期与管道归 app-ui，本模块只吃
//! [`WorkerEvent`]（stdout 行解析产物）、吐 [`ShareAction`]（app-ui 要执行的
//! 副作用：往 worker 写行、开弹窗、喂导入管线、toast）。状态不设第二真源：
//! 角标/进度/确认队列全部由事件流推导。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use uuid::Uuid;

use share::{
    DeviceEntry, DoneState, GroupRoster, PeerSyncState, RosterMember, SendItem, SendRequest,
    ShareDirection, ShareOffer, SyncBookState, SyncMessage, WorkerEvent,
};

use crate::share_control::ShareControlVm;
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
    /// 弹索取审批框（发现面 Request 到达）。审批后的投递走 M0 直发，
    /// 索取本身不自动投递任何字节。
    OpenRequestDialog,
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
    /// 把控制面注册表整册推给 worker（SYNC_STATE；引擎册整体替换）。
    /// READY 时发一次，之后随控制面变更（app-ui 侧）重推。
    PushSyncState,
    /// 把一台设备写进控制面设备册（M2 邀请流的互认入册：贴码方对签发方、
    /// Roster 全体成员、JoinInvite 请求方——app-ui 侧执行 control.pair）。
    PairDevice {
        id: String,
        name: String,
    },
    /// 邀请加入请求到达（M2，签发方视角）：自动互认入册 +（群组）写成员
    /// 册并广播名册。domain_id None = 纯互认。
    JoinRequested {
        peer_id: String,
        device_name: String,
        domain_id: Option<Uuid>,
    },
    /// 群主名册到达（M2，成员视角）：app-ui 侧互认全体成员 + 本地建域/
    /// 更新/移出（名册不含自己 = 被踢出信号）。
    RosterReceived(GroupRoster),
}

/// 入口报价（待确认的来件）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOffer {
    pub batch_id: Uuid,
    pub sender_name: String,
    pub items_total: usize,
    pub total_bytes: u64,
}

/// 索取审批（待处理的来件索取）。素材名由 UI 层经 catalog 解析后展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingRequest {
    pub peer_id: String,
    pub asset_uuid: Uuid,
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
    /// 待审批索取队列（并发索取按先到先弹）。
    requests: Vec<PendingRequest>,
    batches: Vec<ShareBatch>,
    /// 角标真源：资产 uuid → 最近共享状态。批次的在途/终态每次迁移都
    /// 写这里（含 Busy），badge() 查询 O(1)；批次被 evict 不回滚角标
    /// （角标语义 = 「最近一次共享结果」，生命周期长于批次记录）。
    badge_overrides: HashMap<Uuid, ShareBadge>,
    /// import_completed 攒下的待写行（导入回调不在事件循环里，app-ui 下轮取走）。
    pending_writes: Vec<String>,
    /// 已完结批次保留最近这么多条供摘要行/记录回放（超出静默丢弃旧的）。
    final_keep: usize,
    /// 远端可得视图（M1-b 发现面数据源）：对端 id → 该对端当前共享给我的
    /// 可得清单。会话期有效（重启后靠发现面/扫描触发补拉重建），只进不推——
    /// 本机共享态的真源在 ShareControlVm，这里只存「别人给我的视角」。
    peer_views: BTreeMap<String, PeerSyncState>,
    /// 已配对对端集（app-ui 随配对册变化调用 set_trusted_peers）。
    /// 未配对对端的同步报文一律丢弃（fail-closed：陌生人发来的「可得清单」
    /// 绝不进发现面）。
    trusted_peers: HashSet<String>,
    /// 未决邀请加入（M2）：已发 JoinInvite 的签发方 id。其名册到达是引导
    /// 握手的应答——信任收口只放行「未决加入的对端或已配对对端」。
    pending_joins: Vec<String>,
    /// 未读可得数（M2 共享中心角标）：对端视角新增的可得项数。打开发现
    /// 分区（mark_offers_seen）即清零；「有新内容可取」≠自动投递素材，
    /// 不变不变量。
    unseen_offers: usize,
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
            requests: Vec::new(),
            batches: Vec::new(),
            badge_overrides: HashMap::new(),
            pending_writes: Vec::new(),
            final_keep: FINAL_KEEP,
            peer_views: BTreeMap::new(),
            trusted_peers: HashSet::new(),
            pending_joins: Vec::new(),
            unseen_offers: 0,
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

    /// 待审批索取（先到先弹）。
    pub fn current_request(&self) -> Option<&PendingRequest> {
        self.requests.first()
    }

    /// 索取裁决完成（审批发送或拒绝）后从队列移除；返回是否真有队列项。
    pub fn request_dismiss(&mut self) -> bool {
        if self.requests.is_empty() {
            return false;
        }
        self.requests.remove(0);
        true
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
        self.direct_send(&device.id, items)
    }

    /// 按设备 id 直发（M0 推送通道）。右键共享与发现面索取审批后的投递
    /// 共用此路径——两条入口都终止于同一 SEND 行，双确认语义不变。
    pub fn direct_send(&mut self, device_id: &str, items: Vec<SendItem>) -> Vec<ShareAction> {
        let Some(device) = self.devices.iter().find(|d| d.id == device_id).cloned() else {
            return vec![ShareAction::Notice(
                TargetNoticeTone::Warning,
                "设备不在线，稍后再试".into(),
            )];
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

    /// 贴入邀请码（M2 邀请流）：解析（设备/群组按形状自区分）→ 本地互认
    /// 入册（app-ui 侧 PairDevice）+ 外拨签发方发 JoinInvite（纯 id 拨号，
    /// pkarr 地址发现解析直连地址）。群组邀请的名册到达（Roster）前不建域。
    pub fn join_with_invite(&mut self, code: &str) -> Vec<ShareAction> {
        let invite = match share::parse_invite_code(code) {
            Ok(invite) => invite,
            Err(error) => {
                return vec![ShareAction::Notice(
                    TargetNoticeTone::Warning,
                    format!("邀请码不合法：{error}"),
                )]
            }
        };
        if self.self_id.as_deref() == Some(invite.creator_id.as_str()) {
            return vec![ShareAction::Notice(
                TargetNoticeTone::Warning,
                "这是本机自己的邀请码".into(),
            )];
        }
        let self_name = self
            .self_name
            .clone()
            .unwrap_or_else(|| "AssetDeck".to_string());
        let message = SyncMessage::JoinInvite {
            domain_id: invite.domain_id,
            device_name: self_name,
        };
        let Ok(json) = serde_json::to_string(&message) else {
            return vec![ShareAction::Notice(
                TargetNoticeTone::Error,
                "邀请请求构造失败".into(),
            )];
        };
        let prefix = &invite.creator_id[..invite.creator_id.len().min(12)];
        let mut actions = vec![
            ShareAction::PairDevice {
                id: invite.creator_id.clone(),
                name: format!("设备 {prefix}"),
            },
            ShareAction::Send(format!("SYNC_SEND\t{}\t\t{json}", invite.creator_id)),
        ];
        if invite.domain_id.is_some() {
            // 名册引导握手：未决加入标记（Roster 信任收口的应答侧锚点）。
            self.pending_joins.push(invite.creator_id.clone());
            actions.push(ShareAction::Notice(
                TargetNoticeTone::Success,
                "加入请求已发出，等待对方共享中心确认".into(),
            ));
        } else {
            actions.push(ShareAction::Notice(
                TargetNoticeTone::Success,
                "已向对方发起互认，识别完成后可在共享中心看到对方".into(),
            ));
        }
        actions
    }

    /// 发现面索取（M1-b）：向对端发 Request，走同步通道。对端 UI 显式审批
    /// 后才会以 M0 直发投递——索取不自动投递任何字节（素材移动原语不变量）。
    pub fn request_asset(&mut self, peer_id: &str, asset_uuid: Uuid) -> Vec<ShareAction> {
        if !self.trusted_peers.contains(peer_id) {
            return vec![ShareAction::Notice(
                TargetNoticeTone::Warning,
                "对端未配对，不能索取".into(),
            )];
        }
        let Some(device) = self.devices.iter().find(|d| d.id == peer_id) else {
            return vec![ShareAction::Notice(
                TargetNoticeTone::Warning,
                "对端不在线，稍后再试".into(),
            )];
        };
        let json = match serde_json::to_string(&SyncMessage::Request { asset_uuid }) {
            Ok(json) => json,
            Err(e) => {
                return vec![ShareAction::Notice(
                    TargetNoticeTone::Error,
                    format!("索取请求构造失败：{e}"),
                )]
            }
        };
        vec![ShareAction::Send(format!(
            "SYNC_SEND\t{peer_id}\t{}\t{json}",
            device.addrs.join(";")
        ))]
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
                // 通道就绪：把控制面整册推给 worker（引擎册替换），对端
                // 上线拉取才拿得到本机共享态。app-ui 侧的查表闭包在
                // dispatch_share_actions 里组装（VM 零 IO）。
                vec![ShareAction::PushSyncState]
            }
            WorkerEvent::Device(entry) => {
                let id = entry.id.clone();
                let is_self = self.self_id.as_deref() == Some(id.as_str());
                self.merge_device(entry);
                let mut actions = vec![ShareAction::Refresh];
                // M1-b：命中已配对且尚无视图的对端 → 立即补拉全量（事件驱动，
                // 每会话每对端至多一次；之后靠 Delta 推送与发现面刷新保鲜）。
                if !is_self
                    && self.trusted_peers.contains(&id)
                    && !self.peer_views.contains_key(&id)
                {
                    if let Some(line) = self.pull_line(&id, 0) {
                        actions.push(ShareAction::Send(line));
                    }
                }
                actions
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
            WorkerEvent::SyncRecv { peer_id, message } => self.apply_sync(peer_id, message),
            WorkerEvent::SyncSent {
                peer_id,
                ok,
                detail,
            } => {
                if ok {
                    // 成功静默：同步是后台事件，不抢焦点。
                    vec![]
                } else {
                    let short = &peer_id[..peer_id.len().min(12)];
                    vec![ShareAction::Notice(
                        TargetNoticeTone::Warning,
                        format!("警示：与 {short}… 的共享态同步失败：{detail}"),
                    )]
                }
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

    /// 同步通道信任册更新（M1-b；app-ui 随配对册变化调用，整表替换）。
    pub fn set_trusted_peers(&mut self, ids: Vec<String>) {
        self.trusted_peers = ids.into_iter().collect();
    }

    /// 远端可得视图（M1-b 发现面数据源，只读）：对端 id → 它当前共享给
    /// 本机的可得清单。
    pub fn peer_views(&self) -> &BTreeMap<String, PeerSyncState> {
        &self.peer_views
    }

    /// 未读可得数（M2 共享中心角标）：对端视角新增的可得项累计。
    pub fn unseen_offers(&self) -> usize {
        self.unseen_offers
    }

    /// 打开发现分区/共享中心：未读清零（用户已看过）。
    pub fn mark_offers_seen(&mut self) {
        self.unseen_offers = 0;
    }

    /// 未决加入数（M2）：贴码后等待对端名册的请求条数。
    pub fn pending_join_count(&self) -> usize {
        self.pending_joins.len()
    }

    /// 发现面打开：对已知地址的已配对对端全部补拉一轮（用户显式动作驱动，
    /// 非定时器）。无地址的对端跳过（等下次 DEVICE 命中再拉）。
    pub fn refresh_peer_views(&mut self) -> Vec<ShareAction> {
        let ids: Vec<String> = self
            .devices
            .iter()
            .filter(|d| self.trusted_peers.contains(&d.id))
            .map(|d| d.id.clone())
            .collect();
        ids.iter()
            .filter_map(|id| {
                let since = self.peer_views.get(id).map(|v| v.rev).unwrap_or(0);
                self.pull_line(id, since).map(ShareAction::Send)
            })
            .collect()
    }

    // ===== 内部 =====

    /// 应用一条来自对端的同步报文（M1-b 发现面数据流 / M2 邀请流）。
    /// 信任收口：未配对对端的可得性/索取报文一律丢弃，绝不进视图；
    /// **豁免**：JoinInvite 是邀请流的引导报文（发送方尚未入册，持有
    /// domain id 即持有邀请码）；Roster 只放行「未决 JoinInvite 的对端
    /// （引导握手应答）或已配对对端（成员变更广播）」。
    fn apply_sync(&mut self, peer_id: String, message: SyncMessage) -> Vec<ShareAction> {
        match message {
            SyncMessage::JoinInvite {
                domain_id,
                device_name,
            } => {
                vec![
                    ShareAction::JoinRequested {
                        peer_id,
                        device_name,
                        domain_id,
                    },
                    ShareAction::Refresh,
                ]
            }
            SyncMessage::Roster { roster } => {
                let pending = self.pending_joins.iter().position(|id| *id == peer_id);
                if pending.is_none() && !self.trusted_peers.contains(&peer_id) {
                    return Vec::new();
                }
                if let Some(index) = pending {
                    self.pending_joins.remove(index);
                }
                vec![ShareAction::RosterReceived(roster), ShareAction::Refresh]
            }
            _ => {
                if !self.trusted_peers.contains(&peer_id) {
                    return Vec::new();
                }
                match message {
                    // PULL 由 worker 在引擎内应答，不该到 UI：防御性忽略。
                    SyncMessage::Pull { .. } => Vec::new(),
                    SyncMessage::Request { asset_uuid } => {
                        // 索取审批队列：并发索取按先到先弹。信任收口已在
                        // 函数内完成（未配对对端的索取绝不进审批框）。
                        let first = self.requests.is_empty();
                        self.requests.push(PendingRequest {
                            peer_id,
                            asset_uuid,
                        });
                        let mut actions = Vec::new();
                        if first {
                            actions.push(ShareAction::OpenRequestDialog);
                        }
                        actions.push(ShareAction::Refresh);
                        actions
                    }
                    SyncMessage::Snapshot { rev, offers } => {
                        // M2 角标：视角里新增的可得项计未读（打开发现分区
                        // mark_offers_seen 即清；重放/未变的 Snapshot 不计）。
                        let new_count = match self.peer_views.get(&peer_id) {
                            Some(view) => offers
                                .iter()
                                .filter(|o| {
                                    !view.offers.iter().any(|c| c.asset_uuid == o.asset_uuid)
                                })
                                .count(),
                            None => offers.len(),
                        };
                        self.unseen_offers += new_count;
                        self.peer_views
                            .insert(peer_id, PeerSyncState { rev, offers });
                        vec![ShareAction::Refresh]
                    }
                    SyncMessage::Delta {
                        from_rev,
                        to_rev,
                        added,
                        removed,
                    } => self.apply_delta(peer_id, from_rev, to_rev, added, removed),
                    SyncMessage::JoinInvite { .. } | SyncMessage::Roster { .. } => Vec::new(),
                }
            }
        }
    }

    /// Delta 三分支（rev 为发送方全局单调计数，逐动作 +1）：
    /// - 无视图且 from_rev == 0：发送方全新状态，直接立册；
    /// - to_rev ≤ 缓存：重放，丢弃；
    /// - from_rev > 缓存：断档（漏收过推送），原位套用会造成混合旧态，
    ///   丢弃并按缓存 rev 补拉全量（事件驱动自愈，不重试推送）。
    fn apply_delta(
        &mut self,
        peer_id: String,
        from_rev: u64,
        to_rev: u64,
        added: Vec<share::ShareOffer>,
        removed: Vec<Uuid>,
    ) -> Vec<ShareAction> {
        let Some(view) = self.peer_views.get(&peer_id) else {
            if from_rev == 0 {
                self.unseen_offers += added.len();
                self.peer_views.insert(
                    peer_id,
                    PeerSyncState {
                        rev: to_rev,
                        offers: added,
                    },
                );
                return vec![ShareAction::Refresh];
            }
            return self
                .pull_line(&peer_id, 0)
                .map(ShareAction::Send)
                .into_iter()
                .collect();
        };
        let cached = view.rev;
        if to_rev <= cached {
            return Vec::new();
        }
        if from_rev > cached {
            return self
                .pull_line(&peer_id, cached)
                .map(ShareAction::Send)
                .into_iter()
                .collect();
        }
        let view = self.peer_views.get_mut(&peer_id).unwrap();
        view.offers.retain(|o| !removed.contains(&o.asset_uuid));
        for offer in added {
            let is_new = !view.offers.iter().any(|o| o.asset_uuid == offer.asset_uuid);
            if is_new {
                self.unseen_offers += 1;
            }
            view.offers.retain(|o| o.asset_uuid != offer.asset_uuid);
            view.offers.push(offer);
        }
        view.rev = to_rev;
        vec![ShareAction::Refresh]
    }

    /// 组一条 SYNC_SEND 补拉行；对端尚无已知地址时为 None（等下次 DEVICE）。
    fn pull_line(&self, peer_id: &str, since_rev: u64) -> Option<String> {
        let device = self.devices.iter().find(|d| d.id == peer_id)?;
        let json = serde_json::to_string(&SyncMessage::Pull { since_rev }).ok()?;
        Some(format!(
            "SYNC_SEND\t{peer_id}\t{}\t{json}",
            device.addrs.join(";")
        ))
    }
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

/// M2 群主侧名册装配：域成员 + 群主自身（群内互享语义成立——成员本地域
/// 含群主，向域共享即全群可达）。成员名取设备册备注名（缺席留空，接收方
/// 回落 uuid 前缀）。域不存在返回 None。
pub fn group_roster_of(
    control: &ShareControlVm,
    domain_id: Uuid,
    self_id: &str,
    self_name: &str,
) -> Option<GroupRoster> {
    let domain = control.domains().iter().find(|d| d.id == domain_id)?;
    let mut members: Vec<RosterMember> = domain
        .members
        .iter()
        .map(|id| RosterMember {
            id: id.clone(),
            name: control
                .devices()
                .get(id)
                .map(|d| d.name.clone())
                .unwrap_or_default(),
        })
        .collect();
    members.push(RosterMember {
        id: self_id.to_string(),
        name: self_name.to_string(),
    });
    Some(GroupRoster {
        domain_id,
        domain_name: domain.name.clone(),
        members,
    })
}

/// M2 名册广播行：发给 recipients ∩ 本会话已发现的设备（离线跳过——
/// 下次成员变更广播 / 对端拉取自愈）。
pub fn roster_send_lines(
    devices: &[DeviceEntry],
    roster: &GroupRoster,
    recipients: &[String],
) -> Vec<String> {
    let Ok(json) = serde_json::to_string(&SyncMessage::Roster {
        roster: roster.clone(),
    }) else {
        return Vec::new();
    };
    recipients
        .iter()
        .filter_map(|id| {
            let device = devices.iter().find(|d| &d.id == id)?;
            Some(format!(
                "SYNC_SEND\t{}\t{}\t{json}",
                device.id,
                device.addrs.join(";")
            ))
        })
        .collect()
}

/// D80-M1-b SYNC_STATE 行：控制面整册按设备过滤后的引擎册载荷（worker
/// 整体替换，应答 Pull 的唯一来源）。序列化失败返回空行，调用方丢弃。
/// lookup 按引用传（闭包捕获目录句柄，不可 Copy；快照与 Delta 复用同一份）。
pub fn sync_state_line(
    control: &ShareControlVm,
    lookup: &impl Fn(Uuid) -> Option<ShareOffer>,
) -> String {
    let mut peers = BTreeMap::new();
    for device in control.devices().iter() {
        let (rev, offers) = control.snapshot_offers(&device.id, lookup);
        peers.insert(device.id.clone(), PeerSyncState { rev, offers });
    }
    let state = SyncBookState { peers };
    match serde_json::to_string(&state) {
        Ok(json) => format!("SYNC_STATE\t{json}"),
        Err(_) => String::new(),
    }
}

/// D80-M1-b 逐动作 Delta 行：逐接收方 diff before/after 可见集组 Delta，
/// 只发给本会话已发现的设备（`devices` 里没有的已配对设备 = 离线，跳过——
/// 断档守卫让下次 Delta / 补拉自愈）。rev 未推进（如成员册调整，不动 rev）
/// 返回空——接收方经 Pull / 发现面打开全量补拉。
pub fn delta_sync_lines(
    control: &ShareControlVm,
    devices: &[DeviceEntry],
    prev_rev: u64,
    before: &[(String, Vec<Uuid>)],
    lookup: &impl Fn(Uuid) -> Option<ShareOffer>,
) -> Vec<String> {
    let new_rev = control.rev();
    if new_rev == prev_rev {
        return Vec::new();
    }
    let mut lines = Vec::new();
    for (device_id, prev) in before {
        // 动作期间被解配 / 本会话未发现的设备：统一跳过（信任已收口或
        // 无地址可发），靠断档守卫自愈。
        let Some(device) = devices.iter().find(|d| d.id == *device_id) else {
            continue;
        };
        let after: HashSet<Uuid> = control.visible_to(device_id).into_iter().collect();
        let prev_set: HashSet<Uuid> = prev.iter().copied().collect();
        let added: Vec<ShareOffer> = after
            .difference(&prev_set)
            .copied()
            .filter_map(lookup)
            .collect();
        let removed: Vec<Uuid> = prev_set.difference(&after).copied().collect();
        if added.is_empty() && removed.is_empty() {
            continue;
        }
        let message = SyncMessage::Delta {
            from_rev: prev_rev,
            to_rev: new_rev,
            added,
            removed,
        };
        let Ok(json) = serde_json::to_string(&message) else {
            continue;
        };
        lines.push(format!(
            "SYNC_SEND\t{}\t{}\t{json}",
            device_id,
            device.addrs.join(";")
        ));
    }
    lines
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

    fn sync_snapshot_line(peer: &str, rev: u64, names: &[&str]) -> String {
        let snapshot = SyncMessage::Snapshot {
            rev,
            offers: names
                .iter()
                .map(|n| share::ShareOffer {
                    asset_uuid: Uuid::new_v4(),
                    file_name: n.to_string(),
                    kind: AssetKind::Image,
                    size_bytes: 8,
                })
                .collect(),
        };
        format!(
            "SYNC_RECV\t{peer}\t{}",
            serde_json::to_string(&snapshot).unwrap()
        )
    }

    #[test]
    fn sync_only_trusted_peers_enter_views() {
        let mut vm = ready_vm();
        let peer = "d".repeat(52);
        let line = sync_snapshot_line(&peer, 4, &["a.png"]);
        // 未配对（未进信任册）：快照被丢弃，绝不进发现面。
        let actions = vm.handle_line(&line);
        assert!(actions.is_empty());
        assert!(vm.peer_views().get(&peer).is_none());
        // 进信任册后：快照立册。
        vm.set_trusted_peers(vec![peer.clone()]);
        let actions = vm.handle_line(&line);
        assert_eq!(vm.peer_views().get(&peer).unwrap().rev, 4);
        assert!(actions.contains(&ShareAction::Refresh));
    }

    #[test]
    fn delta_apply_replay_and_gap_branches() {
        let mut vm = ready_vm();
        let peer = "d".repeat(52);
        vm.set_trusted_peers(vec![peer.clone()]);
        vm.handle_line(&device_line()); // 提供地址，补拉才组得出行

        // 无视图 + from_rev=0：发送方全新状态直接立册。
        let added = share::ShareOffer {
            asset_uuid: Uuid::new_v4(),
            file_name: "x.png".into(),
            kind: AssetKind::Image,
            size_bytes: 1,
        };
        let delta = SyncMessage::Delta {
            from_rev: 0,
            to_rev: 2,
            added: vec![added.clone()],
            removed: vec![],
        };
        vm.handle_line(&format!(
            "SYNC_RECV\t{peer}\t{}",
            serde_json::to_string(&delta).unwrap()
        ));
        assert_eq!(vm.peer_views().get(&peer).unwrap().rev, 2);

        // from_rev == 缓存：原位套用（同 uuid 替换而非追加）。
        let replacement = share::ShareOffer {
            asset_uuid: added.asset_uuid,
            file_name: "x2.png".into(),
            kind: AssetKind::Image,
            size_bytes: 2,
        };
        let other = share::ShareOffer {
            asset_uuid: Uuid::new_v4(),
            file_name: "y.png".into(),
            kind: AssetKind::Image,
            size_bytes: 3,
        };
        let delta = SyncMessage::Delta {
            from_rev: 2,
            to_rev: 5,
            added: vec![replacement, other],
            removed: vec![],
        };
        let line = format!(
            "SYNC_RECV\t{peer}\t{}",
            serde_json::to_string(&delta).unwrap()
        );
        vm.handle_line(&line);
        let view = vm.peer_views().get(&peer).unwrap();
        assert_eq!(view.rev, 5);
        assert_eq!(view.offers.len(), 2);
        assert!(view.offers.iter().any(|o| o.file_name == "x2.png"));

        // 重放（to_rev ≤ 缓存）：丢弃且无动作。
        let actions = vm.handle_line(&line);
        assert!(actions.is_empty());

        // 断档（from_rev > 缓存）：丢弃并按缓存 rev 补拉全量。
        let delta = SyncMessage::Delta {
            from_rev: 8,
            to_rev: 9,
            added: vec![],
            removed: vec![added.asset_uuid],
        };
        let actions = vm.handle_line(&format!(
            "SYNC_RECV\t{peer}\t{}",
            serde_json::to_string(&delta).unwrap()
        ));
        match &actions[0] {
            ShareAction::Send(l) => {
                assert!(l.starts_with(&format!("SYNC_SEND\t{peer}\t")));
                assert!(l.contains(r#""kind":"pull""#));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn device_discovery_triggers_initial_pull_once() {
        let mut vm = ready_vm();
        let peer = "d".repeat(52);
        vm.set_trusted_peers(vec![peer.clone()]);
        let actions = vm.handle_line(&device_line());
        assert!(actions.iter().any(
            |a| matches!(a, ShareAction::Send(l) if l.starts_with("SYNC_SEND\t") && l.contains(r#""kind":"pull""#))
        ));
        // 已有视图后再次命中：不再补拉。
        vm.handle_line(&sync_snapshot_line(&peer, 1, &["a.png"]));
        let actions = vm.handle_line(&device_line());
        assert!(!actions.iter().any(|a| matches!(a, ShareAction::Send(_))));
    }

    #[test]
    fn refresh_peer_views_pulls_all_trusted_devices() {
        let mut vm = ready_vm();
        let peer = "d".repeat(52);
        let stranger = "e".repeat(52);
        vm.handle_line(&device_line());
        vm.handle_line(&format!("DEVICE\t{stranger}\t小王\t10.0.0.2:1"));
        vm.set_trusted_peers(vec![peer.clone()]);
        let actions = vm.refresh_peer_views();
        assert_eq!(actions.len(), 1, "只拉已配对对端");
        match &actions[0] {
            ShareAction::Send(l) => assert!(l.starts_with(&format!("SYNC_SEND\t{peer}\t"))),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sync_sent_failure_notifies_only_on_failure() {
        let mut vm = ready_vm();
        let peer = "d".repeat(52);
        let actions = vm.handle_line(&format!("SYNC_SENT\t{peer}\tok\t"));
        assert!(actions.is_empty());
        let actions = vm.handle_line(&format!("SYNC_SENT\t{peer}\tfailed\t连接超时"));
        assert!(matches!(
            &actions[0],
            ShareAction::Notice(TargetNoticeTone::Warning, t) if t.contains("连接超时")
        ));
    }

    #[test]
    fn request_asset_builds_sync_send_line() {
        let mut vm = ready_vm();
        let peer = "d".repeat(52);
        vm.handle_line(&device_line());
        let asset = Uuid::new_v4();
        // 未配对：不发线，明确提示。
        let actions = vm.request_asset(&peer, asset);
        assert!(matches!(
            &actions[0],
            ShareAction::Notice(TargetNoticeTone::Warning, _)
        ));
        vm.set_trusted_peers(vec![peer.clone()]);
        let actions = vm.request_asset(&peer, asset);
        match &actions[0] {
            ShareAction::Send(l) => {
                assert!(l.starts_with(&format!("SYNC_SEND\t{peer}\t")));
                assert!(l.contains(r#""kind":"request""#));
                assert!(l.contains(&asset.to_string()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn incoming_request_queues_and_direct_send_by_id() {
        let mut vm = ready_vm();
        let peer = "d".repeat(52);
        vm.set_trusted_peers(vec![peer.clone()]);
        vm.handle_line(&device_line());
        let asset = Uuid::new_v4();
        let request = SyncMessage::Request { asset_uuid: asset };
        let actions = vm.handle_line(&format!(
            "SYNC_RECV\t{peer}\t{}",
            serde_json::to_string(&request).unwrap()
        ));
        assert!(actions.contains(&ShareAction::OpenRequestDialog));
        assert_eq!(vm.current_request().unwrap().peer_id, peer);
        assert_eq!(vm.current_request().unwrap().asset_uuid, asset);

        // 审批发送：按 id 直发（不经设备选择弹窗），双确认语义不变。
        let actions = vm.direct_send(&peer, vec![item(asset)]);
        assert!(matches!(&actions[0], ShareAction::Send(l) if l.starts_with("SEND\t")));
        assert_eq!(vm.badge(asset), Some(ShareBadge::Busy));
        assert!(vm.request_dismiss());
        assert!(vm.current_request().is_none());
        assert!(!vm.request_dismiss(), "空队列 dismiss 返回 false");
    }

    #[test]
    fn sync_state_line_and_delta_lines_are_receiver_filtered() {
        use crate::share_control::ShareControlVm;
        let mut control = ShareControlVm::new();
        let peer = "d".repeat(52);
        control.pair(&peer, "小李", 1).unwrap();
        let domain = control
            .create_domain("家庭组", share::DomainKind::Group, vec![peer.clone()])
            .unwrap();
        let asset = Uuid::new_v4();
        control.mark_shared(asset, domain, 1).unwrap();
        let prev_rev = control.rev();

        let lookup = |uuid: Uuid| -> Option<ShareOffer> {
            (uuid == asset).then(|| ShareOffer {
                asset_uuid: uuid,
                file_name: "a.png".into(),
                kind: AssetKind::Image,
                size_bytes: 8,
            })
        };

        // SYNC_STATE：按设备过滤的整册，行首是协议头。
        let line = super::sync_state_line(&control, &lookup);
        assert!(line.starts_with("SYNC_STATE\t"));
        assert!(line.contains("a.png"));

        // 动作前基线：设备可见集为 [asset]；再标一条 → 对端视角 +1 新增。
        let before = vec![(peer.clone(), control.visible_to(&peer))];
        let asset2 = Uuid::new_v4();
        control.mark_shared(asset2, domain, 2).unwrap();
        let devices = vec![DeviceEntry {
            id: peer.clone(),
            name: "小李".into(),
            addrs: vec!["192.168.1.8:4433".into()],
        }];
        // 两条素材都能物化成可得行（库里真实存在的语义）。
        let lookup = |uuid: Uuid| -> Option<ShareOffer> {
            (uuid == asset || uuid == asset2).then(|| ShareOffer {
                asset_uuid: uuid,
                file_name: "a.png".into(),
                kind: AssetKind::Image,
                size_bytes: 8,
            })
        };
        let lines = super::delta_sync_lines(&control, &devices, prev_rev, &before, &lookup);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with(&format!("SYNC_SEND\t{peer}\t192.168.1.8:4433")));
        assert!(lines[0].contains(r#""kind":"delta""#));
        assert!(lines[0].contains(&asset2.to_string()));

        // rev 未推进（幂等 mark）不发 Delta。
        assert!(!control.mark_shared(asset, domain, 3).unwrap());
        let lines = super::delta_sync_lines(&control, &devices, control.rev(), &before, &lookup);
        assert!(lines.is_empty());

        // 离线设备（不在 devices）跳过——断档守卫自愈。
        let lines = super::delta_sync_lines(&control, &[], prev_rev, &before, &lookup);
        assert!(lines.is_empty());
    }

    #[test]
    fn join_with_invite_device_and_group_forms() {
        let mut vm = ready_vm();
        let creator = "d".repeat(52);

        // 设备邀请码：PairDevice（互认占位名）+ JoinInvite（domain None）+ 无未决。
        let actions = vm.join_with_invite(&creator);
        assert!(matches!(
            &actions[0],
            ShareAction::PairDevice { id, .. } if *id == creator
        ));
        match &actions[1] {
            ShareAction::Send(l) => {
                assert!(l.starts_with(&format!("SYNC_SEND\t{creator}\t")));
                assert!(l.contains(r#""kind":"join_invite""#));
                assert!(l.contains(r#""domain_id":null"#));
            }
            other => panic!("{other:?}"),
        }
        assert!(vm.pending_join_count() == 0);

        // 自己的邀请码 → 拒绝。
        let self_id = vm.self_id().unwrap().to_string();
        let actions = vm.join_with_invite(&self_id);
        assert!(matches!(
            &actions[0],
            ShareAction::Notice(TargetNoticeTone::Warning, _)
        ));

        // 群组邀请码：标记未决（名册引导握手）。
        let domain_id = Uuid::new_v4();
        let code = share::group_invite_code(&creator, domain_id);
        let actions = vm.join_with_invite(&code);
        assert!(vm.pending_join_count() == 1);
        match &actions[1] {
            ShareAction::Send(l) => assert!(l.contains(r#""kind":"join_invite""#)),
            other => panic!("{other:?}"),
        }

        // 坏码 → 明确提示，不发线。
        let actions = vm.join_with_invite("short");
        assert!(matches!(
            &actions[0],
            ShareAction::Notice(TargetNoticeTone::Warning, _)
        ));
        assert!(vm.pending_join_count() == 1, "坏码不产生未决加入");
    }

    #[test]
    fn roster_trust_gate_pending_and_paired() {
        let mut vm = ready_vm();
        let creator = "d".repeat(52);
        let roster = share::GroupRoster {
            domain_id: Uuid::new_v4(),
            domain_name: "家庭组".into(),
            members: vec![share::RosterMember {
                id: creator.clone(),
                name: "群主".into(),
            }],
        };

        // 陌生人名册（无未决、未配对）：丢弃。
        let line = format!(
            "SYNC_RECV\t{creator}\t{}",
            serde_json::to_string(&SyncMessage::Roster {
                roster: roster.clone()
            })
            .unwrap()
        );
        assert!(vm.handle_line(&line).is_empty());

        // 贴群组邀请码 → 未决；名册到达 → 放行并消费未决。
        let code = share::group_invite_code(&creator, Uuid::new_v4());
        vm.join_with_invite(&code);
        let actions = vm.handle_line(&line);
        assert!(actions.contains(&ShareAction::RosterReceived(roster.clone())));
        assert!(vm.pending_join_count() == 0, "名册到达消费未决加入");

        // 已配对对端的成员变更广播：信任册放行（无需未决）。
        vm.set_trusted_peers(vec![creator.clone()]);
        let actions = vm.handle_line(&line);
        assert!(actions.contains(&ShareAction::RosterReceived(roster)));
    }

    #[test]
    fn join_invite_bypasses_trust_gate_and_reaches_owner() {
        let mut vm = ready_vm();
        let joiner = "d".repeat(52);
        let domain_id = Uuid::new_v4();
        let message = SyncMessage::JoinInvite {
            domain_id: Some(domain_id),
            device_name: "小李的电脑".into(),
        };
        // 未配对对端的 JoinInvite：引导报文豁免信任门，直达签发方。
        let actions = vm.handle_line(&format!(
            "SYNC_RECV\t{joiner}\t{}",
            serde_json::to_string(&message).unwrap()
        ));
        match &actions[0] {
            ShareAction::JoinRequested {
                peer_id,
                device_name,
                domain_id: d,
            } => {
                assert_eq!(peer_id, &joiner);
                assert_eq!(device_name, "小李的电脑");
                assert_eq!(*d, Some(domain_id));
            }
            other => panic!("{other:?}"),
        }
        // 设备名不安全 → 协议层拒绝（进不了 apply_sync）。
        let bad = SyncMessage::JoinInvite {
            domain_id: None,
            device_name: "bad\tname".into(),
        };
        let parsed = share::parse_worker_line(&format!(
            "SYNC_RECV\t{joiner}\t{}",
            serde_json::to_string(&bad).unwrap()
        ));
        assert!(matches!(parsed, share::WorkerEvent::Unknown(_)));
    }

    #[test]
    fn unseen_offers_count_and_mark_seen() {
        let mut vm = ready_vm();
        let peer = "d".repeat(52);
        vm.set_trusted_peers(vec![peer.clone()]);
        assert_eq!(vm.unseen_offers(), 0);

        // 快照新增 2 项 → 未读 2；同一条行重放不计（视图里已有同 uuid）。
        let line = sync_snapshot_line(&peer, 1, &["a.png", "b.png"]);
        vm.handle_line(&line);
        assert_eq!(vm.unseen_offers(), 2);
        vm.handle_line(&line);
        assert_eq!(vm.unseen_offers(), 2);

        // 打开发现分区 → 清零。
        vm.mark_offers_seen();
        assert_eq!(vm.unseen_offers(), 0);

        // Delta 新增 → 未读 +1（视图里没有的 uuid 才计）。
        let delta = SyncMessage::Delta {
            from_rev: 1,
            to_rev: 2,
            added: vec![share::ShareOffer {
                asset_uuid: Uuid::new_v4(),
                file_name: "c.png".into(),
                kind: AssetKind::Image,
                size_bytes: 1,
            }],
            removed: vec![],
        };
        vm.handle_line(&format!(
            "SYNC_RECV\t{peer}\t{}",
            serde_json::to_string(&delta).unwrap()
        ));
        assert_eq!(vm.unseen_offers(), 1);
    }

    #[test]
    fn roster_helpers_build_and_filter() {
        use crate::share_control::ShareControlVm;
        let mut control = ShareControlVm::new();
        let peer = "d".repeat(52);
        control.pair(&peer, "小李", 1).unwrap();
        let domain = control
            .create_domain("家庭组", share::DomainKind::Group, vec![peer.clone()])
            .unwrap();
        let self_id = "e".repeat(52);

        // 名册装配：域成员 + 群主自身，名字取设备册。
        let roster = super::group_roster_of(&control, domain, &self_id, "本机").unwrap();
        assert_eq!(roster.members.len(), 2);
        assert_eq!(roster.members[0].id, peer);
        assert_eq!(roster.members[0].name, "小李");
        assert_eq!(roster.members[1].id, self_id);
        assert_eq!(roster.members[1].name, "本机");

        // 广播过滤：只有在线（devices 里有的）接收方组行。
        let devices = vec![DeviceEntry {
            id: peer.clone(),
            name: "小李".into(),
            addrs: vec!["192.168.1.8:4433".into()],
        }];
        let offline = "f".repeat(52);
        let lines = super::roster_send_lines(&devices, &roster, &[peer.clone(), offline]);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with(&format!("SYNC_SEND\t{peer}\t192.168.1.8:4433")));
        assert!(lines[0].contains(r#""kind":"roster""#));
    }
}
