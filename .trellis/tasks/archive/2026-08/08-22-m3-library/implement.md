# M3 执行清单

## Red 顺序（先测后码）

### phash crate
1. `identical_images_hash_distance_is_zero`
2. `slight_brightness_shift_stays_under_threshold`（同渐变图 +8/255 亮度，距离 ≤10）
3. `unrelated_patterns_exceed_threshold`（纯色 vs 棋盘，距离 ≥32）
4. `hamming_distance_known_values`（0x0000 vs 0xFFFF = 16 等）

### library crate
5. `import_copies_file_into_library_layout` — enqueue→Done 后 objects/<uuid>/raw.<ext> 存在、store 可查、rel_path 正确
6. `duplicate_phash_rejected_no_second_copy` — 同文件二次导入返回 Duplicate(existing)，objects 下仅一份
7. `async_copy_metadata_visible_before_done` — Copying 阶段 get_asset 已命中
8. `copy_queue_respects_backpressure_cap` — cap=1，首任务未完时第二次 enqueue 报 Backpressure
9. `manual_category_and_inbox_fallback` — 带 category 用之；None → "待分类"
10. `video_import_dispatches_media_job` — mp4 导入产生 MediaJob 记录且 duration 为 None

## Green 实现序

phash → library::layout → copy queue 状态机 → store 编排 + 去重 → media stub

## Check / 收口

- 全工作区 fmt/clippy/test 三绿
- spec 沉淀：phash 参数与去重阈值进 `.trellis/spec/phash|library/backend/`
- TDD_PLAN 勾选；commit；archive
