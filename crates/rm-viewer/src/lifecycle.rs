//! The connection's life as the user sees it: one phase at a time, every change checked, so the
//! connect window, the reconnect banner and the stats overlay always say the same thing.
//!
//! ```text
//! Idle -> Connecting -> Authenticating -> Negotiating -> EstablishingMedia -> Connected
//! Connected -> Reconnecting -> Connecting ...        (the network dropped; tries for 2 minutes)
//! Connected -> Disconnecting -> Disconnected         (the user closed the session)
//! any step -> Error(why) -> Connecting | Reconnecting | Idle
//! ```

use std::sync::Mutex;
use std::time::Instant;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// finding the Mac (this network, a typed address, or the relay) and joining it
    Connecting,
    /// the end-to-end handshake: the password proved, the keys agreed
    Authenticating,
    /// versions, features and the Mac's capabilities
    Negotiating,
    /// the UDP path, video settings, sound
    EstablishingMedia,
    Connected,
    /// the connection was lost: connecting again by itself
    Reconnecting,
    Disconnecting,
    Disconnected,
    Error(String),
}

impl Phase {
    /// What the user reads.
    pub fn label(&self) -> String {
        match self {
            Phase::Idle => "Ready to connect".into(),
            Phase::Connecting => "Looking for your Mac…".into(),
            Phase::Authenticating => "Checking the password…".into(),
            Phase::Negotiating => "Setting up the session…".into(),
            Phase::EstablishingMedia => "Starting video and sound…".into(),
            Phase::Connected => "Connected".into(),
            Phase::Reconnecting => "Connection lost — connecting again…".into(),
            Phase::Disconnecting => "Disconnecting…".into(),
            Phase::Disconnected => "Disconnected".into(),
            Phase::Error(e) => e.clone(),
        }
    }

    /// Steps of a connection in progress (the spinner shows while one is).
    pub fn busy(&self) -> bool {
        matches!(self, Phase::Connecting | Phase::Authenticating | Phase::Negotiating | Phase::EstablishingMedia | Phase::Reconnecting | Phase::Disconnecting)
    }

    /// Whether `to` may follow this phase.
    pub fn allows(&self, to: &Phase) -> bool {
        use Phase::*;
        match (self, to) {
            (_, Error(_)) => !matches!(self, Idle | Disconnected),
            (Idle | Disconnected | Error(_) | Reconnecting, Connecting) => true,
            (Connecting, Authenticating) | (Authenticating, Negotiating) | (Negotiating, EstablishingMedia) | (EstablishingMedia, Connected) => true,
            (Connected | Error(_), Reconnecting) => true,
            (Connected | Connecting | Authenticating | Negotiating | EstablishingMedia | Reconnecting, Disconnecting) => true,
            (Disconnecting | Connected | Error(_) | Reconnecting, Disconnected) => true,
            (Error(_) | Disconnected, Idle) => true,
            _ => false,
        }
    }
}

struct Current {
    phase: Phase,
    since: Instant,
    /// when the attempt in progress started (Connecting or Reconnecting), for its duration
    attempt: Option<Instant>,
}

static CURRENT: Mutex<Option<Current>> = Mutex::new(None);

/// Move to `to` if it may follow the current phase (a step out of order is logged and ignored:
/// late news from an attempt that was given up must not undo a newer state).
pub fn set(to: Phase) -> bool {
    let mut c = CURRENT.lock().unwrap();
    let from = c.as_ref().map_or(Phase::Idle, |c| c.phase.clone());
    if from == to {
        return true;
    }
    if !from.allows(&to) {
        eprintln!("connection: {from:?} -> {to:?} ignored (out of order)");
        return false;
    }
    // the log says how long each step took (measured here, nothing estimated)
    let now = Instant::now();
    let took = c.as_ref().map_or(0, |c| now.duration_since(c.since).as_millis());
    let attempt = match to {
        Phase::Connecting if from != Phase::Reconnecting => Some(now),
        Phase::Reconnecting => Some(now),
        _ => c.as_ref().and_then(|c| c.attempt),
    };
    match &to {
        Phase::Error(_) => eprintln!("connection: {from:?} failed after {took} ms"),
        Phase::Connected => eprintln!("connection: Connected ({from:?} took {took} ms; {} ms in all)", attempt.map_or(0, |a| now.duration_since(a).as_millis())),
        _ if from == Phase::Idle => eprintln!("connection: {}", to.label()),
        _ => eprintln!("connection: {} ({from:?} took {took} ms)", to.label()),
    }
    *c = Some(Current { phase: to, since: now, attempt });
    true
}

pub fn current() -> Phase {
    CURRENT.lock().unwrap().as_ref().map_or(Phase::Idle, |c| c.phase.clone())
}

/// How long the current phase has lasted.
pub fn age() -> std::time::Duration {
    CURRENT.lock().unwrap().as_ref().map_or(Default::default(), |c| c.since.elapsed())
}

#[cfg(test)]
mod tests {
    use super::Phase::*;

    #[test]
    fn a_connection_goes_through_its_steps_in_order() {
        let steps = [Idle, Connecting, Authenticating, Negotiating, EstablishingMedia, Connected, Reconnecting, Connecting, Authenticating, Negotiating, EstablishingMedia, Connected, Disconnecting, Disconnected, Idle];
        for w in steps.windows(2) {
            assert!(w[0].allows(&w[1]), "{:?} -> {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn steps_out_of_order_are_refused() {
        assert!(!Idle.allows(&Connected), "no connection without the handshake");
        assert!(!Connecting.allows(&Negotiating), "no session before the password is proved");
        assert!(!Authenticating.allows(&Connected));
        assert!(!Disconnected.allows(&Reconnecting));
        assert!(!Idle.allows(&Error("x".into())));
        assert!(Negotiating.allows(&Error("x".into())));
        assert!(Error("x".into()).allows(&Connecting) && Error("x".into()).allows(&Reconnecting));
    }

    #[test]
    fn labels_and_busy() {
        assert!(Connecting.busy() && Reconnecting.busy() && !Connected.busy() && !Error("e".into()).busy());
        assert_eq!(Error("Wrong password.".into()).label(), "Wrong password.");
        assert!(Authenticating.label().contains("password"));
    }
}
