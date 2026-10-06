//! Bounded completion notification policy. No provider work or clock reads.
use crate::control::Activity;
use std::time::Duration;

/// Initial UX acceptance value, not a promise of fullscreen delivery.
pub const SUCCESS_DELAY: Duration = Duration::from_secs(2);
/// Discard successes after a stalled UI/sleep rather than announce old state.
const MAX_DELIVERY_LAG: Duration = Duration::from_secs(10);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Paused,
    Restored,
    Failure,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    pub text: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Delivery {
    Toast { event: Event, sound: bool },
    Sound(Event),
}
pub trait Sink {
    fn toast(&mut self, event: &Event, sound: bool) -> bool;
    fn sound(&mut self, event: &Event) -> bool;
}
pub fn deliver(delivery: &Delivery, sink: &mut impl Sink) -> bool {
    match delivery {
        Delivery::Toast { event, sound } => sink.toast(event, *sound),
        Delivery::Sound(event) => sink.sound(event),
    }
}
pub struct Input<'a> {
    pub pause: u64,
    pub restore: u64,
    pub activity: Activity,
    pub pending: bool,
    pub failure: Option<&'a str>,
    /// Approximate bytes released by the pause being announced; 0 if unknown.
    pub freed: u64,
}
#[derive(Clone, Default)]
pub struct Queue {
    pause: u64,
    restore: u64,
    last_failure: bool,
    queued: Option<(Event, Duration)>,
}
impl Queue {
    pub fn poll(
        &mut self,
        now: Duration,
        input: Input<'_>,
        visual: bool,
        sound: bool,
    ) -> Option<Delivery> {
        let restored = input.restore != self.restore;
        let paused = input.pause != self.pause;
        self.pause = input.pause;
        self.restore = input.restore;
        let pause_current = matches!(
            input.activity,
            Activity::Paused | Activity::ManualHold | Activity::Countdown
        ) && input.pending;
        let restore_current =
            matches!(input.activity, Activity::Watching | Activity::Coexistence) && !input.pending;
        let event = if let Some(failure) = input.failure {
            self.queued = None;
            if self.last_failure {
                return None;
            }
            self.last_failure = true;
            let text = failure.chars().take(512).collect::<String>();
            Some(Event {
                kind: Kind::Failure,
                text,
            })
        } else {
            if pause_current || restore_current {
                self.last_failure = false;
            }
            if restored || paused {
                self.queued = if restored && restore_current {
                    Some((
                        Event {
                            kind: Kind::Restored,
                            text: "AI restored: saved recovery verified.".into(),
                        },
                        now.saturating_add(SUCCESS_DELAY),
                    ))
                } else if !restored && paused && pause_current {
                    Some((
                        Event {
                            kind: Kind::Paused,
                            text: if input.freed > 0 {
                                format!(
                                    "AI paused: about {} freed.",
                                    crate::presentation::size(input.freed)
                                )
                            } else {
                                "AI paused: captured-model unload verified.".into()
                            },
                        },
                        now.saturating_add(SUCCESS_DELAY),
                    ))
                } else {
                    None
                };
            }
            if self
                .queued
                .as_ref()
                .is_some_and(|(_, deadline)| now > deadline.saturating_add(MAX_DELIVERY_LAG))
            {
                self.queued = None;
            }
            if self
                .queued
                .as_ref()
                .is_some_and(|(event, _)| match event.kind {
                    Kind::Paused => !pause_current,
                    Kind::Restored => !restore_current,
                    Kind::Failure => true,
                })
            {
                self.queued = None;
            }
            if self
                .queued
                .as_ref()
                .is_some_and(|(_, deadline)| now >= *deadline)
            {
                self.queued.take().map(|(event, _)| event)
            } else {
                None
            }
        }?;
        if visual {
            Some(Delivery::Toast { event, sound })
        } else if sound {
            Some(Delivery::Sound(event))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Fake {
        calls: Vec<(bool, bool, Kind)>,
        fail: bool,
    }
    impl Sink for Fake {
        fn toast(&mut self, event: &Event, sound: bool) -> bool {
            self.calls.push((true, sound, event.kind));
            !self.fail
        }
        fn sound(&mut self, event: &Event) -> bool {
            self.calls.push((false, true, event.kind));
            !self.fail
        }
    }
    fn paused() -> Input<'static> {
        Input {
            pause: 1,
            restore: 0,
            activity: Activity::Paused,
            pending: true,
            failure: None,
            freed: 0,
        }
    }
    fn restored() -> Input<'static> {
        Input {
            pause: 1,
            restore: 1,
            activity: Activity::Watching,
            pending: false,
            failure: None,
            freed: 0,
        }
    }
    #[test]
    fn preferences_choose_exactly_one_sound_source_and_never_retry_failed_delivery() {
        for (visual, sound) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut queue = Queue::default();
            let mut sink = Fake {
                fail: true,
                ..Default::default()
            };
            assert!(
                queue
                    .poll(Duration::ZERO, paused(), visual, sound)
                    .is_none()
            );
            let delivery = queue.poll(SUCCESS_DELAY, paused(), visual, sound);
            if let Some(delivery) = delivery {
                assert!(!deliver(&delivery, &mut sink));
            }
            assert_eq!(
                sink.calls,
                if visual || sound {
                    vec![(visual, sound, Kind::Paused)]
                } else {
                    vec![]
                }
            );
            assert!(
                queue
                    .poll(Duration::from_secs(20), paused(), visual, sound)
                    .is_none()
            );
            assert_eq!(sink.calls.len(), usize::from(visual || sound));
        }
    }
    #[test]
    fn rapid_restore_replaces_pause_and_starting_work_discards_stale_success() {
        let mut queue = Queue::default();
        assert!(queue.poll(Duration::ZERO, paused(), true, true).is_none());
        assert!(
            queue
                .poll(Duration::from_secs(1), restored(), true, true)
                .is_none()
        );
        assert!(
            queue
                .poll(Duration::from_secs(2), restored(), true, true)
                .is_none()
        );
        assert!(matches!(
            queue.poll(Duration::from_secs(3), restored(), true, true),
            Some(Delivery::Toast {
                event: Event {
                    kind: Kind::Restored,
                    ..
                },
                ..
            })
        ));
        let mut queue = Queue::default();
        queue.poll(Duration::ZERO, paused(), true, true);
        let mut input = paused();
        input.activity = Activity::Restoring;
        assert!(queue.poll(SUCCESS_DELAY, input, true, true).is_none());
        assert!(
            queue
                .poll(Duration::from_secs(10), paused(), true, true)
                .is_none()
        );
    }
    #[test]
    fn failures_are_immediate_deduplicated_and_partial_restore_is_not_success() {
        let mut queue = Queue::default();
        queue.poll(Duration::ZERO, paused(), true, true);
        for time in 1..4 {
            let input = Input {
                pause: 1,
                restore: 1,
                activity: Activity::PartialFailure,
                pending: true,
                failure: Some(if time == 1 {
                    "LM Studio recovery retained: one model failed"
                } else {
                    "LM Studio recovery retry countdown changed"
                }),
                freed: 0,
            };
            let delivery = queue.poll(Duration::from_secs(time), input, true, false);
            if time == 1 {
                assert!(matches!(
                    delivery,
                    Some(Delivery::Toast {
                        event: Event {
                            kind: Kind::Failure,
                            ..
                        },
                        sound: false
                    })
                ));
            } else {
                assert!(delivery.is_none());
            }
        }
        assert!(
            queue
                .poll(Duration::from_secs(10), restored(), true, true)
                .is_none()
        );
    }
    #[test]
    fn stalled_ui_discards_an_old_success_without_repeating_it() {
        let mut queue = Queue::default();
        queue.poll(Duration::ZERO, paused(), true, true);
        assert!(
            queue
                .poll(Duration::from_secs(60), paused(), true, true)
                .is_none()
        );
        assert!(
            queue
                .poll(Duration::from_secs(61), paused(), true, true)
                .is_none()
        );
    }
    #[test]
    fn current_preferences_apply_at_delivery_and_silent_events_do_not_resurrect() {
        let mut queue = Queue::default();
        queue.poll(Duration::ZERO, paused(), true, true);
        assert!(queue.poll(SUCCESS_DELAY, paused(), false, false).is_none());
        assert!(
            queue
                .poll(Duration::from_secs(3), paused(), true, true)
                .is_none()
        );
        queue.poll(Duration::from_secs(4), restored(), false, false);
        assert!(matches!(
            queue.poll(Duration::from_secs(6), restored(), true, false),
            Some(Delivery::Toast { sound: false, .. })
        ));
    }

    #[test]
    fn pause_notification_names_the_freed_memory_when_it_is_known() {
        let input = || Input {
            freed: 18_448_625_171,
            ..paused()
        };
        let mut queue = Queue::default();
        assert!(queue.poll(Duration::ZERO, input(), true, false).is_none());
        let delivered = queue.poll(Duration::from_secs(3), input(), true, false);
        assert!(format!("{delivered:?}").contains("AI paused: about 17.2 GB freed."));
    }
}
