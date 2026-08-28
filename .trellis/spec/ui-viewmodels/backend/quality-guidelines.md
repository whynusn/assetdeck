# Quality Guidelines — ui-viewmodels

## Scenario: 多 IM 目标条与精确选择

### 1. Scope / Trigger

- 修改目标 chip、picker、图钉、窗口刷新、PasteNotice 或运行时装配时适用。

### 2. Signatures

```rust
impl TargetChoice { pub fn selection_key(&self) -> String; }
impl TargetRoutingVm {
    pub fn on_foreground(&mut self, snapshot: &WindowSnapshot);
    pub fn refresh_windows(&mut self, windows: &[WindowSnapshot]);
    pub fn choose(&mut self, selection_key: &str) -> bool;
    pub fn paste(&self, payload: &AssetPayload<'_>, deps: &mut TargetPipelineDeps<'_>)
        -> TargetPasteNotice;
}
```

### 3. Contracts

- 单一 eligible 热目标自动显示，无需用户再选应用。
- picker 的选择键包含 HWND；同 profile 多窗口必须逐个展示。
- 浏览器/Explorer 等无关前台不改写热目标。
- pinned 锁定具体窗口；另一个同 profile 窗口不能覆盖。
- 自动托盘重绑必须有实例级稳定身份依据。只有 profile id 时不自动猜。
- 实例号相同且当前窗口消失时才允许自动重绑；同 profile 不同进程必须保持休眠并要求显式选择。
- VM 不依赖 Slint 类型；Win32 具体实现应由 app 二进制装配。

### 4. Validation & Error Matrix

| 条件 | UI/VM 结果 |
|---|---|
| 无热目标 | Empty chip，可打开冷目标列表 |
| 单一热目标 | Ready chip，零点击 |
| 同 profile 多窗口 | ChooseTarget，精确 HWND 选择 |
| fallback 首次使用 | NeedsConfirmation |
| pinned HWND 消失且无实例证据 | 休眠/灰色，不自动迁到另一窗口 |
| 隐藏但有面积的同 profile 窗口 | 显示为“隐藏中 · 可选择”，准确选择 HWND |
| `UiaStrict` 探测不到 | 已复制、不注入，提示先打开会话/输入框 |
| `verified=false` | Warning notice，提示确认 |

### 5. Good/Base/Bad Cases

- Good：选择 `telegram@202` 后，VM 交给 pipeline 的 binding 仍是 HWND 202。
- Base：热目标微信后打开浏览器，chip 仍显示微信。
- Bad：固定微信 A 后 A 消失，只因微信 B 是唯一 profile 候选就静默重绑 B。

### 6. Tests Required

- `chip_shows_hot_target_without_user_click`。
- `ambiguous_expands_picker` 与 `same_profile_windows_are_selected_by_unique_window_key`。
- `fallback_target_requires_first_use_confirm`。
- `pin_toggle_freezes_chip` 与固定窗口不被同 profile 其他 HWND 替换。
- 实例身份落地后补“无证据不自动重绑”回归；现有唯一候选重绑测试不能作为 AC3 完整证明。

### 7. Wrong vs Correct

#### Wrong

```rust
let replacement = choices.iter().find(|c| c.binding.id == hot.id);
tracker.rebind(&hot.id, replacement.binding.clone());
```

#### Correct

```rust
match stable_instance_match(&hot, &choices) {
    Exact(replacement) => tracker.rebind(&hot.id, replacement.binding),
    None | Ambiguous(_) => keep_dormant_and_require_choice(),
}
```

## Existing Red Lines

- 禁 slint 依赖，VM 公共行为必须可纯 Rust 测试。
- 100k 浏览数据按可见窗口物化，缩略图缓存走 LRU。
- 禁依赖 media/phash/worker 实现 crate。
- Slint 类型不得渗入 VM 公共签名。

## Scenario: 真实素材物化为剪贴板载荷

### 1. Scope / Trigger

- 修改 `catalog_loader::RealAssetResolver`、`MaterializedAsset` 或任何生成
  `AssetPayload::source_path` 的代码时适用。

### 2. Contracts

- `MaterializedAsset::source_path` **必须是绝对路径**，且指向真实存在的文件。库 root 允许由
  调用方传入相对路径（`--library samples/library`），绝对化的责任在本层。
- `meta.rel_path` 以 `/` 分隔存储，必须逐段 `push` 拼接，不能整串 `join`（会产出
  `root\a/b/c` 这类混合分隔的相对路径）。
- 可内联 `png_bytes` 的有两种：PNG 原图，以及旁挂派生 `objects/<uuid>/paste.png`
  （见 DECISIONS D20，供千牛这类不认 jpg 原字节的 IM 使用）。两者都只是
  `fs::read`，**不解码**，「UI 进程不解码」红线不变。
- 其余素材（mp4/无派生的 jpg/…）靠 `source_path` 走 HDROP。

### 3. Validation & Error Matrix

| 条件 | 结果 |
|---|---|
| 相对库 root + `/` 分隔 rel_path | 绝对且存在的 `source_path` |
| 文件缺失 | `CatalogError::Io`，不返回半成品载荷 |
| 非 PNG 且无旁挂 `paste.png` | `png_bytes` 为空，靠路径上框 |
| 非 PNG 但有旁挂 `paste.png` | 内联该派生文件字节，以 `CF_PNG` 交付 |

### 4. Tests Required

- `tests/asset_payload_spec.rs::materialized_source_path_is_absolute_for_relative_library_root`。
- `tests/asset_payload_spec.rs::video_payload_keeps_absolute_file_path_and_no_inline_bytes`。

> 背景：相对路径写进 CF_HDROP 会被接收方 IM 静默丢弃（DECISIONS D14）。这是一类
> 没有错误码、只能靠测试和截图发现的失败，所以守卫必须留在本层。
