/// A run's spending cap and the spend against it.
#[derive(Debug, Clone)]
pub struct Wallet {
    cap: u64,
    spent: u64,
}

impl Wallet {
    /// Create a new wallet with the given cap in cents.
    /// A cap of 0 means unbounded.
    pub fn new(cap: u64) -> Self {
        Wallet { cap, spent: 0 }
    }

    /// Get the cap in cents. 0 means unbounded.
    pub fn cap(&self) -> u64 {
        self.cap
    }

    /// Get the amount spent so far in cents.
    pub fn spent(&self) -> u64 {
        self.spent
    }

    /// Charge a completion with the given cost_cents value.
    /// Accumulates the cost into the spent amount.
    pub fn charge(&mut self, cost_cents_value: u64) {
        self.spent += cost_cents_value;
    }

    /// Get the remaining budget in cents.
    /// Returns None if the cap is unbounded (0).
    /// Never returns a negative value; returns 0 if over budget.
    pub fn remaining(&self) -> Option<u64> {
        if self.cap == 0 {
            None
        } else if self.spent >= self.cap {
            Some(0)
        } else {
            Some(self.cap - self.spent)
        }
    }

    /// Check if spending is strictly over the cap.
    /// Returns false if the cap is unbounded (0).
    /// Equal to cap is not over.
    pub fn over(&self) -> bool {
        self.cap != 0 && self.spent > self.cap
    }

    /// Check if we can afford to spend the given amount in cents.
    /// Returns true if the cap is unbounded (0).
    /// Returns true if spent + cents does not exceed the cap.
    pub fn can_afford(&self, cents: u64) -> bool {
        if self.cap == 0 {
            true
        } else {
            self.spent + cents <= self.cap
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_wallet() {
        let wallet = Wallet::new(1000);
        assert_eq!(wallet.cap(), 1000);
        assert_eq!(wallet.spent(), 0);
    }

    #[test]
    fn test_unbounded_wallet() {
        let wallet = Wallet::new(0);
        assert_eq!(wallet.cap(), 0);
        assert_eq!(wallet.remaining(), None);
        assert!(!wallet.over());
        assert!(wallet.can_afford(1_000_000));
    }

    #[test]
    fn test_charge() {
        let mut wallet = Wallet::new(1000);
        wallet.charge(300);
        assert_eq!(wallet.spent(), 300);
        wallet.charge(200);
        assert_eq!(wallet.spent(), 500);
    }

    #[test]
    fn test_remaining() {
        let mut wallet = Wallet::new(1000);
        assert_eq!(wallet.remaining(), Some(1000));
        wallet.charge(300);
        assert_eq!(wallet.remaining(), Some(700));
        wallet.charge(700);
        assert_eq!(wallet.remaining(), Some(0));
    }

    #[test]
    fn test_over() {
        let mut wallet = Wallet::new(1000);
        assert!(!wallet.over());
        wallet.charge(1000);
        assert!(!wallet.over()); // equal is not over
        wallet.charge(1);
        assert!(wallet.over());
    }

    #[test]
    fn test_can_afford() {
        let mut wallet = Wallet::new(1000);
        assert!(wallet.can_afford(500));
        assert!(wallet.can_afford(1000));
        assert!(!wallet.can_afford(1001));
        wallet.charge(600);
        assert!(wallet.can_afford(400));
        assert!(!wallet.can_afford(401));
    }

    #[test]
    fn test_unbounded_can_afford() {
        let wallet = Wallet::new(0);
        assert!(wallet.can_afford(0));
        assert!(wallet.can_afford(1_000_000_000));
    }

    #[test]
    fn test_unbounded_over() {
        let mut wallet = Wallet::new(0);
        wallet.charge(1_000_000_000);
        assert!(!wallet.over());
    }
}