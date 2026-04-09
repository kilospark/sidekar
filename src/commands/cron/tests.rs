use super::*;

#[test]
fn parse_star() {
    let vals = parse_field("*", 0, 59).unwrap();
    assert_eq!(vals.len(), 60);
    assert_eq!(vals[0], 0);
    assert_eq!(vals[59], 59);
}

#[test]
fn parse_step() {
    let vals = parse_field("*/15", 0, 59).unwrap();
    assert_eq!(vals, vec![0, 15, 30, 45]);
}

#[test]
fn parse_range() {
    let vals = parse_field("1-5", 0, 59).unwrap();
    assert_eq!(vals, vec![1, 2, 3, 4, 5]);
}

#[test]
fn parse_range_with_step() {
    let vals = parse_field("0-20/5", 0, 59).unwrap();
    assert_eq!(vals, vec![0, 5, 10, 15, 20]);
}

#[test]
fn parse_list() {
    let vals = parse_field("1,5,10,15", 0, 59).unwrap();
    assert_eq!(vals, vec![1, 5, 10, 15]);
}

#[test]
fn parse_single() {
    let vals = parse_field("30", 0, 59).unwrap();
    assert_eq!(vals, vec![30]);
}

#[test]
fn parse_schedule_every_5_min() {
    let sched = CronSchedule::parse("*/5 * * * *").unwrap();
    assert!(sched.matches(0, 12, 15, 6, 3));
    assert!(sched.matches(5, 12, 15, 6, 3));
    assert!(!sched.matches(3, 12, 15, 6, 3));
}

#[test]
fn parse_schedule_specific() {
    let sched = CronSchedule::parse("30 9 * * 1-5").unwrap();
    assert!(sched.matches(30, 9, 15, 6, 1));
    assert!(sched.matches(30, 9, 15, 6, 5));
    assert!(!sched.matches(30, 9, 15, 6, 0));
    assert!(!sched.matches(0, 9, 15, 6, 1));
}

#[test]
fn parse_invalid() {
    assert!(CronSchedule::parse("*/5 *").is_err());
    assert!(CronSchedule::parse("60 * * * *").is_err());
    assert!(parse_field("*/0", 0, 59).is_err());
}

#[test]
fn interval_to_cron_minutes() {
    assert_eq!(interval_to_cron("5m").unwrap(), "*/5 * * * *");
    assert_eq!(interval_to_cron("1m").unwrap(), "*/1 * * * *");
    assert_eq!(interval_to_cron("30m").unwrap(), "*/30 * * * *");
}

#[test]
fn interval_to_cron_hours() {
    assert_eq!(interval_to_cron("1h").unwrap(), "0 */1 * * *");
    assert_eq!(interval_to_cron("2h").unwrap(), "0 */2 * * *");
}

#[test]
fn interval_to_cron_seconds_clamp() {
    assert_eq!(interval_to_cron("30s").unwrap(), "*/1 * * * *");
    assert_eq!(interval_to_cron("120s").unwrap(), "*/2 * * * *");
}

#[test]
fn interval_to_cron_large_minutes() {
    assert_eq!(interval_to_cron("120m").unwrap(), "0 */2 * * *");
}

#[test]
fn interval_to_cron_default_unit() {
    assert_eq!(interval_to_cron("10").unwrap(), "*/10 * * * *");
}

#[test]
fn interval_to_cron_invalid() {
    assert!(interval_to_cron("0m").is_err());
    assert!(interval_to_cron("abc").is_err());
}

#[test]
fn cron_action_serde_roundtrip() {
    let tool: CronAction = serde_json::from_str(r#"{"tool":"screenshot"}"#).unwrap();
    assert!(matches!(tool, CronAction::Tool { .. }));

    let bash: CronAction = serde_json::from_str(r#"{"command":"echo hello"}"#).unwrap();
    assert!(matches!(bash, CronAction::Bash { .. }));

    let prompt: CronAction = serde_json::from_str(r#"{"prompt":"check status"}"#).unwrap();
    assert!(matches!(prompt, CronAction::Prompt { .. }));

    let batch: CronAction = serde_json::from_str(r#"{"batch":[{"tool":"read"}]}"#).unwrap();
    assert!(matches!(batch, CronAction::Batch { .. }));
}

#[test]
fn normalize_self_target_to_creator() {
    assert_eq!(
        normalize_cron_target("self", "cheetah-sidekar-1").unwrap(),
        "cheetah-sidekar-1"
    );
    assert_eq!(
        normalize_loaded_target("self", "cheetah-sidekar-1"),
        "cheetah-sidekar-1"
    );
}

#[test]
fn job_belongs_to_concrete_owner_only() {
    assert!(job_belongs_to_agent(
        "cheetah-sidekar-1",
        "cheetah-sidekar-1",
        Some("cheetah-sidekar-1")
    ));
    assert!(!job_belongs_to_agent(
        "cheetah-sidekar-1",
        "cheetah-sidekar-1",
        Some("otter-sidekar-1")
    ));
    assert!(job_belongs_to_agent(
        "self",
        "cheetah-sidekar-1",
        Some("cheetah-sidekar-1")
    ));
}
