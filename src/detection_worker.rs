//! One periodic scanner, one replaceable input, and one latest result.
//! Provider work never runs here. Fresh guards wait for a newer scan, not a cache.
use anyhow::{Result, bail};
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

struct Mailbox<I, O> {
    input: I,
    requested: u64,
    completed: u64,
    output: Option<O>,
    stopped: bool,
}
type Shared<I, O> = Arc<(Mutex<Mailbox<I, O>>, Condvar)>;

pub(crate) struct DetectionWorker<I, O> {
    shared: Shared<I, O>,
    thread: Option<JoinHandle<()>>,
}
impl<I: Clone + Send + 'static, O: Clone + Send + 'static> DetectionWorker<I, O> {
    pub fn start(
        input: I,
        period: impl Fn(&I) -> Duration + Send + 'static,
        mut scan: impl FnMut(&I) -> O + Send + 'static,
        mut publish: impl FnMut(Option<&O>) + Send + 'static,
    ) -> Result<Self> {
        let shared = Arc::new((
            Mutex::new(Mailbox {
                input,
                requested: 1,
                completed: 0,
                output: None,
                stopped: false,
            }),
            Condvar::new(),
        ));
        let worker = shared.clone();
        let thread = thread::Builder::new()
            .name("gamepause-detection".into())
            .spawn(move || {
                let run = || {
                    loop {
                        let (input, sequence) = {
                            let Ok(mailbox) = worker.0.lock() else {
                                break;
                            };
                            if mailbox.stopped {
                                break;
                            }
                            (mailbox.input.clone(), mailbox.requested)
                        };
                        // No mailbox/UI lock spans native process enumeration.
                        let output = scan(&input);
                        let Ok(mut mailbox) = worker.0.lock() else {
                            break;
                        };
                        if mailbox.stopped {
                            break;
                        }
                        if mailbox.requested != sequence {
                            // A settings/inventory change invalidates this older scan.
                            continue;
                        }
                        mailbox.output = Some(output.clone());
                        mailbox.completed = sequence;
                        worker.1.notify_all();
                        drop(mailbox);
                        publish(Some(&output));
                        let Ok(mailbox) = worker.0.lock() else {
                            break;
                        };
                        let Ok((mailbox, _)) =
                            worker
                                .1
                                .wait_timeout_while(mailbox, period(&input), |mailbox| {
                                    !mailbox.stopped && mailbox.requested == sequence
                                })
                        else {
                            break;
                        };
                        if mailbox.stopped {
                            break;
                        }
                    }
                };
                // Mark terminal on panic as well, so guards wake and fail closed.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
                if let Ok(mut mailbox) = worker.0.lock() {
                    mailbox.stopped = true;
                    worker.1.notify_all();
                }
                publish(None);
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// Update the single pending input and require a scan begun after this request.
    /// Timeout is unknown detection; it never returns an older successful result.
    pub fn fresh(&self, input: I, timeout: Duration) -> Result<O> {
        self.fresh_with(timeout, |current| *current = input)
    }
    pub fn fresh_with(&self, timeout: Duration, update: impl FnOnce(&mut I)) -> Result<O> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow::anyhow!("Invalid detection deadline"))?;
        let mut mailbox = self
            .shared
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Detection mailbox unavailable"))?;
        if mailbox.stopped {
            bail!("Detection worker stopped");
        }
        update(&mut mailbox.input);
        mailbox.requested = mailbox
            .requested
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Detection sequence exhausted"))?;
        let sequence = mailbox.requested;
        self.shared.1.notify_all();
        while !mailbox.stopped && mailbox.completed < sequence {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                bail!("Fresh game detection timed out; AI control held");
            }
            mailbox = self
                .shared
                .1
                .wait_timeout(mailbox, remaining)
                .map_err(|_| anyhow::anyhow!("Detection mailbox unavailable"))?
                .0;
        }
        if mailbox.stopped {
            bail!("Detection worker stopped");
        }
        mailbox
            .output
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Fresh game detection unavailable"))
    }
}
impl<I, O> Drop for DetectionWorker<I, O> {
    fn drop(&mut self) {
        if let Ok(mut mailbox) = self.shared.0.lock() {
            mailbox.stopped = true;
            self.shared.1.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn game_seen_during_a_slow_load_interrupts_engine_before_the_next_model() {
        use crate::{
            config::{Config, write_json},
            engine::Engine,
            lmstudio::{Backend, Model, Snapshot},
            recovery::{Binding, Intent, Journal},
        };
        use serde_json::{Value, json};
        struct Slow {
            entered: mpsc::Sender<()>,
            held: mpsc::Receiver<()>,
            loaded: Vec<String>,
        }
        impl Backend for Slow {
            fn snapshot(&mut self) -> Result<Snapshot> {
                bail!("fixture capture unused")
            }
            fn server_state(&mut self) -> Result<Value> {
                Ok(json!({"running":true,"port":1234}))
            }
            fn loaded(&mut self) -> Result<Vec<Value>> {
                Ok(self
                    .loaded
                    .iter()
                    .map(|id| json!({"identifier":id}))
                    .collect())
            }
            fn stop_server(&mut self) -> Result<()> {
                bail!("fixture stop unused")
            }
            fn start_server(&mut self, _: u16) -> Result<()> {
                bail!("fixture start unused")
            }
            fn ensure_server(&mut self, _: u16) -> Result<()> {
                Ok(())
            }
            fn unload(&mut self, _: &str) -> Result<()> {
                bail!("fixture unload unused")
            }
            fn restore(&mut self, model: &Model) -> Result<()> {
                if model.identifier == "first" {
                    self.entered.send(())?;
                    self.held.recv_timeout(Duration::from_secs(3))?;
                }
                self.loaded.push(model.identifier.clone());
                Ok(())
            }
            fn read_config(&mut self, _: &Model) -> Result<Value> {
                Ok(json!({"fields":[]}))
            }
        }
        let folder = std::env::temp_dir().join(format!(
            "gamepause-detection-engine-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&folder).unwrap();
        let path = folder.join("state.json");
        let config = Config::default();
        let snapshot = Snapshot {
            schema: 2,
            server: json!({"running":true,"port":1234}),
            server_stopped: true,
            pause_complete: true,
            games: vec![],
            models: ["first", "second"]
                .into_iter()
                .map(|id| Model {
                    identifier: id.into(),
                    model_key: id.into(),
                    base_key: id.into(),
                    namespace: "llm".into(),
                    ttl_ms: None,
                    load_config: json!({"fields":[]}),
                    native_config: json!({}),
                    stage: "unloaded".into(),
                })
                .collect(),
        };
        let journal = Journal::lm(
            Binding::capture(&config, &snapshot).unwrap(),
            snapshot,
            Intent::Pause,
        );
        write_json(&path, &journal).unwrap();
        let gaming = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let sample = gaming.clone();
        let (observed, observations) = mpsc::channel();
        let worker = Arc::new(
            DetectionWorker::start(
                (),
                |_| Duration::from_millis(20),
                move |_| sample.load(std::sync::atomic::Ordering::SeqCst),
                move |frame| {
                    if frame == Some(&true) {
                        observed.send(()).unwrap();
                    }
                },
            )
            .unwrap(),
        );
        let (entered, started) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let mut engine = Engine::new(
            config,
            Slow {
                entered,
                held,
                loaded: vec![],
            },
            path.clone(),
        )
        .unwrap();
        let guard = worker.clone();
        let control = thread::spawn(move || {
            engine
                .restore(&mut || guard.fresh((), Duration::from_secs(1)).unwrap_or(true))
                .unwrap();
            engine
        });
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        gaming.store(true, std::sync::atomic::Ordering::SeqCst);
        for _ in 0..3 {
            observations.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        assert!(!control.is_finished(), "fixture load must still be blocked");
        release.send(()).unwrap();
        let engine = control.join().unwrap();
        assert_eq!(engine.backend.loaded, vec!["first"]);
        assert!(engine.state.is_some());
        assert_eq!(engine.restore_completions, 0);
        let journal: Journal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(!journal.providers[0].restore_complete);
        let snapshot = journal.providers[0].payload.lm().unwrap();
        assert_eq!(snapshot.models[0].stage, "restored");
        assert_eq!(snapshot.models[1].stage, "unloaded");
        drop(engine);
        drop(worker);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn periodic_detection_continues_during_slow_control_and_fresh_guard_sees_new_game() {
        let gaming = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let scanner = gaming.clone();
        let (tx, rx) = mpsc::channel();
        let worker = DetectionWorker::start(
            (),
            |_| Duration::from_millis(20),
            move |_| scanner.load(std::sync::atomic::Ordering::SeqCst),
            move |value| {
                if let Some(value) = value {
                    tx.send((Instant::now(), *value)).unwrap();
                }
            },
        )
        .unwrap();
        worker.fresh((), Duration::from_secs(1)).unwrap();
        let start = Instant::now();
        // Control remains blocked until the test releases its mock operation.
        let (release, hold) = mpsc::channel();
        let control = thread::spawn(move || {
            hold.recv_timeout(Duration::from_secs(2)).unwrap();
        });
        gaming.store(true, std::sync::atomic::Ordering::SeqCst);
        let mut times = Vec::new();
        while times.len() < 6 {
            let (at, active) = rx.recv_timeout(Duration::from_secs(1)).unwrap();
            if active {
                times.push(at);
            }
        }
        assert!(!control.is_finished());
        let maximum_gap = times
            .windows(2)
            .map(|pair| pair[1].duration_since(pair[0]))
            .max()
            .unwrap();
        eprintln!(
            "Mock blocked-control observation: {} scans in {:?}, maximum gap {:?}",
            times.len(),
            start.elapsed(),
            maximum_gap
        );
        assert!(
            maximum_gap < Duration::from_millis(500),
            "scanner stalled behind control"
        );
        assert!(worker.fresh((), Duration::from_secs(1)).unwrap());
        release.send(()).unwrap();
        control.join().unwrap();
    }

    #[test]
    fn timed_out_guard_never_uses_cached_success_and_replacement_discards_old_scan() {
        let (entered, starts) = mpsc::channel();
        let (release, hold) = mpsc::channel();
        let worker = DetectionWorker::start(
            0u8,
            |_| Duration::from_secs(10),
            move |input| {
                if *input == 1 {
                    entered.send(()).unwrap();
                    hold.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                *input
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(worker.fresh(0, Duration::from_secs(1)).unwrap(), 0);
        thread::scope(|scope| {
            let request = scope.spawn(|| worker.fresh(1, Duration::from_millis(40)));
            starts.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(request.join().unwrap().is_err());
            let replacement = scope.spawn(|| worker.fresh(2, Duration::from_secs(1)));
            // Ensure the newer input is installed before the old scan returns.
            let deadline = Instant::now() + Duration::from_secs(1);
            while worker.shared.0.lock().unwrap().input != 2 {
                assert!(
                    Instant::now() < deadline,
                    "replacement request was not scheduled"
                );
                thread::yield_now();
            }
            release.send(()).unwrap();
            assert_eq!(replacement.join().unwrap().unwrap(), 2);
        });
    }

    #[test]
    fn scanner_panic_wakes_waiters_and_stops_without_respawn() {
        let worker = DetectionWorker::start(
            (),
            |_| Duration::from_secs(10),
            |_| -> bool { panic!("fixture scanner panic") },
            |_| {},
        )
        .unwrap();
        assert!(worker.fresh((), Duration::from_secs(1)).is_err());
        assert!(worker.fresh((), Duration::from_secs(1)).is_err());
    }
}
