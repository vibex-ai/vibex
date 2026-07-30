use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::limits::MarkdownLimits;
use crate::model::NodeId;
#[cfg(feature = "artifact-engines")]
use crate::svg::SvgPolicy;
use crate::svg::{SvgArtifact, SvgPolicyError};

const ARTIFACT_CONTRACT_VERSION: &str = "vibex-markdown-artifact-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactKind {
    InlineMath,
    DisplayMath,
    Mermaid,
    PlantUml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactTheme {
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactRequest {
    pub view_id: Arc<str>,
    pub revision: u64,
    pub node_id: NodeId,
    pub kind: ArtifactKind,
    pub source: Arc<str>,
    pub theme: ArtifactTheme,
    pub foreground_rgb: u32,
    pub font_size: f32,
    pub scale_factor: f32,
}

impl ArtifactRequest {
    pub fn key(&self) -> ArtifactKey {
        let mut digest = Sha256::new();
        digest.update(ARTIFACT_CONTRACT_VERSION.as_bytes());
        digest.update([0]);
        digest.update(match self.kind {
            ArtifactKind::InlineMath => b"inline-math".as_slice(),
            ArtifactKind::DisplayMath => b"display-math".as_slice(),
            ArtifactKind::Mermaid => b"mermaid".as_slice(),
            ArtifactKind::PlantUml => b"plantuml".as_slice(),
        });
        digest.update([0]);
        digest.update(engine_version(self.kind).as_bytes());
        digest.update([0]);
        digest.update(self.source.as_bytes());
        digest.update([0]);
        digest.update(match self.theme {
            ArtifactTheme::Light => b"light".as_slice(),
            ArtifactTheme::Dark => b"dark".as_slice(),
        });
        if matches!(
            self.kind,
            ArtifactKind::InlineMath | ArtifactKind::DisplayMath
        ) {
            digest.update((self.foreground_rgb & 0x00ff_ffff).to_le_bytes());
        }
        digest.update(self.font_size.to_bits().to_le_bytes());
        digest.update(self.scale_factor.to_bits().to_le_bytes());
        ArtifactKey(digest.finalize().into())
    }
}

fn engine_version(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::InlineMath | ArtifactKind::DisplayMath => "mathjax-svg-rs/0.4.0",
        ArtifactKind::Mermaid => "mermaid-rs-renderer/0.3.1",
        ArtifactKind::PlantUml => "vibex-plantuml-mermaid-subset/1",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactKey(pub [u8; 32]);

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ArtifactError {
    #[error("artifact source exceeds the byte limit")]
    SourceLimit,
    #[error("artifact queue is full")]
    QueueFull,
    #[error("artifact engine circuit is temporarily open")]
    CircuitOpen,
    #[error("artifact rendering timed out")]
    Timeout,
    #[error("artifact engine failed: {0}")]
    Engine(String),
    #[error("artifact SVG was rejected: {0}")]
    UnsafeSvg(String),
}

impl From<SvgPolicyError> for ArtifactError {
    fn from(value: SvgPolicyError) -> Self {
        Self::UnsafeSvg(value.to_string())
    }
}

#[derive(Debug, Clone)]
pub enum ArtifactSchedule {
    Cached(Arc<SvgArtifact>),
    Start(ArtifactRequest),
    Queued,
    Existing,
    Rejected(ArtifactError),
}

#[derive(Debug, Clone)]
pub struct ArtifactCompletion {
    pub accepted: bool,
    pub result: Result<Arc<SvgArtifact>, ArtifactError>,
    pub next: Option<ArtifactRequest>,
}

#[derive(Debug, Clone)]
struct CacheEntry {
    artifact: Arc<SvgArtifact>,
    bytes: usize,
    epoch: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct CircuitState {
    failures: u8,
    open_until_epoch: u64,
}

pub struct ArtifactController {
    limits: MarkdownLimits,
    epoch: u64,
    active: BTreeMap<ArtifactKey, ArtifactRequest>,
    queued_keys: BTreeSet<ArtifactKey>,
    queue: VecDeque<ArtifactRequest>,
    cache: BTreeMap<ArtifactKey, CacheEntry>,
    cache_bytes: usize,
    circuits: BTreeMap<ArtifactKind, CircuitState>,
}

impl Default for ArtifactController {
    fn default() -> Self {
        Self::new(MarkdownLimits::default())
    }
}

impl ArtifactController {
    pub fn new(limits: MarkdownLimits) -> Self {
        Self {
            limits,
            epoch: 0,
            active: BTreeMap::new(),
            queued_keys: BTreeSet::new(),
            queue: VecDeque::new(),
            cache: BTreeMap::new(),
            cache_bytes: 0,
            circuits: BTreeMap::new(),
        }
    }

    pub fn schedule(&mut self, request: ArtifactRequest) -> ArtifactSchedule {
        self.epoch = self.epoch.saturating_add(1).max(1);
        if request.source.len() > self.limits.max_artifact_source_bytes {
            return ArtifactSchedule::Rejected(ArtifactError::SourceLimit);
        }
        let key = request.key();
        if let Some(entry) = self.cache.get_mut(&key) {
            entry.epoch = self.epoch;
            return ArtifactSchedule::Cached(entry.artifact.clone());
        }
        if self.active.contains_key(&key) || self.queued_keys.contains(&key) {
            return ArtifactSchedule::Existing;
        }
        let circuit = self.circuits.entry(request.kind).or_default();
        if circuit.open_until_epoch > self.epoch {
            return ArtifactSchedule::Rejected(ArtifactError::CircuitOpen);
        }
        if self.active.len() < self.limits.max_concurrent_artifacts.max(1) {
            self.active.insert(key, request.clone());
            ArtifactSchedule::Start(request)
        } else if self.queue.len() < self.limits.max_artifact_queue {
            self.queued_keys.insert(key);
            self.queue.push_back(request);
            ArtifactSchedule::Queued
        } else {
            ArtifactSchedule::Rejected(ArtifactError::QueueFull)
        }
    }

    pub fn complete(
        &mut self,
        request: &ArtifactRequest,
        result: Result<Arc<SvgArtifact>, ArtifactError>,
        current_view_id: &str,
        current_revision: u64,
        live_nodes: &BTreeSet<NodeId>,
    ) -> ArtifactCompletion {
        self.epoch = self.epoch.saturating_add(1).max(1);
        let key = request.key();
        self.active.remove(&key);
        match &result {
            Ok(artifact) => {
                let circuit = self.circuits.entry(request.kind).or_default();
                circuit.failures = 0;
                circuit.open_until_epoch = 0;
                self.insert_cache(key, artifact.clone());
            }
            Err(_) => {
                let circuit = self.circuits.entry(request.kind).or_default();
                circuit.failures = circuit.failures.saturating_add(1);
                if circuit.failures >= 3 {
                    circuit.open_until_epoch = self.epoch.saturating_add(8);
                    circuit.failures = 0;
                }
            }
        }
        let accepted = request.view_id.as_ref() == current_view_id
            && request.revision == current_revision
            && live_nodes.contains(&request.node_id);
        let next = self.next_request();
        ArtifactCompletion {
            accepted,
            result,
            next,
        }
    }

    pub fn prune(&mut self, view_id: &str, revision: u64, live_nodes: &BTreeSet<NodeId>) {
        self.queue.retain(|request| {
            request.view_id.as_ref() == view_id
                && request.revision == revision
                && live_nodes.contains(&request.node_id)
        });
        self.queued_keys = self.queue.iter().map(ArtifactRequest::key).collect();
    }

    pub fn resident_entries(&self) -> usize {
        self.cache.len()
    }

    pub fn resident_bytes(&self) -> usize {
        self.cache_bytes
    }

    pub fn active_jobs(&self) -> usize {
        self.active.len()
    }

    pub fn queued_jobs(&self) -> usize {
        self.queue.len()
    }

    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.limits.artifact_timeout_ms)
    }

    fn next_request(&mut self) -> Option<ArtifactRequest> {
        if self.active.len() >= self.limits.max_concurrent_artifacts.max(1) {
            return None;
        }
        let request = self.queue.pop_front()?;
        let key = request.key();
        self.queued_keys.remove(&key);
        self.active.insert(key, request.clone());
        Some(request)
    }

    fn insert_cache(&mut self, key: ArtifactKey, artifact: Arc<SvgArtifact>) {
        let bytes = artifact.bytes.len();
        if bytes > self.limits.max_artifact_cache_bytes {
            return;
        }
        if let Some(previous) = self.cache.remove(&key) {
            self.cache_bytes = self.cache_bytes.saturating_sub(previous.bytes);
        }
        self.cache.insert(
            key,
            CacheEntry {
                artifact,
                bytes,
                epoch: self.epoch,
            },
        );
        self.cache_bytes = self.cache_bytes.saturating_add(bytes);
        while self.cache.len() > self.limits.max_artifact_cache_entries
            || self.cache_bytes > self.limits.max_artifact_cache_bytes
        {
            let Some(oldest) = self
                .cache
                .iter()
                .min_by_key(|(_, entry)| entry.epoch)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(entry) = self.cache.remove(&oldest) {
                self.cache_bytes = self.cache_bytes.saturating_sub(entry.bytes);
            }
        }
    }
}

#[cfg(feature = "artifact-engines")]
pub fn render_local_artifact(
    request: &ArtifactRequest,
    policy: SvgPolicy,
) -> Result<Arc<SvgArtifact>, ArtifactError> {
    use crate::engines::{EngineTheme, render_math, render_mermaid, render_plantuml_with_theme};

    let svg = match request.kind {
        ArtifactKind::InlineMath | ArtifactKind::DisplayMath => {
            render_math(&request.source, f64::from(request.font_size))
        }
        ArtifactKind::Mermaid => render_mermaid(
            &request.source,
            match request.theme {
                ArtifactTheme::Light => EngineTheme::Light,
                ArtifactTheme::Dark => EngineTheme::Dark,
            },
        ),
        ArtifactKind::PlantUml => render_plantuml_with_theme(
            &request.source,
            match request.theme {
                ArtifactTheme::Light => EngineTheme::Light,
                ArtifactTheme::Dark => EngineTheme::Dark,
            },
        ),
    }
    .map_err(|error| ArtifactError::Engine(error.to_string()))?;
    let prefix = format!("md-{}-{}", request.revision, request.node_id.0);
    let artifact = if matches!(
        request.kind,
        ArtifactKind::InlineMath | ArtifactKind::DisplayMath
    ) {
        policy.sanitize_with_current_color(&svg, &prefix, request.foreground_rgb)?
    } else {
        policy.sanitize(&svg, &prefix)?
    };
    Ok(Arc::new(artifact))
}

#[cfg(feature = "artifact-engines")]
pub fn render_local_artifact_with_timeout(
    request: ArtifactRequest,
    policy: SvgPolicy,
    timeout: Duration,
) -> Result<Arc<SvgArtifact>, ArtifactError> {
    use std::sync::mpsc::{RecvTimeoutError, TrySendError};

    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = artifact_worker(request.kind)?;
    worker
        .sender
        .try_send((request, policy, sender))
        .map_err(|error| match error {
            TrySendError::Full(_) => ArtifactError::QueueFull,
            TrySendError::Disconnected(_) => {
                ArtifactError::Engine("artifact worker stopped unexpectedly".into())
            }
        })?;
    match receiver.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(ArtifactError::Timeout),
        Err(RecvTimeoutError::Disconnected) => Err(ArtifactError::Engine(
            "artifact worker stopped unexpectedly".into(),
        )),
    }
}

