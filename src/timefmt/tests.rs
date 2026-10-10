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

#[test]
fn an_email_date_header_is_normalised() {
    // The sender's zone, with and without the zone-name comment mailers add.
    for header in [
        "Mon, 14 Sep 2026 05:36:49 +0200",
        "Mon, 14 Sep 2026 05:36:49 +0200 (CEST)",
        "Sun, 13 Sep 2026 20:36:49 -0700 (PDT)",
        "Mon, 14 Sep 2026 03:36:49 GMT",
    ] {
        assert_eq!(
            from_rfc2822(header, Zone::Utc),
            "2026-09-14T03:36:49Z",
            "{header}"
        );
    }
    let local = from_rfc2822("Mon, 14 Sep 2026 05:36:49 +0200 (CEST)", Zone::Local);
    assert_eq!(local, from_epoch(1_789_357_009, Zone::Local));
    // Not a date: shown as it came, never replaced by a guess.
    assert_eq!(
        from_rfc2822("sometime tuesday", Zone::Utc),
        "sometime tuesday"
    );
    assert_eq!(from_rfc2822("", Zone::Utc), "");
}

#[test]
fn every_epoch_unit_lands_on_the_same_second() {
    assert_eq!(
        from_epoch_ms(1_789_357_009_709, Zone::Utc),
        "2026-09-14T03:36:49Z"
    );
    assert_eq!(
        from_epoch_f64(1_789_357_009.709, Zone::Utc),
        "2026-09-14T03:36:49Z"
    );
    assert_eq!(from_epoch_ms(0, Zone::Utc), "-");
    assert_eq!(from_epoch_f64(f64::NAN, Zone::Utc), "-");
}

#[test]
fn a_sql_style_time_with_an_offset_is_read_but_a_zoneless_one_is_not_guessed() {
    assert_eq!(
        from_iso("2026-09-14 03:36:49.709+00:00", Zone::Utc),
        "2026-09-14T03:36:49Z"
    );
    assert_eq!(
        from_iso("2026-09-14 03:36:49", Zone::Utc),
        "2026-09-14 03:36:49"
    );
}

#[test]
fn ago_pairs_with_the_absolute_time_first() {
    let at = 1_789_357_009;
    assert_eq!(
        with_ago(at, at + 3 * 3600 + 5, Zone::Utc),
        "2026-09-14T03:36:49Z (3h ago)"
    );
    assert_eq!(ago(0), "just now");
    assert_eq!(ago(-30), "just now", "a clock that moved is not negative");
    assert_eq!(ago(45), "45s ago");
    assert_eq!(ago(120), "2m ago");
    assert_eq!(ago(3 * 86_400), "3d ago");
    assert_eq!(with_ago(0, at, Zone::Utc), "-");
}

#[test]
fn a_command_scope_sets_the_zone_for_what_runs_inside_it() {
    assert_eq!(zone(), Zone::Utc, "UTC unless asked");
    let inside = scoped_sync(Zone::Local, zone);
    assert_eq!(inside, Zone::Local);
    assert_eq!(zone(), Zone::Utc, "the scope ends with the command");
    // A nested command without the flag keeps the enclosing choice.
    let nested = scoped_sync(Zone::Local, || Zone::from_args(&["list".to_string()]).0);
    assert_eq!(nested, Zone::Local);
}

#[test]
fn run_sync_takes_the_flag_and_scopes_the_body() {
    let args: Vec<String> = ["show", "--local", "7"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let (seen_zone, seen_args) = run_sync(&args, |rest| (zone(), rest.to_vec()));
    assert_eq!(seen_zone, Zone::Local);
    assert_eq!(seen_args, ["show", "7"]);
    let (seen_zone, _) = run_sync(&args[..1], |rest| (zone(), rest.to_vec()));
    assert_eq!(seen_zone, Zone::Utc);
}

#[tokio::test]
async fn an_async_scope_holds_across_awaits() {
    let seen = scoped(Zone::Local, async {
        tokio::task::yield_now().await;
        zone()
    })
    .await;
    assert_eq!(seen, Zone::Local);
    assert_eq!(zone(), Zone::Utc);
}
