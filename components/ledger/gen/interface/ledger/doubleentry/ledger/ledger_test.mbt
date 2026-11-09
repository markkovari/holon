test "validate balanced entry" {
  let entry : @ledger.Entry = {
    id: "tx-1",
    memo: "Initial deposit",
    lines: [
      { account: "bank", amount: 10000L, side: DEBIT },
      { account: "equity", amount: 10000L, side: CREDIT },
    ],
  }
  match @ledger.validate(entry) {
    Ok(_) => ()
    Err(_) => fail("expected validate to succeed")
  }
}

test "validate errors" {
  let too_few : @ledger.Entry = {
    id: "tx-bad-1",
    memo: "Single line",
    lines: [
      { account: "bank", amount: 1000L, side: DEBIT },
    ],
  }
  match @ledger.validate(too_few) {
    Err(TooFewLines) => ()
    _ => fail("expected TooFewLines")
  }

  let nonpos : @ledger.Entry = {
    id: "tx-bad-2",
    memo: "Zero amount",
    lines: [
      { account: "bank", amount: 0L, side: DEBIT },
      { account: "equity", amount: 0L, side: CREDIT },
    ],
  }
  match @ledger.validate(nonpos) {
    Err(Nonpositive(acc)) => assert_eq(acc, "bank")
    _ => fail("expected Nonpositive(bank)")
  }

  let unbal : @ledger.Entry = {
    id: "tx-bad-3",
    memo: "Unbalanced",
    lines: [
      { account: "bank", amount: 1000L, side: DEBIT },
      { account: "equity", amount: 900L, side: CREDIT },
    ],
  }
  match @ledger.validate(unbal) {
    Err(Unbalanced((deb, cred))) => {
      assert_eq(deb, 1000L)
      assert_eq(cred, 900L)
    }
    _ => fail("expected Unbalanced((1000, 900))")
  }
}

test "trial_balance aggregation and sorting" {
  let e1 : @ledger.Entry = {
    id: "tx-1",
    memo: "Capital",
    lines: [
      { account: "zebra", amount: 5000L, side: DEBIT },
      { account: "apple", amount: 5000L, side: CREDIT },
    ],
  }
  let e2 : @ledger.Entry = {
    id: "tx-2",
    memo: "Purchase",
    lines: [
      { account: "mango", amount: 2000L, side: DEBIT },
      { account: "zebra", amount: 2000L, side: CREDIT },
    ],
  }
  match @ledger.trial_balance([e1, e2]) {
    Ok(trial) => {
      assert_eq(trial.balanced, true)
      assert_eq(trial.total_debits, 7000L)
      assert_eq(trial.total_credits, 7000L)
      assert_eq(trial.accounts.length(), 3)
      // Sorted alphabetically: apple, mango, zebra
      assert_eq(trial.accounts[0].account, "apple")
      assert_eq(trial.accounts[0].debits, 0L)
      assert_eq(trial.accounts[0].credits, 5000L)
      assert_eq(trial.accounts[0].net, -5000L)

      assert_eq(trial.accounts[1].account, "mango")
      assert_eq(trial.accounts[1].debits, 2000L)
      assert_eq(trial.accounts[1].credits, 0L)
      assert_eq(trial.accounts[1].net, 2000L)

      assert_eq(trial.accounts[2].account, "zebra")
      assert_eq(trial.accounts[2].debits, 5000L)
      assert_eq(trial.accounts[2].credits, 2000L)
      assert_eq(trial.accounts[2].net, 3000L)
    }
    Err(_) => fail("trial_balance failed")
  }
}