#[cfg(feature = "artifact-engines")]
type ArtifactWorkerJob = (
    ArtifactRequest,
    SvgPolicy,
    std::sync::mpsc::SyncSender<Result<Arc<SvgArtifact>, ArtifactError>>,
);

#[cfg(feature = "artifact-engines")]
struct ArtifactWorker {
    sender: std::sync::mpsc::SyncSender<ArtifactWorkerJob>,
}

#[cfg(feature = "artifact-engines")]
impl ArtifactWorker {
    fn start(name: &'static str) -> Result<Self, String> {
        let (sender, receiver) = std::sync::mpsc::sync_channel::<ArtifactWorkerJob>(2);
        std::thread::Builder::new()
            .name(format!("vibex-markdown-{name}"))
            .spawn(move || {
                while let Ok((request, policy, completion)) = receiver.recv() {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        render_local_artifact(&request, policy)
                    }))
                    .unwrap_or_else(|_| {
                        Err(ArtifactError::Engine(
                            "artifact worker panicked while rendering".into(),
                        ))
                    });
                    let _ = completion.send(result);
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { sender })
    }
}

#[cfg(feature = "artifact-engines")]
fn artifact_worker(kind: ArtifactKind) -> Result<&'static ArtifactWorker, ArtifactError> {
    use std::sync::OnceLock;

    static MATH: OnceLock<Result<ArtifactWorker, String>> = OnceLock::new();
    static MERMAID: OnceLock<Result<ArtifactWorker, String>> = OnceLock::new();
    static PLANTUML: OnceLock<Result<ArtifactWorker, String>> = OnceLock::new();
    let worker = match kind {
        ArtifactKind::InlineMath | ArtifactKind::DisplayMath => {
            MATH.get_or_init(|| ArtifactWorker::start("math"))
        }
        ArtifactKind::Mermaid => MERMAID.get_or_init(|| ArtifactWorker::start("mermaid")),
        ArtifactKind::PlantUml => PLANTUML.get_or_init(|| ArtifactWorker::start("plantuml")),
    };
    worker
        .as_ref()
        .map_err(|error| ArtifactError::Engine(error.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::svg::SvgPolicy;

    fn request(node: u64, revision: u64) -> ArtifactRequest {
        ArtifactRequest {
            view_id: "view".into(),
            revision,
            node_id: NodeId(node),
            kind: ArtifactKind::Mermaid,
            source: "flowchart LR\nA-->B".into(),
            theme: ArtifactTheme::Light,
            foreground_rgb: 0x09090b,
            font_size: 16.0,
            scale_factor: 1.0,
        }
    }

    fn artifact(label: &str) -> Arc<SvgArtifact> {
        Arc::new(
            SvgPolicy::default()
                .sanitize(
                    &format!(
                        "<svg width=\"10\" height=\"10\" viewBox=\"0 0 10 10\"><text>{label}</text></svg>"
                    ),
                    label,
                )
                .unwrap(),
        )
    }

    #[test]
    fn controller_bounds_concurrency_queues_and_fences_stale_results() {
        let mut controller = ArtifactController::new(MarkdownLimits {
            max_concurrent_artifacts: 1,
            max_artifact_queue: 1,
            ..MarkdownLimits::default()
        });
        let first = request(1, 1);
        let mut second = request(2, 2);
        second.source = "flowchart LR\nB-->C".into();
        let mut third = request(3, 2);
        third.source = "flowchart LR\nC-->D".into();
        assert!(matches!(
            controller.schedule(first.clone()),
            ArtifactSchedule::Start(_)
        ));
        assert!(matches!(
            controller.schedule(second.clone()),
            ArtifactSchedule::Queued
        ));
        assert!(matches!(
            controller.schedule(third),
            ArtifactSchedule::Rejected(ArtifactError::QueueFull)
        ));

        let completion = controller.complete(
            &first,
            Ok(artifact("first")),
            "view",
            2,
            &BTreeSet::from([NodeId(2)]),
        );
        assert!(!completion.accepted);
        assert_eq!(completion.next.unwrap().node_id, NodeId(2));
    }

    #[test]
    fn controller_cache_is_bounded_by_entries_and_bytes() {
        let mut controller = ArtifactController::new(MarkdownLimits {
            max_concurrent_artifacts: 1,
            max_artifact_cache_entries: 1,
            max_artifact_cache_bytes: 4096,
            ..MarkdownLimits::default()
        });
        for node in 1..=2 {
            let mut request = request(node, 1);
            request.source = format!("flowchart LR\nA{node}-->B{node}").into();
            assert!(matches!(
                controller.schedule(request.clone()),
                ArtifactSchedule::Start(_)
            ));
            controller.complete(
                &request,
                Ok(artifact(&node.to_string())),
                "view",
                1,
                &BTreeSet::from([NodeId(node)]),
            );
        }
        assert_eq!(controller.resident_entries(), 1);
        assert!(controller.resident_bytes() <= 4096);
    }

    #[test]
    fn controller_opens_the_engine_circuit_after_repeated_failures() {
        let mut controller = ArtifactController::new(MarkdownLimits {
            max_concurrent_artifacts: 1,
            ..MarkdownLimits::default()
        });
        for node in 1..=3 {
            let mut failed = request(node, 1);
            failed.source = format!("flowchart LR\nA{node}-->B{node}").into();
            assert!(matches!(
                controller.schedule(failed.clone()),
                ArtifactSchedule::Start(_)
            ));
            controller.complete(
                &failed,
                Err(ArtifactError::Timeout),
                "view",
                1,
                &BTreeSet::from([NodeId(node)]),
            );
        }

        let rejected = request(9, 1);
        assert!(matches!(
            controller.schedule(rejected),
            ArtifactSchedule::Rejected(ArtifactError::CircuitOpen)
        ));
    }

    #[test]
    fn repeated_identical_artifacts_reuse_the_completed_cache_entry() {
        let mut controller = ArtifactController::default();
        let first = request(1, 1);
        let mut duplicate = first.clone();
        duplicate.node_id = NodeId(2);
        assert!(matches!(
            controller.schedule(first.clone()),
            ArtifactSchedule::Start(_)
        ));
        assert!(matches!(
            controller.schedule(duplicate.clone()),
            ArtifactSchedule::Existing
        ));
        controller.complete(
            &first,
            Ok(artifact("shared")),
            "view",
            1,
            &BTreeSet::from([NodeId(1), NodeId(2)]),
        );
        assert!(matches!(
            controller.schedule(duplicate),
            ArtifactSchedule::Cached(_)
        ));
    }

    #[cfg(feature = "artifact-engines")]
    #[test]
    fn local_artifacts_pass_the_svg_policy() {
        for (kind, source) in [
            (ArtifactKind::InlineMath, r"\frac{a}{b}"),
            (ArtifactKind::Mermaid, "flowchart LR\nA-->B"),
            (
                ArtifactKind::PlantUml,
                "@startuml\nparticipant A\nA -> B: hello\n@enduml",
            ),
        ] {
            let mut request = request(kind as u64 + 1, 1);
            request.kind = kind;
            request.source = source.into();
            let artifact = render_local_artifact(&request, SvgPolicy::default())
                .unwrap_or_else(|error| panic!("{kind:?}: {error}"));
            assert!(!artifact.bytes.is_empty());
            assert!(artifact.width_px > 0.0 && artifact.height_px > 0.0);
        }
    }

    #[cfg(feature = "artifact-engines")]
    #[test]
    fn math_artifacts_resolve_the_requested_foreground_color() {
        let mut light = request(1, 1);
        light.kind = ArtifactKind::InlineMath;
        light.source = r"E = mc^2".into();

        let mut dark = light.clone();
        dark.theme = ArtifactTheme::Dark;
        dark.foreground_rgb = 0xfafafa;

        assert_ne!(light.key(), dark.key());
        let light = render_local_artifact(&light, SvgPolicy::default()).unwrap();
        let dark = render_local_artifact(&dark, SvgPolicy::default()).unwrap();
        let light_svg = String::from_utf8(light.bytes.to_vec()).unwrap();
        let dark_svg = String::from_utf8(dark.bytes.to_vec()).unwrap();

        assert!(light_svg.contains("color=\"#09090b\""));
        assert!(light_svg.contains("fill=\"#09090b\""));
        assert!(dark_svg.contains("color=\"#fafafa\""));
        assert!(dark_svg.contains("fill=\"#fafafa\""));
        assert_ne!(light.bytes, dark.bytes);
    }
}
