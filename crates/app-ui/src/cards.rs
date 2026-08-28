//! 卡片数据提供者（综合分析报告「三.3」）：把 AssetId → 卡片渲染所需数据。
//!
//! TileData 的 kind/preview 由这里统一产出；新增素材类别时只需扩展 media
//! 注册表 + 本文件的 match，不必改 .slint。右上角类型徽标的图形由 slint 侧
//! 按 kind 直取 Glyph 表（Path.commands 只吃 path 数据，字符串名传不进去，
//! 故不再经 Rust 转发图标名）。

use std::cell::RefCell;
use std::collections::HashMap;

use ui_viewmodels::{AssetId, AssetKind, RealAssetResolver};

/// 一张卡片所需的全部表现数据（纯数据，与 slint TileData 对应）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileCardData {
    /// domain::AssetKind 判别值（slint 侧用 UiEnums.card-kind-* 比较）。
    pub kind: AssetKind,
    /// 文本类素材的首行预览（截断）；其余类别空串。
    pub preview: String,
}

/// 卡片数据提供者：id → 表现数据（纯接口，便于测试替身与未来数据源）。
pub trait TileCardDataProvider {
    /// `kind` 由调用方（VM 索引层）给出，避免每张卡重复读库判型。
    fn card_data(&self, kind: AssetKind, id: AssetId) -> TileCardData;
}

/// 文本预览最大字符数（首行截断，超过的部分由 slint elide）。
const PREVIEW_MAX_CHARS: usize = 48;
/// 预览缓存条目上限：可见窗口外的条目由 push_tiles 自然淘汰
//（缓存只是防同一批瓦片反复读盘）。
const PREVIEW_CACHE_CAP: usize = 256;

/// 基于真实库 resolver 的卡片数据提供者：
/// kind 来自 VM，preview 懒读文本首行并做有界缓存；每类素材配一枚
/// 类型徽标图标（字形名直接对应 theme.slint 的 Glyph 表）。
pub struct ResolverCardProvider<'a> {
    resolver: &'a RealAssetResolver,
    previews: RefCell<HashMap<u32, Option<String>>>,
}

impl<'a> ResolverCardProvider<'a> {
    pub fn new(resolver: &'a RealAssetResolver) -> Self {
        Self {
            resolver,
            previews: RefCell::new(HashMap::new()),
        }
    }

    /// 文本首行预览：只对 Text 素材发起物化（materialize 内部有字节 LRU，
    /// 文本路径无缓存；可见窗口量级下每批一次读盘可接受）。
    fn preview_for(&self, id: AssetId) -> String {
        if let Some(cached) = self.previews.borrow().get(&id.0) {
            return cached.clone().unwrap_or_default();
        }
        let value = self
            .resolver
            .materialize(id)
            .ok()
            .flatten()
            .filter(|asset| asset.kind == AssetKind::Text)
            .map(|asset| {
                let first_line = asset.text.lines().next().unwrap_or("").trim();
                if first_line.chars().count() > PREVIEW_MAX_CHARS {
                    let truncated: String = first_line.chars().take(PREVIEW_MAX_CHARS).collect();
                    format!("{truncated}…")
                } else {
                    first_line.to_string()
                }
            })
            .unwrap_or_default();
        if self.previews.borrow().len() >= PREVIEW_CACHE_CAP {
            self.previews.borrow_mut().clear();
        }
        self.previews.borrow_mut().insert(id.0, Some(value.clone()));
        value
    }
}

impl TileCardDataProvider for ResolverCardProvider<'_> {
    fn card_data(&self, kind: AssetKind, id: AssetId) -> TileCardData {
        let preview = if kind == AssetKind::Text {
            self.preview_for(id)
        } else {
            String::new()
        };
        TileCardData { kind, preview }
    }
}
