use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Queue {
    pub ids: Vec<i64>,
    pub current: Option<usize>,
    pub shuffle: bool,
    pub repeat: Repeat,
    history: Vec<usize>,
    remaining: Vec<usize>,
}

impl Queue {
    pub fn current_id(&self) -> Option<i64> {
        self.current.and_then(|i| self.ids.get(i).copied())
    }

    pub fn start(&mut self, ids: Vec<i64>, index: usize) -> Option<i64> {
        self.ids = ids;
        self.current = (index < self.ids.len()).then_some(index);
        self.history.clear();
        self.reset_shuffle();
        self.current_id()
    }

    pub fn select(&mut self, index: usize) -> Option<i64> {
        if index >= self.ids.len() {
            return None;
        }
        if let Some(current) = self.current {
            self.remember(current);
        }
        self.current = Some(index);
        self.remaining.retain(|i| *i != index);
        self.current_id()
    }

    pub fn enqueue(&mut self, id: i64) {
        self.remaining.push(self.ids.len());
        self.ids.push(id);
        self.remaining.shuffle(&mut rand::rng());
    }

    pub fn set_shuffle(&mut self, value: bool) {
        self.shuffle = value;
        self.history.clear();
        self.reset_shuffle();
    }

    fn remember(&mut self, index: usize) {
        if self.history.len() >= 1000 {
            self.history.remove(0);
        }
        self.history.push(index);
    }

    fn reset_shuffle(&mut self) {
        self.remaining = (0..self.ids.len())
            .filter(|i| Some(*i) != self.current)
            .collect();
        self.remaining.shuffle(&mut rand::rng());
    }

    pub fn advance(&mut self, automatic: bool) -> Option<i64> {
        if self.ids.is_empty() {
            return None;
        }
        if automatic && self.repeat == Repeat::One {
            return self.current_id();
        }
        let next = if self.current.is_none() {
            0
        } else if self.shuffle {
            if self.remaining.is_empty() {
                if self.repeat == Repeat::Off {
                    return None;
                }
                self.reset_shuffle();
            }
            self.remaining.pop().or(self.current)?
        } else {
            let next = self.current.unwrap() + 1;
            if next < self.ids.len() {
                next
            } else if self.repeat != Repeat::Off {
                0
            } else {
                return None;
            }
        };
        if let Some(current) = self.current {
            self.remember(current);
        }
        self.current = Some(next);
        self.current_id()
    }

    pub fn previous(&mut self) -> Option<i64> {
        let next = if self.shuffle {
            self.history.pop()?
        } else {
            self.current?.checked_sub(1)?
        };
        self.current = Some(next);
        self.current_id()
    }

    pub fn retain(&mut self, mut exists: impl FnMut(i64) -> bool) {
        let current = self.current_id();
        let old_len = self.ids.len();
        self.ids.retain(|id| exists(*id));
        if self.ids.len() == old_len {
            return;
        }
        self.current = current.and_then(|id| self.ids.iter().position(|v| *v == id));
        self.history.clear();
        self.reset_shuffle();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn shuffle_visits_each_track_once_then_stops() {
        let mut q = Queue::default();
        q.set_shuffle(true);
        q.start((0..30).collect(), 7);
        let mut seen = HashSet::from([7]);
        while let Some(id) = q.advance(true) {
            assert!(seen.insert(id));
        }
        assert_eq!(seen.len(), 30);
    }

    #[test]
    fn repeat_one_applies_to_eof_but_next_still_advances() {
        let mut q = Queue::default();
        q.start(vec![10, 20], 0);
        q.repeat = Repeat::One;
        assert_eq!(q.advance(true), Some(10));
        assert_eq!(q.advance(false), Some(20));
        assert_eq!(q.advance(false), Some(10));
    }

    #[test]
    fn previous_follows_shuffle_history_and_removal_keeps_current_identity() {
        let mut q = Queue::default();
        q.set_shuffle(true);
        q.start(vec![1, 2, 3, 4], 2);
        let next = q.advance(false).unwrap();
        assert_ne!(next, 3);
        assert_eq!(q.previous(), Some(3));
        q.retain(|id| id != 1);
        assert_eq!(q.current_id(), Some(3));
        q.retain(|_| false);
        assert_eq!(q.advance(false), None);
    }
}
