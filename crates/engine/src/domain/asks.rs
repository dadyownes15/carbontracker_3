//! [Asks]: whether a cut relies on a request already sent to a provider, or
//! sends one. Pure: the runtime feeds it what the provider's gate is doing.

use super::Timestamp;
use super::gate::Asker;
use super::session::RequestId;

/// What the provider's gate is doing when a cut arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateState {
    /// A request sent now starts a read now.
    Ready,
    /// Cooling down or reading.
    Unready,
    /// Out past its timeout, or retired: a request fails at once.
    Failing,
    /// A `Never` provider that has answered with samples: it answers every
    /// cut at once.
    Done,
}

/// What a cut does about one provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asked {
    /// Relies on a request already sent.
    Relies(RequestId),
    /// Sends a new request.
    Send,
    /// Needs no request: the provider has answered everything it will.
    Nothing,
}

/// One provider's requests, as cuts see them.
#[derive(Debug, Default)]
pub(crate) struct Asks {
    /// The latest read started, out or landed, and when.
    latest_read: Option<(RequestId, Timestamp)>,
    /// A cut's request answered without a read while the provider has stayed
    /// unready since.
    unready_answer: Option<RequestId>,
    /// A `Never` provider: its value cannot change, so a read out answers a
    /// cut whenever it started.
    never: bool,
    /// The read out, not yet answered.
    out: Option<RequestId>,
}

impl Asks {
    /// For a provider whose cooldown is `Never`.
    pub(crate) fn never() -> Self {
        Self { never: true, ..Self::default() }
    }

    /// A cut stamped `stamp` arrives while the gate is in `gate`, with
    /// `queued` waiting for it to be ready. The first row that fits decides.
    pub(crate) fn cut(&self, stamp: Timestamp, gate: GateState, queued: Option<RequestId>) -> Asked {
        if let Some((read, started)) = self.latest_read
            && started >= stamp
        {
            return Asked::Relies(read);
        }
        if gate == GateState::Done {
            return Asked::Nothing;
        }
        if let (true, GateState::Unready, Some(read)) = (self.never, gate, self.out) {
            return Asked::Relies(read);
        }
        match gate {
            GateState::Done => Asked::Nothing,
            GateState::Ready | GateState::Failing => Asked::Send,
            GateState::Unready => match queued.or(self.unready_answer) {
                Some(request) => Asked::Relies(request),
                None => Asked::Send,
            },
        }
    }

    /// A read for `request` started at `at`.
    pub(crate) fn started(&mut self, request: RequestId, at: Timestamp) {
        self.latest_read = Some((request, at));
        self.out = Some(request);
    }

    /// `request`, sent by `asker`, was answered; `read` if a read made for it
    /// returned. A cut's answer without a read is relied on while the
    /// provider stays unready; a watchdog's never is.
    pub(crate) fn answered(&mut self, request: RequestId, asker: Asker, read: bool) {
        if self.out == Some(request) {
            self.out = None;
        }
        if !read && asker == Asker::Cut {
            self.unready_answer = Some(request);
        }
    }

