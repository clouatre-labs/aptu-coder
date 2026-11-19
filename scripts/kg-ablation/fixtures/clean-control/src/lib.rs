//! Clean control pack: no defect. Reviewers should report no defects.

// Simple ledger fixture; no defects expected.
pub struct Ledger {
    pub balance: i64,
}

impl Ledger {
    pub fn deposit(&mut self, amount: i64) {
        self.balance += amount;
    }

    pub fn withdraw(&mut self, amount: i64) -> bool {
        if self.balance >= amount {
            self.balance -= amount;
            true
        } else {
            false
        }
    }
}

pub fn total(ledgers: &[Ledger]) -> i64 {
    ledgers.iter().map(|l| l.balance).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deposit_and_withdraw_round_trip() {
        let mut l = Ledger { balance: 10 };
        l.deposit(5);
        assert!(l.withdraw(15));
        assert_eq!(l.balance, 0);
        assert!(!l.withdraw(1));
    }

    #[test]
    fn total_sums_balances() {
        let ls = [Ledger { balance: 3 }, Ledger { balance: 4 }];
        assert_eq!(total(&ls), 7);
    }
}
