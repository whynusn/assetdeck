//! Per-import destination and phase-one receipt. No current-view/global batch lookup.
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

pub(crate) type Phase1Callback = Arc<dyn Fn(Result<(), String>) + Send + Sync>;

/// 导入管线的对账尾部参数：一次性的 phase1 回执回调，加上是否允许把已
/// 落库的行重新发布为新的 Add（仅显式本地共享；远端批次回执不得自发
/// 替代 Add）。
pub(crate) struct ReconcileTail {
    pub callback: Option<Phase1Callback>,
    pub allow_space_readd: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct DoneStats {
    pub imported: u32,
    pub skipped: u32,
    pub similar: u32,
    pub failed: u32,
}

impl DoneStats {
    pub fn parse(line: &str) -> Option<Self> {
        let rest = line.strip_prefix("done:")?;
        let mut stats = Self::default();
        let mut saw_imported = false;
        let mut saw_failed = false;
        for field in rest.split_whitespace() {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            match key {
                "imported" => {
                    stats.imported = value.parse().ok()?;
                    saw_imported = true;
                }
                "skipped" => stats.skipped = value.parse().ok()?,
                "similar" => stats.similar = value.parse().ok()?,
                "failed" => {
                    stats.failed = value.parse().ok()?;
                    saw_failed = true;
                }
                _ => {}
            }
        }
        (saw_imported && saw_failed).then_some(stats)
    }
}

#[derive(Clone)]
pub(crate) struct ImportCompletion(Arc<CompletionState>);

struct CompletionState {
    target: PathBuf,
    callback: Option<Phase1Callback>,
    completed: AtomicBool,
}

impl ImportCompletion {
    pub fn new(target: PathBuf, callback: Option<Phase1Callback>) -> Self {
        Self(Arc::new(CompletionState {
            target,
            callback,
            completed: AtomicBool::new(false),
        }))
    }

    pub fn target(&self) -> &Path {
        &self.0.target
    }

    pub fn refreshes(&self, visible_root: &Path) -> bool {
        self.target() == visible_root
    }

    /// Receipts require both successful child exit and a complete, failure-free summary.
    pub fn phase1_outcome(
        child_success: bool,
        message: &str,
        stats: Option<DoneStats>,
    ) -> Result<(), String> {
        if !child_success {
            return Err(if message.trim().is_empty() {
                "导入管线失败".to_string()
            } else {
                message.trim().to_string()
            });
        }
        match stats {
            Some(stats) if stats.failed == 0 => Ok(()),
            Some(stats) => Err(format!("{} 项素材导入失败", stats.failed)),
            None => Err("导入工具未返回完整结果，无法确认入库".to_string()),
        }
    }

    /// A complete summary means durable rows exist even when some inputs failed.
    pub fn has_durable_summary(child_success: bool, stats: Option<DoneStats>) -> bool {
        child_success && stats.is_some()
    }

    /// True only for the first terminal event; callers gate follow-up work on it.
    pub fn complete(&self, outcome: Result<(), String>) -> bool {
        self.complete_after_durable(outcome, || Ok(())).is_some()
    }

    /// Claims the terminal event, publishes durable success once, then delivers its receipt.
    pub fn complete_after_durable(
        &self,
        outcome: Result<(), String>,
        publish: impl FnOnce() -> Result<(), String>,
    ) -> Option<Result<(), String>> {
        let durable = outcome.is_ok();
        self.complete_after_reconcile(outcome, durable, publish)
    }

    /// Claims the terminal event and optionally reconciles durable child results.
    /// A partial import may reconcile successful rows but must still return its NACK.
    pub fn complete_after_reconcile(
        &self,
        outcome: Result<(), String>,
        durable: bool,
        publish: impl FnOnce() -> Result<(), String>,
    ) -> Option<Result<(), String>> {
        if self.0.completed.swap(true, Ordering::SeqCst) {
            return None;
        }
        let published = if durable { publish() } else { Ok(()) };
        let outcome = match (outcome, published) {
            (Ok(()), Ok(())) => Ok(()),
            (Ok(()), Err(error)) => Err(error),
            (Err(error), _) => Err(error),
        };
        if let Some(callback) = &self.0.callback {
            callback(outcome.clone());
        }
        Some(outcome)
    }

