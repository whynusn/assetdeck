# Quality Guidelines — pipeline

## 红线（每条都有对应守卫测试，见 TDD_PLAN M6）

1. **auto-send 默认关**：配置默认值快照测试 `auto_send_flag_defaults_off`；关状态下注入序列**不得含 VK_RETURN**（`auto_send_off_never_synththesizes_enter`）。
2. **先写剪贴板后切焦点**：`paste_writes_clipboard_before_focus_switch`——顺序颠倒会导致粘贴到旧内容。
3. **焦点校验失败降级为仅复制**：mock WindowProvider 返回死窗口断言。
4. **唤起面板时记录前一前台窗口**：`previous_foreground_window_recorded_on_panel_invoke`。
5. 真实 SendInput 测试（`real_sendinput_into_notepad`）必须 `#[ignore]`，仅本地手动跑。

## 平台抽象纪律

- pipeline 只依赖 platform 的 trait（Clipboard/Injector/WindowProvider）；Win32 细节全部在 platform crate。UIPI 风险（管理员窗口收不到普通进程 SendInput）由降级路径兜底，不在 pipeline 特判。

## Code Review 清单

- [ ] 新格式协商项是否走表驱动而非 if-else 链？
- [ ] 是否有任何路径绕过焦点校验直接注入？
- [ ] Enter 合成是否仍在独立开关之后？
