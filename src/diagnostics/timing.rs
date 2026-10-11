use serde::{Deserialize, Serialize};
use std::{io, time::Instant};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    SlotWait,
    StartupWait,
    ImageWait,
    ImageBuild,
    Boot,
    Source,
    BaseSetup,
    DependencySetup,
    Capabilities,
    Baseline,
    ProviderPreparation,
    ProviderConnection,
    FirstResponse,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Self::SlotWait => "Worker slot wait",
            Self::StartupWait => "Codemod startup wait",
            Self::ImageWait => "Shared image wait",
            Self::ImageBuild => "Image build",
            Self::Boot => "VM boot",
            Self::Source => "Source and worktrees",
            Self::BaseSetup => "Base setup and isolation",
            Self::DependencySetup => "Dependency setup",
            Self::Capabilities => "Capability probes",
            Self::Baseline => "Starting baseline",
            Self::ProviderPreparation => "Provider preparation",
            Self::ProviderConnection => "Provider connection",
            Self::FirstResponse => "First model response",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Observation {
    pub phase: Phase,
    pub duration_ms: u64,
    pub source: String,
    pub environment: Option<String>,
    pub completed: bool,
    #[serde(default)]
    pub attempt: Option<String>,
}

#[derive(Clone, Copy)]
pub struct Timing<'a> {
    source: &'a str,
    environment: Option<&'a str>,
    emit: &'a dyn Fn(Observation),
}

impl<'a> Timing<'a> {
    pub fn new(source: &'a str, emit: &'a dyn Fn(Observation)) -> Self {
        Self {
            source,
            environment: None,
            emit,
        }
    }

    pub fn quiet() -> Self {
        Self::new("", &|_| {})
    }

    pub fn with_environment(self, environment: &'a str) -> Self {
        Self {
            environment: Some(environment),
            ..self
        }
    }
    pub fn emit(self, observation: Observation) {
        (self.emit)(observation);
    }

    pub fn span(self, phase: Phase) -> Span<'a> {
        Span {
            timing: self,
            phase,
            started: Instant::now(),
            completed: false,
        }
    }

    pub fn measure<T>(
        self,
        phase: Phase,
        operation: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let started = Instant::now();
        let result = operation();
        self.elapsed(phase, started, result.is_ok());
        result
    }

    pub fn elapsed(self, phase: Phase, started: Instant, completed: bool) {
        (self.emit)(Observation {
            phase,
            duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            source: self.source.into(),
            environment: self.environment.map(str::to_owned),
            completed,
            attempt: None,
        });
    }
}

pub struct Span<'a> {
    timing: Timing<'a>,
    phase: Phase,
    started: Instant,
    completed: bool,
}

impl Span<'_> {
    pub fn finish(mut self) {
        self.completed = true;
    }
}

impl Drop for Span<'_> {
    fn drop(&mut self) {
        self.timing
            .elapsed(self.phase, self.started, self.completed);
    }
}

pub struct FirstResponse(Option<(String, Option<String>, Instant)>);

impl FirstResponse {
    pub fn new() -> Self {
        Self(None)
    }
    pub fn start(&mut self, attempt: &str, environment: Option<&str>) {
        self.0 = Some((
            attempt.into(),
            environment.map(str::to_owned),
            Instant::now(),
        ));
    }
    pub fn observe(&mut self, message: &serde_json::Value) -> Option<Observation> {
        let method = message["method"].as_str().unwrap_or("");
        let model_output = matches!(
            method,
            "item/agentMessage/delta"
                | "item/reasoning/summaryTextDelta"
                | "item/reasoning/textDelta"
        ) || matches!(method, "item/started" | "item/completed")
            && matches!(
                message["params"]["item"]["type"].as_str(),
                Some("agentMessage" | "reasoning" | "commandExecution" | "mcpToolCall")
            );
        if !model_output {
            return None;
        }
        let (attempt, environment, started) = self.0.take()?;
        Some(Observation {
            phase: Phase::FirstResponse,
            duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
            source: String::new(),
            environment,
            completed: true,
            attempt: Some(attempt),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_model_output_records_first_response_once_per_attempt() {
        let mut clock = FirstResponse::new();
        clock.start("attempt-1", Some("sha256:guest"));
        assert!(clock.observe(&json!({"method":"turn/started"})).is_none());
        let observation = clock
            .observe(&json!({"method":"item/reasoning/textDelta","params":{"delta":"thinking"}}))
            .unwrap();
        assert_eq!(observation.phase, Phase::FirstResponse);
        assert_eq!(observation.attempt.as_deref(), Some("attempt-1"));
        assert_eq!(observation.environment.as_deref(), Some("sha256:guest"));
        assert!(
            clock
                .observe(&json!({"method":"item/agentMessage/delta"}))
                .is_none()
        );
        clock.start("attempt-2", None);
        assert_eq!(
            clock
                .observe(
                    &json!({"method":"item/completed","params":{"item":{"type":"agentMessage"}}})
                )
                .unwrap()
                .attempt
                .as_deref(),
            Some("attempt-2")
        );
    }
    #[test]
    fn failed_operation_keeps_its_source_and_failure_observation() {
        let observations = std::cell::RefCell::new(Vec::new());
        let emit = |observation| observations.borrow_mut().push(observation);
        let result: io::Result<()> = Timing::new("source-1", &emit)
            .with_environment("environment-1")
            .measure(Phase::Source, || {
                Err(io::Error::other("source import failed"))
            });
        assert_eq!(result.unwrap_err().to_string(), "source import failed");
        let observations = observations.into_inner();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].source, "source-1");
        assert_eq!(
            observations[0].environment.as_deref(),
            Some("environment-1")
        );
        assert_eq!(observations[0].phase, Phase::Source);
        assert!(!observations[0].completed);
    }

    #[test]
    fn connection_failure_and_readiness_record_one_span_each() {
        let observations = std::cell::RefCell::new(Vec::new());
        let emit = |observation| observations.borrow_mut().push(observation);
        let timing = Timing::new("source-1", &emit).with_environment("sha256:guest");
        {
            let _connection = timing.span(Phase::ProviderConnection);
        }
        timing.span(Phase::ProviderConnection).finish();
        let observations = observations.into_inner();
        assert_eq!(observations.len(), 2);
        assert!(!observations[0].completed);
        assert!(observations[1].completed);
        assert!(
            observations
                .iter()
                .all(|observation| observation.source == "source-1"
                    && observation.environment.as_deref() == Some("sha256:guest"))
        );
    }
}
