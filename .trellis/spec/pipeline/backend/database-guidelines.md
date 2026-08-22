# Database Guidelines — pipeline

- pipeline 不接触数据库；资产解析所需的元数据由调用方（ui-viewmodels/library）传入。
- auto-send 开关等配置的持久化归 app 配置层（M6 落地时定：JSON 配置文件或 store 表），pipeline 只读运行时值。默认值快照测试锁定「默认关」。
