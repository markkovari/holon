//! Locks that survive a panic somewhere else.
//!
//! `std`'s `Mutex` poisons itself when a thread panics while holding it, so one
//! transient panic would permanently kill a host whose KV store, route table and
//! instance map are all behind locks on the request path. In safe Rust a panic
//! cannot leave a `HashMap` half-updated, so the guarded data is intact: take the
//! guard anyway and let the original panic be what gets reported.
//!
//! ponytail: `parking_lot` would remove poisoning but is a dependency for three
//! lines of behaviour; revisit if lock contention ever shows up in a profile.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

/// Lock, recovering from a poisoning caused by somebody else's panic.
pub fn held<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Read, recovering the same way.
pub fn reading<T>(l: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    l.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write, recovering the same way.
pub fn writing<T>(l: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    l.write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn a_lock_poisoned_by_another_thread_is_still_usable() {
        let m = Arc::new(Mutex::new(vec![1, 2, 3]));
        let poisoner = Arc::clone(&m);
        // Panic while holding it, which is what poisons it.
        let _ = std::thread::spawn(move || {
            let _g = poisoner.lock().unwrap();
            panic!("something else went wrong");
        })
        .join();

        assert!(m.lock().is_err(), "the lock really is poisoned");
        assert_eq!(*held(&m), vec![1, 2, 3], "and the data behind it is intact");
    }

    #[test]
    fn an_rwlock_recovers_too() {
        let l = Arc::new(RwLock::new(7));
        let poisoner = Arc::clone(&l);
        let _ = std::thread::spawn(move || {
            let _g = poisoner.write().unwrap();
            panic!("boom");
        })
        .join();
        assert_eq!(*reading(&l), 7);
        *writing(&l) = 8;
        assert_eq!(*reading(&l), 8);
    }
}
