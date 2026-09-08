//! Cairn domain model.
//!
//! Policy, channels, sessions and admission control — the rules of the server, expressed
//! as pure data and pure functions over an explicit clock. No sockets, no runtime, no
//! storage. That is what lets the parts most likely to be argued about (who gets
//! operator, what warnet mode gates, when a bot is throttled) be tested exhaustively and
//! deterministically.
//!
//! The I/O that drives this lives in `cairnd`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod channel;
pub mod limits;
pub mod policy;
pub mod session;

pub use channel::{AccountId, Channel, ChannelClass, JoinDenial, JoinOutcome, LeaveOutcome};
pub use limits::{
    AdmissionTable, ConnClass, FloodTracker, FloodVerdict, KeyId, KeyRegistry, KeyVerdict,
    Rejection,
};
pub use policy::{ChatOrdering, ConnLimits, FloodPenalty, FloodPolicy, Gate, Policy, ServerMode};
pub use session::{Deadlines, SessionState};
