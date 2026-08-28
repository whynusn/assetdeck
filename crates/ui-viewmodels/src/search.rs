//! 搜索 Provider：把用户查询编译为 [`domain::Filter`] 的统一检索门面。
//!
//! D52 混合路由：≥3 字符查询且库可用 → FTS5 命中经 uuid→行号二分映射为
//! [`domain::Filter::NameIn`]（FTS 行不随软删移除，求值侧与活集求交）；
//! 短查询 / 无 FTS 源（bench、演示库）→ [`domain::Filter::NameContains`]
//! 内存扫描。D51：分类/标签/文件名三路全部大小写不敏感（共享 domain::text
//! 折叠工具，ASCII 与 Unicode 同一语义）。

use std::fmt;

/// 搜索范围（D51 四档）：名子句 ∪ 分类子句 ∪ 标签子句 的取舍。
/// FTS 表只有 name 列——FileName 档之外的档位恒内存路。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchScope {
    /// 全部（= v1 现行为 + 大小写修复）。
    All,
    /// 仅文件名。
    FileName,
    /// 仅分类名。
    Category,
    /// 仅标签名。
    Tag,
}

/// 搜索参数错误。调用方收到 [`SearchError::EmptyQuery`] 时回落 `base`
/// 过滤器，视图保持不变；收到 [`SearchError::FtsUnavailable`] 的只发生在
/// `FtsNameSource` 内部（Provider 自行降级内存路，不外抛此变体）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchError {
    /// 查询为空或纯空白（无法编译出有意义的 Filter，回落 base）。
    EmptyQuery,
    /// FTS 查询失败（库损坏 / 打不开）。调用方应降级而非失败。
    FtsUnavailable,
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchError::EmptyQuery => write!(f, "查询为空或纯空白"),
            SearchError::FtsUnavailable => write!(f, "FTS 检索不可用"),
        }
    }
}

impl std::error::Error for SearchError {}

/// FTS 名检索接缝（D52）：FTS 查询 + uuid→AssetId 二分封装在 resolver 内
/// （它同持 store 与升序 uuids），provider 只见 trait——ui-viewmodels 保持可 mock。
pub trait FtsNameSource {
    /// 返回按行号升序的 id 集（FTS 命中 → binary_search 映射；未知 uuid 跳过）。
    fn name_ids(&self, query: &str, limit: usize) -> Result<Vec<u32>, SearchError>;
}

/// 搜索门面：`search(query, scope, base)` 把用户查询编译为新的
/// [`domain::Filter`]。
///
/// `base` 是调用方当前的基线过滤器（分类/标签视图等）：成功时以返回的
/// Filter 替换基线；`Err` 时调用方回落 `base`，当前视图不做任何改动。
pub trait SearchProvider {
    fn search(
        &self,
        query: &str,
        scope: SearchScope,
        base: &domain::Filter,
    ) -> Result<domain::Filter, SearchError>;
}

/// FTS 名子句的行数上限：命中上限与后续 NameIn 求值成本都封顶；
/// 浏览期候选再被 grid 截断，无内存风险（design §2）。
const FTS_QUERY_LIMIT: usize = 10_000;

/// trigram 下限：SQLite trigram 分词只认 ≥3 字符查询；更短的查询恒走内存路。
const FTS_MIN_CHARS: usize = 3;

/// v1 混合实现：分类/标签名折叠命中 ∪ 名子句（FTS 路 / 内存路），
/// 按范围取子句集组装为 `AnyOf`（FileName 档直接返回名子句本体）。
pub struct HybridSearchProvider<'a> {
    pub facets: &'a crate::catalog_loader::LibraryFacets,
    /// bench / 演示库 / 测试 mock 无 FTS 源 = None → 名子句恒内存路。
    pub fts: Option<&'a dyn FtsNameSource>,
}

impl HybridSearchProvider<'_> {
    /// 名子句：≥3 字符且有 FTS 源 → NameIn（FTS 失败降级内存路，视图不断）；
    /// 其余 → NameContains。
    fn name_clause(&self, query: &str) -> domain::Filter {
        if query.chars().count() >= FTS_MIN_CHARS {
            if let Some(fts) = self.fts {
                match fts.name_ids(query, FTS_QUERY_LIMIT) {
                    Ok(ids) => return domain::Filter::NameIn(ids),
                    Err(error) => {
                        log::warn!("FTS 名检索失败，降级内存路：{error}");
                    }
                }
            }
        }
        domain::Filter::NameContains(query.to_string())
    }
}

