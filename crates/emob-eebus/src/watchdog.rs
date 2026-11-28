//! Receipt-time LPC state machine. The caller supplies monotonically ordered
//! events and the evaluation instant. Each process starts in init (LPC-901).

use eebus::model::{CmdData, LoadControlLimitId};
use emob_smart::GridLimit;
use rust_decimal::Decimal;
use time::{Duration, OffsetDateTime};

use crate::{EebusError, limit::entry_for, read_limit_write};

/// Default residence in failsafe before autonomous operation (LPC-022, LPC-922).
pub const MIN_FAILSAFE_RESIDENCE: Duration = Duration::hours(2);
const HEARTBEAT_TIMEOUT: Duration = Duration::seconds(120);
const RECOVERY_WINDOW: Duration = Duration::seconds(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Init,
    Controlled {
        accepted_at: OffsetDateTime,
        active_kw: Option<Decimal>,
    },
    Lost {
        last_heartbeat: OffsetDateTime,
    },
}

/// Volatile control state for one commissioned Energy Guard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watchdog {
    failsafe_kw: Decimal,
    residence: Duration,
    heartbeat: Option<OffsetDateTime>,
    last_event: Option<OffsetDateTime>,
    deadline: Option<OffsetDateTime>,
    phase: Phase,
}

impl Watchdog {
    /// Start in init at the site's failsafe power. The local autonomous-release
    /// policy bounds the configured residence to the LPC-022 range (2–24 h).
    #[must_use]
    pub fn new(failsafe_kw: Decimal, residence: Duration) -> Self {
        Self {
            failsafe_kw,
            residence: residence.clamp(MIN_FAILSAFE_RESIDENCE, Duration::hours(24)),
            heartbeat: None,
            last_event: None,
            deadline: None,
            phase: Phase::Init,
        }
    }

    fn phase_at(&self, now: OffsetDateTime) -> Phase {
        if matches!(self.phase, Phase::Controlled { .. })
            && let Some(last_heartbeat) = self.heartbeat
            && now - last_heartbeat >= HEARTBEAT_TIMEOUT
        {
            return Phase::Lost { last_heartbeat };
        }
        self.phase
    }

    /// Receive a heartbeat. A late heartbeat cannot reinstate a lost limit.
    pub fn heard_from(&mut self, at: OffsetDateTime) {
        if self.last_event.is_some_and(|last| at < last) {
            return;
        }
        self.phase = self.phase_at(at);
        self.last_event = Some(at);
        self.heartbeat = Some(at);
    }

    /// Evaluate a wire write, using the fragment for field presence and the
    /// SPINE-resolved value for the requested state.
    ///
    /// # Errors
    ///
    /// Refuses malformed, inapplicable or out-of-window instructions. A valid
    /// but inapplicable recovery write exits to unlimited/controlled (LPC-918).
    pub fn on_write(
        &mut self,
        data: &CmdData,
        resolved: &CmdData,
        id: LoadControlLimitId,
        at: OffsetDateTime,
    ) -> Result<GridLimit, EebusError> {
        if self.last_event.is_some_and(|last| at < last) {
            return Err(EebusError::OutOfSequence);
        }
        self.phase = self.phase_at(at);
        self.last_event = Some(at);
        let recovering = !matches!(self.phase, Phase::Controlled { .. });
        if recovering
            && self
                .heartbeat
                .is_none_or(|last| at - last > RECOVERY_WINDOW)
        {
            return Err(EebusError::HeartbeatRequired);
        }
        match self.accept_write(data, resolved, id, at) {
            Ok(()) => Ok(self.limit_at(at)),
            Err(error) => {
                if recovering
                    && matches!(
                        error,
                        EebusError::OutsidePrecision | EebusError::UnknownValidity
                    )
                {
                    self.phase = Phase::Controlled {
                        accepted_at: at,
                        active_kw: None,
                    };
                }
                Err(error)
            }
        }
    }

    fn accept_write(
        &mut self,
        data: &CmdData,
        resolved: &CmdData,
        id: LoadControlLimitId,
        at: OffsetDateTime,
    ) -> Result<(), EebusError> {
        let fragment = entry_for(data, id)?;
        let asked = read_limit_write(resolved, id)?;
        let writes_end = fragment
            .time_period
            .as_ref()
            .is_some_and(|p| p.end_time.is_some());
        let deadline = match asked.valid_for {
            None => None,
            Some(duration) if writes_end => Some(
                at.checked_add(duration)
                    .ok_or(EebusError::OutsidePrecision)?,
            ),
            Some(_) => Some(self.deadline.ok_or(EebusError::UnknownValidity)?),
        };
        // LPC-009 deactivates the limit on loss of contact. LPC-004 keeps the
        // remaining validity running even while the limit is deactivated.
        let was_active = matches!(self.limit_at(at), GridLimit::Curtailed(_));
        let active = asked.is_active
            && fragment.is_limit_active.unwrap_or(was_active)
            && deadline.is_none_or(|end| at < end);
        let thousand = Decimal::from(1000);
        let kw = asked.watts / thousand;
        if kw.checked_mul(thousand) != Some(asked.watts) {
            return Err(EebusError::OutsidePrecision);
        }
        self.deadline = deadline;
        self.phase = Phase::Controlled {
            accepted_at: at,
            active_kw: active.then_some(kw),
        };
        Ok(())
    }

    /// Effective state at or after the most recent receipt. Replay earlier receipts for a historical query.
    #[must_use]
    pub fn limit_at(&self, now: OffsetDateTime) -> GridLimit {
        match self.phase_at(now) {
            Phase::Init => GridLimit::Initializing {
                limit_kw: self.failsafe_kw,
            },
            Phase::Controlled {
                accepted_at,
                active_kw,
            } => {
                if let Some(kw) = active_kw
                    && self.deadline.is_none_or(|end| now < end)
                {
                    GridLimit::curtailed(kw, accepted_at)
                } else {
                    GridLimit::InContact {
                        heard_at: self.heartbeat.unwrap_or(accepted_at),
                    }
                }
            }
            Phase::Lost { last_heartbeat }
                if now - last_heartbeat >= HEARTBEAT_TIMEOUT + self.residence =>
            {
                GridLimit::Autonomous {
                    silent_since: last_heartbeat,
                }
            }
            Phase::Lost { last_heartbeat } => GridLimit::failsafe(self.failsafe_kw, last_heartbeat),
        }
    }
}
