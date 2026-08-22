# Database Guidelines — media

- 接口 crate 不接触数据库。
- 相关契约：缩略图产物路径必须用 `store::Store::thumbnail_cache_path`（两级分片）；worker 产出的时长/帧数据回写走 store 的 AssetMeta 更新，media 类型只承载任务与结果，不承载持久化职责。
