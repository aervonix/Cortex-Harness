//! A few rules to break: the code `cargo xtask mutate` is tested on.

/// Whether a tier's tests took longer than its budget.
pub fn over(seconds: f64, budget: f64) -> bool {
    seconds > budget
}

/// Whether they took more than one and a half times the budget.
pub fn far_over(seconds: f64, budget: f64) -> bool {
    seconds > 1.5 * budget
}

/// What to print for a tier. Only the label of a tier over its budget is
/// tested.
pub fn label(over: bool) -> &'static str {
    if over { "over" } else { "within" }
}

/// The sum of the times. The order they are added in changes nothing a test
/// can see.
pub fn total(times: &[u32]) -> u32 {
    times.iter().sum()
}

#[cfg(test)]
mod tests {
    use super::{far_over, label, over, total};

    #[test]
    fn at_the_budget_is_not_over() {
        assert!(!over(20.0, 20.0));
        assert!(over(20.5, 20.0));
    }

    #[test]
    fn the_line_is_one_and_a_half() {
        assert!(!far_over(30.0, 20.0));
        assert!(far_over(30.5, 20.0));
    }

    #[test]
    fn far_over_is_over() {
        assert!(!far_over(25.0, 20.0) && over(25.0, 20.0));
    }

    #[test]
    fn a_tier_over_its_budget_says_so() {
        assert_eq!(label(true), "over");
    }

    #[test]
    fn the_total_is_the_sum() {
        assert_eq!(total(&[1, 2, 3]), 6);
    }
}