    /// Claims completion when the UI event loop is unavailable; no receipt can be sent.
    pub fn finish_without_callback(&self) -> bool {
        !self.0.completed.swap(true, Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn successful_child_with_failed_items_must_nack() {
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let observed = receipts.clone();
        let completion = ImportCompletion::new(
            PathBuf::from("team-a"),
            Some(Arc::new(move |result| {
                observed.lock().unwrap().push(result)
            })),
        );
        let stats = DoneStats::parse("done: imported=1 skipped=0 similar=0 failed=1");
        completion.complete(ImportCompletion::phase1_outcome(true, "", stats));
        let receipts = receipts.lock().unwrap();
        assert!(
            receipts[0].is_err(),
            "exit=0 must not ACK a partially failed batch"
        );
    }

    #[test]
    fn production_summary_ignores_root_and_unknown_fields() {
        let stats = DoneStats::parse(r"done: imported=8 skipped=0 similar=0 failed=0 total=8 root=C:\Users\Test User\team extra=word").unwrap();
        assert_eq!(stats.imported, 8);
        assert!(ImportCompletion::phase1_outcome(true, "", Some(stats)).is_ok());
        assert!(ImportCompletion::phase1_outcome(true, "", None).is_err());
        assert!(DoneStats::parse("done: imported=8").is_none());
        assert!(DoneStats::parse("done: imported=8 failed=bad").is_none());
    }

    #[test]
    fn durable_publish_precedes_receipt_and_happens_once() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed = events.clone();
        let completion = ImportCompletion::new(
            PathBuf::from("team-a"),
            Some(Arc::new(move |result| {
                assert!(result.is_ok());
                observed.lock().unwrap().push("receipt");
            })),
        );
        assert_eq!(
            completion.complete_after_durable(Ok(()), || {
                events.lock().unwrap().push("publish");
                Ok(())
            }),
            Some(Ok(()))
        );
        assert!(completion
            .clone()
            .complete_after_durable(Ok(()), || panic!("duplicate publish"))
            .is_none());
        assert_eq!(*events.lock().unwrap(), vec!["publish", "receipt"]);
        let failed = ImportCompletion::new(PathBuf::from("team-a"), None);
        assert!(failed
            .complete_after_durable(Err("partial failure".into()), || panic!(
                "failed import published"
            ))
            .unwrap()
            .is_err());
    }

    #[test]
    fn failed_operation_publication_cannot_ack_durable_bytes() {
        let received = Arc::new(Mutex::new(None));
        let observed = received.clone();
        let completion = ImportCompletion::new(
            PathBuf::from("team-a"),
            Some(Arc::new(move |result| {
                *observed.lock().unwrap() = Some(result);
            })),
        );
        assert!(completion
            .complete_after_durable(Ok(()), || Err("operation log unavailable".into()))
            .unwrap()
            .is_err());
        assert!(received.lock().unwrap().as_ref().unwrap().is_err());
    }

    #[test]
    fn partial_failure_publishes_durable_rows_but_receipt_stays_nack() {
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let observed = receipts.clone();
        let completion = ImportCompletion::new(
            PathBuf::from("team-a"),
            Some(Arc::new(move |result| {
                observed.lock().unwrap().push(result)
            })),
        );
        // Durable rows exist (complete summary) despite failed items: reconcile
        // must run, yet the batch receipt must remain a NACK.
        let publishes = Arc::new(Mutex::new(0usize));
        let observed_publish = publishes.clone();
        let outcome = completion
            .complete_after_reconcile(Err("1 项素材导入失败".into()), true, || {
                *observed_publish.lock().unwrap() += 1;
                Ok(())
            })
            .unwrap();
        assert!(outcome.is_err(), "partial failure must not ACK");
        assert_eq!(
            *publishes.lock().unwrap(),
            1,
            "durable rows must be published"
        );
        assert_eq!(receipts.lock().unwrap().len(), 1);
        assert!(receipts.lock().unwrap()[0].is_err());
        // A duplicate terminal event neither republishes nor re-receipts.
        assert!(completion
            .clone()
            .complete_after_reconcile(Err("again".into()), true, || panic!("duplicate publish"))
            .is_none());
        assert_eq!(
            *publishes.lock().unwrap(),
            1,
            "duplicate terminal event must not republish"
        );
        assert_eq!(receipts.lock().unwrap().len(), 1);
    }

    #[test]
    fn unavailable_event_loop_consumes_completion_without_claiming_a_receipt() {
        let completion = ImportCompletion::new(
            PathBuf::from("team-a"),
            Some(Arc::new(|_| panic!("no UI callback available"))),
        );
        assert!(completion.finish_without_callback());
        assert!(!completion.complete(Err("shutdown".into())));
    }

    #[test]
    fn immutable_target_survives_view_switch_and_later_import() {
        let first = ImportCompletion::new(PathBuf::from("team-a"), None);
        let second = ImportCompletion::new(PathBuf::from("team-b"), None);
        assert_eq!(first.target(), Path::new("team-a"));
        assert!(!first.refreshes(second.target()));
        assert!(first.refreshes(Path::new("team-a")));
    }

    #[test]
    fn failure_and_duplicate_terminal_events_receipt_exactly_once() {
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let observed = receipts.clone();
        let completion = ImportCompletion::new(
            PathBuf::from("team-a"),
            Some(Arc::new(move |result| {
                observed.lock().unwrap().push(result)
            })),
        );
        assert!(completion.complete(Err("child failed to start".into())));
        assert!(!completion.clone().complete(Ok(())));
        assert_eq!(receipts.lock().unwrap().len(), 1);
        assert!(receipts.lock().unwrap()[0].is_err());
    }
}
