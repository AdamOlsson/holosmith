//! `abort_grace`: the one duration in the config file.
//!
//! The spec writes it as "10 s", not as `abort_grace_s`, so the file spells the unit out
//! (`10s`, `1500ms`, `2m`). A bare number is refused: its unit would be a guess.

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

const MILLIS_PER_SECOND: u64 = 1000;
const MILLIS_PER_MINUTE: u64 = 60 * MILLIS_PER_SECOND;

/// A span of time from the config file, held as whole milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Duration {
    millis: u64,
}

impl Duration {
    pub fn from_millis(millis: u64) -> Self {
        Self { millis }
    }

    /// The value as whole milliseconds. Private: the only caller is the `From` impl below, which
    /// is what a caller that waits actually uses.
    fn millis(self) -> u64 {
        self.millis
    }
}

/// The runtime's own span, so a caller that waits (`abort_grace`'s ladder) does not have to know
/// the config's representation.
impl From<Duration> for std::time::Duration {
    fn from(config: Duration) -> Self {
        Self::from_millis(config.millis())
    }
}

impl FromStr for Duration {
    type Err = String;

    /// `1500ms`, `10s` or `2m`: a whole number, a unit suffix, and more than zero.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let (number, unit) = text.split_at(digits);
        if number.is_empty() {
            return Err(format!(
                "{text:?} must start with a whole number and end with a unit, like \"10s\""
            ));
        }
        let scale = match unit {
            "ms" => 1,
            "s" => MILLIS_PER_SECOND,
            "m" => MILLIS_PER_MINUTE,
            "" => {
                return Err(format!(
                    "{text:?} has no unit: write \"10s\", \"1500ms\" or \"2m\", because a bare \
                     number does not say which unit it means"
                ));
            }
            other => {
                return Err(format!(
                    "{other:?} is not a duration unit: use ms, s or m, like \"10s\""
                ));
            }
        };
        let value: u64 = number
            .parse()
            .map_err(|_| format!("{number:?} is too large to be a duration"))?;
        let millis = value
            .checked_mul(scale)
            .ok_or_else(|| format!("{text:?} is too large to be a duration"))?;
        if millis == 0 {
            return Err(format!(
                "{text:?} must be greater than zero: the abort ladder needs a grace period"
            ));
        }
        Ok(Self { millis })
    }
}

/// Whole seconds as `<n>s`, anything else as `<n>ms`.
impl fmt::Display for Duration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.millis.is_multiple_of(MILLIS_PER_SECOND) {
            write!(f, "{}s", self.millis / MILLIS_PER_SECOND)
        } else {
            write!(f, "{}ms", self.millis)
        }
    }
}

impl Serialize for Duration {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_str(&text).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Duration, String> {
        Duration::from_str(text)
    }

    #[test]
    fn parses_each_suffix() {
        assert_eq!(
            parse("1500ms").expect("ms parses"),
            Duration::from_millis(1500)
        );
        assert_eq!(
            parse("10s").expect("s parses"),
            Duration::from_millis(10_000)
        );
        assert_eq!(
            parse("2m").expect("m parses"),
            Duration::from_millis(120_000)
        );
    }

    #[test]
    fn formats_whole_seconds_as_seconds_and_the_rest_as_milliseconds() {
        assert_eq!(Duration::from_millis(10_000).to_string(), "10s");
        assert_eq!(Duration::from_millis(60_000).to_string(), "60s");
        assert_eq!(Duration::from_millis(1500).to_string(), "1500ms");
        assert_eq!(Duration::from_millis(999).to_string(), "999ms");
    }

    #[test]
    fn display_round_trips_through_parse() {
        for millis in [1, 7, 999, 1000, 1500, 10_000, 60_000, 90_061] {
            let duration = Duration::from_millis(millis);
            assert_eq!(
                parse(&duration.to_string()).expect("a rendered duration parses back"),
                duration
            );
        }
    }

    #[test]
    fn rejects_a_bare_number_and_says_why() {
        let message = parse("10").expect_err("a bare number is refused");
        assert!(message.contains("has no unit"), "{message}");
        assert!(message.contains("\"10s\""), "{message}");
    }

    #[test]
    fn rejects_zero() {
        let message = parse("0s").expect_err("zero is refused");
        assert!(message.contains("greater than zero"), "{message}");
        let message = parse("0ms").expect_err("zero is refused");
        assert!(message.contains("greater than zero"), "{message}");
    }

    #[test]
    fn rejects_an_unknown_unit() {
        let message = parse("10h").expect_err("hours are not a unit here");
        assert!(message.contains("not a duration unit"), "{message}");
    }

    #[test]
    fn rejects_a_missing_number() {
        let message = parse("s").expect_err("a unit without a number is refused");
        assert!(message.contains("whole number"), "{message}");
    }
}
