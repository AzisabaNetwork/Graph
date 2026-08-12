pub(crate) fn matches_half_open_range<T: PartialOrd>(
    value: Option<&T>,
    from: Option<&T>,
    to: Option<&T>,
) -> bool {
    if from.is_none() && to.is_none() {
        return true;
    }
    value.is_some_and(|value| {
        from.is_none_or(|from| value >= from) && to.is_none_or(|to| value < to)
    })
}

pub(crate) fn is_valid_half_open_range<T: PartialOrd>(from: Option<&T>, to: Option<&T>) -> bool {
    !matches!((from, to), (Some(from), Some(to)) if from >= to)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_from_inclusively_and_to_exclusively() {
        assert!(matches_half_open_range(Some(&2), Some(&2), Some(&3)));
        assert!(!matches_half_open_range(Some(&2), Some(&1), Some(&2)));
        assert!(!matches_half_open_range(None, Some(&1), None));
        assert!(matches_half_open_range::<i32>(None, None, None));
    }

    #[test]
    fn requires_lower_bound_to_precede_upper_bound() {
        assert!(is_valid_half_open_range(Some(&1), Some(&2)));
        assert!(!is_valid_half_open_range(Some(&2), Some(&2)));
        assert!(!is_valid_half_open_range(Some(&3), Some(&2)));
        assert!(is_valid_half_open_range(Some(&1), None));
        assert!(is_valid_half_open_range(None, Some(&2)));
    }
}
