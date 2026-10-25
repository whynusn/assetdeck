//! Slint adapter: owns the timer and renders the pure notification model.
use crate::{AppWindow, NoticeRow};
use slint::{ComponentHandle, ModelRc, Timer, TimerMode, VecModel};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use ui_viewmodels::notifications::{Notice, Notifications, Severity};
use ui_viewmodels::TargetNoticeTone;

thread_local! {
    static STATE: RefCell<Notifications> = RefCell::new(Notifications::default());
    static TIMER: Timer = Timer::default();
}

fn tone(severity: Severity) -> i32 {
    match severity {
        Severity::Success => crate::ui_enums::notice_tone(TargetNoticeTone::Success),
        Severity::Warning => crate::ui_enums::notice_tone(TargetNoticeTone::Warning),
        Severity::Error => crate::ui_enums::notice_tone(TargetNoticeTone::Error),
        Severity::Info => crate::ui_enums::NOTICE_TONE_INFO,
    }
}

fn render(ui: &AppWindow) {
    STATE.with(|slot| {
        let state = slot.borrow();
        ui.set_notice_text(
            state
                .toast()
                .map_or_else(String::new, Notice::preview)
                .into(),
        );
        if let Some(n) = state.toast() {
            ui.set_notice_tone(tone(n.severity));
        }
        ui.set_notice_banner(state.banner().map_or("", |n| n.text.as_str()).into());
        ui.set_notice_history(ModelRc::new(VecModel::from(
            state
                .history()
                .map(|n| NoticeRow {
                    text: n.text.as_str().into(),
                    tone: tone(n.severity),
                })
                .collect::<Vec<_>>(),
        )));
    });
}

pub fn publish(ui: &AppWindow, severity: Severity, text: String) {
    STATE.with(|slot| slot.borrow_mut().push(Notice::new(severity, text)));
    render(ui);
    TIMER.with(|timer| {
        if timer.running() {
            return;
        }
        let weak = ui.as_weak();
        let last = Rc::new(RefCell::new(Instant::now()));
        timer.start(TimerMode::Repeated, Duration::from_millis(100), move || {
            let Some(ui) = weak.upgrade() else {
                TIMER.with(Timer::stop);
                return;
            };
            let now = Instant::now();
            let elapsed = now.duration_since(*last.borrow());
            *last.borrow_mut() = now;
            let finished = STATE.with(|slot| {
                let mut state = slot.borrow_mut();
                state.advance(
                    elapsed,
                    ui.get_notice_hovered() || ui.get_notice_history_open(),
                );
                state.toast().is_none()
            });
            if finished {
                ui.set_notice_text("".into());
                TIMER.with(Timer::stop);
            }
        });
    });
}

/// Compatibility boundary for existing pipeline/share feedback. Presentation is
/// transient regardless of tone; persistent problems use set_problem explicitly.
pub fn show_notice(ui: &AppWindow, tone: TargetNoticeTone, text: String) {
    let severity = match tone {
        TargetNoticeTone::Success => Severity::Success,
        TargetNoticeTone::Warning => Severity::Warning,
        TargetNoticeTone::Error => Severity::Error,
    };
    publish(ui, severity, text);
}

pub fn set_problem(ui: &AppWindow, key: &'static str, text: String) {
    STATE.with(|slot| {
        slot.borrow_mut()
            .set_problem(key, Notice::new(Severity::Error, text))
    });
    render(ui);
}

pub fn resolve(ui: &AppWindow, key: &str) {
    STATE.with(|slot| slot.borrow_mut().resolve(key));
    render(ui);
}

pub fn wire(ui: &AppWindow) {
    let weak = ui.as_weak();
    ui.on_notice_dismissed(move || {
        STATE.with(|slot| slot.borrow_mut().dismiss());
        TIMER.with(Timer::stop);
        if let Some(ui) = weak.upgrade() {
            ui.set_notice_text("".into());
        }
    });
}
