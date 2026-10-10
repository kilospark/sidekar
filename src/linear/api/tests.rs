use super::*;
use crate::test_http::MockServer;

fn linear_at(server: &MockServer) -> Linear {
    Linear::with_url(MockServer::client(), &server.base, "lin_api_test".into())
}

#[test]
fn open_issues_only_unless_asked() {
    let f = issue_filter(&IssueQuery::default()).unwrap();
    assert_eq!(f["state"]["type"]["nin"], json!(["completed", "canceled"]));
    let all = IssueQuery {
        include_closed: true,
        ..Default::default()
    };
    assert!(issue_filter(&all).is_none(), "nothing narrows it");
}

#[test]
fn a_state_matches_by_name_or_type() {
    let q = IssueQuery {
        state: Some("In Review".into()),
        ..Default::default()
    };
    let f = issue_filter(&q).unwrap();
    assert_eq!(f["state"]["or"][0]["name"]["eqIgnoreCase"], "In Review");
    assert_eq!(f["state"]["or"][1]["type"]["eq"], "in review");
}

#[test]
fn every_filter_lands_where_linear_expects_it() {
    let q = IssueQuery {
        team: Some("ENG".into()),
        assignee: Some("me".into()),
        project: Some("Launch".into()),
        label: Some("bug".into()),
        priority: Some(1),
        include_closed: true,
        ..Default::default()
    };
    let f = issue_filter(&q).unwrap();
    assert_eq!(f["team"]["key"]["eqIgnoreCase"], "ENG");
    assert_eq!(f["assignee"]["isMe"]["eq"], true);
    assert_eq!(f["project"]["name"]["containsIgnoreCase"], "Launch");
    assert_eq!(f["labels"]["some"]["name"]["eqIgnoreCase"], "bug");
    assert_eq!(f["priority"]["eq"], 1);
    assert!(f.get("state").is_none());
}