    /// The provider changed state: a read started, returned or timed out,
    /// or it is ready. An answer from before is relied on no more.
    pub(crate) fn transition(&mut self) {
        self.unready_answer = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ReadoutId;

    fn ms(ms: i64) -> Timestamp {
        chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::milliseconds(ms)
    }

    fn r(n: usize) -> RequestId {
        ReadoutId(n)
    }

    /// AC/MEAS-6
    #[test]
    fn a_cut_relies_on_a_read_that_started_after_it() {
        let mut asks = Asks::default();
        asks.started(r(7), ms(3400));
        assert_eq!(asks.cut(ms(3300), GateState::Unready, None), Asked::Relies(r(7)));
    }

    /// AC/MEAS-6
    #[test]
    fn a_cut_relies_on_a_read_that_started_at_its_stamp() {
        let mut asks = Asks::default();
        asks.started(r(7), ms(3300));
        assert_eq!(asks.cut(ms(3300), GateState::Ready, None), Asked::Relies(r(7)));
    }

    /// AC/MEAS-6
    #[test]
    fn a_read_started_before_the_cut_is_not_relied_on() {
        let mut asks = Asks::default();
        asks.started(r(7), ms(3290));
        assert_eq!(asks.cut(ms(3300), GateState::Ready, None), Asked::Send);
    }

    /// AC/MEAS-7
    #[test]
    fn a_ready_provider_is_sent_a_request() {
        assert_eq!(Asks::default().cut(ms(0), GateState::Ready, None), Asked::Send);
    }

    /// AC/MEAS-8, AC/MEAS-11, AC/PROF-1
    #[test]
    fn a_cut_relies_on_the_queued_request() {
        let mut asks = Asks::default();
        asks.started(r(1), ms(3000));
        assert_eq!(asks.cut(ms(3300), GateState::Unready, None), Asked::Send);
        // The gate queued that request as r(2).
        assert_eq!(asks.cut(ms(3310), GateState::Unready, Some(r(2))), Asked::Relies(r(2)));
        assert_eq!(asks.cut(ms(3900), GateState::Unready, Some(r(2))), Asked::Relies(r(2)));
        asks.started(r(2), ms(4000));
        assert_eq!(asks.cut(ms(3950), GateState::Unready, None), Asked::Relies(r(2)), "the queued read answers all of them");
    }

    /// AC/MEAS-9
    #[test]
    fn while_unready_a_cut_relies_on_the_last_answer_without_a_read() {
        let mut asks = Asks::default();
        asks.started(r(1), ms(2900));
        assert_eq!(asks.cut(ms(3050), GateState::Unready, None), Asked::Send);
        asks.answered(r(2), Asker::Cut, false);
        for stamp in [3300, 3310, 3750, 3900] {
            assert_eq!(asks.cut(ms(stamp), GateState::Unready, None), Asked::Relies(r(2)));
        }
    }

    /// AC/MEAS-12
    #[test]
    fn after_a_transition_a_cut_is_sent_a_new_request() {
        let mut asks = Asks::default();
        asks.answered(r(2), Asker::Cut, false);
        asks.transition();
        assert_eq!(asks.cut(ms(63_910), GateState::Ready, None), Asked::Send);
        assert_eq!(asks.cut(ms(63_910), GateState::Unready, None), Asked::Send);
    }

    /// AC/MEAS-12
    #[test]
    fn a_watchdog_answer_without_a_read_is_not_relied_on() {
        let mut asks = Asks::default();
        asks.answered(r(5), Asker::Watchdog, false);
        assert_eq!(asks.cut(ms(1000), GateState::Unready, None), Asked::Send);
    }

    /// AC/MEAS-66
    #[test]
    fn a_cut_relies_on_a_watchdog_read() {
        let mut asks = Asks::default();
        asks.started(r(5), ms(3500));
        assert_eq!(asks.cut(ms(3400), GateState::Unready, None), Asked::Relies(r(5)));
    }

    /// AC/MEAS-10, AC/MEAS-33
    #[test]
    fn a_failing_provider_is_sent_a_request_that_fails_at_once() {
        let mut asks = Asks::default();
        asks.answered(r(2), Asker::Cut, false);
        assert_eq!(asks.cut(ms(5500), GateState::Failing, None), Asked::Send);
    }

    /// AC/MEAS-5
    #[test]
    fn the_first_row_that_fits_decides() {
        let mut asks = Asks::default();
        asks.started(r(4), ms(3400));
        assert_eq!(asks.cut(ms(3300), GateState::Unready, Some(r(5))), Asked::Relies(r(4)));
    }

    /// AC/MEAS-15, AC/MEAS-35
    #[test]
    fn a_never_provider_that_has_answered_is_relied_on_with_no_request() {
        assert_eq!(Asks::default().cut(ms(0), GateState::Done, None), Asked::Nothing);
    }

    /// AC/MEAS-15
    #[test]
    fn a_never_provider_before_its_first_answer_follows_the_table() {
        let asks = Asks::default();
        assert_eq!(asks.cut(ms(0), GateState::Ready, None), Asked::Send);
        assert_eq!(asks.cut(ms(0), GateState::Unready, Some(r(1))), Asked::Relies(r(1)));
    }

    /// AC/MEAS-15, AC/MEAS-58
    #[test]
    fn a_cut_to_a_never_provider_relies_on_the_read_out_whenever_it_started() {
        let mut asks = Asks::never();
        asks.started(r(1), ms(3000));
        assert_eq!(asks.cut(ms(3300), GateState::Unready, None), Asked::Relies(r(1)));
        asks.answered(r(1), Asker::Cut, true);
        assert_eq!(asks.cut(ms(3400), GateState::Ready, None), Asked::Send, "an incomplete answer is read again");
        assert_eq!(asks.cut(ms(3500), GateState::Done, None), Asked::Nothing);
    }
}
