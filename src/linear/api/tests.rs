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
    assert_eq!(issues(&l, &q).await.unwrap().items[0].identifier, "ENG-1");
    let q = IssueQuery {
        limit: 5,
        ..Default::default()
    };
    assert_eq!(issues(&l, &q).await.unwrap().items[0].identifier, "ENG-2");
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
        ..Default::default()
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
        ..Default::default()
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

#[test]
fn since_reads_short_durations_and_passes_dates_through() {
    assert_eq!(since("7d").unwrap(), "-P7D");
    assert_eq!(since("2w").unwrap(), "-P2W");
    assert_eq!(since("12h").unwrap(), "-PT12H");
    assert_eq!(since("30m").unwrap(), "-PT30M");
    assert_eq!(since("2026-10-01").unwrap(), "2026-10-01");
    assert_eq!(since("-P3D").unwrap(), "-P3D");
    assert!(since("soon").is_err());
    assert!(since("5y").is_err());
}

#[test]
fn issue_activity_filters_on_updated_at_including_closed() {
    let f = issue_filter(&IssueQuery {
        team: Some("ENG".into()),
        include_closed: true,
        updated_since: Some("-P7D".into()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(f["updatedAt"], json!({"gt": "-P7D"}));
    assert!(f.get("state").is_none(), "closed issues count as activity");
}

#[test]
fn comment_activity_is_scoped_through_the_issue() {
    let all = comment_filter(None, None, "-P1D");
    assert_eq!(all["createdAt"], json!({"gt": "-P1D"}));
    assert_eq!(all["issue"], json!({"null": false}), "issue comments only");
    let scoped = comment_filter(Some("ENG"), Some("Web"), "-P1D");
    assert_eq!(scoped["issue"]["team"]["key"]["eqIgnoreCase"], "ENG");
    assert_eq!(
        scoped["issue"]["project"]["name"]["containsIgnoreCase"],
        "Web"
    );
}

#[test]
fn notifications_parse_issue_and_project_kinds() {
    let issue = notification_from(&json!({
        "id": "n1", "type": "issueComment", "title": "t", "createdAt": "2026-10-10T10:00:00.000Z",
        "readAt": null, "archivedAt": null, "actor": {"name": "Ann Lee", "displayName": "ann"},
        "issue": {"identifier": "ENG-1", "title": "Bug"}, "comment": {"body": "\n  first line\nsecond"}
    }));
    assert!(!issue.read && !issue.archived);
    assert_eq!(issue.actor, "ann");
    assert_eq!(issue.issue, "ENG-1");
    assert_eq!(issue.comment, "first line");
    let project = notification_from(&json!({
        "id": "n2", "type": "projectUpdateCreated", "readAt": "2026-10-09T00:00:00Z",
        "project": {"name": "Web"}
    }));
    assert!(project.read);
    assert_eq!(project.project, "Web");
    assert_eq!(project.issue, "");
}

#[test]
fn first_line_skips_blanks_and_truncates() {
    assert_eq!(first_line("\n\n  hi there \nmore", 50), "hi there");
    assert_eq!(first_line("abcdef", 3), "abc…");
    assert_eq!(first_line("", 3), "");
}

#[tokio::test]
async fn the_inbox_is_newest_first_and_can_show_only_unread() {
    let page = json!({"data": {"notificationsUnreadCount": 1, "notifications": {"nodes": [
        {"id": "old", "type": "issueAssignedToYou", "createdAt": "2026-10-01T00:00:00Z", "readAt": null},
        {"id": "new", "type": "issueComment", "createdAt": "2026-10-09T00:00:00Z", "readAt": "2026-10-09T01:00:00Z"}
    ]}}});
    let server = MockServer::sequence(vec![page.clone(), page]);
    let (unread, all) = notifications(&linear_at(&server), false, false, 10)
        .await
        .unwrap();
    assert_eq!(unread, 1);
    assert_eq!(
        all.items.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        ["new", "old"]
    );
    let (_, only) = notifications(&linear_at(&server), true, false, 10)
        .await
        .unwrap();
    assert_eq!(
        only.items.iter().map(|n| n.id.as_str()).collect::<Vec<_>>(),
        ["old"]
    );
    let vars = &server.requests()[0].json()["variables"];
    assert_eq!(vars["includeArchived"], false);
}

#[tokio::test]
async fn marking_read_sets_read_at_and_unread_clears_it() {
    let ok = json!({"data": {"notificationUpdate": {"success": true}}});
    let server = MockServer::sequence(vec![ok.clone(), ok]);
    mark_notification(&linear_at(&server), "n1", true)
        .await
        .unwrap();
    mark_notification(&linear_at(&server), "n1", false)
        .await
        .unwrap();
    let reqs = server.requests();
    let read_at = reqs[0].json()["variables"]["input"]["readAt"].clone();
    let at = read_at.as_str().unwrap();
    assert!(
        at.ends_with('Z') && at.contains('T') && at.len() == 20,
        "{at}"
    );
    assert_eq!(reqs[1].json()["variables"]["input"]["readAt"], Value::Null);
}

#[tokio::test]
async fn archiving_reports_failure() {
    let server = MockServer::sequence(vec![
        json!({"data": {"notificationArchive": {"success": false}}}),
    ]);
    assert!(
        archive_notification(&linear_at(&server), "n1")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn organization_names_the_workspace_and_viewer() {
    let server = MockServer::sequence(vec![json!({"data": {
        "organization": {"id": "o", "name": "Acme", "urlKey": "acme", "userCount": 12},
        "viewer": {"name": "Kay B", "displayName": "kay", "email": "k@acme.dev"}}})]);
    let o = organization(&linear_at(&server)).await.unwrap();
    assert_eq!(
        (o.name.as_str(), o.url_key.as_str(), o.users),
        ("Acme", "acme", 12)
    );
    assert_eq!(
        (o.viewer.as_str(), o.viewer_email.as_str()),
        ("kay", "k@acme.dev")
    );
}

#[tokio::test]
async fn activity_asks_for_issues_then_comments() {
    let server = MockServer::sequence(vec![
        json!({"data": {"issues": {"nodes": [{"identifier": "ENG-2", "updatedAt": "2026-10-09T00:00:00Z"}]}}}),
        json!({"data": {"comments": {"nodes": [{"body": "done", "createdAt": "2026-10-10T00:00:00Z",
            "user": {"name": "Ann"}, "issue": {"identifier": "ENG-2", "title": "x"}}]}}}),
    ]);
    let a = activity(&linear_at(&server), Some("ENG"), None, "-P7D", 5)
        .await
        .unwrap();
    assert_eq!(a.issues[0].identifier, "ENG-2");
    assert_eq!(a.comments[0].author, "Ann");
    let reqs = server.requests();
    assert_eq!(
        reqs[0].json()["variables"]["filter"]["updatedAt"]["gt"],
        "-P7D"
    );
    assert_eq!(
        reqs[1].json()["variables"]["filter"]["issue"]["team"]["key"]["eqIgnoreCase"],
        "ENG"
    );
}

#[test]
fn history_entries_read_as_changes() {
    let h = json!({
        "fromState": {"name": "Todo"}, "toState": {"name": "In Progress"},
        "fromAssignee": null, "toAssignee": {"name": "Bob B", "displayName": "bob"},
        "fromPriority": 0.0, "toPriority": 2.0,
        "addedLabels": [{"name": "bug"}], "removedLabels": [],
        "updatedDescription": true
    });
    assert_eq!(
        describe_history(&h),
        "state Todo → In Progress; assignee none → bob; priority none → high; +labels bug; edited description"
    );
    assert_eq!(describe_history(&json!({})), "updated");
    assert_eq!(
        describe_history(&json!({"archived": true, "autoArchived": true})),
        "auto-archived"
    );
}

#[tokio::test]
async fn history_is_oldest_first() {
    let server = MockServer::sequence(vec![
        json!({"data": {"issue": {"identifier": "ENG-1", "title": "T",
        "history": {"nodes": [
            {"createdAt": "2026-10-02T00:00:00Z", "toTitle": "T", "fromTitle": "t"},
            {"createdAt": "2026-10-01T00:00:00Z", "actor": {"name": "Ann"}}
        ]}}}}),
    ]);
    let (title, h) = history(&linear_at(&server), "ENG-1", 10).await.unwrap();
    assert_eq!(title, "ENG-1 T");
    assert_eq!(h[0].actor, "Ann");
    assert_eq!(h[1].change, "title t → T");
}

#[test]
fn teams_and_projects_carry_the_useful_fields() {
    let t = team_from(
        &json!({"id": "1", "key": "ENG", "name": "Eng", "private": true,
        "issueCount": 42, "cyclesEnabled": true, "description": "d"}),
    );
    assert!(t.private && t.cycles_enabled);
    assert_eq!(t.issue_count, 42);
    let p = project_from(
        &json!({"name": "Web", "startDate": "2026-09-01", "health": "onTrack",
        "teams": {"nodes": [{"key": "ENG"}, {"key": "DES"}]}, "url": "u"}),
    );
    assert_eq!(p.teams, ["ENG", "DES"]);
    assert_eq!(
        (p.start.as_str(), p.health.as_str()),
        ("2026-09-01", "onTrack")
    );
}

fn routed(
    route: impl Fn(&crate::test_http::Request) -> (u16, String, Vec<u8>) + Send + Sync + 'static,
) -> MockServer {
    MockServer::start_raw(route)
}

fn gql(v: Value) -> (u16, String, Vec<u8>) {
    (200, "application/json".into(), v.to_string().into_bytes())
}

#[test]
fn uploads_are_found_in_markdown_with_their_names() {
    let md = "See ![shot.png](https://uploads.linear.app/o/a/b) and [log](https://uploads.linear.app/o/c/d).\n\
              Bare: https://uploads.linear.app/o/e/report%20v2.pdf and again ![x](https://uploads.linear.app/o/a/b)\n\
              Not ours: https://example.com/uploads.linear.app/x";
    assert_eq!(
        uploads_in(md),
        vec![
            (
                "shot.png".to_string(),
                "https://uploads.linear.app/o/a/b".to_string()
            ),
            (
                "log".to_string(),
                "https://uploads.linear.app/o/c/d".to_string()
            ),
            (
                "report v2.pdf".to_string(),
                "https://uploads.linear.app/o/e/report%20v2.pdf".to_string()
            ),
        ]
    );
    assert!(uploads_in("nothing here").is_empty());
}

fn detail_with_files() -> IssueDetail {
    issue_detail_from(&json!({
        "identifier": "ENG-1", "title": "T",
        "description": "Repro: ![a.png](https://uploads.linear.app/o/1/2)",
        "comments": {"nodes": [{"body": "log [b.txt](https://uploads.linear.app/o/3/4)",
            "createdAt": "2026-10-10T10:00:00Z", "user": {"name": "Ann"}}]},
        "attachments": {"nodes": [
            {"id": "at1", "title": "PR #5", "url": "https://github.com/o/r/pull/5", "sourceType": "github"},
            {"id": "at2", "title": "spec.pdf", "subtitle": "spec.pdf · 1.0M", "url": "https://uploads.linear.app/o/5/6"}
        ]}
    }))
}

#[test]
fn issues_list_attachments_and_embedded_files() {
    let i = detail_with_files();
    assert_eq!(i.attachments.len(), 2);
    assert!(!i.attachments[0].is_upload() && i.attachments[1].is_upload());
    assert_eq!(
        i.attachments[0].line(),
        "PR #5\tgithub\thttps://github.com/o/r/pull/5"
    );
    let files = embedded_files(&i);
    assert_eq!(files[0].place, "description");
    assert_eq!(files[1].place, "comment by Ann 2026-10-10");
    let all = downloadable_files(&i);
    assert_eq!(
        all.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
        ["a.png", "b.txt", "spec.pdf"],
        "uploaded attachments are downloadable, links are not"
    );
    let shown = render_issue(&i);
    assert!(shown.contains("Attachments (2):"), "{shown}");
    assert!(shown.contains("Uploaded files in the text (2)"), "{shown}");
}

#[test]
fn embedded_markdown_matches_linears_editor() {
    assert_eq!(markdown_for("a.png", "image/png", "U"), "![a.png](U)");
    assert_eq!(
        markdown_for("[x].pdf", "application/pdf", "U"),
        "[x.pdf](U)"
    );
    assert_eq!(with_files(Some("hi\n"), "[f](U)"), "hi\n\n[f](U)");
    assert_eq!(with_files(None, "[f](U)"), "[f](U)");
    assert_eq!(with_files(Some("hi"), ""), "hi");
}

#[tokio::test]
async fn downloads_send_the_key_only_to_linear_uploads() {
    let server = routed(|_| (200, "image/png".into(), vec![1, 2, 3]));
    let linear = linear_at(&server);
    let (bytes, ct) = linear
        .download(&format!("{}/o/1/2", server.base))
        .await
        .unwrap();
    assert_eq!((bytes, ct.as_str()), (vec![1, 2, 3], "image/png"));
    assert_eq!(
        server.requests()[0].headers["authorization"],
        "lin_api_test"
    );
    let err = linear
        .download("https://github.com/o/r/pull/5")
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not a Linear upload"), "{err}");
    assert!(
        linear
            .download("https://uploads.linear.app.evil.dev/x")
            .await
            .is_err()
    );
    assert_eq!(server.requests().len(), 1);
}

fn upload_server() -> MockServer {
    let base = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let b2 = base.clone();
    let server = routed(move |req| {
        if req.method == "PUT" {
            return (200, "text/plain".into(), Vec::new());
        }
        let q = req.json()["query"].as_str().unwrap_or("").to_string();
        if q.contains("fileUpload") {
            gql(
                json!({"data": {"fileUpload": {"success": true, "uploadFile": {
                "uploadUrl": format!("{}/signed", b2.lock().unwrap()),
                "assetUrl": "https://uploads.linear.app/o/new/file",
                "headers": [{"key": "x-goog-meta-k", "value": "v"}]}}}}),
            )
        } else if q.contains("attachmentCreate") {
            gql(json!({"data": {"attachmentCreate": {"success": true,
                "attachment": {"id": "at", "title": "t", "url": "https://uploads.linear.app/o/new/file"}}}}))
        } else if q.contains("attachmentLinkURL") {
            gql(json!({"data": {"attachmentLinkURL": {"success": true,
                "attachment": {"id": "at", "title": "PR", "url": "https://github.com/o/r/pull/9", "sourceType": "github"}}}}))
        } else {
            gql(
                json!({"data": {"issue": {"id": "uuid-1", "identifier": "ENG-1",
                "team": {"id": "t", "key": "ENG"}, "labels": {"nodes": []}}}}),
            )
        }
    });
    *base.lock().unwrap() = server.base.clone();
    server
}

#[tokio::test]
async fn uploads_put_to_the_signed_url_without_the_key() {
    let server = upload_server();
    let url = upload_file(
        &linear_at(&server),
        "a.txt",
        "text/plain",
        b"hello".to_vec(),
    )
    .await
    .unwrap();
    assert_eq!(url, "https://uploads.linear.app/o/new/file");
    let reqs = server.requests();
    let vars = &reqs[0].json()["variables"];
    assert_eq!(
        (vars["filename"].as_str(), vars["size"].as_i64()),
        (Some("a.txt"), Some(5))
    );
    let put = &reqs[1];
    assert_eq!((put.method.as_str(), put.path()), ("PUT", "/signed"));
    assert_eq!(put.body, "hello");
    assert_eq!(put.headers["x-goog-meta-k"], "v");
    assert_eq!(put.headers["content-type"], "text/plain");
    assert!(!put.headers.contains_key("authorization"));
}

#[tokio::test]
async fn attaching_a_file_uploads_then_creates_the_attachment() {
    let server = upload_server();
    let dir = std::env::temp_dir().join(format!("sidekar-lin-att-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("spec.pdf");
    std::fs::write(&path, b"%PDF").unwrap();
    attach_file(&linear_at(&server), "ENG-1", path.to_str().unwrap(), None)
        .await
        .unwrap();
    let reqs = server.requests();
    let last = reqs.last().unwrap().json();
    let input = &last["variables"]["input"];
    assert_eq!(input["issueId"], "uuid-1");
    assert_eq!(input["title"], "spec.pdf");
    assert_eq!(input["url"], "https://uploads.linear.app/o/new/file");
    assert_eq!(input["subtitle"], "spec.pdf · 4B");
    assert_eq!(
        reqs[1].json()["variables"]["contentType"],
        "application/pdf"
    );
    std::fs::remove_dir_all(dir).ok();
}

#[tokio::test]
async fn linking_a_url_uses_attachment_link_url() {
    let server = upload_server();
    let a = link_url(
        &linear_at(&server),
        "ENG-1",
        "https://github.com/o/r/pull/9",
        Some("PR"),
    )
    .await
    .unwrap();
    assert_eq!(a.source, "github");
    let vars = &server.requests()[1].json()["variables"];
    assert_eq!(
        (vars["issueId"].as_str(), vars["title"].as_str()),
        (Some("uuid-1"), Some("PR"))
    );
    assert!(
        link_url(&linear_at(&server), "ENG-1", "ftp://x", None)
            .await
            .is_err()
    );
}

mod paging {
    use super::*;

    fn page(conn: &str, nodes: Value, next: Option<&str>) -> Value {
        json!({"data": {conn: {"nodes": nodes, "pageInfo": {
            "hasNextPage": next.is_some(), "endCursor": next}}}})
    }

    #[tokio::test]
    async fn labels_follow_the_cursor_to_the_last_page() {
        let server = MockServer::sequence(vec![
            page(
                "issueLabels",
                json!([{"id": "L1", "name": "bug"}]),
                Some("c1"),
            ),
            page("issueLabels", json!([{"id": "L2", "name": "zebra"}]), None),
        ]);
        let all = labels(&linear_at(&server), Some("T1")).await.unwrap();
        assert_eq!(
            all.iter().map(|l| l.id.as_str()).collect::<Vec<_>>(),
            ["L1", "L2"]
        );
        let reqs = server.requests();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].json()["variables"]["after"], Value::Null);
        assert_eq!(reqs[1].json()["variables"]["after"], "c1");
        assert_eq!(reqs[1].json()["variables"]["teamId"], "T1");
        // A label past the first page is still found by name.
        assert_eq!(pick_labels(&all, &["Zebra".into()]).unwrap(), ["L2"]);
    }

    #[tokio::test]
    async fn an_exact_project_name_on_a_later_page_wins() {
        let server = MockServer::sequence(vec![
            page(
                "projects",
                json!([{"id": "P1", "name": "Launch v2"}, {"id": "P2", "name": "Launch beta"}]),
                Some("c1"),
            ),
            page("projects", json!([{"id": "P3", "name": "launch"}]), None),
        ]);
        assert_eq!(
            resolve_project(&linear_at(&server), "Launch")
                .await
                .unwrap(),
            "P3"
        );
    }

    #[tokio::test]
    async fn a_list_cut_at_the_limit_says_there_is_more() {
        let rows = |ids: &[&str]| {
            json!(
                ids.iter()
                    .map(|i| json!({"identifier": i}))
                    .collect::<Vec<_>>()
            )
        };
        let server = MockServer::sequence(vec![
            page("issues", rows(&["A-1", "A-2"]), Some("c1")),
            page("issues", rows(&["A-3"]), Some("c2")),
        ]);
        let q = IssueQuery {
            include_closed: true,
            limit: 3,
            ..Default::default()
        };
        let got = issues(&linear_at(&server), &q).await.unwrap();
        assert_eq!(got.items.len(), 3);
        assert!(got.more, "Linear said there was a next page");
        let reqs = server.requests();
        assert_eq!(reqs.len(), 2, "stops once it has enough");
        assert_eq!(reqs[0].json()["variables"]["first"], 3);
        assert_eq!(
            reqs[1].json()["variables"]["first"],
            1,
            "asks only for what is left"
        );

        // All of it fit: no note.
        let server = MockServer::sequence(vec![page("issues", rows(&["A-1"]), None)]);
        let got = issues(&linear_at(&server), &q).await.unwrap();
        assert!(!got.more);
    }

    #[tokio::test]
    async fn a_filtered_user_list_pages_until_it_has_enough() {
        let user = |id: &str, active: bool| json!({"id": id, "name": id, "active": active});
        let server = MockServer::sequence(vec![
            page(
                "users",
                json!([user("a", false), user("b", true)]),
                Some("c1"),
            ),
            page(
                "users",
                json!([user("c", false), user("d", true), user("e", true)]),
                None,
            ),
        ]);
        let active = |p: &Person| p.active;
        let got = users(&linear_at(&server), 2, Some(&active)).await.unwrap();
        assert_eq!(
            got.items.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["b", "d"]
        );
        assert!(got.more, "e was left over");
        assert_eq!(server.requests()[0].json()["variables"]["first"], 250);
    }

    #[tokio::test]
    async fn cycles_are_sorted_across_pages_then_cut() {
        let c = |n: i64| json!({"id": format!("C{n}"), "number": n, "team": {"key": "ENG"}});
        let server = MockServer::sequence(vec![
            page("cycles", json!([c(1), c(3)]), Some("c1")),
            page("cycles", json!([c(2)]), None),
        ]);
        let got = cycles(&linear_at(&server), Some("ENG"), true, 2)
            .await
            .unwrap();
        assert_eq!(
            got.items.iter().map(|c| c.number).collect::<Vec<_>>(),
            [3, 2]
        );
        assert!(got.more);
    }

    #[tokio::test]
    async fn reading_all_unread_stops_paging_once_every_unread_one_is_found() {
        let n = |id: &str, read: bool| {
            json!({"id": id, "createdAt": "2026-10-01T00:00:00Z",
                   "readAt": if read { json!("2026-10-02T00:00:00Z") } else { Value::Null }})
        };
        let p = |nodes: Value, next: Option<&str>| {
            let mut v = page("notifications", nodes, next);
            v["data"]["notificationsUnreadCount"] = json!(2);
            v
        };
        let server = MockServer::sequence(vec![
            p(json!([n("a", false), n("b", true)]), Some("c1")),
            p(json!([n("c", true), n("d", false)]), Some("c2")),
            p(json!([n("e", false)]), None),
        ]);
        let (unread, got) = notifications(&linear_at(&server), true, false, usize::MAX)
            .await
            .unwrap();
        assert_eq!(unread, 2);
        let mut ids: Vec<&str> = got.items.iter().map(|n| n.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, ["a", "d"]);
        assert!(!got.more);
        assert_eq!(server.requests().len(), 2, "the third page was not needed");
        assert_eq!(server.requests()[1].json()["variables"]["after"], "c1");
    }
}

#[test]
fn a_bare_upload_link_ending_a_sentence_drops_the_full_stop() {
    let md = "Logs at https://uploads.linear.app/o/f/run.log. And (https://uploads.linear.app/o/g/b.png), too.";
    let found = uploads_in(md);
    assert_eq!(
        found,
        [
            (
                "run.log".to_string(),
                "https://uploads.linear.app/o/f/run.log".to_string()
            ),
            (
                "b.png".to_string(),
                "https://uploads.linear.app/o/g/b.png".to_string()
            ),
        ]
    );
}
