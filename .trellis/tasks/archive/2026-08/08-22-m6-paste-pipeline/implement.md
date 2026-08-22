# Implement — M6 粘贴管线

## 顺序清单(Red→Green→Refactor)

1. **platform trait 层(协议先行)**
   - [ ] src/lib.rs:ClipboardPayload/WindowHandle/PlatformError/三 trait;零 cfg 零 win32 import。
   - [ ] 红灯→绿灯:`format_negotiation_table_image_video_text` 先行的前提是 pipeline 存在,故本步只保证 platform 编译 + clippy 干净。
2. **pipeline 协商表**
   - [ ] 红灯:`format_negotiation_table_image_video_text`(tests/pipeline_spec.rs):Image→Png、Video→Files(源路径)、Text→Text 三断言 + 未知组合 None。
   - [ ] 绿灯:negotiate.rs 表驱动实现。
3. **配置红线**
   - [ ] 红灯:`auto_send_flag_defaults_off`:serde_json 序列化 `PasteConfig::default()` 与字面量 `{"auto_send":false}` 全等。
   - [ ] 绿灯:Default 实现。
4. **会话与降级**
   - [ ] mock 基建:Op 日志 + MockSink/MockFocus/MockInjector(Arc<Mutex<Vec<Op>>> 共享)。
   - [ ] 红灯:`previous_foreground_window_recorded_on_panel_invoke`(begin_panel 后 previous_foreground == watcher.foreground)。
   - [ ] 红灯:`paste_writes_clipboard_before_focus_switch`(Op 日志:WriteClipboard 下标 < CheckAlive/Inject 下标)。
   - [ ] 红灯:`focus_check_failure_degrades_to_copy_only`(is_alive=false → CopiedOnly 且 Inject 零次)。
   - [ ] 红灯:`auto_send_off_never_synththesizes_enter`(off → 注入序列不含 VK_RETURN=0x0D 且 outcome==Injected;on 对照组含)。
   - [ ] 绿灯:PasteSession::paste 完整编排。
5. **win32 真实实现(cfg(windows))**
   - [ ] Win32Focus/Win32Injector(GetForegroundWindow/IsWindow/SendInput);Win32Clipboard(Text/HDROP 必做,Png 注册格式尽力,Dib 直通字节)。
   - [ ] tests/win32_manual.rs:`real_sendinput_into_notepad` 标 `#[ignore]`,注释「本地手动跑;CI 不跑真实注入」——启动 notepad → 轮询前台 → SendInput 文本键序列 → 断言无 Err(实际输入效果人工确认)。CI 不跑(默认 ignore 即满足)。
6. **收尾验证**

## 验证命令(CI 同序)

```powershell
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace          # ignore 测试自动跳过
```

## 审查门

- 门 1(步骤 2 后):协商表三行全覆盖,表驱动形态(match 结构清晰可扩展),非 if-else 散落。
- 门 2(步骤 4 后):五个红线测试全部基于 Op 日志精确断言(下标比较而非宽松 contains 滥用;VK_RETURN 用 contains 是语义正确的例外)。
- 门 3(全部后):三命令全绿;既有 43 测试零改动通过;platform lib.rs 无 cfg(windows)(grep 自查)。

## 回滚点

- 步骤 1–4(pipeline + traits)独立可合;步骤 5 win32 失败不影响 mock 测试全绿。

## 明确不做(防 scope creep)

- UI 接线(M7)、剪贴板格式嗅探、多 profile 扩展、UIPI 特判(由降级路径天然兜底)。
