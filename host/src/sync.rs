//! Locks that survive a panic somewhere else.
//!
//! `std`'s `Mutex` poisons itself when a thread panics while holding it, and every
//! later `lock().unwrap()` panics too. In a host that serves requests, that turns
//! ONE transient panic into a permanently dead node: the KV store, the route table
//! and the instance map are all behind locks on the request path, so the second
//! request after the first panic fails, and so does every request after that. The
//! process stays up, answering nothing, which is the worst of the available
//! failure modes.
//!
//! Poisoning is a conservative signal, not a corruption detector. In safe Rust a
//! panic cannot leave a `HashMap` half-updated — there is no exception unwinding
//! through a partially-written node — so the guarded data is intact, and refusing
//! to look at it buys nothing. The panic that poisoned the lock is the bug worth
//! fixing; the poisoning is just how it spreads.
//!
//! So: take the guard anyway, and let the original panic be the thing that gets
//! reported.
//!
//! `parking_lot` would remove poisoning entirely and is the usual answer to this,
//! but it is a dependency added to change three lines of behaviour, and this host
//! deliberately runs on very little. ponytail: revisit if lock contention ever
//! shows up in a profile, since that is the argument for parking_lot that is not
//! about poisoning.

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
