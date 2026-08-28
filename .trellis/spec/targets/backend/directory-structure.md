# Directory Structure — targets

## 实际布局

```text
crates/targets/
├── Cargo.toml
└── src/
    ├── lib.rs       # 模块与平台无关类型再导出
    ├── model.rs     # TargetId / TargetBinding / Health / Readiness
    ├── profile.rs   # load_profiles(&str, Option<&str>) 与格式策略
    ├── matcher.rs   # WindowSnapshot -> 匹配、歧义、fallback
    ├── tracker.rs   # 无时间依赖的粘性热目标状态机
    └── health.rs    # 健康等级与 L3 报告判定；当前不是 L3 执行器
```

## 所有权

- `TargetId` 是稳定逻辑身份；`TargetBinding.hwnd` 是可失效的运行时绑定，二者不得合并。
- `WindowSnapshot` 由 `platform` 定义和采集，`targets` 只做纯决策。
- `profile.rs` 只解析调用方传入的 TOML 字符串，不选择路径、不读写文件、不决定升级策略。
- `matcher.rs` 拥有“是否可自动追踪”的判断；UI 不得自行按 exe/title 重写一套匹配逻辑。
- `tracker.rs` 拥有热目标、图钉与窗口消失状态迁移；不得加入 TTL、计时器或平台回调。
- `health.rs` 当前只判定输入报告。真实 L0-L3 探测、哨兵写入/读回/清场由上层编排，完成前不得宣称已具备体检闭环。

## 身份层次

```text
Profile.id                 应用画像身份，例如 wechat
Stable target instance     账号/会话/窗口实例身份，M8 当前尚未完整建模
TargetBinding.hwnd         当前 Win32 窗口句柄，可能因托盘往返而变化
```

同一 IM 多开时，`Profile.id` 不足以证明两个 HWND 属于同一逻辑目标。自动托盘重绑必须依赖稳定实例身份或用户明确选择；否则保持 `hwnd=None` 或返回歧义。

## 禁止

- 在本 crate 引入 `std::fs`、注册表、环境变量或应用配置目录。
- 在 `tracker.rs` 根据“最近几秒”改变热目标。
- 用 profile 列表顺序解决同分歧义。
- 把 generic fallback 当成浏览器、Explorer 等未知窗口的自动热目标。
- 仅凭相同 profile id，把图钉窗口重绑到另一个已存在窗口。
