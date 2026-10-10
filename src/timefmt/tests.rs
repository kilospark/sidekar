use super::*;

#[test]
fn utc_is_iso_with_z_to_the_second() {
    assert_eq!(
        from_iso("2026-09-14T03:36:49.709Z", Zone::Utc),
        "2026-09-14T03:36:49Z"
    );
    assert_eq!(
        from_iso("2026-09-14T03:36:49Z", Zone::Utc),
        "2026-09-14T03:36:49Z"
    );
    // An offset in the input is normalised to UTC.
    assert_eq!(
        from_iso("2026-09-13T23:36:49-04:00", Zone::Utc),
        "2026-09-14T03:36:49Z"
    );
    assert_eq!(from_epoch(1_789_357_009, Zone::Utc), "2026-09-14T03:36:49Z");
    assert_eq!(
        from_slack_ts("1789357009.709100", Zone::Utc),
        "2026-09-14T03:36:49Z"
    );
}

#[test]
fn nothing_and_nonsense_are_not_invented() {
    assert_eq!(from_iso("", Zone::Utc), "");
    assert_eq!(from_iso("  ", Zone::Local), "");
    assert_eq!(from_iso("yesterday", Zone::Utc), "yesterday");
    assert_eq!(from_epoch(0, Zone::Utc), "-");
    assert_eq!(from_slack_ts("p123", Zone::Utc), "p123");
}

#[test]
fn local_carries_an_explicit_offset_for_the_same_moment() {
    let shown = from_iso("2026-09-14T03:36:49.709Z", Zone::Local);
    // Whatever this machine's zone, the offset is explicit and the moment is
    // the same one.
    assert!(!shown.ends_with('Z'), "{shown}");
    let tail = &shown[shown.len() - 6..];
    assert!(
        (tail.starts_with('+') || tail.starts_with('-')) && tail.as_bytes()[3] == b':',
        "{shown}"
    );
    let back = DateTime::parse_from_rfc3339(&shown).unwrap();
    assert_eq!(
        back.with_timezone(&Utc).to_rfc3339(),
        "2026-09-14T03:36:49+00:00"
    );
    assert_eq!(
        from_epoch(1_789_357_009, Zone::Local),
        shown,
        "epoch and ISO agree"
    );
}

#[test]
fn local_is_picked_from_anywhere_in_the_args_and_removed() {
    let args: Vec<String> = ["read", "#eng", "--local", "--limit", "5"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (zone, rest) = Zone::from_args(&args);
    assert_eq!(zone, Zone::Local);
    assert_eq!(rest, ["read", "#eng", "--limit", "5"]);
    let (zone, rest) = Zone::from_args(&rest);
    assert_eq!(zone, Zone::Utc);
    assert_eq!(rest.len(), 4);
}
