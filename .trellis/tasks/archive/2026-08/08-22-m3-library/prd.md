# M3 库管理与导入管线

## 需求（对应 DECISIONS D5/D6/D7）

1. **.library 布局**：`meta.db`（store）+ `objects/<uuid>/raw.<ext>` + `thumbs/`（复用 store::thumbnail_cache_path）
2. **pHash 去重（红线）**：导入前计算 64-bit pHash，库内已有汉明距离 ≤ 阈值(默认 8)的资产 → 返回 Duplicate，**不产生第二份拷贝**
3. **异步拷贝队列**：
   - 有界容量，满时入队背压（不静默丢弃）
   - 状态机 Pending→Copying→Done|Failed
   - **体感瞬时入库**（D7）：元数据在拷贝完成前即可查询（rel_path 指向最终位置）
4. **分类**（D5）：导入请求携带分类则用之；未带 → 归入「待分类」收件箱常量
5. **视频**（D6）：按扩展名识别 → 派发缩略图/时长任务描述符给 media trait（worker 实装在 M4）；duration 在回调前为 None

## 验收标准

- [ ] TDD_PLAN M3 的 7 个红灯测试全绿
- [ ] phash：纯内存确定性测试（程序化渐变图），同图距离 0 / 微扰 <阈值 / 无关图 >阈值
- [ ] 全工作区三绿；TDD_PLAN 勾选提交

## 范围外

- worker 进程实装（M4）、UI 触发、监视文件夹、真实 ffmpeg 调用
