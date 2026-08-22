# Database Guidelines — bench-harness

- 合成库经 library/Store 公共 API 写入临时 .library 包（tempfile 生命周期归 harness），禁止手拼 SQLite。
- 生成器只造元数据 + 占位缩略图（渐变程序化图，无版权问题）；不走真实解码路径，保证 100k 规模生成在秒级。
