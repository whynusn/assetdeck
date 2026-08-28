//! 分类规则器：导入时为素材推断分类的扩展点（综合分析报告「三.6」）。
//!
//! 现状：分类主要来自导入目录名或千牛 EmotionConfig 的 groupName。未来做
//! 「按扩展名/文件名/元数据自动分类」时，新增一个实现 `CategoryRule` 的规则并
//! 加入 `RuleChain` 即可，不需要改导入编排代码。

use std::path::Path;

/// 导入时已知的、参与分类决策的上下文。
#[derive(Debug, Clone, Copy, Default)]
pub struct CategoryContext<'a> {
    /// 显式分类（用户选择 / 配置声明）；None 表示未指定。
    pub explicit: Option<&'a str>,
    /// 素材包声明的小组名（千牛 EmotionConfig.json 的 groupName）。
    pub group: Option<&'a str>,
}

/// 一条分类规则：返回 Some(分类名) 表示命中并采用该分类；None 表示不适用。
pub trait CategoryRule: Send + Sync {
    /// 规则名（诊断/日志用）。
    fn name(&self) -> &str;

    fn apply(&self, source: &Path, ctx: &CategoryContext<'_>) -> Option<String>;
}

/// 用「素材所在目录名」作为分类（目录导入的既有语义）。
pub struct ParentDirectoryRule;

impl CategoryRule for ParentDirectoryRule {
    fn name(&self) -> &str {
        "parent-dir"
    }

    fn apply(&self, source: &Path, _ctx: &CategoryContext<'_>) -> Option<String> {
        source
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(|name| name.to_string())
    }
}

/// 用素材包声明的小组名作为分类（千牛 EmotionConfig groupName 语义）。
pub struct GroupNameRule;

impl CategoryRule for GroupNameRule {
    fn name(&self) -> &str {
        "group-name"
    }

    fn apply(&self, _source: &Path, ctx: &CategoryContext<'_>) -> Option<String> {
        ctx.group.map(|g| g.to_string())
    }
}

/// 规则链：按声明顺序取首个命中者（后添加的规则不覆盖先命中的）。
#[derive(Default)]
pub struct RuleChain {
    rules: Vec<Box<dyn CategoryRule>>,
}

impl RuleChain {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, rule: Box<dyn CategoryRule>) -> &mut Self {
        self.rules.push(rule);
        self
    }

    /// 解析最终分类：显式 > 链内首个命中 > None（回落待分类收件箱）。
    pub fn resolve(&self, source: &Path, ctx: &CategoryContext<'_>) -> Option<String> {
        if let Some(explicit) = ctx.explicit.filter(|name| !name.is_empty()) {
            return Some(explicit.to_string());
        }
        self.rules
            .iter()
            .find_map(|rule| rule.apply(source, ctx))
            .filter(|name| !name.is_empty())
    }
}

/// 默认规则集：千牛 groupName 优先，目录名兜底（与导入路径既有语义一致）。
pub fn default_import_chain() -> RuleChain {
    let mut chain = RuleChain::new();
    chain.push(Box::new(GroupNameRule));
    chain.push(Box::new(ParentDirectoryRule));
    chain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_category_wins_over_rules() {
        let chain = default_import_chain();
        let ctx = CategoryContext {
            explicit: Some("手选"),
            group: Some("包声明"),
        };
        assert_eq!(
            chain.resolve(Path::new("分类A/图.png"), &ctx),
            Some("手选".to_string())
        );
    }

    #[test]
    fn group_name_beats_parent_directory() {
        let chain = default_import_chain();
        let ctx = CategoryContext {
            explicit: None,
            group: Some("表情组"),
        };
        assert_eq!(
            chain.resolve(Path::new("任意目录/贴纸.png"), &ctx),
            Some("表情组".to_string())
        );
    }

    #[test]
    fn parent_directory_is_the_fallback_rule() {
        let chain = default_import_chain();
        let ctx = CategoryContext::default();
        assert_eq!(
            chain.resolve(Path::new("素材包/促销海报/主图.jpg"), &ctx),
            Some("促销海报".to_string())
        );
    }

    #[test]
    fn unresolved_falls_back_to_inbox_semantics() {
        let chain = RuleChain::new(); // 空链
        assert_eq!(chain.resolve(Path::new("root/a.png"), &CategoryContext::default()), None);
    }
}
