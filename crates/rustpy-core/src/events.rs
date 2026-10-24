//! A typed publish/subscribe event bus (roadmap §0.1).
//!
//! Any `Clone + Send + 'static` type is an event type. Subscribers get a
//! channel receiver for one event type and drain it whenever they like (the
//! UI once per frame, a worker in its loop); publishing never blocks. A
//! subscription ends when its receiver is dropped.

use std::any::{Any, TypeId};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use parking_lot::Mutex;
use rustc_hash::FxHashMap;

/// The senders of each event type (`Sender<E>` boxed as `Any`).
type Subscribers = FxHashMap<TypeId, Vec<Box<dyn Any + Send>>>;

/// Typed publish/subscribe hub; clones share the subscriber lists.
#[derive(Clone, Default)]
pub struct EventBus {
    subscribers: Arc<Mutex<Subscribers>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribes to events of type `E`: every later [`EventBus::publish`]
    /// of an `E` is delivered to the returned receiver.
    pub fn subscribe<E: Clone + Send + 'static>(&self) -> Receiver<E> {
        let (tx, rx) = crossbeam_channel::unbounded::<E>();
        self.subscribers
            .lock()
            .entry(TypeId::of::<E>())
            .or_default()
            .push(Box::new(tx));
        rx
    }

    /// Delivers `event` to every live subscriber of `E`; returns how many got
    /// it. Subscribers whose receiver was dropped are removed.
    pub fn publish<E: Clone + Send + 'static>(&self, event: E) -> usize {
        let mut subscribers = self.subscribers.lock();
        let Some(list) = subscribers.get_mut(&TypeId::of::<E>()) else {
            return 0;
        };
        let mut delivered = 0;
        list.retain(|s| match s.downcast_ref::<Sender<E>>() {
            Some(tx) if tx.send(event.clone()).is_ok() => {
                delivered += 1;
                true
            }
            _ => false,
        });
        if list.is_empty() {
            subscribers.remove(&TypeId::of::<E>());
        }
        delivered
    }

    /// Number of subscribers of `E` (including ones dropped since the last publish).
    pub fn subscriber_count<E: 'static>(&self) -> usize {
        self.subscribers
            .lock()
            .get(&TypeId::of::<E>())
            .map_or(0, Vec::len)
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("event_types", &self.subscribers.lock().len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct Saved(&'static str);

    #[derive(Clone, Debug, PartialEq)]
    struct Closed(u32);

    #[test]
    fn events_reach_subscribers_of_their_type_only() {
        let bus = EventBus::new();
        let saved_a = bus.subscribe::<Saved>();
        let saved_b = bus.subscribe::<Saved>();
        let closed = bus.subscribe::<Closed>();
        assert_eq!(bus.publish(Saved("a.py")), 2);
        assert_eq!(bus.publish(Closed(7)), 1);
        assert_eq!(saved_a.try_iter().collect::<Vec<_>>(), vec![Saved("a.py")]);
        assert_eq!(saved_b.try_iter().collect::<Vec<_>>(), vec![Saved("a.py")]);
        assert_eq!(closed.try_iter().collect::<Vec<_>>(), vec![Closed(7)]);
    }

    #[test]
    fn publishing_without_subscribers_is_a_no_op() {
        let bus = EventBus::new();
        assert_eq!(bus.publish(Saved("x")), 0);
        let late = bus.subscribe::<Saved>();
        assert!(
            late.try_recv().is_err(),
            "events are not replayed to late subscribers"
        );
    }

    #[test]
    fn dropped_subscribers_are_pruned() {
        let bus = EventBus::new();
        let keep = bus.subscribe::<Closed>();
        let gone = bus.subscribe::<Closed>();
        drop(gone);
        assert_eq!(bus.subscriber_count::<Closed>(), 2);
        assert_eq!(bus.publish(Closed(1)), 1);
        assert_eq!(bus.subscriber_count::<Closed>(), 1);
        drop(keep);
        assert_eq!(bus.publish(Closed(2)), 0);
        assert_eq!(bus.subscriber_count::<Closed>(), 0);
    }

    #[test]
    fn clones_share_subscribers_and_cross_threads() {
        let bus = EventBus::new();
        let rx = bus.subscribe::<Closed>();
        let publisher = bus.clone();
        std::thread::spawn(move || {
            for i in 0..100 {
                publisher.publish(Closed(i));
            }
        })
        .join()
        .unwrap();
        let got: Vec<u32> = rx.try_iter().map(|c| c.0).collect();
        assert_eq!(got, (0..100).collect::<Vec<_>>(), "in publish order");
    }
}
