//! The specialists, whose work is computation.
//!
//! # Written as code, not as a prompt
//!
//! Every specialist here is a deterministic function over data a daemon already
//! holds. None of them calls a model, and that is the design rather than a
//! stage on the way to one: these questions have exact answers — which meter
//! accounts for today's refusals, which points offer a tariff the article
//! forbids — and an answer that varied between two runs of the same input would
//! be useless in the queue it lands in and indefensible in the dispute it feeds.
//!
//! What `agentplane` provides for a function like that is not inference. It is
//! the **journal**: the run, its input, its answer and every effect are written
//! to an append-only hash-chained log, so "why did the queue say that in March"
//! is a replay rather than an argument. That is worth having for a pure function
//! too — arguably most for one, because a replay of a pure function is exact.
//!
//! A specialist whose work genuinely needs a model — reading a partner's
//! free-text dispute, say — is a manifest rather than a module, and it goes
//! through the same [`crate::advice::Advice`] leaf as these do.

pub mod compliance;
pub mod evidence;
pub mod tariff;