impl SearchProvider for HybridSearchProvider<'_> {
    fn search(
        &self,
        query: &str,
        scope: SearchScope,
        _base: &domain::Filter,
    ) -> Result<domain::Filter, SearchError> {
        if query.trim().is_empty() {
            return Err(SearchError::EmptyQuery);
        }
        if matches!(scope, SearchScope::FileName) {
            return Ok(self.name_clause(query));
        }
        let mut clauses: Vec<domain::Filter> = Vec::new();
        if matches!(scope, SearchScope::All | SearchScope::Category) {
            clauses.extend(
                self.facets
                    .category_matches(query)
                    .into_iter()
                    .map(domain::Filter::InCategory),
            );
        }
        if matches!(scope, SearchScope::All | SearchScope::Tag) {
            clauses.extend(
                self.facets
                    .tag_matches(query)
                    .into_iter()
                    .map(domain::Filter::HasTag),
            );
        }
        if matches!(scope, SearchScope::All) {
            clauses.push(self.name_clause(query));
        }
        Ok(domain::Filter::AnyOf(clauses))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use domain::{CategoryId, Filter, TagId};
    use store::{AssetMeta, Store};

    use super::{HybridSearchProvider, SearchError, SearchProvider, SearchScope};
    use crate::catalog_loader::load_real_library;

    /// 用真实 Store 装配微型库（LibraryFacets 字段私有、无公开注入途径，
    /// 走 load_real_library 公开路径造库；与 tests/facets_spec.rs 同款画法）。
    fn scaffold(tag: &str) -> PathBuf {
        let root = PathBuf::from("target").join("tmp").join(tag);
        if root.exists() {
            let _ = fs::remove_dir_all(&root);
        }
        fs::create_dir_all(&root).expect("建库目录失败");
        let store = Store::open(&root.join("meta.db")).expect("打开 meta.db 失败");
        let rows: [(&str, &str, Option<&str>, &[&str]); 2] = [
            (
                "a0000000-0000-0000-0000-000000000000",
                "促销海报.png",
                Some("促销"),
                &[],
            ),
            (
                "b0000000-0000-0000-0000-000000000000",
                "两周年.png",
                Some("风景"),
                &["促销"],
            ),
        ];
        for (index, (uuid, file_name, category, tags)) in rows.iter().enumerate() {
            store
                .upsert_asset(&AssetMeta {
                    uuid: uuid.to_string(),
                    file_name: file_name.to_string(),
                    rel_path: format!("objects/{uuid}/{file_name}"),
                    category: category.map(|c| c.to_string()),
                    tags: tags.iter().map(|t| t.to_string()).collect(),
                    size_bytes: 1,
                    created_at: index as i64,
                    imported_at: index as i64,
                    phash: None,
                    width: None,
                    height: None,
                })
                .expect("写资产元数据失败");
        }
        root
    }

    #[test]
    fn query_hits_category_and_file_name() {
        let root = scaffold("search-cat-and-name");
        let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
        let provider = HybridSearchProvider {
            facets: resolver.facets(),
            fts: None,
        };

        let filter = provider
            .search("促销", SearchScope::All, &Filter::All)
            .expect("非空查询应 Ok");

        let category_id = resolver.facets().category_id("促销").expect("应有该分类").0;
        let promo_tag_id = resolver
            .facets()
            .tags()
            .iter()
            .find(|e| e.name == "促销")
            .expect("应有该标签")
            .id;
        assert_eq!(
            filter,
            Filter::AnyOf(vec![
                Filter::InCategory(CategoryId(category_id)),
                Filter::HasTag(TagId(promo_tag_id)),
                Filter::NameContains("促销".to_string()),
            ]),
            "分类名命中 + 标签名命中 + 文件名 NameContains 合并为 AnyOf"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn query_without_facet_hit_keeps_name_contains() {
        let root = scaffold("search-name-only");
        let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
        let provider = HybridSearchProvider {
            facets: resolver.facets(),
            fts: None,
        };

        let filter = provider
            .search("不存在的词", SearchScope::All, &Filter::All)
            .expect("非空查询应 Ok");
        assert_eq!(
            filter,
            Filter::AnyOf(vec![Filter::NameContains("不存在的词".to_string())]),
            "facets 无命中时仅 NameContains"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn empty_or_blank_query_is_err() {
        let root = scaffold("search-empty");
        let (_index, resolver) = load_real_library(&root).expect("装载真实库失败");
        let provider = HybridSearchProvider {
            facets: resolver.facets(),
            fts: None,
        };
        assert_eq!(
            provider.search("", SearchScope::All, &Filter::All),
            Err(SearchError::EmptyQuery),
            "空查询 Err"
        );
        assert_eq!(
            provider.search("   \t\n", SearchScope::All, &Filter::All),
            Err(SearchError::EmptyQuery),
            "纯空白查询 Err"
        );
        let _ = fs::remove_dir_all(&root);
    }
}
