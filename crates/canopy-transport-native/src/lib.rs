//! Canopy in-process (native) transport: the compiled-in counterpart to the
//! WASM-sandboxed transport.
//!
//! When the guest is linked directly into the host process there is no address
//! space to cross, so a "transport" here is just a pair of shared queues. The op
//! batches the guest emits and the event batches the host returns are moved as
//! whole `Vec<u8>` — the **exact same bytes** the sandboxed transport would carry,
//! only without serialization, a copy across a boundary, or a trust check. Swapping
//! this for the WASM transport changes the delivery mechanism and the trust model,
//! never the wire format.
//!
//! [`channel`] yields a connected [`GuestEnd`] / [`HostEnd`] pair backed by
//! `Rc<RefCell<VecDeque<_>>>`. The guest's [`Transport`] impl `send`s op batches the
//! host [`HostEnd::drain_ops`] reads, and the host [`HostEnd::push_event`]s event
//! batches the guest's [`Transport::poll_events`] drains. Single-threaded by
//! construction (`Rc`/`RefCell`), matching the rest of the guest runtime.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use canopy_traits::{Transport, TransportError};

/// A queue of opaque byte batches shared between the two ends of a [`channel`].
type Queue = Rc<RefCell<VecDeque<Vec<u8>>>>;

/// Create a connected guest/host pair sharing two queues: one for guest→host op
/// batches, one for host→guest event batches.
///
/// Both ends are clones of the same `Rc`s, so dropping one leaves the other able to
/// read whatever was already queued. There is no backpressure or size limit in this
/// in-process transport.
pub fn channel() -> (GuestEnd, HostEnd) {
    let ops: Queue = Rc::new(RefCell::new(VecDeque::new()));
    let events: Queue = Rc::new(RefCell::new(VecDeque::new()));
    (
        GuestEnd {
            ops: Rc::clone(&ops),
            events: Rc::clone(&events),
        },
        HostEnd { ops, events },
    )
}

/// The guest-facing end of a native channel. This is the type that implements
/// [`Transport`]: `send` enqueues an op batch for the host; `poll_events` drains the
/// host's pending event batches.
#[derive(Clone)]
pub struct GuestEnd {
    ops: Queue,
    events: Queue,
}

impl GuestEnd {
    /// Number of op batches still queued for the host to drain.
    pub fn pending_ops(&self) -> usize {
        self.ops.borrow().len()
    }

    /// Number of event batches still queued for this guest to poll.
    pub fn pending_events(&self) -> usize {
        self.events.borrow().len()
    }
}

impl Transport for GuestEnd {
    fn send(&mut self, batch: &[u8]) -> Result<(), TransportError> {
        self.ops.borrow_mut().push_back(batch.to_vec());
        Ok(())
    }

    fn poll_events(&mut self, out: &mut Vec<u8>) -> Result<(), TransportError> {
        let mut events = self.events.borrow_mut();
        while let Some(batch) = events.pop_front() {
            out.extend_from_slice(&batch);
        }
        Ok(())
    }
}

/// The host-facing end of a native channel. The host drains the op batches the
/// guest sent and pushes event batches back for the guest to poll.
#[derive(Clone)]
pub struct HostEnd {
    ops: Queue,
    events: Queue,
}

impl HostEnd {
    /// Pop the next op batch the guest sent, in send order, or `None` if the queue
    /// is empty.
    pub fn next_ops(&mut self) -> Option<Vec<u8>> {
        self.ops.borrow_mut().pop_front()
    }

    /// Drain every queued op batch in send order, returning them as a list.
    pub fn drain_ops(&mut self) -> Vec<Vec<u8>> {
        self.ops.borrow_mut().drain(..).collect()
    }

    /// Queue one event batch for the guest to receive via [`Transport::poll_events`].
    pub fn push_event(&mut self, batch: &[u8]) {
        self.events.borrow_mut().push_back(batch.to_vec());
    }

    /// Number of op batches waiting to be drained.
    pub fn pending_ops(&self) -> usize {
        self.ops.borrow().len()
    }

    /// Number of event batches queued for the guest.
    pub fn pending_events(&self) -> usize {
        self.events.borrow().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_batches_arrive_in_order_and_events_round_trip() {
        let (mut guest, mut host) = channel();

        // Guest emits two op batches; the host drains them in order.
        guest.send(&[1, 2, 3]).unwrap();
        guest.send(&[4, 5]).unwrap();
        assert_eq!(host.pending_ops(), 2);

        assert_eq!(host.next_ops().as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(host.next_ops().as_deref(), Some(&[4, 5][..]));
        assert_eq!(host.next_ops(), None);
        assert_eq!(host.pending_ops(), 0);

        // Host pushes an event; the guest polls it back out.
        host.push_event(&[9, 9]);
        assert_eq!(guest.pending_events(), 1);
        let mut out = Vec::new();
        guest.poll_events(&mut out).unwrap();
        assert_eq!(out, alloc::vec![9, 9]);
        assert_eq!(guest.pending_events(), 0);

        // A second poll with nothing queued is a no-op that leaves `out` intact.
        guest.poll_events(&mut out).unwrap();
        assert_eq!(out, alloc::vec![9, 9]);
    }

    #[test]
    fn drain_ops_returns_all_batches_at_once() {
        let (mut guest, mut host) = channel();
        guest.send(&[1]).unwrap();
        guest.send(&[2]).unwrap();
        guest.send(&[3]).unwrap();

        let drained = host.drain_ops();
        assert_eq!(
            drained,
            alloc::vec![alloc::vec![1], alloc::vec![2], alloc::vec![3]]
        );
        assert_eq!(host.pending_ops(), 0);
    }

    #[test]
    fn poll_events_concatenates_multiple_event_batches() {
        let (mut guest, mut host) = channel();
        host.push_event(&[1, 2]);
        host.push_event(&[3, 4]);
        let mut out = Vec::new();
        guest.poll_events(&mut out).unwrap();
        assert_eq!(out, alloc::vec![1, 2, 3, 4]);
    }
}
