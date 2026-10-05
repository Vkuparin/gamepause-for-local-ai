//! Kind-checked dispatch of independent adapter runtimes.
use crate::{
    config::{Config, normalized_endpoint},
    coordinator::{Outcome, Planned, Runtime},
    discovery::Game,
    lm_session::{LMRuntime, Work as LMWork},
    lmstudio::Backend,
    provider::{Guarantee, Kind},
    recovery::{Binding, Entry, Intent, Payload},
};
use anyhow::{Result, bail};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(
    tag = "kind",
    content = "snapshot",
    rename_all = "lowercase",
    deny_unknown_fields
)]
pub enum Routed<L, O> {
    LMStudio(L),
    Ollama(O),
}
pub struct Work<L, O> {
    binding: Binding,
    operation: Routed<L, O>,
}
#[derive(Clone)]
pub struct Router<L, O> {
    pub lm: L,
    pub ollama: O,
}
fn lm_entry<L: Clone, O>(entry: &Entry<Routed<L, O>>) -> Result<Entry<L>> {
    match &entry.payload {
        Routed::LMStudio(payload) if entry.binding.kind == Kind::LMStudio => Ok(Entry {
            binding: entry.binding.clone(),
            payload: payload.clone(),
            restore_complete: entry.restore_complete,
        }),
        _ => bail!("Provider kind/payload mismatch; recovery retained"),
    }
}
fn ollama_entry<L, O: Clone>(entry: &Entry<Routed<L, O>>) -> Result<Entry<O>> {
    match &entry.payload {
        Routed::Ollama(payload) if entry.binding.kind == Kind::Ollama => Ok(Entry {
            binding: entry.binding.clone(),
            payload: payload.clone(),
            restore_complete: entry.restore_complete,
        }),
        _ => bail!("Provider kind/payload mismatch; recovery retained"),
    }
}
impl<L: Runtime, O: Runtime> Runtime for Router<L, O> {
    type Payload = Routed<L::Payload, O::Payload>;
    type Work = Work<L::Work, O::Work>;
    fn capture(&mut self, binding: &Binding, games: &[Game]) -> Result<Entry<Self::Payload>> {
        Ok(match binding.kind {
            Kind::LMStudio => {
                let entry = self.lm.capture(binding, games)?;
                Entry {
                    binding: entry.binding,
                    payload: Routed::LMStudio(entry.payload),
                    restore_complete: entry.restore_complete,
                }
            }
            Kind::Ollama => {
                let entry = self.ollama.capture(binding, games)?;
                Entry {
                    binding: entry.binding,
                    payload: Routed::Ollama(entry.payload),
                    restore_complete: entry.restore_complete,
                }
            }
        })
    }
    fn validate(&self, entry: &Entry<Self::Payload>, games: &[Game]) -> Result<()> {
        match entry.binding.kind {
            Kind::LMStudio => self.lm.validate(&lm_entry(entry)?, games),
            Kind::Ollama => self.ollama.validate(&ollama_entry(entry)?, games),
        }
    }
    fn validate_transition(&self, original: &Self::Payload, updated: &Self::Payload) -> Result<()> {
        match (original, updated) {
            (Routed::LMStudio(original), Routed::LMStudio(updated)) => {
                self.lm.validate_transition(original, updated)
            }
            (Routed::Ollama(original), Routed::Ollama(updated)) => {
                self.ollama.validate_transition(original, updated)
            }
            _ => bail!("Provider payload reassignment refused; recovery retained"),
        }
    }
    fn remember_games(&self, payload: &mut Self::Payload, games: &[Game]) {
        match payload {
            Routed::LMStudio(payload) => self.lm.remember_games(payload, games),
            Routed::Ollama(payload) => self.ollama.remember_games(payload, games),
        }
    }
    fn begin(&self, payload: &mut Self::Payload, intent: Intent) -> Result<()> {
        match payload {
            Routed::LMStudio(payload) => self.lm.begin(payload, intent),
            Routed::Ollama(payload) => self.ollama.begin(payload, intent),
        }
    }
    fn complete(&self, payload: &Self::Payload, intent: Intent) -> bool {
        match payload {
            Routed::LMStudio(payload) => self.lm.complete(payload, intent),
            Routed::Ollama(payload) => self.ollama.complete(payload, intent),
        }
    }
    fn can_continue(&self, entry: &Entry<Self::Payload>, intent: Intent) -> bool {
        match entry.binding.kind {
            Kind::LMStudio => {
                lm_entry(entry).is_ok_and(|entry| self.lm.can_continue(&entry, intent))
            }
            Kind::Ollama => {
                ollama_entry(entry).is_ok_and(|entry| self.ollama.can_continue(&entry, intent))
            }
        }
    }
    fn retry(&self, binding: &Binding) {
        match binding.kind {
            Kind::LMStudio => self.lm.retry(binding),
            Kind::Ollama => self.ollama.retry(binding),
        }
    }
    fn note(&self, payload: &Self::Payload) -> String {
        match payload {
            Routed::LMStudio(payload) => self.lm.note(payload),
            Routed::Ollama(payload) => self.ollama.note(payload),
        }
    }
    fn plan(
        &mut self,
        entry: &Entry<Self::Payload>,
        intent: Intent,
    ) -> Result<Planned<Self::Payload, Self::Work>> {
        let (checkpoint, operation) = match entry.binding.kind {
            Kind::LMStudio => {
                let planned = self.lm.plan(&lm_entry(entry)?, intent)?;
                (
                    Routed::LMStudio(planned.checkpoint),
                    Routed::LMStudio(planned.work),
                )
            }
            Kind::Ollama => {
                let planned = self.ollama.plan(&ollama_entry(entry)?, intent)?;
                (
                    Routed::Ollama(planned.checkpoint),
                    Routed::Ollama(planned.work),
                )
            }
        };
        Ok(Planned {
            checkpoint,
            work: Work {
                binding: entry.binding.clone(),
                operation,
            },
        })
    }
    fn execute(&mut self, binding: &Binding, work: Self::Work) -> Result<Outcome<Self::Payload>> {
        // Never execute a queued unit against another entry, even of the same kind.
        if serde_json::to_value(binding)? != serde_json::to_value(&work.binding)? {
            bail!("Planned provider binding changed; control refused and recovery retained");
        }
        Ok(match (binding.kind, work.operation) {
            (Kind::LMStudio, Routed::LMStudio(work)) => {
                let outcome = self.lm.execute(binding, work)?;
                Outcome {
                    payload: Routed::LMStudio(outcome.payload),
                    restore_complete: outcome.restore_complete,
                }
            }
            (Kind::Ollama, Routed::Ollama(work)) => {
                let outcome = self.ollama.execute(binding, work)?;
                Outcome {
                    payload: Routed::Ollama(outcome.payload),
                    restore_complete: outcome.restore_complete,
                }
            }
            _ => bail!("Provider work kind mismatch; control refused"),
        })
    }
}

