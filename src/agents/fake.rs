//! `FakeAgentAdapter` (plan §6.4): scripted agents for tests and demos — no process, no
//! tokens. A script lists turns per `role` (or `role@profile`); each turn is a list of
//! steps (delay, text, tool activity, file writes, usage, denial, completion, failure,
//! crash, hang). A replay turn feeds a recorded real stream through the real parsers.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::adapter::{AgentAdapter, AgentSession, TurnId};
use super::events::replay;
use crate::config::ProfileKind;
use crate::domain::{
    permission_denials_detail, AgentEvent, AgentHealth, ErrorClass, OpenSession, TokenUsage,
    TurnInput, UsageCapabilities,
};
use crate::usage::{Accounting, Buckets};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FakeStep {
    Delay {
        ms: u64,
    },
    Text {
        text: String,
    },
    Tool {
        name: String,
        #[serde(default)]
        summary: String,
    },
    /// Write (or append to) a file relative to the session's cwd — an Implementer's edits.
    WriteFile {
        path: String,
        content: String,
        #[serde(default)]
        append: bool,
    },
    Usage {
        #[serde(default)]
        input: u64,
        #[serde(default)]
        cache_read: u64,
        #[serde(default)]
        output: u64,
    },
    /// A permission denial. Like with the real adapters, any denial fails the turn: a later
    /// `complete` ends it as `PermissionDenied`, keeping its payload (plan §6.2).
    Deny {
        tool: String,
        #[serde(default)]
        detail: String,
    },
    Complete {
        #[serde(default)]
        text: String,
        #[serde(default)]
        structured: Option<Value>,
    },
    Fail {
        class: ErrorClass,
        #[serde(default)]
        detail: String,
    },
    /// The process died without a terminal event.
    Crash,
    /// Never answers again (until interrupted) — for stall/timeout tests.
    Hang,
    /// Replay a recorded stream (`claude` or `codex` JSONL) through the real parsers. A file
    /// that cannot be read fails the turn with a detail naming it.
    Replay {
        agent: String,
        file: PathBuf,
        #[serde(default)]
        exit_code: Option<i32>,
    },
    /// Internal: an already-built event (replayed streams, the session id).
    #[serde(skip)]
    Emit(Box<AgentEvent>),
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FakeTurn {
    #[serde(default)]
    pub steps: Vec<FakeStep>,
}

/// `{ "turns": { "planner": [ {steps…}, … ], "implementer@codex-openai": [ … ] } }`
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FakeScript {
    #[serde(default)]
    pub turns: BTreeMap<String, Vec<FakeTurn>>,
    /// Per-profile usage capabilities instead of the real adapter's (R41 fixtures).
    #[serde(default)]
    pub capabilities: BTreeMap<String, UsageCapabilities>,
    /// When not empty, only these profiles are fake; every other profile runs its real CLI.
    /// For checks that script one agent and use the real CLI for another — the injection
    /// canary scripts the implementer and asks the real reviewer (`scripts/m9-check.ps1`, R61).
    #[serde(default)]
    pub only_profiles: Vec<String>,
}

impl FakeScript {
    /// Whether `profile` is fake under this script ([`FakeScript::only_profiles`]).
    pub fn fakes(&self, profile: &str) -> bool {
        self.only_profiles.is_empty() || self.only_profiles.iter().any(|p| p == profile)
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut s: FakeScript =
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        // replay files are relative to the script
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        for turns in s.turns.values_mut() {
            for t in turns {
                for st in &mut t.steps {
                    if let FakeStep::Replay { file, .. } = st {
                        if file.is_relative() {
                            *file = base.join(&*file);
                        }
                    }
                }
            }
        }
        Ok(s)
    }
}

/// One recorded call, for assertions.
#[derive(Debug, Clone, Serialize)]
pub struct FakeCall {
    pub key: String,
    pub profile: String,
    pub label: String,
    pub prompt: String,
    pub resume: Option<String>,
    /// The context file's content at call time (§7), if the session had one.
    pub context: Option<String>,
}

#[derive(Default)]
struct Shared {
    queues: HashMap<String, VecDeque<FakeTurn>>,
    capabilities: BTreeMap<String, UsageCapabilities>,
    calls: Vec<FakeCall>,
    sessions: u32,
}

/// One adapter instance serves every profile (the profile is part of the lookup key).
#[derive(Clone)]
pub struct FakeAgentAdapter {
    shared: Arc<Mutex<Shared>>,
    profile: String,
    kind: ProfileKind,
}

impl FakeAgentAdapter {
    pub fn new(script: FakeScript) -> Self {
        let capabilities = script.capabilities.clone();
        let queues = script
            .turns
            .into_iter()
            .map(|(k, v)| (k, v.into_iter().collect()))
            .collect();
        Self {
            shared: Arc::new(Mutex::new(Shared {
                queues,
                capabilities,
                ..Default::default()
            })),
            profile: String::new(),
            kind: ProfileKind::Claude,
        }
    }

    /// The same script state, presented as `profile` (of `kind`).
    pub fn for_profile(&self, profile: &str, kind: ProfileKind) -> Self {
        Self {
            shared: self.shared.clone(),
            profile: profile.to_string(),
            kind,
        }
    }

    pub fn calls(&self) -> Vec<FakeCall> {
        self.shared.lock().expect("lock").calls.clone()
    }
}

#[async_trait]
impl AgentAdapter for FakeAgentAdapter {
    async fn open(&self, req: OpenSession) -> Result<Box<dyn AgentSession>> {
        let n = {
            let mut s = self.shared.lock().expect("lock");
            s.sessions += 1;
            s.sessions
        };
        Ok(Box::new(FakeSession {
            adapter: self.clone(),
            external: req
                .resume
                .clone()
                .or_else(|| Some(format!("fake-{}-{n}", self.profile))),
            req,
            pending: VecDeque::new(),
            done: true,
            hang: false,
            turns: 0,
            announced: false,
            env: Default::default(),
            denials: 0,
            delay_until: None,
        }))
    }

    async fn health(&self) -> Result<AgentHealth> {
        Ok(AgentHealth {
            exe: format!("fake:{}", self.profile),
            version: Some("0.0.0-fake".into()),
        })
    }

    fn usage_capabilities(&self) -> UsageCapabilities {
        let s = self.shared.lock().expect("lock");
        s.capabilities
            .get(&self.profile)
            .copied()
            .unwrap_or_else(|| super::cli::capabilities(self.kind))
    }
}

struct FakeSession {
    adapter: FakeAgentAdapter,
    req: OpenSession,
    external: Option<String>,
    pending: VecDeque<FakeStep>,
    done: bool,
    hang: bool,
    turns: u32,
    announced: bool,
    /// The current turn's environment (`$TEMP/…` in `write_file` paths, R46).
    env: std::collections::BTreeMap<String, String>,
    /// Permission denials in the current turn: any fails it (TS-01).
    denials: usize,
    /// End of a running `delay` step. Kept in the session so the delay survives the turn
    /// loop dropping `next_event` at each limits poll (a plain sleep was cut to ~200 ms).
    delay_until: Option<tokio::time::Instant>,
}

#[async_trait]
impl AgentSession for FakeSession {
    fn external_id(&self) -> Option<&str> {
        self.external.as_deref()
    }

    async fn start_turn(&mut self, input: TurnInput) -> Result<TurnId> {
        let role = self.req.role.as_str();
        let key_p = format!("{role}@{}", self.adapter.profile);
        let turn = {
            let mut s = self.adapter.shared.lock().expect("lock");
            let key = if s.queues.contains_key(&key_p) {
                key_p.clone()
            } else {
                role.to_string()
            };
            s.calls.push(FakeCall {
                key: key.clone(),
                profile: self.adapter.profile.clone(),
                label: input.label.clone(),
                prompt: input.prompt.clone(),
                resume: if self.turns > 0 || self.req.resume.is_some() {
                    self.external.clone()
                } else {
                    None
                },
                context: self
                    .req
                    .context_file
                    .as_ref()
                    .and_then(|f| std::fs::read_to_string(f).ok()),
            });
            s.queues.get_mut(&key).and_then(VecDeque::pop_front)
        };
        // like the real adapter, keep the prompt next to the capture
        std::fs::create_dir_all(&self.req.capture_dir)?;
        std::fs::write(
            self.req
                .capture_dir
                .join(format!("{}.prompt.md", input.label)),
            &input.prompt,
        )?;
        self.pending = match turn {
            Some(t) => t.steps.into_iter().collect(),
            None => VecDeque::from([FakeStep::Fail {
                class: ErrorClass::Unknown,
                detail: format!("fake script has no turn left for `{key_p}` / `{role}`"),
            }]),
        };
        if !self.announced {
            self.announced = true;
            let replays = matches!(self.pending.front(), Some(FakeStep::Replay { .. }));
            if let (false, Some(id)) = (replays, self.external.clone()) {
                self.pending
                    .push_front(FakeStep::Emit(Box::new(AgentEvent::SessionIdentified(id))));
            }
        }
        self.done = false;
        self.hang = false;
        self.denials = 0;
        self.delay_until = None;
        self.turns += 1;
        self.env = input.env.clone();
        Ok(self.turns)
    }

    async fn next_event(&mut self) -> Option<AgentEvent> {
        if self.done {
            return None;
        }
        loop {
            if self.hang {
                std::future::pending::<()>().await;
            }
            if let Some(until) = self.delay_until {
                tokio::time::sleep_until(until).await;
                self.delay_until = None;
            }
            let Some(step) = self.pending.pop_front() else {
                self.done = true;
                return None; // no terminal step: like a crash without a result
            };
            match step {
                FakeStep::Delay { ms } => {
                    self.delay_until =
                        Some(tokio::time::Instant::now() + Duration::from_millis(ms));
                }
                FakeStep::Text { text } => return Some(AgentEvent::TextDelta(text)),
                FakeStep::Tool { name, summary } => {
                    return Some(AgentEvent::ToolActivity { name, summary })
                }
                FakeStep::WriteFile {
                    path,
                    content,
                    append,
                } => {
                    // `$TEMP/x` writes into the turn's TEMP (R46), like a test runner would
                    let p = match (path.strip_prefix("$TEMP/"), self.env.get("TEMP")) {
                        (Some(r), Some(t)) => PathBuf::from(t).join(r),
                        _ => self.req.cwd.join(&path),
                    };
                    if let Some(d) = p.parent() {
                        let _ = std::fs::create_dir_all(d);
                    }
                    let res = if append {
                        use std::io::Write;
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&p)
                            .and_then(|mut f| f.write_all(content.as_bytes()))
                    } else {
                        std::fs::write(&p, content)
                    };
                    let _ = res;
                    return Some(AgentEvent::ToolActivity {
                        name: "file_change".into(),
                        summary: path,
                    });
                }
                FakeStep::Usage {
                    input,
                    cache_read,
                    output,
                } => {
                    return Some(AgentEvent::Usage(TokenUsage {
                        buckets: Some(Buckets {
                            uncached_input: input,
                            cache_read,
                            cache_write: 0,
                            output,
                            reasoning: None,
                        }),
                        accounting: Accounting::Exact,
                    }))
                }
                FakeStep::Deny { tool, detail } => {
                    self.denials += 1;
                    return Some(AgentEvent::PermissionDenied { tool, detail });
                }
                FakeStep::Complete { text, structured } => {
                    self.done = true;
                    if self.denials > 0 {
                        // like the real parser: the payload stays, for diagnostics only
                        return Some(AgentEvent::TurnFailed {
                            class: ErrorClass::PermissionDenied,
                            detail: permission_denials_detail(self.denials),
                            structured,
                        });
                    }
                    return Some(AgentEvent::TurnCompleted {
                        final_text: text,
                        structured,
                    });
                }
                FakeStep::Fail { class, detail } => {
                    self.done = true;
                    return Some(AgentEvent::TurnFailed {
                        class,
                        detail,
                        structured: None,
                    });
                }
                FakeStep::Crash => {
                    self.done = true;
                    return None;
                }
                FakeStep::Hang => self.hang = true,
                FakeStep::Emit(e) => {
                    match &*e {
                        AgentEvent::SessionIdentified(id) => self.external = Some(id.clone()),
                        AgentEvent::PermissionDenied { .. } => self.denials += 1,
                        _ => {}
                    }
                    if e.is_terminal() {
                        self.done = true;
                    }
                    return Some(*e);
                }
                FakeStep::Replay {
                    agent,
                    file,
                    exit_code,
                } => {
                    let kind = if agent == "codex" {
                        ProfileKind::Codex
                    } else {
                        ProfileKind::Claude
                    };
                    let text = match std::fs::read_to_string(&file) {
                        Ok(t) => t,
                        // a script error, not a scenario: never an empty stream (a "crash")
                        Err(e) => {
                            self.done = true;
                            return Some(AgentEvent::TurnFailed {
                                class: ErrorClass::Unknown,
                                detail: format!(
                                    "fake script: cannot read the replay file {}: {e}",
                                    file.display()
                                ),
                                structured: None,
                            });
                        }
                    };
                    let events = replay(kind, &text, exit_code.or(Some(0)));
                    for e in events.into_iter().rev() {
                        self.pending.push_front(FakeStep::from_event(e));
                    }
                }
            }
        }
    }

    async fn interrupt(&mut self) -> Result<()> {
        self.pending.clear();
        self.hang = false;
        self.delay_until = None;
        self.pending.push_back(FakeStep::Fail {
            class: ErrorClass::ProcessCrashed,
            detail: "interrupted".into(),
        });
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

impl FakeStep {
    /// Internal: re-queue a replayed event.
    fn from_event(e: AgentEvent) -> FakeStep {
        FakeStep::Emit(Box::new(e))
    }
}
