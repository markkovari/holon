pub fn dollars(cents: i32) -> String {
    let dollars = cents / 100;
    let remaining_cents = (cents % 100).abs();
    format!("${}.{:02}", dollars, remaining_cents)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dollars() {
        assert_eq!(dollars(0), "$0.00");
        assert_eq!(dollars(5), "$0.05");
        assert_eq!(dollars(1234), "$12.34");
    }
}