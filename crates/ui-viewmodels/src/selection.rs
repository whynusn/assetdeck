//! D47 选区状态机 + D48 右键菜单数据。
//!
//! 纯数据设计：修饰键由壳层从 Slint `PointerEvent.modifiers` 传入（spike S1
//! 确认 release 携带 Ctrl/Shift），本模块不碰键盘、不依赖渲染，全部可单测。
//! 与 `grid_vm` 的协作经 `view`（当前过滤+排序后的 id 序列）完成：Shift 范围、
//! 全选、选区输出都按视图序，与瓦片呈现一致。
//!
//! 红线 A：模式即屏蔽——`Mode::Multi` 下 `LibraryGridVm` 绝不发 `OpenAsset`。
//! 资源管理器语义对齐：Shift = 锚点范围替换；Ctrl+Shift = 范围并入；锚点
//! 只在非范围点击时移动。

use std::collections::HashSet;

use domain::AssetId;

/// 点击时的键盘修饰态（壳层从 PointerEvent.modifiers 映射）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
}

/// 多选模式开关（D47）：Normal = 今日常态（单击无操作、双击上框）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Normal,
    Multi,
}

/// 右键菜单动作 id（D48 五项，穷举测试锁死）。壳层回传 menu-action(id)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// 素材进剪贴板 = 上框语义止步处，绝不合成回车（D13）。
    Copy,
    MoveToCategory,
    Rename,
    Properties,
    /// 进回收站（D46 软删），非彻底删除。
    Delete,
}

/// 菜单五项 + 文案（CONTEXT.md 用语穷举：复制/移动到分类/重命名/属性/删除）。
/// 顺序即渲染顺序；`LibraryGridVm::context_menu` 据此组装。
pub const MENU_ITEMS: &[(MenuAction, &str)] = &[
    (MenuAction::Copy, "复制"),
    (MenuAction::MoveToCategory, "移动到分类"),
    (MenuAction::Rename, "重命名"),
    (MenuAction::Properties, "属性"),
    (MenuAction::Delete, "删除"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuItem {
    pub action: MenuAction,
    pub label: &'static str,
    pub enabled: bool,
}

/// 右键菜单的 VM 侧描述：目标集（R11：有选区且命中在选区内 = 全选区，
/// 否则收窄到命中瓦片）+ 五项条目。壳层只渲染 + 回传动作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextMenuSpec {
    pub targets: Vec<AssetId>,
    pub items: Vec<MenuItem>,
}

/// 选区集合 + 锚点 + 模式。`view`（视图 id 序列）由调用方提供，本结构不自持，
/// 避免与 `LibraryGridVm::ids` 出现第二套真相。
#[derive(Debug, Default)]
pub struct Selection {
    set: HashSet<AssetId>,
    anchor: Option<AssetId>,
    mode: Mode,
}

impl Selection {
    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn is_multi(&self) -> bool {
        self.mode == Mode::Multi
    }

    pub fn enter_multi(&mut self) {
        self.mode = Mode::Multi;
    }

    /// 退出多选模式 = 清空选区恢复常态（R9）。常态下调用同样清选区，
    /// 给「Ctrl+A 后按 Esc 反选」这类操作留一条统一退出口。
    pub fn exit_multi(&mut self) {
        self.mode = Mode::Normal;
        self.set.clear();
        self.anchor = None;
    }

    pub fn contains(&self, id: AssetId) -> bool {
        self.set.contains(&id)
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }

    /// 选区按视图序输出；不在当前视图的残留 id 被过滤（防御式，
    /// 正常情况下 `prune_to_view` 已保证选区 ⊆ 视图）。
    pub fn ids_in_view(&self, view: &[AssetId]) -> Vec<AssetId> {
        view.iter()
            .copied()
            .filter(|id| self.set.contains(id))
            .collect()
    }

    /// 过滤/搜索后清理掉出视图的选中项（操作条不得对隐形素材动手）。
    /// 锚点同样失效；模式保留（切换分类继续多选是常见流）。
    pub fn prune_to_view(&mut self, view: &[AssetId]) {
        self.set.retain(|id| view.contains(id));
        if let Some(a) = self.anchor {
            if !view.contains(&a) {
                self.anchor = None;
            }
        }
    }

    /// 单击/修饰点击的统一入口。返回是否发生了选区变化（壳层据此发
    /// SelectionChanged）。`view` 必须包含 id，否则忽略（迟到消息容错）。
    pub fn on_click(&mut self, id: AssetId, mods: Modifiers, view: &[AssetId]) -> bool {
        if !view.contains(&id) {
            return false;
        }
        match (mods.ctrl, mods.shift) {
            // 常态无修饰单击 = 今日行为：选区不动、不发事件（红线 B 回归守卫）；
            // 但锚点跟手——随后的 Shift/Ctrl+Shift 范围以最后点中的瓦片为起点。
            // 多选模式内无修饰单击 = 切换选中（D47/R8）。
            (false, false) => {
                self.anchor = Some(id);
                if self.is_multi() {
                    self.toggle(id);
                    true
                } else {
                    false
                }
            }
            // Ctrl 加选不挪锚点：Ctrl+Shift 的范围仍以最后一次直点为起点
            // （与资源管理器习惯一致）。
            (true, false) => {
                self.toggle(id);
                true
            }
            (false, true) => self.replace_with_range(id, view),
            (true, true) => self.add_range(id, view),
        }
    }

    fn toggle(&mut self, id: AssetId) {
        if !self.set.insert(id) {
            self.set.remove(&id);
        }
    }

    /// 锚点→id 的视图序闭区间；无锚点（或锚点掉出视图）退化为单选。
    fn range(&self, id: AssetId, view: &[AssetId]) -> Vec<AssetId> {
        let Some(anchor) = self.anchor.filter(|a| view.contains(a)) else {
            return vec![id];
        };
        let (i, j) = (
            view.iter().position(|v| *v == anchor).unwrap_or_default(),
            view.iter().position(|v| *v == id).unwrap_or_default(),
        );
        let (lo, hi) = if i <= j { (i, j) } else { (j, i) };
        view[lo..=hi].to_vec()
    }

    fn replace_with_range(&mut self, id: AssetId, view: &[AssetId]) -> bool {
        let span = self.range(id, view);
        let next: HashSet<AssetId> = span.into_iter().collect();
        let changed = next != self.set;
        self.set = next;
        changed
    }

    fn add_range(&mut self, id: AssetId, view: &[AssetId]) -> bool {
        let before = self.set.len();
        self.set.extend(self.range(id, view));
        before != self.set.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[u32]) -> Vec<AssetId> {
        v.iter().map(|i| AssetId(*i)).collect()
    }

    #[test]
    fn plain_click_in_normal_mode_never_mutates() {
        let mut s = Selection::default();
        let view = ids(&[0, 1, 2]);
        assert!(!s.on_click(AssetId(1), Modifiers::default(), &view));
        assert!(s.is_empty());
    }

    #[test]
    fn unknown_id_is_ignored() {
        let mut s = Selection::default();
        s.enter_multi();
        assert!(!s.on_click(AssetId(99), Modifiers::default(), &ids(&[0, 1])));
    }
}
