//! Point availability, its most recent source report and the instant its meaning changed.

use emob_service::publication::ConnectorObservation;
use time::OffsetDateTime;

#[derive(Debug, Clone, Default)]
pub(super) struct ConnectorState {
    /// Effective status. Its timestamp always belongs to the underlying device report.
    observed: Option<ConnectorObservation>,
    /// The device's actual word, before applying an operator reservation.
    device_status: Option<String>,
    /// Reports at or below a broken source boundary cannot restore trust.
    fence: Option<OffsetDateTime>,
    changed: Option<OffsetDateTime>,
}

impl ConnectorState {
    fn fence_at(&mut self, at: OffsetDateTime) {
        self.fence = self.fence.max(Some(at));
    }

    pub(super) fn invalidate(&mut self, at: OffsetDateTime, source_transition: bool) {
        self.fence = self.fence.max(self.observed.as_ref().map(|o| o.at));
        self.fence_at(at);
        let had_evidence = self.observed.take().is_some();
        self.device_status = None;
        if had_evidence || source_transition {
            self.changed = self.changed.max(Some(at));
        }
    }

    pub(super) fn lose_clock(&mut self, received: OffsetDateTime) -> bool {
        let had_evidence = self.observed.is_some();
        self.invalidate(received, false);
        had_evidence
    }

    pub(super) fn observe(
        &mut self,
        mut report: ConnectorObservation,
        received_at: Option<OffsetDateTime>,
        reserved_until: Option<OffsetDateTime>,
    ) -> bool {
        if let Some(received) = received_at
            && report.at > received
        {
            // Keep the receiver clock as the loss-of-trust boundary, never the future device clock.
            return self.lose_clock(received);
        }
        if let Some(old) = &self.observed {
            if report.at < old.at {
                return false;
            }
            if report.at == old.at {
                if self.device_status.as_deref() == Some(&report.status) {
                    return false;
                }
                // Two different words for one source instant: neither is authoritative.
                self.invalidate(report.at, true);
                return true;
            }
        }
        if self.fence.is_some_and(|cutoff| report.at <= cutoff) {
            return false;
        }
        self.device_status = Some(report.status.clone());
        if report.status == "Available" && reserved_until.is_some_and(|until| report.at < until) {
            report.status = "Reserved".into();
        }
        if self
            .observed
            .as_ref()
            .is_none_or(|old| old.status != report.status)
        {
            self.changed = Some(report.at);
        }
        self.observed = Some(report);
        true
    }

    pub(super) fn reserve(&mut self, at: OffsetDateTime) {
        self.fence_at(at);
        if let Some(report) = &mut self.observed
            && report.status == "Available"
        {
            report.status = "Reserved".into();
            self.changed = Some(at);
        }
    }

    pub(super) fn release(&mut self, at: OffsetDateTime) {
        self.fence_at(at);
        if self.reserved_as_of(at) {
            self.invalidate(at, true);
        }
    }

    pub(super) const fn has_report(&self) -> bool {
        self.observed.is_some()
    }

    pub(super) fn reserved_as_of(&self, at: OffsetDateTime) -> bool {
        self.observed
            .as_ref()
            .is_some_and(|o| o.status == "Reserved" && o.at <= at)
    }

    pub(super) fn projection(&self) -> (Option<ConnectorObservation>, Option<OffsetDateTime>) {
        (self.observed.clone(), self.changed)
    }
}
