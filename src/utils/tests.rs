use super::epoch_to_date;

#[test]
fn epoch_to_date_non_positive() {
    assert_eq!(epoch_to_date(0), "—");
    assert_eq!(epoch_to_date(-1), "—");
}

#[test]
fn epoch_to_date_known_instants() {
    assert_eq!(epoch_to_date(1), "1970-01-01 00:00:01 UTC");
    assert_eq!(epoch_to_date(86_400), "1970-01-02 00:00:00 UTC");
}
