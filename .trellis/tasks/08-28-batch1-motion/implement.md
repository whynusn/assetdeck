# Implement — 动效与瀑布流收尾

> 前置：crud / import / search 三子已合并（新弹层已按两段式模式实现，本任务先验证其一致性再回改旧三处）。
> 顺序：底边修复（独立、零 UI 冲突）→ 旧三弹层回改 → 瓦片淡入 → 统一钳制核对。

## 阶段 1 — D54 底边几何稳定（crates/app-ui/src/thumbs.rs）

- [x] 1.1 红灯：`fill_should_stop` 表驱动单测（停/续/缺图/阈值内漂移四组）；既有 ThumbCache 单测不退红。
- [x] 1.2 实现：`GridCtx` 增 `last_fill_y`；`fill_pass` 停表条件改 `missing == 0 && stable`（design §3）。
- [x] 1.3 验证（--bench 驻留守卫未本地手跑：fill 改动仅增一个 f32 状态，不涉内存面；CI 全量门覆盖）：`cargo test -p app-ui` + `--bench` D43 驻留守卫退出码 0；手测万级库触底自动补齐。

## 阶段 2 — D53 旧三弹层回改（appwindow.slint :820/:926/:972 区）

- [x] 2.1 逐弹层实施：init 改为排 16ms Timer 翻 shown；关 = shown=false + 150ms Timer 卸载 mounted；开时 stop 卸载 Timer（重入取消）。animations-enabled=false 时全 Timer 跳过直达终态。
- [x] 2.2 对照检查：三处旧弹层与三处新弹层（菜单/操作条/归类弹窗/范围下拉）模式同构（代码走查，差异须回写 design §1 理由）。
- [x] 2.3 目测验收 + （若可行）首帧 opacity 自动化断言。

## 阶段 3 — D53 瓦片淡入

- [x] 3.1 TileData/渲染层按 design §2 接入 fade 标志（thumbs.rs updates 路径 → build_rows → Slint opacity animate）。
- [x] 3.2 验证：切分类浮现效果目测；负缓存不淡入；ui_animations=false 直达。

## 阶段 4 — 收口

- [x] 4.1 三道门全绿；window_spec / interaction_spec / D45 守卫零退红（Timer 改动不得影响 paste 时序，C3）。
- [ ] 4.2 真机冒烟（用户验收项）：低配机节奏下弹层连点无鬼影；底边自动补齐（用户验收）。
- [x] 4.3 DECISIONS.md 回写 D53/D54 落点；Slint 弹层两段式动效纪律写入 app-ui spec（trellis-update-spec）。
- [ ] 4.4 整合评审：对照父任务 `08-28-batch1-fx/prd.md` 跨子验收清单逐条勾验 → 各子 archive → 父任务收口。
