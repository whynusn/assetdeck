//! D46 回收站：库内软删除的目录迁移与对账。
//!
//! 语义分层（与 store 的 tombstone 标志互补）：
//! - store 持真相：`deleted` 标志（软删/恢复的可见性过滤全在 store 读取侧）。
//! - 本模块管物理布局：正本目录 `objects/<uuid>/` ⇄ `trash/<uuid>/` 的同卷 rename。
//! - 缩略图（`thumbs/…/<uuid>.png`）软删时**原地不搬**（体积小、恢复零成本、
//!   回收站视图仍能出略图）；彻底删除时连带清除。
//! - 会话一致性：导入去重的内存 pHash 索引随软删**摘除**、随恢复**回填**——
//!   回收站素材不得挡新导入（D46），恢复后又要重新挡（D7 去重必做）。
//!
//! 一致性锚点是 DB：任何「标志说删了、正本还在 objects」或反向漂移都视为
//! 崩溃残留，由 `Library::open` 时自动执行的 [`reconcile_trash_at`] 对账修复。

use std::fs;
use std::path::{Path, PathBuf};

use store::Store;

use crate::{Library, LibraryError, Result};

/// 回收站子目录名（与 objects/、thumbs/ 同级，符合库包目录约定）。
pub const TRASH_DIR: &str = "trash";

fn object_dir(root: &Path, uuid: &str) -> PathBuf {
    root.join("objects").join(uuid)
}

fn trash_dir(root: &Path, uuid: &str) -> PathBuf {
    root.join(TRASH_DIR).join(uuid)
}

/// 缩略图唯一扩展名约定（与 derive-thumbs 写侧一致：png）。
const THUMB_EXT: &str = "png";

fn remove_thumb_file(root: &Path, uuid: &str) {
    let p = root.join(Store::thumbnail_cache_path(uuid, THUMB_EXT));
    let _ = fs::remove_file(&p);
    // paste.png 在对象目录里随目录一并处理（objects 或 trash），无需单独清。
}

/// 8 字节大端 phash → u64（不足/超出不判定，返回 None）。
fn hash_of_be(bytes: &[u8]) -> Option<u64> {
    <[u8; 8]>::try_from(bytes).ok().map(u64::from_be_bytes)
}

impl Library {
    /// 会话内把某素材的 pHash 摘出去重记忆（软删/purge 时调用）。
    fn forget_phash(&self, uuid: &str) -> Result<()> {
        let Some(Some(bytes)) = self.store.get_asset(uuid)?.map(|m| m.phash) else {
            return Ok(());
        };
        let Some(hv) = hash_of_be(&bytes) else {
            return Ok(());
        };
        if let Some(index_mutex) = self.phash_index() {
            index_mutex.lock().unwrap().remove_hash(hv);
        }
        Ok(())
    }

    /// 恢复时把 pHash 回填去重记忆（已含则跳过，幂等）。
    fn remember_phash(&self, uuid: &str) -> Result<()> {
        let Some(meta) = self.store.get_asset(uuid)? else {
            return Ok(());
        };
        let Some(hv) = meta.phash.as_deref().and_then(hash_of_be) else {
            return Ok(());
        };
        if let Some(index_mutex) = self.phash_index() {
            let mut index = index_mutex.lock().unwrap();
            if !index.hashes.contains(&hv) {
                index.hashes.push(hv);
                index.session_uuids.insert(hv, uuid.to_string());
            }
        }
        Ok(())
    }

    /// 移入回收站：置 tombstone 标志 → 正本目录 rename 进 trash/ → 摘除
    /// pHash 去重记忆。rename 失败回滚本行标志并报错，绝不留下
    /// 「标志说删、正本还在 objects」的不一致。返回真正移入的行数。
    ///
    /// 逐行独立处理：多选删除里个别失败不影响其余（宁缺勿错，部分成功由
    /// 调用方按返回值提示）。
    pub fn move_to_trash(&self, uuids: &[&str]) -> Result<usize> {
        let root = &self.root;
        let mut moved = 0usize;
        for uuid in uuids {
            // 1. 先置标（幂等；单行事务复用批量 API）。
            if self.store.soft_delete_assets(&[uuid])? == 0 {
                // 未命中两种可能：本已在回收站（幂等，计完成），或行不存在（不计）。
                if self.store.is_deleted(uuid)? {
                    moved += 1;
                }
                continue;
            }
            let src = object_dir(root, uuid);
            let dst = trash_dir(root, uuid);
            if dst.exists() {
                // objects 与 trash 并存 = 上次崩溃留下，需先 reconcile；本次拒动。
                let _ = self.store.restore_assets(&[uuid]);
                return Err(LibraryError::Trash {
                    uuid: (*uuid).to_string(),
                    reason: "objects 与 trash 同时存在，需启动对账".to_string(),
                });
            }
            // 缺正本 = 只有元数据行，软删即完成（计成功）。
            if src.exists() {
                fs::create_dir_all(root.join(TRASH_DIR))?;
                match fs::rename(&src, &dst) {
                    Ok(()) => {}
                    Err(e) => {
                        // 回滚标志：宁可不删，也不制造标志与物理布局的分裂。
                        let _ = self.store.restore_assets(&[uuid]);
                        return Err(LibraryError::Trash {
                            uuid: (*uuid).to_string(),
                            reason: e.to_string(),
                        });
                    }
                }
            }
            // 2. 成功进回收站才摘记忆（回滚路径的记忆保持原样）。
            self.forget_phash(uuid)?;
            moved += 1;
        }
        Ok(moved)
    }

