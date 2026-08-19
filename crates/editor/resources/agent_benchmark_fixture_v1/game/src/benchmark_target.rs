/// Returns the deterministic baseline score used by Agent Benchmark code tasks.
pub fn fixture_score(value: i32) -> i32 {
    value + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_score_baseline_is_valid() {
        assert_eq!(fixture_score(4), 5);
    }
}