/// Loopback transport with a control claim retained by the engine-owned adapter.
pub struct ClaimedHttp {
    http: crate::ollama_session::Http,
    binding: Binding,
    claims: crate::ownership::SharedClaims,
}
impl ClaimedHttp {
    fn claim(&self) -> Result<()> {
        self.claims
            .lock()
            .map_err(|_| anyhow::anyhow!("Ollama ownership lock failed"))?
            .claim(&self.binding.id, Kind::Ollama, &[&self.binding.endpoint])
    }
}
impl crate::ollama_session::Transport for ClaimedHttp {
    fn request(&mut self, path: &str, body: Option<serde_json::Value>) -> Result<Vec<u8>> {
        if path == "/api/generate" {
            self.claim()?;
        }
        crate::ollama_session::Transport::request(&mut self.http, path, body)
    }
}
#[derive(Default)]
pub struct OllamaRuntime {
    adapter: Option<crate::ollama_session::Adapter<ClaimedHttp>>,
    active: bool,
    error: String,
}
impl Runtime for OllamaRuntime {
    type Payload = crate::ollama_session::Snapshot;
    type Work = crate::ollama_session::Work;
    fn capture(&mut self, binding: &Binding, games: &[Game]) -> Result<Entry<Self::Payload>> {
        let adapter = self
            .adapter
            .as_mut()
            .filter(|_| self.active)
            .ok_or_else(|| anyhow::anyhow!("Ollama runtime unavailable: {}", self.error))?;
        adapter.transport.claim()?;
        adapter.capture(binding, games)
    }
    fn validate(&self, entry: &Entry<Self::Payload>, games: &[Game]) -> Result<()> {
        if !entry.restore_complete
            && let Some(adapter) = &self.adapter
        {
            return adapter.validate(entry, games);
        }
        crate::recovery::Journal {
            schema: 3,
            session: crate::recovery::Session {
                games: games.to_vec(),
                intent: if entry.restore_complete {
                    Intent::Restore
                } else {
                    Intent::Reconcile
                },
            },
            providers: vec![Entry {
                binding: entry.binding.clone(),
                payload: Payload::Ollama(entry.payload.clone()),
                restore_complete: entry.restore_complete,
            }],
        }
        .validate()
    }
    fn validate_transition(&self, old: &Self::Payload, new: &Self::Payload) -> Result<()> {
        old.validate_transition(new)
    }
    fn begin(&self, payload: &mut Self::Payload, _: Intent) -> Result<()> {
        payload.begin();
        Ok(())
    }
    fn complete(&self, payload: &Self::Payload, intent: Intent) -> bool {
        intent == Intent::Pause && payload.pause_complete
    }
    fn retry(&self, binding: &Binding) {
        if let Some(adapter) = &self.adapter {
            adapter.retry(binding);
        }
    }
    fn note(&self, payload: &Self::Payload) -> String {
        payload.note()
    }
    fn plan(
        &mut self,
        entry: &Entry<Self::Payload>,
        intent: Intent,
    ) -> Result<Planned<Self::Payload, Self::Work>> {
        self.adapter
            .as_mut()
            .filter(|_| self.active)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Ollama runtime unavailable: {}; recovery retained",
                    self.error
                )
            })?
            .plan(entry, intent)
    }
    fn execute(&mut self, binding: &Binding, work: Self::Work) -> Result<Outcome<Self::Payload>> {
        self.adapter
            .as_mut()
            .filter(|_| self.active)
            .ok_or_else(|| anyhow::anyhow!("Ollama runtime unavailable; recovery retained"))?
            .execute(binding, work)
    }
}
pub struct OllamaRef<'a>(&'a mut OllamaRuntime);
impl Runtime for OllamaRef<'_> {
    type Payload = crate::ollama_session::Snapshot;
    type Work = crate::ollama_session::Work;
    fn capture(&mut self, binding: &Binding, games: &[Game]) -> Result<Entry<Self::Payload>> {
        self.0.capture(binding, games)
    }
    fn validate(&self, entry: &Entry<Self::Payload>, games: &[Game]) -> Result<()> {
        self.0.validate(entry, games)
    }
    fn validate_transition(&self, old: &Self::Payload, new: &Self::Payload) -> Result<()> {
        self.0.validate_transition(old, new)
    }
    fn begin(&self, payload: &mut Self::Payload, intent: Intent) -> Result<()> {
        self.0.begin(payload, intent)
    }
    fn complete(&self, payload: &Self::Payload, intent: Intent) -> bool {
        self.0.complete(payload, intent)
    }
    fn retry(&self, binding: &Binding) {
        self.0.retry(binding);
    }
    fn note(&self, payload: &Self::Payload) -> String {
        self.0.note(payload)
    }
    fn plan(
        &mut self,
        entry: &Entry<Self::Payload>,
        intent: Intent,
    ) -> Result<Planned<Self::Payload, Self::Work>> {
        self.0.plan(entry, intent)
    }
    fn execute(&mut self, binding: &Binding, work: Self::Work) -> Result<Outcome<Self::Payload>> {
        self.0.execute(binding, work)
    }
}
pub struct Providers<'a, B: Backend> {
    pub router: Router<LMRuntime<B>, OllamaRef<'a>>,
}
impl<'a, B: Backend> Providers<'a, B> {
    pub fn with_progress(mut self, progress: crate::lm_session::Progress) -> Self {
        self.router.lm = self.router.lm.with_progress(progress);
        self
    }
    pub fn new(
        backend: B,
        config: Config,
        verify_raw: bool,
        ollama: &'a mut OllamaRuntime,
    ) -> Self {
        ollama.configure(&config);
        Self {
            router: Router {
                lm: LMRuntime::new(backend, config, verify_raw),
                ollama: OllamaRef(ollama),
            },
        }
    }
}
impl OllamaRuntime {
    fn configure(&mut self, config: &Config) {
        let configured = config
            .providers
            .iter()
            .find(|provider| provider.kind() == Kind::Ollama && provider.enabled());
        self.active = config.mode == "active" && configured.is_some();
        if !self.active {
            // Pending recovery retains its original clock and endpoint ownership.
            // Engine settings replacement releases this only with no obligations.
            self.error = "provider is disabled or observation is active".into();
        } else if self.adapter.is_none() {
            let provider = configured.unwrap();
            let created = (|| -> Result<_> {
                let endpoint = normalized_endpoint(provider.endpoint())?;
                let binding = Binding {
                    id: provider.id().into(),
                    kind: Kind::Ollama,
                    endpoint: endpoint.clone(),
                    configured_endpoint: endpoint.clone(),
                    payload_version: 1,
                    guarantee: Guarantee::SupportedFields,
                };
                let transport = ClaimedHttp {
                    http: crate::ollama_session::Http::new(&endpoint)?,
                    binding: binding.clone(),
                    claims: Default::default(),
                };
                crate::ollama_session::Adapter::new(
                    transport,
                    crate::ollama_session::SystemClock::default(),
                    binding,
                )
            })();
            match created {
                Ok(adapter) => self.adapter = Some(adapter),
                Err(error) => self.error = format!("{error:#}"),
            }
        }
    }
}
impl<B: Backend> Providers<'_, B> {
    pub fn bindings(&self) -> Result<Vec<Binding>> {
        self.router
            .lm
            .config
            .providers
            .iter()
            .filter(|provider| provider.enabled())
            .map(|provider| {
                let endpoint = normalized_endpoint(provider.endpoint())?;
                Ok(Binding {
                    id: provider.id().into(),
                    kind: provider.kind(),
                    endpoint: endpoint.clone(),
                    configured_endpoint: endpoint,
                    payload_version: 1,
                    guarantee: match provider.kind() {
                        Kind::LMStudio => Guarantee::CapturedConfiguration,
                        Kind::Ollama => Guarantee::SupportedFields,
                    },
                })
            })
            .collect()
    }
}
fn wrap(entry: &Entry<Payload>) -> Entry<Routed<Payload, crate::ollama_session::Snapshot>> {
    Entry {
        binding: entry.binding.clone(),
        payload: match &entry.payload {
            Payload::LMStudio(_) => Routed::LMStudio(entry.payload.clone()),
            Payload::Ollama(snapshot) => Routed::Ollama(snapshot.clone()),
        },
        restore_complete: entry.restore_complete,
    }
}
fn unwrap(payload: Routed<Payload, crate::ollama_session::Snapshot>) -> Result<Payload> {
    match payload {
        Routed::LMStudio(payload) => Ok(payload),
        Routed::Ollama(snapshot) => Ok(Payload::Ollama(snapshot)),
    }
}
impl<B: Backend> Runtime for Providers<'_, B> {
    type Payload = Payload;
    type Work = Work<LMWork, crate::ollama_session::Work>;
    fn capture(&mut self, binding: &Binding, games: &[Game]) -> Result<Entry<Payload>> {
        let entry = self.router.capture(binding, games)?;
        Ok(Entry {
            binding: entry.binding,
            payload: unwrap(entry.payload)?,
            restore_complete: entry.restore_complete,
        })
    }
    fn validate(&self, entry: &Entry<Payload>, games: &[Game]) -> Result<()> {
        self.router.validate(&wrap(entry), games)
    }
    fn validate_transition(&self, original: &Payload, updated: &Payload) -> Result<()> {
        match (original, updated) {
            (Payload::LMStudio(_), Payload::LMStudio(_)) => {
                self.router.lm.validate_transition(original, updated)
            }
            (Payload::Ollama(old), Payload::Ollama(new)) => old.validate_transition(new),
            _ => bail!("Provider payload kind changed; all recovery retained"),
        }
    }
    fn remember_games(&self, payload: &mut Payload, games: &[Game]) {
        self.router.lm.remember_games(payload, games);
    }
    fn begin(&self, payload: &mut Payload, intent: Intent) -> Result<()> {
        match payload {
            Payload::LMStudio(_) => self.router.lm.begin(payload, intent),
            Payload::Ollama(snapshot) => {
                snapshot.begin();
                Ok(())
            }
        }
    }
    fn complete(&self, payload: &Payload, intent: Intent) -> bool {
        match payload {
            Payload::LMStudio(_) => self.router.lm.complete(payload, intent),
            Payload::Ollama(snapshot) => intent == Intent::Pause && snapshot.pause_complete,
        }
    }
    fn can_continue(&self, entry: &Entry<Payload>, intent: Intent) -> bool {
        self.router.can_continue(&wrap(entry), intent)
    }
    fn retry(&self, binding: &Binding) {
        self.router.retry(binding);
    }
    fn note(&self, payload: &Payload) -> String {
        match payload {
            Payload::LMStudio(_) => String::new(),
            Payload::Ollama(snapshot) => snapshot.note(),
        }
    }
    fn plan(
        &mut self,
        entry: &Entry<Payload>,
        intent: Intent,
    ) -> Result<Planned<Payload, Self::Work>> {
        let planned = self.router.plan(&wrap(entry), intent)?;
        Ok(Planned {
            checkpoint: unwrap(planned.checkpoint)?,
            work: planned.work,
        })
    }
    fn execute(&mut self, binding: &Binding, work: Self::Work) -> Result<Outcome<Payload>> {
        let outcome = self.router.execute(binding, work)?;
        Ok(Outcome {
            payload: unwrap(outcome.payload)?,
            restore_complete: outcome.restore_complete,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        coordinator::{Coordinator, State, Step, Store},
        recovery::Journal,
    };
    use std::time::Duration;
    trait Data: Clone {
        fn stage(&self) -> u8;
        fn set_stage(&mut self, stage: u8);
        fn original(&self) -> String;
    }
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct LMData {
        raw: String,
        stage: u8,
    }
    impl Data for LMData {
        fn stage(&self) -> u8 {
            self.stage
        }
        fn set_stage(&mut self, stage: u8) {
            self.stage = stage;
        }
        fn original(&self) -> String {
            self.raw.clone()
        }
    }
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct OllamaData {
        expires_at: u64,
        stage: u8,
    }
    impl Data for OllamaData {
        fn stage(&self) -> u8 {
            self.stage
        }
        fn set_stage(&mut self, stage: u8) {
            self.stage = stage;
        }
        fn original(&self) -> String {
            self.expires_at.to_string()
        }
    }
    #[derive(Clone)]
    struct Fake<T> {
        value: T,
        capture_fail: bool,
        fail: Option<u8>,
        after_effect: bool,
        controls: usize,
    }
    impl<T: Data> Runtime for Fake<T> {
        type Payload = T;
        type Work = T;
        fn capture(&mut self, binding: &Binding, _: &[Game]) -> Result<Entry<T>> {
            if self.capture_fail {
                bail!("fixture capture failure");
            }
            Ok(Entry {
                binding: binding.clone(),
                payload: self.value.clone(),
                restore_complete: false,
            })
        }
        fn validate(&self, _: &Entry<T>, _: &[Game]) -> Result<()> {
            Ok(())
        }
        fn validate_transition(&self, old: &T, new: &T) -> Result<()> {
            if old.original() != new.original() {
                bail!("fixture originals changed");
            }
            Ok(())
        }
        fn begin(&self, payload: &mut T, _: Intent) -> Result<()> {
            payload.set_stage(0);
            Ok(())
        }
        fn complete(&self, payload: &T, intent: Intent) -> bool {
            intent == Intent::Pause && payload.stage() == 2
        }
        fn plan(&mut self, entry: &Entry<T>, intent: Intent) -> Result<Planned<T, T>> {
            let mut payload = entry.payload.clone();
            let stage = if intent == Intent::Pause {
                1
            } else if entry.payload.stage() >= 4 {
                5
            } else {
                3
            };
            payload.set_stage(stage);
            Ok(Planned {
                checkpoint: payload.clone(),
                work: payload,
            })
        }
        fn execute(&mut self, _: &Binding, mut work: T) -> Result<Outcome<T>> {
            let stage = work.stage();
            if self.fail == Some(stage) && !self.after_effect {
                bail!("fixture pre-effect failure");
            }
            self.controls += 1;
            work.set_stage(stage + 1);
            self.value = work.clone();
            if self.fail == Some(stage) {
                bail!("fixture lost response after effect");
            }
            Ok(Outcome {
                restore_complete: stage == 5,
                payload: work,
            })
        }
    }
    type TestRouter = Router<Fake<LMData>, Fake<OllamaData>>;
    fn router() -> TestRouter {
        Router {
            lm: Fake {
                value: LMData {
                    raw: "fixture raw LM settings".into(),
                    stage: 0,
                },
                capture_fail: false,
                fail: None,
                after_effect: false,
                controls: 0,
            },
            ollama: Fake {
                value: OllamaData {
                    expires_at: 1234567,
                    stage: 0,
                },
                capture_fail: false,
                fail: None,
                after_effect: false,
                controls: 0,
            },
        }
    }
    fn bindings() -> Vec<Binding> {
        [Kind::LMStudio, Kind::Ollama]
            .into_iter()
            .map(|kind| {
                let (id, endpoint, guarantee) = match kind {
                    Kind::LMStudio => ("lm", "127.0.0.1:1234", Guarantee::CapturedConfiguration),
                    Kind::Ollama => ("ollama", "127.0.0.1:11434", Guarantee::SupportedFields),
                };
                Binding {
                    id: id.into(),
                    kind,
                    endpoint: endpoint.into(),
                    configured_endpoint: endpoint.into(),
                    payload_version: 1,
                    guarantee,
                }
            })
            .collect()
    }
    #[derive(Default)]
    struct Disk {
        journal: Option<Journal<Routed<LMData, OllamaData>>>,
        fail: bool,
        fail_clear: bool,
        writes: usize,
    }
    impl Store<Routed<LMData, OllamaData>> for Disk {
        fn save(&mut self, journal: Option<&Journal<Routed<LMData, OllamaData>>>) -> Result<()> {
            self.writes += 1;
            if self.fail || (self.fail_clear && journal.is_none()) {
                bail!("fixture persistence failure");
            }
            self.journal = journal.map(|journal| {
                serde_json::from_slice(&serde_json::to_vec(journal).unwrap()).unwrap()
            });
            Ok(())
        }
    }
    #[test]
    fn failed_clear_remains_ready_after_continuation_without_repeating_control() {
        let retry = Duration::from_secs(10);
        let mut c = Coordinator::new(router(), Disk::default(), bindings(), None, retry).unwrap();
        for _ in 0..6 {
            c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
                .unwrap();
        }
        c.store.fail_clear = true;
        let mut failed_clear = false;
        for _ in 0..8 {
            if c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
                .is_err()
            {
                failed_clear = true;
                break;
            }
        }
        assert!(failed_clear);
        let controls = (c.runtime.lm.controls, c.runtime.ollama.controls);
        let (runtime, mut store, memory) = c.into_parts();
        assert!(memory.persistence_pending());
        assert!(memory.ready(Intent::Restore, retry));
        assert!(
            memory
                .journal()
                .unwrap()
                .providers
                .iter()
                .all(|entry| entry.restore_complete)
        );
        store.fail_clear = false;
        let mut resumed = Coordinator::resume(runtime, store, bindings(), memory, retry).unwrap();
        assert_eq!(
            resumed
                .advance(Intent::Restore, &[], retry, &mut || false)
                .unwrap(),
            Step::Completed
        );
        assert_eq!(
            controls,
            (resumed.runtime.lm.controls, resumed.runtime.ollama.controls)
        );
        assert!(resumed.journal().is_none());
    }
    #[test]
    fn continuation_preserves_independent_deadlines_and_healthy_completion() {
        let mut runtime = router();
        runtime.ollama.fail = Some(1);
        let retry = Duration::from_secs(10);
        let mut c = Coordinator::new(runtime, Disk::default(), bindings(), None, retry).unwrap();
        for _ in 0..8 {
            c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
                .unwrap();
        }
        let healthy = c.runtime.lm.controls;
        let writes = c.store.writes;
        let (mut runtime, store, memory) = c.into_parts();
        assert!(!memory.ready(Intent::Pause, Duration::from_secs(9)));
        assert!(memory.ready(Intent::Pause, retry));
        assert!(
            memory.ready(Intent::Restore, Duration::ZERO),
            "direction change bypasses old pause backoff"
        );
        runtime.ollama.fail = None;
        let mut resumed = Coordinator::resume(runtime, store, bindings(), memory, retry).unwrap();
        assert_eq!(
            resumed
                .advance(Intent::Pause, &[], Duration::from_secs(9), &mut || false)
                .unwrap(),
            Step::Idle
        );
        assert_eq!(resumed.store.writes, writes);
        assert_eq!(resumed.runtime.lm.controls, healthy);
        resumed
            .advance(Intent::Pause, &[], retry, &mut || false)
            .unwrap();
        assert!(resumed.pause_complete());
        assert_eq!(resumed.runtime.lm.controls, healthy);
        let (_, _, memory) = resumed.into_parts();
        assert!(!memory.ready(Intent::Pause, retry));
    }
    #[test]
    fn continuation_retains_global_failed_write_gate_and_manual_retry() {
        let retry = Duration::from_secs(10);
        let disk = Disk {
            fail: true,
            ..Default::default()
        };
        let mut c = Coordinator::new(router(), disk, bindings(), None, retry).unwrap();
        assert!(
            c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
                .is_err()
        );
        let (runtime, store, memory) = c.into_parts();
        assert!(memory.persistence_pending());
        let mut resumed = Coordinator::resume(runtime, store, bindings(), memory, retry).unwrap();
        assert!(
            resumed
                .advance(Intent::Pause, &[], retry, &mut || false)
                .is_err()
        );
        assert_eq!(
            resumed.runtime.lm.controls + resumed.runtime.ollama.controls,
            0
        );
        resumed.store.fail = false;
        resumed.runtime.ollama.capture_fail = true;
        for _ in 0..8 {
            resumed
                .advance(Intent::Pause, &[], retry, &mut || false)
                .unwrap();
        }
        let (mut runtime, store, mut memory) = resumed.into_parts();
        assert!(!memory.ready(Intent::Pause, Duration::from_secs(11)));
        memory.request_retry();
        assert!(memory.ready(Intent::Pause, Duration::from_secs(11)));
        runtime.ollama.capture_fail = false;
        let mut resumed = Coordinator::resume(runtime, store, bindings(), memory, retry).unwrap();
        for _ in 0..4 {
            resumed
                .advance(Intent::Pause, &[], Duration::from_secs(11), &mut || false)
                .unwrap();
        }
        assert!(resumed.pause_complete());
    }
    #[test]
    fn distinct_adapters_continue_healthy_work_through_each_routed_failure_and_retry() {
        for lm_fails in [true, false] {
            for point in [0, 1, 3, 5] {
                for after_effect in [false, true] {
                    let mut runtime = router();
                    if point == 0 {
                        if lm_fails {
                            runtime.lm.capture_fail = true;
                        } else {
                            runtime.ollama.capture_fail = true;
                        }
                    } else if point == 1 {
                        if lm_fails {
                            runtime.lm.fail = Some(point);
                            runtime.lm.after_effect = after_effect;
                        } else {
                            runtime.ollama.fail = Some(point);
                            runtime.ollama.after_effect = after_effect;
                        }
                    }
                    let mut c = Coordinator::new(
                        runtime,
                        Disk::default(),
                        bindings(),
                        None,
                        Duration::from_secs(10),
                    )
                    .unwrap();
                    for _ in 0..8 {
                        c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
                            .unwrap();
                    }
                    let (failed, healthy) = if lm_fails {
                        ("lm", "ollama")
                    } else {
                        ("ollama", "lm")
                    };
                    assert_eq!(c.statuses()[healthy].state, State::Paused);
                    if point <= 1 {
                        assert_eq!(c.statuses()[failed].state, State::Failed);
                        assert!(!c.pause_complete());
                        let controls = (c.runtime.lm.controls, c.runtime.ollama.controls);
                        assert_eq!(
                            c.advance(Intent::Pause, &[], Duration::from_secs(9), &mut || false)
                                .unwrap(),
                            Step::Idle
                        );
                        assert_eq!(controls, (c.runtime.lm.controls, c.runtime.ollama.controls));
                        c.runtime.lm.capture_fail = false;
                        c.runtime.ollama.capture_fail = false;
                        c.runtime.lm.fail = None;
                        c.runtime.ollama.fail = None;
                        for _ in 0..5 {
                            c.advance(Intent::Pause, &[], Duration::from_secs(10), &mut || false)
                                .unwrap();
                        }
                        assert!(c.pause_complete());
                        let healthy_after = if lm_fails {
                            c.runtime.ollama.controls
                        } else {
                            c.runtime.lm.controls
                        };
                        let healthy_before = if lm_fails { controls.1 } else { controls.0 };
                        assert_eq!(
                            healthy_after, healthy_before,
                            "completed healthy provider stays quiet"
                        );
                    }
                    if point >= 3 {
                        if lm_fails {
                            c.runtime.lm.fail = Some(point);
                            c.runtime.lm.after_effect = after_effect;
                        } else {
                            c.runtime.ollama.fail = Some(point);
                            c.runtime.ollama.after_effect = after_effect;
                        }
                    }
                    for _ in 0..8 {
                        c.advance(Intent::Restore, &[], Duration::from_secs(11), &mut || false)
                            .unwrap();
                    }
                    assert_eq!(c.statuses()[healthy].state, State::Restored);
                    if point >= 3 {
                        assert!(c.journal().is_some());
                        let reports = c.reports(Duration::from_secs(11));
                        assert!(reports.iter().any(|report| report.id == failed
                            && report.pending
                            && report.state == State::Failed));
                        assert!(reports.iter().any(|report| report.id == healthy
                            && !report.pending
                            && report.state == State::Restored));
                        c.runtime.lm.fail = None;
                        c.runtime.ollama.fail = None;
                        for _ in 0..6 {
                            c.advance(Intent::Restore, &[], Duration::from_secs(21), &mut || false)
                                .unwrap();
                        }
                    }
                    assert!(c.journal().is_none());
                    assert!(c.store.journal.is_none());
                    assert_eq!(c.runtime.lm.value.raw, "fixture raw LM settings");
                    assert_eq!(c.runtime.ollama.value.expires_at, 1234567);
                }
            }
        }
    }
    #[test]
    fn mismatched_payload_and_reassigned_queued_work_refuse_before_control() {
        let mut runtime = router();
        let bindings = bindings();
        let wrong = Entry {
            binding: bindings[0].clone(),
            payload: Routed::Ollama(runtime.ollama.value.clone()),
            restore_complete: false,
        };
        assert!(runtime.validate(&wrong, &[]).is_err());
        assert!(runtime.plan(&wrong, Intent::Pause).is_err());
        let entry = runtime.capture(&bindings[0], &[]).unwrap();
        let planned = runtime.plan(&entry, Intent::Pause).unwrap();
        let mut reassigned = bindings[0].clone();
        reassigned.id = "different-id".into();
        assert!(runtime.execute(&reassigned, planned.work).is_err());
        assert_eq!(runtime.lm.controls, 0);
        assert_eq!(runtime.ollama.controls, 0);
        assert!(
            runtime
                .validate_transition(&entry.payload, &wrong.payload)
                .is_err()
        );
    }
    #[test]
    fn typed_envelope_restart_preserves_distinct_originals_after_partial_restore() {
        let mut c = Coordinator::new(
            router(),
            Disk::default(),
            bindings(),
            None,
            Duration::from_secs(10),
        )
        .unwrap();
        for _ in 0..6 {
            c.advance(Intent::Pause, &[], Duration::ZERO, &mut || false)
                .unwrap();
        }
        c.runtime.ollama.fail = Some(5);
        for _ in 0..8 {
            c.advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
                .unwrap();
        }
        let saved = c.store.journal.clone().unwrap();
        assert!(
            saved
                .providers
                .iter()
                .any(|entry| entry.binding.kind == Kind::LMStudio && entry.restore_complete)
        );
        assert!(
            saved
                .providers
                .iter()
                .any(|entry| entry.binding.kind == Kind::Ollama && !entry.restore_complete)
        );
        let mut runtime = c.runtime.clone();
        runtime.ollama.fail = None;
        let mut restarted = Coordinator::new(
            runtime,
            Disk::default(),
            bindings(),
            Some(saved),
            Duration::from_secs(10),
        )
        .unwrap();
        for _ in 0..8 {
            restarted
                .advance(Intent::Restore, &[], Duration::ZERO, &mut || false)
                .unwrap();
        }
        assert!(restarted.journal().is_none());
        assert_eq!(restarted.runtime.lm.value.raw, "fixture raw LM settings");
        assert_eq!(restarted.runtime.ollama.value.expires_at, 1234567);
    }
}