    /// 从回收站恢复：正本目录 rename 回 objects/ → 复位标志 → 回填 pHash 记忆。
    /// 先搬成功再复位（搬回失败时标志仍为 1，下次 reconcile 不致误判为活行）。
    /// 返回真正恢复的行数。
    pub fn restore_from_trash(&self, uuids: &[&str]) -> Result<usize> {
        let root = &self.root;
        let mut restored = 0usize;
        for uuid in uuids {
            if self.store.is_deleted(uuid)? {
                let src = trash_dir(root, uuid);
                let dst = object_dir(root, uuid);
                if src.exists() {
                    fs::create_dir_all(root.join("objects"))?;
                    fs::rename(&src, &dst).map_err(|e| LibraryError::Trash {
                        uuid: (*uuid).to_string(),
                        reason: e.to_string(),
                    })?;
                }
                // 目录本就不在（只有元数据行）：复位标志即可。
                self.store.restore_assets(&[uuid])?;
                self.remember_phash(uuid)?;
                restored += 1;
            }
        }
        Ok(restored)
    }

    /// 彻底删除：硬删元数据行（FTS 触发 + tags 级联由 store 保证）+ 清除
    /// `trash/<uuid>/` 与残留 `objects/<uuid>/` 正本 + 缩略图 + 去重记忆。
    /// 不可恢复。返回清除的行数。
    pub fn purge(&self, uuids: &[&str]) -> Result<usize> {
        let root = &self.root;
        let mut purged = 0usize;
        for uuid in uuids {
            // 记忆摘除在删行前（get_asset 还读得到 phash）；对未经软删直接
            // purge 的调用方同样成立。
            self.forget_phash(uuid)?;
            if self.store.delete_asset(uuid)? {
                purged += 1;
            }
            // 物理清理不依赖行是否存在（purge 半途崩溃后 reconcile/再清需要幂等）。
            let _ = fs::remove_dir_all(trash_dir(root, uuid));
            let _ = fs::remove_dir_all(object_dir(root, uuid));
            remove_thumb_file(root, uuid);
        }
        Ok(purged)
    }

    /// 清空回收站：枚举所有 tombstone 行逐个彻底删除。手动动作，无自动过期。
    pub fn empty_trash(&self) -> Result<usize> {
        let uuids = self.store.deleted_uuids()?;
        let refs: Vec<&str> = uuids.iter().map(String::as_str).collect();
        self.purge(&refs)
    }

    /// 启动对账的显式入口（`Library::open` 已自动跑；供 CLI/测试触发）。
    /// 语义见 [`reconcile_trash_at`]。
    pub fn reconcile_trash(&self) -> Result<usize> {
        reconcile_trash_at(&self.root, &self.store)
    }
}

/// 对账实现（自由函数：`Library::build` 在 self 成形前即需调用）。
///
/// - 标志=1 但正本还在 objects（rename 前崩）→ 补搬进 trash。
/// - 标志=0/无行 但正本落在 trash（搬完还没复位就崩 / purge 残留）→ 行存在
///   且未标删的补回 objects；无行的清掉孤儿目录。
///
/// 返回修复条数。pHash 记忆无需处理：装载发生在对账之后（见 build 顺序）。
pub(crate) fn reconcile_trash_at(root: &Path, store: &Store) -> Result<usize> {
    let trash_root = root.join(TRASH_DIR);
    let deleted = store.deleted_uuids()?;
    let mut fixed = 0usize;

    // 方向一：软删标志在，正本却还留在 objects。
    for uuid in &deleted {
        let src = object_dir(root, uuid);
        if src.exists() {
            let dst = trash_dir(root, uuid);
            if dst.exists() {
                // 两处并存无法判定正本归属，保守跳过（人工/后续清）。
                continue;
            }
            fs::create_dir_all(&trash_root)?;
            fs::rename(&src, &dst).map_err(|e| LibraryError::Trash {
                uuid: uuid.clone(),
                reason: e.to_string(),
            })?;
            fixed += 1;
        }
    }

    // 方向二：trash/ 下的目录若对应活行或未登记行。
    if trash_root.is_dir() {
        for entry in fs::read_dir(&trash_root)? {
            let entry = entry?;
            if !entry.path().is_dir() {
                continue;
            }
            let uuid = entry.file_name().to_string_lossy().to_string();
            match store.get_asset(&uuid)? {
                Some(meta) => {
                    // 行存在但未标删除：崩溃于「搬完还没复位」→ 补回 objects。
                    if !store.is_deleted(&meta.uuid)? {
                        let src = trash_dir(root, &meta.uuid);
                        let dst = object_dir(root, &meta.uuid);
                        if !dst.exists() {
                            fs::create_dir_all(root.join("objects"))?;
                            fs::rename(&src, &dst)?;
                        } else {
                            // objects 已有正本：trash 是重复残留，清之。
                            let _ = fs::remove_dir_all(&src);
                        }
                        fixed += 1;
                    }
                }
                None => {
                    // 无行 = purge 已删元数据、目录清理崩溃残留 → 收掉孤儿。
                    let _ = fs::remove_dir_all(entry.path());
                    fixed += 1;
                }
            }
        }
    }
    Ok(fixed)
}
