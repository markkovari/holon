#[allow(warnings)]
mod bindings;
use bindings::exports::demo::ordinal::suffix::Guest;
struct Component;

/// A number with its English ordinal suffix: 1->"1st", 2->"2nd", 3->"3rd",
/// 4->"4th", and the 11/12/13 exception ("11th","12th","13th"), so 111->"111th"
/// but 21->"21st". UNIMPLEMENTED — the tests are the exact spec.
pub fn ordinal(n: u32) -> String {
    let suffix = if (11..=13).contains(&(n % 100)) {
        "th"
    } else {
        match n % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

impl Guest for Component {
    fn ordinal(n: u32) -> String { ordinal(n) }
}
bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::ordinal;
    #[test] fn ones() {
        assert_eq!(ordinal(1), "1st"); assert_eq!(ordinal(2), "2nd");
        assert_eq!(ordinal(3), "3rd"); assert_eq!(ordinal(4), "4th");
    }
    #[test] fn the_teens_are_all_th() {
        assert_eq!(ordinal(11), "11th"); assert_eq!(ordinal(12), "12th");
        assert_eq!(ordinal(13), "13th");
    }
    #[test] fn tens_and_hundreds() {
        assert_eq!(ordinal(21), "21st"); assert_eq!(ordinal(22), "22nd");
        assert_eq!(ordinal(23), "23rd"); assert_eq!(ordinal(101), "101st");
        assert_eq!(ordinal(111), "111th"); assert_eq!(ordinal(113), "113th");
    }
}
