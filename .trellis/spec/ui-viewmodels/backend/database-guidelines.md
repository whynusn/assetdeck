# Database Guidelines — ui-viewmodels

- VM 不直接持有 Connection；通过 library/Store 门面访问。查询结果进入 VM 的模型切片（分页窗口），全量数据留在 index 位图 + store。
- SmartFolder 的持久化（filter+sorter serde）经 store 落库；VM 只操作 domain::SmartFolder 类型。
