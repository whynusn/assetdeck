# Logging Guidelines — bench-harness

- 输出面向 CI 解析：采样结果打印为单行结构化格式（时间戳、RSS 字节、阶段标签），人类可读摘要走 stderr。
- 趋势数据存 artifact（JSON），格式变更须兼容历史对比或注明断点。
