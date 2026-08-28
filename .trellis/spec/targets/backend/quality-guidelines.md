# Quality Guidelines — targets

## Scenario: 精确多 IM 目标路由

### 1. Scope / Trigger

- Trigger：新增或修改 profile、窗口匹配、热目标、图钉、托盘重绑、健康等级或目标选择身份。
- 目标：一次操作只能路由到用户明确或可靠追踪到的具体窗口；无法证明时不猜。

### 2. Signatures

```rust
pub fn load_profiles(
    builtin: &str,
    user: Option<&str>,
) -> Result<ProfileSet, ProfileError>;

pub fn resolve_eligible_snapshot(
    profiles: &ProfileSet,
    snapshot: &WindowSnapshot,
) -> Option<ResolvedTarget>;

pub fn resolve_profile_windows(
    profile: &Profile,
    windows: &[WindowSnapshot],
) -> MatchResult;

pub fn matching_profile_windows(
    profile: &Profile,
    windows: &[WindowSnapshot],
) -> Vec<ResolvedTarget>;

impl TargetTracker {
    pub fn on_foreground(&mut self, eligible: Option<TargetBinding>, own_panel: bool);
    pub fn hot(&self) -> Option<&TargetBinding>;
    pub fn pin(&mut self, target: TargetBinding);
    pub fn select_explicit(&mut self, target: TargetBinding);
    pub fn rebind(&mut self, id: &TargetId, target: TargetBinding) -> bool;
    pub fn on_window_gone(&mut self, hwnd: WindowHandle);
}
```

### 3. Contracts

- `TargetId` 与 HWND 分离；HWND 消失不会删除稳定目标身份。
- `TargetBinding.instance_id` 是当前会话级实例线索；同 profile 的另一进程不得自动取代它。
- 自动热目标只接受明确命中内置/用户 profile 的 eligible 窗口。generic fallback 只用于显式捕获与确认。
- 多个 profile 对同一前台窗口最高分并列时，`resolve_eligible_snapshot` 返回 `None`，不得按配置顺序猜测。
- 冷目标选择身份必须包含具体 HWND，例如 `TargetId@HWND`；只传 `TargetId` 无法区分同一 IM 多开。
- 图钉锁定具体 `TargetBinding`。另一个相同 profile 的前台窗口不得覆盖它。
- 自动托盘重绑必须证明是同一稳定目标实例。只有 profile id 时证据不足，不得静默绑定到另一个窗口。
- 同一 TOML 文档重复 profile id 返回 `ProfileError::DuplicateId`；不得覆盖、破坏顺序表或 panic。
- `targets` 不拥有 IO、Win32 实现或时间策略。
- `Health::Green` 只能来自通过且已清场、无 Enter 的 L3 报告；窗口未运行是灰色 `Unknown`，不是红色故障。
- `ReadinessMode::UiaStrict` 是“证明后才注入”的产品档；`UiaShallow` 仍保留探不到可注入的历史语义。
- 候选按「可选择性」淘汰而不是全量罗列：最小化窗口、悬浮条、通知窗、Loading 壳不得入列
  （千牛单进程能开出 20+ 个窗口）。判定只依赖窗口可见性 + 类名画像 + 标题模板，
  不得依赖快捷键或进程枚举顺序（微信 `Ctrl+Alt+W` 会被多实例/千牛抢占）。见 DECISIONS D17。
- `Profile::paste_sends` 声明「哪些格式在该 IM 上粘贴即发送」（千牛 `["files"]`，微信为空）。
  这是画像级事实，不是全局行为；pipeline 据此在未开自动发送时降级为只复制。见 DECISIONS D18。
- `Profile::input_anchor` 是客户区**比例**锚点（`x_ratio` / `y_ratio`），供 platform 层在 UIA 聚焦失败时
  点击输入框。取值必须在 `0.0..=1.0`，越界返回 `ProfileError::InvalidAnchor`，**不静默夹紧**——
  静默夹紧会让错配画像看起来能用，实际点在别的控件上。见 DECISIONS D21。
- `Profile` 因内嵌 f32 比例锚点只实现 `PartialEq`，不实现 `Eq`；`ResolvedTarget` / `MatchResult` 随之同步。

### 4. Validation & Error Matrix

| 条件 | 结果 |
|---|---|
| builtin/user TOML 语法错误 | `ProfileError::Parse` |
| id 缺失或空白 | `ProfileError::MissingId` |
| builtin 缺 label | `ProfileError::MissingLabel` |
| 同一文档重复 id | `ProfileError::DuplicateId` |
| title/not-ready 正则非法 | `ProfileError::InvalidRegex` |
| 未命中任何 profile 的前台窗口 | 自动路径 `None`；显式路径可生成 fallback |
| 最高分 profile 并列 | 自动路径 `None` |
| 同 profile 多个最高分窗口 | `MatchResult::Ambiguous` |
| 图钉 HWND 消失且无法证明重绑对象 | 保留稳定身份、`hwnd=None`，等待显式选择 |
| 同 profile 实例号不同 | 保持休眠，不自动跨实例重绑 |
| L0/L2 失败 | `Health::Red`、不可启用 |
| 窗口未运行 | `Health::Unknown`、不可启用 |
| L2 通过但无 L3 | `Health::Yellow` |
| L3 读回一致、清场完成且无 Enter | `Health::Green` |
| `input_anchor` 越界（不在 0.0..=1.0） | `ProfileError::InvalidAnchor{profile,x,y}` |
| 画像未声明 `input_anchor` | `focus_anchor()` 返回 `None`，聚焦只能靠 UIA |

### 5. Good/Base/Bad Cases

- Good：用户明确选择 `telegram@202`，后续激活和注入都使用 HWND 202，不触碰微信 HWND 101。
- Base：人在微信，随后浏览器/Explorer/本应用成为前台，热目标仍保持微信且无 TTL。
- Bad：微信 A 被固定后关闭，代码仅因微信 B 也使用 profile id `wechat` 就自动把图钉迁到 B。

### 6. Tests Required

- profile：字段级覆盖、非法正则、同文档重复 id。
- matcher：未知窗口 fallback、fallback 不可自动追踪、同分 profile 不猜、同 profile 多窗口歧义。
- tracker：eligible 唯一改写、无关/自身窗口忽略、无 TTL、图钉冻结、窗口消失保留身份。
- 精确实例：选择键包含 HWND；固定窗口不得被同 profile 的另一个窗口替换；无稳定实例证据时自动重绑必须失败或转歧义。
- health：未运行为灰、L2 为黄、只有真实 L3 报告为绿、注入序列无 `0x0D`。
- profile 锚点：解析后可暴露为 `platform::FocusAnchor`；缺省时无点击目标；越界被拒而非夹紧；
  用户画像可覆盖内置锚点。
- 属性测试：任意长度非 eligible 事件序列后热目标恒定。

### 7. Wrong vs Correct

#### Wrong

```rust
// 仅 profile id 相同，无法证明 candidate 是原来的固定窗口。
if candidate.id == pinned.id {
    tracker.rebind(&pinned.id, candidate);
}
```

#### Correct

```rust
match prove_same_target_instance(&pinned, &candidate) {
    Some(instance) => tracker.rebind(instance.id(), candidate),
    None => keep_dormant_or_require_explicit_choice(),
}
```

当前代码尚未提供 `prove_same_target_instance`；在该身份模型落地前，调用方必须选择保守分支。

## Mechanical Guards

```powershell
rg 'windows-sys|windows::' crates/targets
rg 'std::fs|std::io' crates/targets/src
rg 'Instant|SystemTime|Duration' crates/targets/src/tracker.rs
cargo test -p targets
```
