#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Haiku,
    Sonnet,
    Opus,
}

impl Tier {
    pub fn model(self) -> &'static str {
        match self {
            Tier::Haiku => "claude-3-haiku-20240307",
            Tier::Sonnet => "claude-3-5-sonnet-20241022",
            Tier::Opus => "claude-3-opus-20240229",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Tier::Haiku => 0,
            Tier::Sonnet => 1,
            Tier::Opus => 2,
        }
    }

    pub fn up(self) -> Tier {
        match self {
            Tier::Haiku => Tier::Sonnet,
            Tier::Sonnet => Tier::Opus,
            Tier::Opus => Tier::Opus,
        }
    }

    pub fn of(id: &str) -> Tier {
        if id.contains("haiku") {
            Tier::Haiku
        } else if id.contains("sonnet") {
            Tier::Sonnet
        } else {
            Tier::Opus
        }
    }
}

pub fn tier_for(attempt: u32, ceiling: Tier) -> Tier {
    let mut tier = Tier::Haiku;
    for _ in 0..attempt {
        tier = tier.up();
    }
    if tier.rank() > ceiling.rank() {
        ceiling
    } else {
        tier
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering() {
        assert!(Tier::Haiku < Tier::Sonnet);
        assert!(Tier::Sonnet < Tier::Opus);
        assert!(Tier::Haiku < Tier::Opus);
    }

    #[test]
    fn rank_values() {
        assert_eq!(Tier::Haiku.rank(), 0);
        assert_eq!(Tier::Sonnet.rank(), 1);
        assert_eq!(Tier::Opus.rank(), 2);
    }

    #[test]
    fn up_escalates() {
        assert_eq!(Tier::Haiku.up(), Tier::Sonnet);
        assert_eq!(Tier::Sonnet.up(), Tier::Opus);
        assert_eq!(Tier::Opus.up(), Tier::Opus);
    }

    #[test]
    fn of_maps_ids() {
        assert_eq!(Tier::of("claude-3-haiku-20240307"), Tier::Haiku);
        assert_eq!(Tier::of("claude-3-5-sonnet-20241022"), Tier::Sonnet);
        assert_eq!(Tier::of("claude-3-opus-20240229"), Tier::Opus);
        assert_eq!(Tier::of("some-unknown-model"), Tier::Opus);
    }

    #[test]
    fn tier_for_escalates_with_ceiling_opus() {
        assert_eq!(tier_for(0, Tier::Opus), Tier::Haiku);
        assert_eq!(tier_for(1, Tier::Opus), Tier::Sonnet);
        assert_eq!(tier_for(2, Tier::Opus), Tier::Opus);
        assert_eq!(tier_for(3, Tier::Opus), Tier::Opus);
    }

    #[test]
    fn tier_for_capped_by_ceiling() {
        assert_eq!(tier_for(0, Tier::Haiku), Tier::Haiku);
        assert_eq!(tier_for(1, Tier::Haiku), Tier::Haiku);
        assert_eq!(tier_for(2, Tier::Haiku), Tier::Haiku);

        assert_eq!(tier_for(0, Tier::Sonnet), Tier::Haiku);
        assert_eq!(tier_for(1, Tier::Sonnet), Tier::Sonnet);
        assert_eq!(tier_for(2, Tier::Sonnet), Tier::Sonnet);
    }
}