#[test]
fn people_are_found_by_email_or_name() {
    assert_eq!(user_filter("a@b.co")["email"]["eqIgnoreCase"], "a@b.co");
    assert_eq!(
        user_filter("@Alice")["or"][1]["displayName"]["eqIgnoreCase"],
        "Alice"
    );
    assert_eq!(user_filter("ME")["isMe"]["eq"], true);
    let unassigned = issue_filter(&IssueQuery {
        assignee: Some("none".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(unassigned["assignee"]["null"], true);
}

#[test]
fn priorities_read_by_number_or_name() {
    assert_eq!(parse_priority("urgent").unwrap(), 1);
    assert_eq!(parse_priority("High").unwrap(), 2);
    assert_eq!(parse_priority("normal").unwrap(), 3);
    assert_eq!(parse_priority("4").unwrap(), 4);
    assert_eq!(parse_priority("none").unwrap(), 0);
    assert!(parse_priority("asap").is_err());
}

#[test]
fn uuids_are_recognised_and_identifiers_are_not() {
    assert!(is_uuid("0f8fad5b-d9cb-469f-a165-70867728950e"));
    assert!(!is_uuid("ENG-123"));
    assert!(!is_uuid("0f8fad5b-d9cb-469f-a165"));
}

fn st(name: &str, kind: &str) -> WorkflowState {
    WorkflowState {
        id: format!("id-{name}"),
        name: name.into(),
        kind: kind.into(),
        position: 0.0,
    }
}

#[test]
fn a_state_is_picked_by_name_then_by_unique_type() {
    let all = vec![
        st("Todo", "unstarted"),
        st("In Progress", "started"),
        st("Done", "completed"),
        st("Canceled", "canceled"),
        st("Duplicate", "canceled"),
    ];
    assert_eq!(
        pick_state(&all, "in progress").unwrap().id,
        "id-In Progress"
    );
    assert_eq!(pick_state(&all, "started").unwrap().id, "id-In Progress");
    // Two canceled states: the type alone is ambiguous, but the name is not.
    assert_eq!(pick_state(&all, "canceled").unwrap().id, "id-Canceled");
    let err = pick_state(&all, "Shipped").unwrap_err().to_string();
    assert!(err.contains("Todo, In Progress"), "{err}");
}

#[test]
fn a_team_label_wins_over_a_workspace_label_of_the_same_name() {
    let all = vec![
        Label {
            id: "ws".into(),
            name: "Bug".into(),
            team: "".into(),
            is_group: false,
        },
        Label {
            id: "team".into(),
            name: "bug".into(),
            team: "ENG".into(),
            is_group: false,
        },
        Label {
            id: "grp".into(),
            name: "Area".into(),
            team: "".into(),
            is_group: true,
        },
    ];
    assert_eq!(pick_labels(&all, &["BUG".into()]).unwrap(), vec!["team"]);
    // A label group cannot be applied to an issue.
    assert!(pick_labels(&all, &["Area".into()]).is_err());
}

#[test]
fn an_issue_reads_with_comments_oldest_first() {
    let v = json!({
        "id": "u", "identifier": "ENG-7", "title": "Fix login", "url": "https://linear.app/x/issue/ENG-7",
        "priorityLabel": "High", "state": {"name": "In Progress", "type": "started"},
        "assignee": {"name": "Alice Smith", "displayName": "alice"}, "team": {"key": "ENG", "name": "Engineering"},
        "description": "Steps:\n1. log in", "labels": {"nodes": [{"name": "bug"}]},
        "cycle": {"number": 12, "name": null}, "parent": null,
        "children": {"nodes": [{"identifier": "ENG-8", "title": "sub", "state": {"name": "Todo"}}]},
        "comments": {"nodes": [
            {"body": "second", "createdAt": "2026-10-02T00:00:00Z", "user": {"name": "Bob", "displayName": ""}},
            {"body": "first", "createdAt": "2026-10-01T00:00:00Z", "user": null}
        ]}
    });
    let i = issue_detail_from(&v);
    assert_eq!(i.row.assignee, "alice");
    assert_eq!(i.cycle, "12");
    assert!(i.parent.is_none());
    assert_eq!(i.comments[0].body, "first");
    assert_eq!(i.comments[0].author, "(integration)");
    assert_eq!(i.comments[1].author, "Bob");
    let text = render_issue(&i);
    assert!(
        text.starts_with("ENG-7: Fix login\nState: In Progress · Priority: High · Assignee: alice")
    );
    assert!(text.contains("Labels: bug"));
    assert!(text.contains("Sub-issues (1):\n  ENG-8\tTodo\tsub"));
    assert!(text.contains("Comments (2):\n--- (integration)"));
}

#[test]
fn cycles_say_where_they_are() {
    assert_eq!(
        cycle_from(&json!({"isActive": true, "number": 3})).status,
        "current"
    );
    assert_eq!(cycle_from(&json!({"isNext": true})).status, "next");
    assert_eq!(cycle_from(&json!({"isPast": true})).status, "past");
    assert_eq!(cycle_from(&json!({})).status, "upcoming");
}

#[test]
fn documents_are_well_formed() {
    // Balanced braces and parentheses, and every $variable used is declared.
    for doc in ALL_DOCUMENTS {
        assert_eq!(doc.matches('{').count(), doc.matches('}').count(), "{doc}");
        assert_eq!(doc.matches('(').count(), doc.matches(')').count(), "{doc}");
        let (head, body) = doc.split_once('{').unwrap();
        for word in body.split(|c: char| !c.is_alphanumeric() && c != '$' && c != '_') {
            if let Some(name) = word.strip_prefix('$') {
                assert!(
                    head.contains(&format!("${name}:")),
                    "${name} undeclared in {doc}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_text_query_uses_search_and_a_bare_one_lists() {
    let server = MockServer::sequence(vec![
        json!({"data": {"searchIssues": {"nodes": [{"identifier": "ENG-1", "title": "a"}]}}}),
        json!({"data": {"issues": {"nodes": [{"identifier": "ENG-2", "title": "b"}]}}}),
    ]);
    let l = linear_at(&server);
    let q = IssueQuery {
        text: Some("login".into()),
        limit: 5,
        ..Default::default()
    };
    assert_eq!(issues(&l, &q).await.unwrap()[0].identifier, "ENG-1");
    let q = IssueQuery {
        limit: 5,
        ..Default::default()
    };
    assert_eq!(issues(&l, &q).await.unwrap()[0].identifier, "ENG-2");
    let reqs = server.requests();
    assert!(
        reqs[0].json()["query"]
            .as_str()
            .unwrap()
            .contains("searchIssues")
    );
    assert_eq!(reqs[0].json()["variables"]["term"], "login");
    assert_eq!(reqs[0].json()["variables"]["first"], 5);
    assert!(
        reqs[1].json()["query"]
            .as_str()
            .unwrap()
            .contains("issues(filter")
    );
}

#[tokio::test]
async fn an_update_resolves_names_against_the_issues_team() {
    let server = MockServer::start(|req| {
        let q = req.json()["query"].as_str().unwrap_or("").to_string();
        let body = if q.contains("labels { nodes { id name } } } }")
            && q.contains("team { id key }")
        {
            json!({"data": {"issue": {"id": "issue-uuid", "identifier": "ENG-7",
                "team": {"id": "team-uuid", "key": "ENG"}, "labels": {"nodes": []}}}})
        } else if q.contains("workflowStates") {
            json!({"data": {"workflowStates": {"nodes": [
                {"id": "s-todo", "name": "Todo", "type": "unstarted", "position": 0},
                {"id": "s-done", "name": "Done", "type": "completed", "position": 1}
            ]}}})
        } else if q.contains("issueLabels") {
            json!({"data": {"issueLabels": {"nodes": [
                {"id": "l-bug", "name": "bug", "isGroup": false, "team": {"key": "ENG"}}
            ]}}})
        } else if q.contains("users(") {
            json!({"data": {"users": {"nodes": [{"id": "u-me", "name": "Me", "email": "me@x.co"}]}}})
        } else if q.contains("issueUpdate") {
            json!({"data": {"issueUpdate": {"success": true,
                "issue": {"identifier": "ENG-7", "url": "https://linear.app/x/issue/ENG-7"}}}})
        } else {
            json!({"errors": [{"message": format!("unexpected query {q}")}]})
        };
        (200, body.to_string())
    });
    let changes = Changes {
        state: Some("done".into()),
        assignee: Some("me".into()),
        add_labels: vec!["Bug".into()],
        priority: Some(2),
        ..Default::default()
    };
    let (id, url) = update(&linear_at(&server), "ENG-7", &changes)
        .await
        .unwrap();
    assert_eq!(id, "ENG-7");
    assert!(url.ends_with("ENG-7"));
    let reqs = server.requests();
    let states_req = reqs
        .iter()
        .find(|r| r.body.contains("workflowStates"))
        .unwrap();
    assert_eq!(states_req.json()["variables"]["teamId"], "team-uuid");
    let m = reqs
        .iter()
        .find(|r| r.body.contains("issueUpdate"))
        .unwrap()
        .json();
    assert_eq!(m["variables"]["id"], "issue-uuid");
    let input = &m["variables"]["input"];
    assert_eq!(input["stateId"], "s-done");
    assert_eq!(input["assigneeId"], "u-me");
    assert_eq!(input["addedLabelIds"], json!(["l-bug"]));
    assert_eq!(input["priority"], 2);
    assert!(input.get("title").is_none(), "only what changed is sent");
}

#[tokio::test]
async fn a_title_only_create_costs_one_mutation() {
    let server = MockServer::sequence(vec![json!({"data": {"issueCreate": {"success": true,
        "issue": {"identifier": "ENG-9", "url": "u"}}}})]);
    let team = Team {
        id: "t1".into(),
        key: "ENG".into(),
        name: "Eng".into(),
    };
    let c = Changes {
        title: Some("New".into()),
        ..Default::default()
    };
    let (id, _) = create(&linear_at(&server), &team, &c).await.unwrap();
    assert_eq!(id, "ENG-9");
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0].json()["variables"]["input"],
        json!({"title": "New", "teamId": "t1"})
    );
}

#[tokio::test]
async fn a_create_without_a_title_is_refused_before_any_call() {
    let server = MockServer::sequence(vec![]);
    let team = Team {
        id: "t".into(),
        key: "K".into(),
        name: "".into(),
    };
    assert!(
        create(&linear_at(&server), &team, &Changes::default())
            .await
            .is_err()
    );
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn a_missing_issue_is_named() {
    let server = MockServer::sequence(vec![json!({"data": {"issue": null}})]);
    let err = issue(&linear_at(&server), "ENG-404")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("ENG-404"), "{err}");
}
