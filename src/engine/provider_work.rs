//! Drives the coordinator's pause and restore units for the engine. The two
//! loops are kept separate: they complete and are interrupted differently.
//! The continuation is taken and returned here so no adapter clock, claim or
//! completion evidence is dropped between calls.

use super::Engine;
use crate::{
    control::Activity,
    lmstudio::Backend,
    recovery::{Intent, Journal},
};
use anyhow::{Result, bail};
use std::time::{Duration, Instant};
impl<B: Backend> Engine<B> {
    pub(super) fn pause_units(
        &mut self,
        cancelled: &mut dyn FnMut() -> bool,
        only_lm: bool,
    ) -> Result<()> {
        use crate::coordinator::{Coordinator, JournalFile, State, Step};
        let journal = self.projected_journal()?;
        let memory = self.take_continuation(&journal)?;
        let progress = if memory.is_some() {
            self.adapter_progress.take().unwrap_or_default()
        } else {
            Default::default()
        };
        self.set_activity(if !self.pending() {
            Activity::Capturing
        } else {
            Activity::Unloading
        });
        let power_guard = &self.power_guard;
        let control = self.control_config();
        let runtime = crate::provider_runtime::Providers::new(
            &mut self.backend,
            control,
            false,
            &mut self.ollama_runtime,
            &mut self.process_runtime,
        )
        .with_progress(progress);
        let mut bindings = runtime.bindings()?;
        if let Some(kind) = self
            .verify_scope
            .or(only_lm.then_some(crate::provider::Kind::LMStudio))
        {
            bindings.retain(|binding| binding.kind == kind);
        }
        let overhead = bindings.len().saturating_mul(2).saturating_add(4);
        let mut coordinator = if let Some(memory) = memory {
            Coordinator::resume(
                runtime,
                JournalFile(self.path.clone()),
                bindings,
                memory,
                Duration::from_secs_f64(self.config.retry_seconds),
            )?
        } else {
            Coordinator::new(
                runtime,
                JournalFile(self.path.clone()),
                bindings,
                journal,
                Duration::from_secs_f64(self.config.retry_seconds),
            )?
        };
        let began = Instant::now();
        let mut units = 0usize;
        let result = (|| {
            loop {
                let now = self.provider_now.saturating_add(began.elapsed());
                let result =
                    coordinator.advance(Intent::Pause, &self.remembered_games, now, &mut || {
                        power_guard.as_ref().is_some_and(|guard| guard()) || cancelled()
                    });
                if let Some(journal) = coordinator.journal() {
                    self.recovery = Some(journal.clone());
                    let lm = journal
                        .providers
                        .iter()
                        .find(|entry| entry.binding.kind == crate::provider::Kind::LMStudio);
                    self.binding = lm.map(|entry| entry.binding.clone());
                    self.state = lm.map(|entry| entry.payload.lm().cloned()).transpose()?;
                    self.intent = journal.session.intent;
                    if coordinator.persistence_pending()
                        && let Some(snapshot) = &mut self.state
                    {
                        snapshot.pause_complete = false;
                    }
                } else {
                    // A verified retired session can clear before a new capture.
                    self.recovery = None;
                    self.state = None;
                    self.binding = None;
                }
                self.provider_statuses = coordinator.reports(now);
                if let Some(observer) = &mut self.provider_progress {
                    observer(&self.provider_statuses);
                }
                let step = result?;
                if step == Step::Interrupted {
                    if power_guard.as_ref().is_some_and(|guard| guard()) {
                        bail!("Power state changed; pause interrupted and recovery retained");
                    }
                    bail!("Game detected; recovery retained");
                }
                if step == Step::Idle {
                    let failures = coordinator
                        .reports(now)
                        .into_iter()
                        .filter(|status| status.state == State::Failed)
                        .map(|status| format!("{}: {}", status.kind.name(), status.error))
                        .collect::<Vec<_>>();
                    if !failures.is_empty() {
                        bail!("{}", failures.join("; "));
                    }
                    if coordinator
                        .statuses()
                        .values()
                        .any(|status| status.state == State::Deferred)
                    {
                        return Err(crate::provider::InferenceBusy.into());
                    }
                    bail!("Provider pause is incomplete; recovery retained");
                }
                if coordinator.pause_complete() {
                    return Ok(());
                }
                if step == Step::Captured {
                    self.activity = Activity::Unloading;
                    self.message = Activity::Unloading.progress_message().into();
                    if let Some(observer) = &mut self.progress {
                        observer(Activity::Unloading);
                    }
                }
                units += 1;
                if units
                    > self
                        .recovery
                        .as_ref()
                        .map_or(0, |journal| {
                            journal
                                .providers
                                .iter()
                                .map(|entry| match &entry.payload {
                                    crate::recovery::Payload::LMStudio(snapshot) => {
                                        snapshot.models.len()
                                    }
                                    crate::recovery::Payload::Ollama(snapshot) => snapshot.units(),
                                    crate::recovery::Payload::Process(snapshot) => {
                                        snapshot.pause_units()
                                    }
                                })
                                .sum::<usize>()
                        })
                        .saturating_add(overhead)
                {
                    bail!("Provider pause did not converge; recovery retained");
                }
            }
        })();
        let (runtime, _, memory) = coordinator.into_parts();
        self.adapter_progress = Some(runtime.router.lm.progress());
        drop(runtime);
        self.coordinator_memory = Some(memory);
        result
    }
    /// Drain serial restore units, including healthy models after a local failure.
    /// A fresh game guard still runs at every coordinator boundary.
    pub(super) fn restore_units(
        &mut self,
        cancelled: &mut dyn FnMut() -> bool,
        compare: bool,
    ) -> Result<bool> {
        use crate::coordinator::{Coordinator, JournalFile, Step};
        let journal = self.projected_journal()?;
        let limit = journal
            .as_ref()
            .map_or(0, |journal| {
                journal
                    .providers
                    .iter()
                    .map(|entry| match &entry.payload {
                        crate::recovery::Payload::LMStudio(snapshot) => snapshot.models.len(),
                        crate::recovery::Payload::Ollama(snapshot) => snapshot.units(),
                        crate::recovery::Payload::Process(snapshot) => snapshot.units(),
                    })
                    .sum::<usize>()
                    .saturating_mul(2)
                    .saturating_add(journal.providers.len().saturating_mul(4))
            })
            .saturating_add(6)
            .saturating_add(crate::lmstudio::MAX_RESIDENTS);
        let memory = self.take_continuation(&journal)?;
        let progress = if memory.is_some() {
            self.adapter_progress.take().unwrap_or_default()
        } else {
            Default::default()
        };
        let power_guard = &self.power_guard;
        let control = self.control_config();
        let runtime = crate::provider_runtime::Providers::new(
            &mut self.backend,
            control,
            compare,
            &mut self.ollama_runtime,
            &mut self.process_runtime,
        )
        .with_progress(progress);
        let mut bindings = runtime.bindings()?;
        // Raw field comparison is an LM capability; verification scopes each
        // phase to the provider it is testing.
        if let Some(kind) = self
            .verify_scope
            .or(compare.then_some(crate::provider::Kind::LMStudio))
        {
            bindings.retain(|binding| binding.kind == kind);
        }
        let mut coordinator = if let Some(memory) = memory {
            Coordinator::resume(
                runtime,
                JournalFile(self.path.clone()),
                bindings,
                memory,
                Duration::from_secs_f64(self.config.retry_seconds),
            )?
        } else {
            Coordinator::new(
                runtime,
                JournalFile(self.path.clone()),
                bindings,
                journal,
                Duration::from_secs_f64(self.config.retry_seconds),
            )?
        };
        let began = Instant::now();
        let result = (|| {
            for _ in 0..limit {
                let now = self.provider_now.saturating_add(began.elapsed());
                let result =
                    coordinator.advance(Intent::Restore, &self.remembered_games, now, &mut || {
                        power_guard.as_ref().is_some_and(|guard| guard()) || cancelled()
                    });
                // Preserve durable/dirty progress on every result, including failed writes.
                if let Some(journal) = coordinator.journal() {
                    self.recovery = Some(journal.clone());
                    let lm = journal
                        .providers
                        .iter()
                        .find(|entry| entry.binding.kind == crate::provider::Kind::LMStudio);
                    self.binding = lm.map(|entry| entry.binding.clone());
                    self.state = lm.map(|entry| entry.payload.lm().cloned()).transpose()?;
                    self.intent = journal.session.intent;
                    if coordinator.persistence_pending()
                        && let Some(snapshot) = &mut self.state
                    {
                        snapshot.pause_complete = false;
                    }
                }
                self.provider_statuses = coordinator.reports(now);
                if let Some(observer) = &mut self.provider_progress {
                    observer(&self.provider_statuses);
                }
                match result? {
                    Step::Completed => return Ok(true),
                    Step::Interrupted => return Ok(false),
                    Step::Idle => {
                        let failures = coordinator
                            .reports(now)
                            .into_iter()
                            .filter(|status| !status.error.is_empty())
                            .map(|status| format!("{}: {}", status.kind.name(), status.error))
                            .collect::<Vec<_>>();
                        bail!("{}; recovery retained", failures.join("; "));
                    }
                    _ => {}
                }
            }
            bail!("Provider restoration did not converge; recovery retained")
        })();
        let (runtime, _, memory) = coordinator.into_parts();
        self.adapter_progress = Some(runtime.router.lm.progress());
        drop(runtime);
        self.coordinator_memory = Some(memory);
        result
    }
    pub(super) fn provider_work_ready(&self, intent: Intent, now: f64) -> bool {
        self.coordinator_memory
            .as_ref()
            .map_or(now >= self.retry_at, |memory| {
                if memory.persistence_pending() {
                    now >= self.retry_at
                } else {
                    memory.ready(intent, self.provider_now)
                }
            })
    }
    pub(super) fn retry_providers(&mut self) {
        if let Some(memory) = &mut self.coordinator_memory {
            memory.request_retry();
        }
    }
    pub(super) fn take_continuation(
        &mut self,
        journal: &Option<Journal>,
    ) -> Result<Option<crate::coordinator::Continuation<crate::recovery::Payload>>> {
        if let Some(memory) = &self.coordinator_memory {
            let mut expected = memory.journal().cloned();
            if let Some(expected) = &mut expected {
                for entry in &mut expected.providers {
                    if memory.persistence_pending()
                        && let crate::recovery::Payload::LMStudio(snapshot) = &mut entry.payload
                    {
                        snapshot.pause_complete = false;
                    }
                }
            }
            if &expected != journal {
                bail!(
                    "Recovery view changed outside the coordinator; control held and recovery retained"
                );
            }
        }
        Ok(self.coordinator_memory.take())
    }
}
