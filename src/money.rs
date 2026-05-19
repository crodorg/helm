//! Stripe + Mercury balance overlay.
//!
//! Both checks shell out to the operator's existing CLIs (`stripe-pp-cli`
//! and `mercury-pp-cli`) — no SDK is pulled into `Cargo.toml`. Each CLI
//! already handles its own auth (env var or `auth set-token`) and emits
//! JSON when passed `--json --no-input --no-color`.
//!
//! Auth keys are env-only on the operator's side (`STRIPE_SECRET_KEY` /
//! `MERCURY_BEARER_AUTH`); helm never reads them. If a CLI is missing or
//! its auth is unset, the corresponding panel renders an error string
//! rather than blocking startup.

use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::thread;

use serde::Deserialize;

/// Aggregate Stripe balance (default account, per-currency rolled into
/// the first slot). Values are in the currency's smallest unit (cents for
/// USD).
#[derive(Debug, Clone, PartialEq)]
pub struct StripeSnapshot {
    pub available_cents: i64,
    pub pending_cents: i64,
    pub currency: String,
}

/// One Mercury account. Balances are in the account's currency (always
/// USD in practice for Mercury).
#[derive(Debug, Clone, PartialEq)]
pub struct MercuryAccount {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub current_balance: f64,
    pub available_balance: f64,
    pub currency: String,
}

/// Joined snapshot consumed by the UI.
#[derive(Debug, Clone, Default)]
pub struct MoneyCache {
    pub stripe: Option<StripeSnapshot>,
    pub stripe_error: Option<String>,
    pub mercury: Vec<MercuryAccount>,
    pub mercury_error: Option<String>,
}

impl MoneyCache {
    pub fn mercury_total(&self, field: BalanceField) -> Option<f64> {
        if self.mercury.is_empty() {
            return None;
        }
        let total: f64 = self
            .mercury
            .iter()
            .map(|a| match field {
                BalanceField::Current => a.current_balance,
                BalanceField::Available => a.available_balance,
            })
            .sum();
        Some(total)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum BalanceField {
    Current,
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoneySlot {
    Stripe,
    Mercury,
}

#[derive(Debug)]
pub struct MoneyResult {
    pub slot: MoneySlot,
    pub output: Result<String, String>,
}

/// Fire two parallel CLI calls. Each thread sends a single `MoneyResult`.
/// Caller drains via `try_recv` from the main loop.
pub fn spawn_money_fetch() -> Receiver<MoneyResult> {
    let (tx, rx) = channel();

    // Stripe: balance is the default account's available/pending split.
    let tx_s = tx.clone();
    thread::spawn(move || {
        let result = shell_json("stripe-pp-cli", &["balance"]);
        let _ = tx_s.send(MoneyResult {
            slot: MoneySlot::Stripe,
            output: result,
        });
    });

    // Mercury: list every account on the configured organization.
    let tx_m = tx;
    thread::spawn(move || {
        let result = shell_json("mercury-pp-cli", &["accounts"]);
        let _ = tx_m.send(MoneyResult {
            slot: MoneySlot::Mercury,
            output: result,
        });
    });

    rx
}

/// Run a pp-cli with JSON output. `--no-input --no-color` keeps the CLI
/// non-interactive without applying `--compact` (which strips the very
/// fields helm needs). 30s timeout is enforced by the CLI itself; this
/// wrapper just waits for `Command::output`.
fn shell_json(bin: &str, sub_args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(sub_args).args(["--json", "--no-input", "--no-color"]);
    match cmd.output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            Err(format!(
                "{bin} exit {}: {}",
                o.status.code().unwrap_or(-1),
                shorten_err(&stderr)
            ))
        }
        Err(e) => Err(format!("{bin} spawn failed: {e}")),
    }
}

/// CLI errors include multi-line auth hints; keep the leading "Error:" or
/// first non-blank line so the UI cell stays single-line.
fn shorten_err(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Parse `stripe-pp-cli balance` JSON. Stripe returns:
///
/// ```json
/// {
///   "object": "balance",
///   "available": [{"amount": 12345, "currency": "usd", ...}],
///   "pending":   [{"amount": 678,   "currency": "usd", ...}]
/// }
/// ```
///
/// Helm sums per-bucket amounts (multi-currency accounts get the rolled-up
/// total in the first listed currency, which is the typical USD-only case).
pub fn parse_stripe_balance(json: &str) -> Result<StripeSnapshot, String> {
    #[derive(Deserialize)]
    struct Amount {
        amount: i64,
        currency: String,
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(default)]
        available: Vec<Amount>,
        #[serde(default)]
        pending: Vec<Amount>,
    }
    let resp: Resp = serde_json::from_str(json).map_err(|e| format!("stripe balance json: {e}"))?;
    let currency = resp
        .available
        .first()
        .map(|a| a.currency.clone())
        .or_else(|| resp.pending.first().map(|a| a.currency.clone()))
        .unwrap_or_else(|| "usd".into());
    Ok(StripeSnapshot {
        available_cents: resp.available.iter().map(|a| a.amount).sum(),
        pending_cents: resp.pending.iter().map(|a| a.amount).sum(),
        currency,
    })
}

/// Parse `mercury-pp-cli accounts` JSON. Mercury returns:
///
/// ```json
/// { "accounts": [
///     { "id": "...", "name": "Operating", "kind": "checking",
///       "currentBalance": 12345.67, "availableBalance": 12000.00,
///       "currency": "USD", ... }
/// ]}
/// ```
pub fn parse_mercury_accounts(json: &str) -> Result<Vec<MercuryAccount>, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Acc {
        #[serde(default)]
        id: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        current_balance: f64,
        #[serde(default)]
        available_balance: f64,
        #[serde(default = "default_currency")]
        currency: String,
    }
    #[derive(Deserialize)]
    struct Resp {
        accounts: Vec<Acc>,
    }
    fn default_currency() -> String {
        "USD".into()
    }
    let resp: Resp =
        serde_json::from_str(json).map_err(|e| format!("mercury accounts json: {e}"))?;
    Ok(resp
        .accounts
        .into_iter()
        .map(|a| MercuryAccount {
            id: a.id,
            name: a.name,
            kind: a.kind,
            current_balance: a.current_balance,
            available_balance: a.available_balance,
            currency: a.currency,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRIPE_BALANCE_FIXTURE: &str = r#"{
      "object": "balance",
      "available": [
        { "amount": 12345, "currency": "usd", "source_types": {"card": 12345} }
      ],
      "pending": [
        { "amount": 678, "currency": "usd", "source_types": {"card": 678} }
      ],
      "livemode": false
    }"#;

    const STRIPE_BALANCE_MULTI_CURRENCY: &str = r#"{
      "object": "balance",
      "available": [
        { "amount": 5000, "currency": "usd" },
        { "amount": 3000, "currency": "eur" }
      ],
      "pending": []
    }"#;

    const STRIPE_BALANCE_EMPTY: &str = r#"{
      "object": "balance",
      "available": [],
      "pending": []
    }"#;

