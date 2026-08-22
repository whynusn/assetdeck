# Database Guidelines — worker

- worker 产物的落点：
  - 缩略图 → `thumbs/{u}/{uu}/{uuid}.{ext}`（必须用 `store::thumbnail_cache_path`）；
  - 时长/帧数等元数据 → 经池回报给宿主，由 library 层走 `store.upsert_asset` 回写；worker **不直接打开 meta.db**，避免双写竞争。
- 坏文件隔离：解析失败的源文件记录 uuid + 失败原因，由上层标记状态；worker 不移动/删除用户文件。
