# Database Guidelines — phash

- 纯算法 crate 不接触数据库。
- 存储契约：hash 以 **u64 大端字节**存 store 的 `phash BLOB` 列；读取时 `u64::from_be_bytes` 还原后用 `hamming_distance` 比对（比对逻辑在 library 层，阈值 8）。
- 视频资产 phash 为 None（不参与图片去重），列可空。
