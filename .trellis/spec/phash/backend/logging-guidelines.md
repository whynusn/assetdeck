# Logging Guidelines — phash

- 纯算法零日志。性能观测：单图 pHash 计算在导入路径上，若未来成为瓶颈用 criterion 基准量化，禁止打点日志污染热路径。
