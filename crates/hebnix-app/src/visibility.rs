//! Admit one visibility request at a time. Windows also waits for native,
//! render and taskbar barriers before taking the next one; Linux has none of
//! those, so a change is complete as soon as it has been applied.

use std::sync::Mutex;

use eframe::egui::Context;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Queued,
    Changing,
}

static PHASE: Mutex<Phase> = Mutex::new(Phase::Idle);

fn phase() -> std::sync::MutexGuard<'static, Phase> {
    PHASE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// reserve the one pending toggle; false while another is still in flight
pub fn queue_toggle() -> bool {
    let mut phase = phase();
    if *phase != Phase::Idle {
        return false;
    }
    *phase = Phase::Queued;
    true
}

pub fn cancel_queued_toggle() {
    let mut phase = phase();
    if *phase == Phase::Queued {
        *phase = Phase::Idle;
    }
}

pub fn begin_change(_ctx: &Context, _hidden: bool) -> bool {
    let mut phase = phase();
    if *phase == Phase::Changing {
        return false;
    }
    *phase = Phase::Changing;
    true
}

pub fn applied(_ctx: &Context) {
    *phase() = Phase::Idle;
}

pub fn note_ui_frame(_ctx: &Context) {}

pub fn poll(_ctx: &Context) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_toggle_at_a_time_until_applied() {
        let ctx = Context::default();
        cancel_queued_toggle();
        assert!(queue_toggle());
        assert!(!queue_toggle());
        assert!(begin_change(&ctx, true));
        assert!(!begin_change(&ctx, false));
        applied(&ctx);
        assert!(queue_toggle());
        cancel_queued_toggle();
    }

    #[test]
    fn stalled_startup_or_toggle_releases_controls_without_replaying_requests() {
        for hidden in [true, false] {
            for drawn in [false, true] {
                let mut gate = Gate::new();
                assert!(gate.begin(hidden, 0));
                let Phase::Changing(mut pending) = gate.phase else {
                    unreachable!()
                };
                pending.applied = true;
                pending.drawn_frame = drawn.then_some(1);
                // Cover both a missing first frame and a stalled shell update.
                pending.taskbar = drawn.then_some(1);
                gate.phase = Phase::Changing(pending);
                assert_eq!(
                    gate.settling(1),
                    Some(pending),
                    "native recovery must run without a drawn UI frame"
                );
                if !hidden && !drawn {
                    assert!(gate.candidate(1).is_none());
                }
                assert!(
                    gate.expire(pending.started_at + Duration::from_secs(2))
                        .is_none()
                );
                assert!(!gate.queue());
                assert_eq!(
                    gate.expire(pending.started_at + Duration::from_secs(3)),
                    Some(pending)
                );
                assert_eq!(gate.phase, Phase::Idle);
                assert!(!gate.complete(pending, 1, true));
                assert!(gate.queue());
                assert!(!gate.queue());
                assert!(gate.begin(!hidden, 2));
            }
        }
    }
}
