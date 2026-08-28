# Error Handling — targets

## 画像输入

- `load_profiles` 对格式、缺失 id、builtin 缺 label、重复 id 与非法正则返回 `ProfileError`。
- 禁止用默认值吞掉 TOML 语法错误或非法正则。
- 用户 profile 对 builtin 同 id 做字段级覆盖；同一份 user 文档内重复 id 仍是错误。

## 匹配与歧义

- “没有匹配”是正常状态：自动路径返回 `None`，冷目标解析返回 `MatchResult::None`。
- “多个等价候选”必须返回 `MatchResult::Ambiguous` 或保持无热目标，不得取第一个。
- generic fallback 不是错误，但必须带 `fallback=true`，并由上层要求首次确认；它不能自动污染热目标。

## 身份失效

- HWND 消失后只把 `TargetBinding.hwnd` 清为 `None`，保留 `TargetId` 和用户选择语义。
- 无法证明新 HWND 属于同一稳定实例时，保持休眠或要求用户选择；错误绑定比少注入一次更严重。

## 就绪度边界

- `Readiness::NotReady(reason)` 是明确否证；上层不得注入。
- `Readiness::Unknown` 是探测不可用，不等于 `NotReady`；上层可尝试注入但必须返回 `verified=false`。
- `targets` 只定义产品语义，平台探测错误到三态的转换由 `platform`/`pipeline` 完成。
