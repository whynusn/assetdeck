# Quality Guidelines — library

## 红线（DECISIONS.md D5/D6/D7 落地点）

1. **pHash 先算后拷，去重必做**：重复拖入零磁盘代价（`duplicate_phash_rejected_no_second_copy`）。阈值联动见 `.trellis/spec/phash/backend/quality-guidelines.md`，勿单独改 `DEDUP_THRESHOLD = 8`。
2. **体感瞬时入库**：enqueue 同步完成 解码→pHash→去重→落库，字节拷贝异步。元数据可见先于拷贝完成（`async_copy_metadata_visible_before_copy_done`）。
3. **背压上限**：active ≥ capacity 直接 Backpressure（默认 16，测试用 open_with_capacity 注入小值）。
4. **UI 进程不解码红线在此层的体现**：视频导入只派发 `MediaJob` 给 dispatcher，本 crate 不做任何视频解码；图片解码仅限 pHash 所需的灰度化。
5. **未分类兜底**：category 为 None 时落「待分类」收件箱（INBOX_CATEGORY），禁止静默丢分类。

## 测试要求

- 异步语义必须确定性可测：`set_paused` 钩子或轮询辅助；禁止 sleep 猜时长式断言。
- 新增导入行为 → import_pipeline.rs 加红灯测试先行。

## Code Review 清单

- [ ] 新路径是否绕过了去重检查？
- [ ] 失败路径是否触发 rollback？
- [ ] 是否在库线程做了重活（应属 worker）？
