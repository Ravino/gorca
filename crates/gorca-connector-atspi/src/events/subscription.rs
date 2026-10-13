//! The subscription a fill strategy adds on top of the shared base set.
//!
//! The base set (`REGISTERED_EVENTS`, [`gorca_dbus::drain::default_match_rules`]) is shared and
//! unchanged. An arm may add one item from a **closed whitelist**: the `org.a11y.atspi.Cache`
//! signals (`AddAccessible`/`RemoveAccessible`), which are delivered only with an explicit
//! interface match rule (B-1 C-S). Every such signal is counted in msgs/s.

use gorca_dbus::drain::{cache_match_rule, default_match_rules};

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Subscription {
    /// Adds `interface='org.a11y.atspi.Cache'` (PW-8 maps the signals to `NodeRemoved` and
    /// `ChildrenChanged`).
    pub cache_signals: bool,
}

impl Subscription {
    pub const NONE: Subscription = Subscription {
        cache_signals: false,
    };

    /// All match rules of the drain: the base set plus this subscription.
    pub fn match_rules(self) -> Vec<String> {
        let mut rules = default_match_rules();
        if self.cache_signals {
            rules.push(cache_match_rule());
        }
        rules
    }
}

/// The arm's extra subscription. Default: none.
pub(crate) fn extra_subscription() -> Subscription {
    Subscription::NONE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_adds_nothing_to_the_base_set() {
        assert_eq!(extra_subscription(), Subscription::NONE);
        assert_eq!(Subscription::NONE.match_rules(), default_match_rules());
    }

    #[test]
    fn cache_signals_add_exactly_one_interface_rule() {
        let rules = Subscription {
            cache_signals: true,
        }
        .match_rules();
        assert_eq!(rules.len(), default_match_rules().len() + 1);
        assert_eq!(
            rules.last().map(String::as_str),
            Some("type='signal',interface='org.a11y.atspi.Cache'")
        );
    }
}
