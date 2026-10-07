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
/// The two-way router carries LM Studio and Ollama; `Providers` handles the
/// process provider itself and never passes it down.
const UNROUTED: &str = "Process provider is not routed here; recovery retained";
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
            Kind::Process => bail!("{UNROUTED}"),
        })
    }
    fn validate(&self, entry: &Entry<Self::Payload>, games: &[Game]) -> Result<()> {
        match entry.binding.kind {
            Kind::LMStudio => self.lm.validate(&lm_entry(entry)?, games),
            Kind::Ollama => self.ollama.validate(&ollama_entry(entry)?, games),
            Kind::Process => bail!("{UNROUTED}"),
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
            Kind::Process => false,
        }
    }
    fn retry(&self, binding: &Binding) {
        match binding.kind {
            Kind::LMStudio => self.lm.retry(binding),
            Kind::Ollama => self.ollama.retry(binding),
            Kind::Process => (),
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
            Kind::Process => bail!("{UNROUTED}"),
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
/// Engine-owned process adapter, kept across ticks like the Ollama runtime.
#[derive(Default)]
pub struct ProcessRuntime {
    adapter: Option<crate::process_session::Adapter<crate::process_session::Windows>>,
    active: bool,
}
impl ProcessRuntime {
    fn configure(&mut self, config: &Config) {
        let configured = config.providers.iter().find_map(|provider| match provider {
            crate::config::Provider::Process { id, apps, .. } if provider.enabled() => {
                Some((id, apps))
            }
            _ => None,
        });
        self.active = config.mode == "active" && configured.is_some();
        if self.adapter.is_none()
            && let Some((id, apps)) = configured
        {
            self.adapter = crate::process_session::Adapter::new(
                crate::process_session::Windows,
                process_binding(id),
                apps.clone(),
            )
            .ok();
        }
    }
    fn adapter(
        &mut self,
    ) -> Result<&mut crate::process_session::Adapter<crate::process_session::Windows>> {
        self.adapter
            .as_mut()
            .filter(|_| self.active)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Other AI apps are turned off or observation is active; recovery retained"
                )
            })
    }
}
fn process_binding(id: &str) -> Binding {
    Binding {
        id: id.into(),
        kind: Kind::Process,
        endpoint: crate::process_session::ROUTE.into(),
        configured_endpoint: crate::process_session::ROUTE.into(),
        payload_version: 1,
        guarantee: Guarantee::ProcessRelaunch,
    }
}
fn process_entry(entry: &Entry<Payload>) -> Result<Entry<crate::process_session::Snapshot>> {
    match &entry.payload {
        Payload::Process(snapshot) if entry.binding.kind == Kind::Process => Ok(Entry {
            binding: entry.binding.clone(),
            payload: snapshot.clone(),
            restore_complete: entry.restore_complete,
        }),
        _ => bail!("Provider kind/payload mismatch; recovery retained"),
    }
}
pub enum ProviderWork {
    Routed(Work<LMWork, crate::ollama_session::Work>),
    Process(crate::process_session::Work),
}
pub struct Providers<'a, B: Backend> {
    pub router: Router<LMRuntime<B>, OllamaRef<'a>>,
    processes: &'a mut ProcessRuntime,
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
        processes: &'a mut ProcessRuntime,
    ) -> Self {
        ollama.configure(&config);
        processes.configure(&config);
        Self {
            router: Router {
                lm: LMRuntime::new(backend, config, verify_raw),
                ollama: OllamaRef(ollama),
            },
            processes,
        }
    }
}
impl OllamaRuntime {
    pub fn captured_bytes(&self) -> u64 {
        self.adapter
            .as_ref()
            .map_or(0, |adapter| adapter.captured_bytes)
    }
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
                if provider.kind() == Kind::Process {
                    return Ok(process_binding(provider.id()));
                }
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
                        Kind::Process => Guarantee::ProcessRelaunch,
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
            // Never reached: `Providers` handles process entries before routing.
            Payload::Process(_) => Routed::LMStudio(entry.payload.clone()),
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
    type Work = ProviderWork;
    fn capture(&mut self, binding: &Binding, games: &[Game]) -> Result<Entry<Payload>> {
        if binding.kind == Kind::Process {
            let entry = self.processes.adapter()?.capture(binding, games)?;
            return Ok(Entry {
                binding: entry.binding,
                payload: Payload::Process(entry.payload),
                restore_complete: entry.restore_complete,
            });
        }
        let entry = self.router.capture(binding, games)?;
        Ok(Entry {
            binding: entry.binding,
            payload: unwrap(entry.payload)?,
            restore_complete: entry.restore_complete,
        })
    }
    fn validate(&self, entry: &Entry<Payload>, games: &[Game]) -> Result<()> {
        if entry.binding.kind == Kind::Process || matches!(entry.payload, Payload::Process(_)) {
            let typed = process_entry(entry)?;
            return match &self.processes.adapter {
                Some(adapter) if !entry.restore_complete => adapter.validate(&typed, games),
                _ => typed.payload.validate(),
            };
        }
        self.router.validate(&wrap(entry), games)
    }
    fn validate_transition(&self, original: &Payload, updated: &Payload) -> Result<()> {
        match (original, updated) {
            (Payload::LMStudio(_), Payload::LMStudio(_)) => {
                self.router.lm.validate_transition(original, updated)
            }
            (Payload::Ollama(old), Payload::Ollama(new)) => old.validate_transition(new),
            (Payload::Process(old), Payload::Process(new)) => old.validate_transition(new),
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
            Payload::Process(snapshot) => {
                snapshot.begin();
                Ok(())
            }
        }
    }
    fn complete(&self, payload: &Payload, intent: Intent) -> bool {
        match payload {
            Payload::LMStudio(_) => self.router.lm.complete(payload, intent),
            Payload::Ollama(snapshot) => intent == Intent::Pause && snapshot.pause_complete,
            Payload::Process(snapshot) => intent == Intent::Pause && snapshot.pause_complete,
        }
    }
    fn can_continue(&self, entry: &Entry<Payload>, intent: Intent) -> bool {
        entry.binding.kind != Kind::Process && self.router.can_continue(&wrap(entry), intent)
    }
    fn retry(&self, binding: &Binding) {
        self.router.retry(binding);
    }
    fn note(&self, payload: &Payload) -> String {
        match payload {
            Payload::LMStudio(_) => String::new(),
            Payload::Ollama(snapshot) => snapshot.note(),
            Payload::Process(snapshot) => snapshot.note(),
        }
    }
    fn plan(
        &mut self,
        entry: &Entry<Payload>,
        intent: Intent,
    ) -> Result<Planned<Payload, Self::Work>> {
        if entry.binding.kind == Kind::Process {
            let planned = self
                .processes
                .adapter()?
                .plan(&process_entry(entry)?, intent)?;
            return Ok(Planned {
                checkpoint: Payload::Process(planned.checkpoint),
                work: ProviderWork::Process(planned.work),
            });
        }
        let planned = self.router.plan(&wrap(entry), intent)?;
        Ok(Planned {
            checkpoint: unwrap(planned.checkpoint)?,
            work: ProviderWork::Routed(planned.work),
        })
    }
    fn execute(&mut self, binding: &Binding, work: Self::Work) -> Result<Outcome<Payload>> {
        match work {
            ProviderWork::Process(work) => {
                let outcome = self.processes.adapter()?.execute(binding, work)?;
                Ok(Outcome {
                    payload: Payload::Process(outcome.payload),
                    restore_complete: outcome.restore_complete,
                })
            }
            ProviderWork::Routed(work) => {
                let outcome = self.router.execute(binding, work)?;
                Ok(Outcome {
                    payload: unwrap(outcome.payload)?,
                    restore_complete: outcome.restore_complete,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests;
