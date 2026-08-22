# M3 设计

## crate 划分

### phash（纯函数库，零 IO）
- `perceptual_hash(&GrayImage) -> u64`：resize 32×32 → naive DCT-II → 左上 8×8 去 DC → 中位数阈值取 64 bit
- `hamming_distance(a: u64, b: u64) -> u32` = `(a^b).count_ones()`
- 不引第三方哈希库；image crate 已在工作区依赖树内

### library（编排层）
```
LibraryLayout { root }            # meta.db / objects/<uuid>/raw.<ext> / thumbs/
ImportRequest { source, category, tags }
CopyQueue { cap }                 # 有界队列 + 单工作线程
  enqueue(req) -> ImportTicket    # 满 => EnqueueError::Backpressure
  state(ticket) -> CopyState      # Pending | Copying{copied,total} | Done{uuid} | Failed{reason}
MediaDispatcher (trait)           # video 任务派发口，M3 用记录型 stub，M4 接 worker
```

## 关键决策

| 决策 | 理由 |
|---|---|
| pHash 先算后拷 | 去重失败即零磁盘代价（D7 双倍占用红线） |
| 元数据先落库、拷贝异步补完 | 体感瞬时入库（D7 义务）；Done 前 rel_path 即终位 |
| 去重查 store.phash 列（BLOB 8B）+ 内存线性比对 | M3 规模够用；百万级时升级为 index 层位图/分桶 |
| uuid 由 library 生成（uuid v4），domain u32 映射延后到 UI 层组装 | 保持各 crate 边界 |
| 分类常量 INBOX_CATEGORY = "待分类" | D5 收件箱语义 |

## 测试策略

- 全部程序化夹具：tempdir 写真实小文件（拷贝路径需真文件）；图像用 image crate 内存构造再编码 PNG
- 拷贝状态机用轮询+超时断言；backpressure 用 cap=1 + 阻塞中的首个任务验证第二请求被拒
