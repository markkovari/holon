//! Pure loyalty stamp-card state transitions (tested natively without a host).

use serde::{Deserialize, Serialize};

pub const DEFAULT_STAMPS_REQUIRED: u32 = 8;
pub const DEFAULT_TOKEN_TTL_SECS: i64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StampCard {
    pub id: String,
    pub customer_name: String,
    pub email: String,
    pub stamps: u32,
    pub total_stamps_earned: u32,
    pub free_coffees_earned: u32,
    pub free_coffees_redeemed: u32,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StampToken {
    pub id: String,
    pub code: String,
    pub drink: String,
    pub stamps: u32,
    pub issued_by: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub claimed_by: Option<String>,
    pub claimed_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RewardVoucher {
    pub id: String,
    pub code: String,
    pub card_id: String,
    pub customer_name: String,
    pub issued_at: i64,
    pub redeemed: bool,
    pub redeemed_at: Option<i64>,
    pub redeemed_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    AlreadyClaimed,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimOutcome {
    pub new_stamps: u32,
    pub vouchers_earned: u32,
}

/// Validates a stamp token and applies its stamps to `card`.
/// Returns how many free-coffee vouchers were unlocked (supports multi-stamp drops and rollover).
pub fn apply_claim(
    card: &mut StampCard,
    token: &mut StampToken,
    stamps_required: u32,
    now_secs: i64,
) -> Result<ClaimOutcome, ClaimError> {
    if token.claimed_by.is_some() {
        return Err(ClaimError::AlreadyClaimed);
    }
    if now_secs > token.expires_at {
        return Err(ClaimError::Expired);
    }
    let goal = stamps_required.max(1);
    let added = token.stamps.max(1);
    token.claimed_by = Some(card.id.clone());
    token.claimed_at = Some(now_secs);

    let total = card.stamps.saturating_add(added);
    let vouchers_earned = total / goal;
    let remainder = total % goal;

    card.stamps = remainder;
    card.total_stamps_earned = card.total_stamps_earned.saturating_add(added);
    card.free_coffees_earned = card.free_coffees_earned.saturating_add(vouchers_earned);

    Ok(ClaimOutcome { new_stamps: remainder, vouchers_earned })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemError {
    AlreadyRedeemed,
}

pub fn apply_redeem(
    voucher: &mut RewardVoucher,
    card: &mut StampCard,
    barista: &str,
    now_secs: i64,
) -> Result<(), RedeemError> {
    if voucher.redeemed {
        return Err(RedeemError::AlreadyRedeemed);
    }
    voucher.redeemed = true;
    voucher.redeemed_at = Some(now_secs);
    voucher.redeemed_by = Some(barista.to_string());
    card.free_coffees_redeemed = card.free_coffees_redeemed.saturating_add(1);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_card(stamps: u32) -> StampCard {
        StampCard {
            id: "card_1".into(),
            customer_name: "Alice".into(),
            email: "alice@example.test".into(),
            stamps,
            total_stamps_earned: stamps,
            free_coffees_earned: 0,
            free_coffees_redeemed: 0,
            created_at: 1000,
        }
    }

    fn sample_token(stamps: u32, expires_at: i64) -> StampToken {
        StampToken {
            id: "tok_1".into(),
            code: "STAMP-ABC123".into(),
            drink: "Oat Flat White".into(),
            stamps,
            issued_by: "barista-1".into(),
            issued_at: 1000,
            expires_at,
            claimed_by: None,
            claimed_at: None,
        }
    }

    #[test]
    fn collecting_x_stamps_unlocks_free_coffee_and_rolls_over() {
        let mut card = sample_card(7);
        let mut tok = sample_token(2, 2000);
        let out = apply_claim(&mut card, &mut tok, 8, 1200).unwrap();
        assert_eq!(out, ClaimOutcome { new_stamps: 1, vouchers_earned: 1 });
        assert_eq!(card.stamps, 1);
        assert_eq!(card.free_coffees_earned, 1);
        assert_eq!(card.total_stamps_earned, 9);
        assert_eq!(tok.claimed_by.as_deref(), Some("card_1"));
    }

    #[test]
    fn double_scan_of_same_qr_token_is_rejected() {
        let mut card = sample_card(0);
        let mut tok = sample_token(1, 2000);
        assert!(apply_claim(&mut card, &mut tok, 8, 1100).is_ok());
        assert_eq!(apply_claim(&mut card, &mut tok, 8, 1105), Err(ClaimError::AlreadyClaimed));
        assert_eq!(card.stamps, 1);
    }

    #[test]
    fn expired_qr_token_is_rejected() {
        let mut card = sample_card(3);
        let mut tok = sample_token(1, 1500);
        assert_eq!(apply_claim(&mut card, &mut tok, 8, 1501), Err(ClaimError::Expired));
        assert_eq!(card.stamps, 3);
    }

    #[test]
    fn voucher_redemption_is_single_use() {
        let mut card = sample_card(0);
        card.free_coffees_earned = 1;
        let mut v = RewardVoucher {
            id: "vch_1".into(),
            code: "FREE-XYZ".into(),
            card_id: "card_1".into(),
            customer_name: "Alice".into(),
            issued_at: 1200,
            redeemed: false,
            redeemed_at: None,
            redeemed_by: None,
        };
        assert!(apply_redeem(&mut v, &mut card, "barista-1", 1300).is_ok());
        assert_eq!(card.free_coffees_redeemed, 1);
        assert_eq!(
            apply_redeem(&mut v, &mut card, "barista-1", 1305),
            Err(RedeemError::AlreadyRedeemed)
        );
    }
}
