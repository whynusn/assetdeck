//! 侧栏分类行 → 稳定 `CategoryId` 的映射表（D87）。
//!
//! 为什么单独成表：分类 id 不再等于侧栏行号——id 来自 `categories` 表、跨重载
//! 稳定，而侧栏行按名排序、随时可能移位。Slint 侧栏的选中凭据是行号
//! （`selected-category` 口径 = **模型下标减一**，「全部」不占），故壳层两侧
//! 各需一次换算；本类型把这对换算收在一处并被测试锁定，避免边界加减写错。
//!
//! 行 0 = 「全部」（None，不是任何分类），其后与 [`FacetEntry`] 条目表同序。

use domain::CategoryId;

use crate::catalog_loader::FacetEntry;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CategoryRows(Vec<Option<CategoryId>>);

impl CategoryRows {
    /// 由分类条目表构造（顺序即侧栏顺序，行首自动补「全部」）。
    pub fn from_entries(entries: &[FacetEntry]) -> Self {
        CategoryRows(
            std::iter::once(None)
                .chain(entries.iter().map(|entry| Some(CategoryId(entry.id))))
                .collect(),
        )
    }

    /// 侧栏「分类序号」（Slint `selected-category` 口径：模型下标减一）→ id。
    pub fn id_at(&self, sidebar_index: usize) -> Option<CategoryId> {
        self.0.get(sidebar_index + 1).copied().flatten()
    }

    /// 分类 id → 侧栏「分类序号」。失效 id（分类已删/库已切换）返回 None。
    pub fn index_of(&self, id: CategoryId) -> Option<i32> {
        self.0
            .iter()
            .position(|row| *row == Some(id))
            .map(|position| position as i32 - 1)
    }

    /// 模型下标（含行首「全部」）→ id；行 0 恒 None。
    pub fn id_at_model_row(&self, row: usize) -> Option<CategoryId> {
        self.0.get(row).copied().flatten()
    }

    /// 分类条目数（不含「全部」）。
    pub fn len(&self) -> usize {
        self.0.len().saturating_sub(1)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u32, name: &str) -> FacetEntry {
        FacetEntry {
            id,
            name: name.to_string(),
            count: 0,
        }
    }

    /// 行首「全部」占位：模型下标 0 恒为 None，分类从下标 1 起。
    #[test]
    fn row_zero_is_all_and_categories_start_at_one() {
        let rows = CategoryRows::from_entries(&[entry(7, "甲"), entry(9, "乙")]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.id_at_model_row(0), None, "「全部」不是分类");
        assert_eq!(rows.id_at_model_row(1), Some(CategoryId(7)));
        assert_eq!(rows.id_at_model_row(2), Some(CategoryId(9)));
        assert_eq!(rows.id_at_model_row(3), None, "越界返回 None");
    }

    /// 侧栏序号口径 = 模型下标减一（Slint 的 `(i - 1) == selected-category`）。
    #[test]
    fn sidebar_index_matches_slint_off_by_one() {
        let rows = CategoryRows::from_entries(&[entry(7, "甲"), entry(9, "乙")]);
        assert_eq!(rows.id_at(0), Some(CategoryId(7)), "首个分类序号为 0");
        assert_eq!(rows.id_at(1), Some(CategoryId(9)));
        assert_eq!(rows.id_at(2), None, "越界返回 None");
    }

    /// 两个方向互为逆运算，且与 id 数值无关（id 稀疏/大值同样正确）。
    #[test]
    fn index_and_id_round_trip_for_sparse_ids() {
        let rows = CategoryRows::from_entries(&[entry(3, "甲"), entry(42, "乙"), entry(900, "丙")]);
        for (index, id) in [(0, 3u32), (1, 42), (2, 900)] {
            let category = rows.id_at(index).expect("序号应能取到 id");
            assert_eq!(category, CategoryId(id));
            assert_eq!(rows.index_of(category), Some(index as i32));
        }
        assert_eq!(rows.index_of(CategoryId(999)), None, "未知 id 无对应行");
        assert_eq!(
            rows.index_of(CategoryId(0)),
            None,
            "收件箱不进这张表（id 0 由条目表显式给出）"
        );
    }

    /// 收件箱作为条目时序号正常占用：它是 id 0，但仍是「一个分类行」。
    #[test]
    fn inbox_entry_occupies_a_normal_row() {
        let rows = CategoryRows::from_entries(&[entry(0, "待分类"), entry(5, "风景")]);
        assert_eq!(rows.id_at(0), Some(CategoryId(0)));
        assert_eq!(rows.index_of(CategoryId(0)), Some(0));
        assert_eq!(rows.id_at(1), Some(CategoryId(5)));
    }

    /// 空库：只有「全部」一行，任何序号/模型下标都取不到分类。
    #[test]
    fn empty_entries_yield_all_only() {
        let rows = CategoryRows::from_entries(&[]);
        assert!(rows.is_empty());
        assert_eq!(rows.id_at(0), None);
        assert_eq!(rows.id_at_model_row(0), None);
        assert_eq!(rows.index_of(CategoryId(1)), None);
    }
}
