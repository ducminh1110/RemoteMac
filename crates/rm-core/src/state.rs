//! Provisioning state machine. `Ready` is only reachable through a completed
//! agent handshake; there is deliberately no shortcut from `Connecting`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    AuthFailed,
    WorkflowFailed,
    RunnerUnavailable,
    BootstrapFailed,
    AgentFailed,
    ConnectionFailed,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Idle,
    Authenticating,
    RequestingRunner,
    Queued,
    Provisioning,
    Bootstrapping,
    StartingAgent,
    Connecting,
    Ready,
    Stopping,
    Failed(Failure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Begin,
    Authenticated,
    WorkflowDispatched,
    RunnerAssigned,
    RunnerStarted,
    BootstrapDone,
    AgentStarted,
    /// Agent completed handshake AND reported capabilities.
    HandshakeComplete,
    Stop,
    Stopped,
    Fail(Failure),
    Reset,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid transition {from:?} --{event:?}-->")]
pub struct StateError {
    pub from: SessionState,
    pub event: Event,
}

impl SessionState {
    pub fn next(self, event: Event) -> Result<SessionState, StateError> {
        use Event as E;
        use SessionState as S;
        let to = match (self, event) {
            (S::Idle, E::Begin) => S::Authenticating,
            (S::Authenticating, E::Authenticated) => S::RequestingRunner,
            (S::RequestingRunner, E::WorkflowDispatched) => S::Queued,
            (S::Queued, E::RunnerAssigned) => S::Provisioning,
            (S::Provisioning, E::RunnerStarted) => S::Bootstrapping,
            (S::Bootstrapping, E::BootstrapDone) => S::StartingAgent,
            (S::StartingAgent, E::AgentStarted) => S::Connecting,
            (S::Connecting, E::HandshakeComplete) => S::Ready,
            (S::Idle | S::Failed(_), E::Stopped) => S::Idle,
            (S::Idle, E::Stop) => S::Idle,
            (s, E::Stop) if s != S::Stopping => S::Stopping,
            (S::Stopping, E::Stopped) => S::Idle,
            (S::Failed(_), E::Reset) | (S::Ready, E::Reset) => S::Idle,
            (S::Idle | S::Stopping | S::Failed(_), E::Fail(_)) => {
                return Err(StateError { from: self, event })
            }
            (_, E::Fail(f)) => S::Failed(f),
            _ => return Err(StateError { from: self, event }),
        };
        Ok(to)
    }

    /// What the UI may label "Connected".
    pub fn is_connected(self) -> bool {
        self == SessionState::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Event::*;

    #[test]
    fn happy_path() {
        let mut s = SessionState::Idle;
        for e in [Begin, Authenticated, WorkflowDispatched, RunnerAssigned, RunnerStarted, BootstrapDone, AgentStarted] {
            s = s.next(e).unwrap();
            assert!(!s.is_connected());
        }
        assert_eq!(s, SessionState::Connecting);
        s = s.next(HandshakeComplete).unwrap();
        assert!(s.is_connected());
    }

    #[test]
    fn cannot_skip_to_ready() {
        for s in [SessionState::Idle, SessionState::Queued, SessionState::StartingAgent, SessionState::Connecting] {
            if s != SessionState::Connecting {
                assert!(s.next(HandshakeComplete).is_err());
            }
        }
        assert!(SessionState::StartingAgent.next(HandshakeComplete).is_err());
    }

    #[test]
    fn failure_from_any_active_state_then_reset() {
        let s = SessionState::Provisioning.next(Fail(Failure::RunnerUnavailable)).unwrap();
        assert_eq!(s, SessionState::Failed(Failure::RunnerUnavailable));
        assert!(!s.is_connected());
        assert_eq!(s.next(Reset).unwrap(), SessionState::Idle);
        assert!(s.next(Begin).is_err());
    }

    #[test]
    fn stop_always_possible_and_ends_idle() {
        for s in [SessionState::Queued, SessionState::Ready, SessionState::Failed(Failure::Timeout)] {
            let st = s.next(Stop).unwrap();
            assert_eq!(st, SessionState::Stopping);
            assert_eq!(st.next(Stopped).unwrap(), SessionState::Idle);
        }
    }
}