    const MERCURY_ACCOUNTS_FIXTURE: &str = r#"{
      "accounts": [
        {
          "id": "acc-1",
          "name": "Operating",
          "kind": "checking",
          "currentBalance": 12345.67,
          "availableBalance": 12000.00,
          "currency": "USD",
          "status": "active"
        },
        {
          "id": "acc-2",
          "name": "Tax Savings",
          "kind": "savings",
          "currentBalance": 50000.00,
          "availableBalance": 50000.00,
          "currency": "USD",
          "status": "active"
        }
      ],
      "total": 2
    }"#;

    #[test]
    fn parses_stripe_balance_typical() {
        let s = parse_stripe_balance(STRIPE_BALANCE_FIXTURE).expect("parses");
        assert_eq!(s.available_cents, 12345);
        assert_eq!(s.pending_cents, 678);
        assert_eq!(s.currency, "usd");
    }

    #[test]
    fn parses_stripe_balance_sums_multi_currency() {
        let s = parse_stripe_balance(STRIPE_BALANCE_MULTI_CURRENCY).expect("parses");
        // available rolled up across currencies; UI surfaces a single
        // figure tagged with the first listed currency.
        assert_eq!(s.available_cents, 8000);
        assert_eq!(s.pending_cents, 0);
        assert_eq!(s.currency, "usd");
    }

    #[test]
    fn parses_stripe_balance_empty_defaults_to_usd() {
        let s = parse_stripe_balance(STRIPE_BALANCE_EMPTY).expect("parses");
        assert_eq!(s.available_cents, 0);
        assert_eq!(s.pending_cents, 0);
        assert_eq!(s.currency, "usd");
    }

    #[test]
    fn rejects_malformed_stripe_balance() {
        assert!(parse_stripe_balance("not json").is_err());
        assert!(parse_stripe_balance(r#"{"available": "oops"}"#).is_err());
    }

    #[test]
    fn parses_mercury_accounts_fixture() {
        let v = parse_mercury_accounts(MERCURY_ACCOUNTS_FIXTURE).expect("parses");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].id, "acc-1");
        assert_eq!(v[0].name, "Operating");
        assert_eq!(v[0].kind, "checking");
        assert_eq!(v[0].current_balance, 12345.67);
        assert_eq!(v[0].available_balance, 12000.00);
        assert_eq!(v[0].currency, "USD");
        assert_eq!(v[1].name, "Tax Savings");
    }

    #[test]
    fn parses_mercury_accounts_with_missing_currency() {
        let json = r#"{"accounts":[{"id":"a","name":"x","currentBalance":1.0,"availableBalance":1.0}]}"#;
        let v = parse_mercury_accounts(json).expect("parses");
        assert_eq!(v[0].currency, "USD");
    }

    #[test]
    fn rejects_malformed_mercury_accounts() {
        assert!(parse_mercury_accounts("not json").is_err());
        assert!(parse_mercury_accounts(r#"{"accounts": "oops"}"#).is_err());
    }

    #[test]
    fn mercury_totals_sum_per_field() {
        let cache = MoneyCache {
            mercury: parse_mercury_accounts(MERCURY_ACCOUNTS_FIXTURE).unwrap(),
            ..Default::default()
        };
        assert_eq!(cache.mercury_total(BalanceField::Current), Some(62345.67));
        assert_eq!(cache.mercury_total(BalanceField::Available), Some(62000.00));
    }

    #[test]
    fn mercury_totals_none_when_empty() {
        let cache = MoneyCache::default();
        assert_eq!(cache.mercury_total(BalanceField::Current), None);
    }

    #[test]
    fn shorten_err_returns_first_non_blank() {
        assert_eq!(shorten_err(""), "");
        assert_eq!(shorten_err("\n\n  Error: nope  \nhint: ..."), "Error: nope");
    }
}
