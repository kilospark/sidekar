//! Linear issues, comments, teams, projects, cycles and people.
//!
//! Every query is a `const` so a test can check each one against Linear's
//! published schema shape without a network call, and so a reader sees the
//! exact document sent.

use super::Linear;
use anyhow::{Result, bail};
use serde_json::{Value, json};

macro_rules! issue_row {
    () => {
        "id identifier title priority priorityLabel url updatedAt \
         state { name type } assignee { name displayName } team { key }"
    };
}

pub const Q_VIEWER: &str =
    "query { viewer { id name displayName email organization { name urlKey } } }";

pub const Q_TEAMS: &str = "query { teams(first: 250) { nodes { \
    id key name description private issueCount cyclesEnabled } } }";

pub const Q_TEAM_BY_KEY: &str = "query($key: String!) { \
    teams(filter: { key: { eqIgnoreCase: $key } }, first: 1) { nodes { id key name } } }";

pub const Q_ISSUES: &str = concat!(
    "query($filter: IssueFilter, $first: Int) { \
     issues(filter: $filter, first: $first, orderBy: updatedAt) { nodes { ",
    issue_row!(),
    " } } }"
);

pub const Q_SEARCH: &str = concat!(
    "query($term: String!, $filter: IssueFilter, $first: Int) { \
     searchIssues(term: $term, filter: $filter, first: $first) { nodes { ",
    issue_row!(),
    " } } }"
);

pub const Q_ISSUE: &str = "query($id: String!) { issue(id: $id) { \
    id identifier title description url priority priorityLabel estimate dueDate \
    createdAt updatedAt \
    state { name type } assignee { name displayName email } creator { name displayName } \
    team { id key name } project { name } cycle { number name } \
    labels { nodes { id name } } parent { identifier title } \
    children(first: 50) { nodes { identifier title state { name } } } \
    comments(first: 100) { nodes { id body createdAt user { name displayName } } } \
    attachments(first: 50) { nodes { id title subtitle url sourceType createdAt \
    creator { name displayName } } } } }";

/// Just what an update needs to resolve names: the team and current labels.
pub const Q_ISSUE_CONTEXT: &str = "query($id: String!) { issue(id: $id) { \
    id identifier team { id key } labels { nodes { id name } } } }";

pub const Q_STATES: &str = "query($teamId: ID!) { \
    workflowStates(filter: { team: { id: { eq: $teamId } } }, first: 100) { \
    nodes { id name type position } } }";

pub const Q_LABELS_FOR_TEAM: &str = "query($teamId: ID!) { \
    issueLabels(first: 250, filter: { or: [ { team: { id: { eq: $teamId } } }, \
    { team: { null: true } } ] }) { nodes { id name isGroup team { key } } } }";

pub const Q_LABELS: &str =
    "query { issueLabels(first: 250) { nodes { id name isGroup team { key } } } }";

pub const Q_USERS: &str = "query($filter: UserFilter, $first: Int) { \
    users(filter: $filter, first: $first) { nodes { id name displayName email active } } }";

pub const Q_PROJECTS: &str = "query($filter: ProjectFilter, $first: Int) { \
    projects(filter: $filter, first: $first, orderBy: updatedAt) { nodes { \
    id name url progress startDate targetDate health status { name } lead { name displayName } \
    teams(first: 10) { nodes { key } } } } }";

pub const Q_CYCLES: &str = "query($filter: CycleFilter, $first: Int) { \
    cycles(filter: $filter, first: $first) { nodes { \
    id number name startsAt endsAt isActive isNext isPast progress team { key } } } }";

pub const Q_ACTIVE_CYCLE: &str =
    "query($id: String!) { team(id: $id) { activeCycle { id number } } }";

pub const M_CREATE: &str = "mutation($input: IssueCreateInput!) { \
    issueCreate(input: $input) { success issue { id identifier title url } } }";

pub const M_UPDATE: &str = "mutation($id: String!, $input: IssueUpdateInput!) { \
    issueUpdate(id: $id, input: $input) { success issue { identifier title url \
    state { name } assignee { displayName } priorityLabel } } }";

pub const M_COMMENT: &str = "mutation($input: CommentCreateInput!) { \
    commentCreate(input: $input) { success comment { id url } } }";

pub const M_FILE_UPLOAD: &str = "mutation($contentType: String!, $filename: String!, $size: Int!) { \
    fileUpload(contentType: $contentType, filename: $filename, size: $size) { success \
    uploadFile { uploadUrl assetUrl headers { key value } } } }";

pub const M_ATTACHMENT_CREATE: &str = "mutation($input: AttachmentCreateInput!) { \
    attachmentCreate(input: $input) { success attachment { id title url } } }";

pub const M_ATTACHMENT_LINK: &str = "mutation($issueId: String!, $url: String!, $title: String) { \
    attachmentLinkURL(issueId: $issueId, url: $url, title: $title) { success \
    attachment { id title url sourceType } } }";

pub const Q_ORGANIZATION: &str = "query { organization { id name urlKey userCount } \
    viewer { name displayName email } }";

/// The inbox. `Notification` is an interface; the issue and project kinds
/// carry what they are about.
pub const Q_NOTIFICATIONS: &str = "query($first: Int, $includeArchived: Boolean) { \
    notificationsUnreadCount \
    notifications(first: $first, includeArchived: $includeArchived, orderBy: createdAt) { nodes { \
    id type title subtitle url createdAt readAt archivedAt snoozedUntilAt \
    actor { name displayName } \
    ... on IssueNotification { issue { identifier title } comment { body } } \
    ... on ProjectNotification { project { name } } } } }";

pub const M_NOTIFICATION_UPDATE: &str = "mutation($id: String!, $input: NotificationUpdateInput!) { \
    notificationUpdate(id: $id, input: $input) { success } }";

pub const M_NOTIFICATION_ARCHIVE: &str =
    "mutation($id: String!) { notificationArchive(id: $id) { success } }";

pub const Q_COMMENTS: &str = "query($filter: CommentFilter, $first: Int) { \
    comments(filter: $filter, first: $first, orderBy: updatedAt) { nodes { \
    id body url createdAt updatedAt user { name displayName } issue { identifier title } } } }";

pub const Q_HISTORY: &str = "query($id: String!, $first: Int) { issue(id: $id) { identifier title \
    history(first: $first) { nodes { createdAt actor { name displayName } \
    fromState { name } toState { name } fromAssignee { name displayName } \
    toAssignee { name displayName } fromPriority toPriority fromTitle toTitle \
    addedLabels { name } removedLabels { name } fromProject { name } toProject { name } \
    fromCycle { number } toCycle { number } fromParent { identifier } toParent { identifier } \
    fromDueDate toDueDate fromEstimate toEstimate updatedDescription archived trashed \
    autoClosed autoArchived } } } }";

/// Every document above, for the schema test.
pub const ALL_DOCUMENTS: &[&str] = &[
    Q_VIEWER,
    Q_TEAMS,
    Q_TEAM_BY_KEY,
    Q_ISSUES,
    Q_SEARCH,
    Q_ISSUE,
    Q_ISSUE_CONTEXT,
    Q_STATES,
    Q_LABELS_FOR_TEAM,
    Q_LABELS,
    Q_USERS,
    Q_PROJECTS,
    Q_CYCLES,
    Q_ACTIVE_CYCLE,
    M_CREATE,
    M_UPDATE,
    M_COMMENT,
    Q_ORGANIZATION,
    Q_NOTIFICATIONS,
    M_NOTIFICATION_UPDATE,
    M_NOTIFICATION_ARCHIVE,
    Q_COMMENTS,
    Q_HISTORY,
    M_FILE_UPLOAD,
    M_ATTACHMENT_CREATE,
    M_ATTACHMENT_LINK,
];

fn s(v: &Value, ptr: &str) -> String {
    v.pointer(ptr)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

fn nodes<'a>(v: &'a Value, ptr: &str) -> Vec<&'a Value> {
    v.pointer(ptr)
        .and_then(|n| n.as_array())
        .map(|a| a.iter().collect())
        .unwrap_or_default()
}

/// A person's name as Linear's UI shows it: display name, else full name.
fn person(v: &Value, ptr: &str) -> String {
    let d = s(v, &format!("{ptr}/displayName"));
    if d.is_empty() {
        s(v, &format!("{ptr}/name"))
    } else {
        d
    }
}

// ---------------------------------------------------------------------------
// Viewer, teams, people
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Viewer {
    pub id: String,
    pub name: String,
    pub email: String,
    pub org: String,
    pub url_key: String,
}

pub async fn viewer(linear: &Linear) -> Result<Viewer> {
    let d = linear.query(Q_VIEWER, json!({})).await?;
    Ok(Viewer {
        id: s(&d, "/viewer/id"),
        name: person(&d, "/viewer"),
        email: s(&d, "/viewer/email"),
        org: s(&d, "/viewer/organization/name"),
        url_key: s(&d, "/viewer/organization/urlKey"),
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Team {
    pub id: String,
    pub key: String,
    pub name: String,
    pub description: String,
    pub private: bool,
    pub issue_count: u64,
    pub cycles_enabled: bool,
}

pub(crate) fn team_from(v: &Value) -> Team {
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    Team {
        id: s(v, "/id"),
        key: s(v, "/key"),
        name: s(v, "/name"),
        description: s(v, "/description"),
        private: b("private"),
        issue_count: v.get("issueCount").and_then(|x| x.as_u64()).unwrap_or(0),
        cycles_enabled: b("cyclesEnabled"),
    }
}

pub async fn teams(linear: &Linear) -> Result<Vec<Team>> {
    let d = linear.query(Q_TEAMS, json!({})).await?;
    Ok(nodes(&d, "/teams/nodes")
        .into_iter()
        .map(team_from)
        .collect())
}

/// A team by key (`ENG`). With no key, the only team, if there is just one.
pub async fn resolve_team(linear: &Linear, key: Option<&str>) -> Result<Team> {
    match key {
        Some(k) => {
            let d = linear.query(Q_TEAM_BY_KEY, json!({"key": k})).await?;
            if let Some(t) = nodes(&d, "/teams/nodes").first() {
                return Ok(team_from(t));
            }
            let all = teams(linear).await?;
            bail!(
                "no team with key {k}. Teams: {}",
                all.iter()
                    .map(|t| t.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        None => {
            let mut all = teams(linear).await?;
            if all.len() == 1 {
                return Ok(all.remove(0));
            }
            bail!(
                "this needs --team <KEY>; the workspace has several: {}",
                all.iter()
                    .map(|t| t.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub email: String,
    pub active: bool,
}

fn person_from(v: &Value) -> Person {
    Person {
        id: s(v, "/id"),
        name: s(v, "/name"),
        display_name: s(v, "/displayName"),
        email: s(v, "/email"),
        active: v.get("active").and_then(|a| a.as_bool()).unwrap_or(true),
    }
}

pub async fn users(linear: &Linear, limit: usize) -> Result<Vec<Person>> {
    let d = linear
        .query(Q_USERS, json!({"first": limit.clamp(1, 250)}))
        .await?;
    Ok(nodes(&d, "/users/nodes")
        .into_iter()
        .map(person_from)
        .collect())
}

/// The `UserFilter` for "who is this": `me`, an email, or a name.
pub(crate) fn user_filter(who: &str) -> Value {
    let w = who.trim().trim_start_matches('@');
    if w.eq_ignore_ascii_case("me") {
        json!({"isMe": {"eq": true}})
    } else if w.contains('@') {
        json!({"email": {"eqIgnoreCase": w}})
    } else {
        json!({"or": [
            {"name": {"eqIgnoreCase": w}},
            {"displayName": {"eqIgnoreCase": w}}
        ]})
    }
}

pub async fn resolve_user(linear: &Linear, who: &str) -> Result<Person> {
    if is_uuid(who) {
        return Ok(Person {
            id: who.to_string(),
            name: String::new(),
            display_name: String::new(),
            email: String::new(),
            active: true,
        });
    }
    let d = linear
        .query(Q_USERS, json!({"filter": user_filter(who), "first": 10}))
        .await?;
    let found: Vec<Person> = nodes(&d, "/users/nodes")
        .into_iter()
        .map(person_from)
        .collect();
    match found.as_slice() {
        [one] => Ok(one.clone()),
        [] => bail!("no Linear user matches {who}; `sidekar linear users` lists them"),
        many => bail!(
            "{who} matches several people: {}. Use their email.",
            many.iter()
                .map(|p| format!("{} <{}>", p.name, p.email))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

pub(crate) fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts.iter().map(|p| p.len()).eq([8, 4, 4, 4, 12])
        && parts
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
}

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRow {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub state_type: String,
    pub priority: String,
    pub assignee: String,
    pub team: String,
    pub updated: String,
    pub url: String,
}

pub(crate) fn issue_row_from(v: &Value) -> IssueRow {
    IssueRow {
        id: s(v, "/id"),
        identifier: s(v, "/identifier"),
        title: s(v, "/title"),
        state: s(v, "/state/name"),
        state_type: s(v, "/state/type"),
        priority: s(v, "/priorityLabel"),
        assignee: person(v, "/assignee"),
        team: s(v, "/team/key"),
        updated: s(v, "/updatedAt"),
        url: s(v, "/url"),
    }
}

/// What `linear issues` filters on. Every field is optional.
#[derive(Debug, Clone, Default)]
pub struct IssueQuery {
    pub text: Option<String>,
    pub team: Option<String>,
    pub state: Option<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub label: Option<String>,
    pub priority: Option<i64>,
    /// Include completed and canceled issues. Off by default: what is still
    /// open is nearly always the question.
    pub include_closed: bool,
    /// Only issues updated after this (`DateTimeOrDuration`: an ISO time or
    /// a duration such as `-P7D`).
    pub updated_since: Option<String>,
    pub limit: usize,
}

/// The state types that mean an issue is finished.
const CLOSED_TYPES: &[&str] = &["completed", "canceled"];

/// The `IssueFilter` for a query, or `None` when nothing narrows it.
pub(crate) fn issue_filter(q: &IssueQuery) -> Option<Value> {
    let mut f = serde_json::Map::new();
    if let Some(t) = &q.team {
        f.insert("team".into(), json!({"key": {"eqIgnoreCase": t}}));
    }
    match &q.state {
        // A state names either a workflow state ("In Review") or a type
        // ("started"), and teams name their states differently, so match both.
        Some(st) => {
            f.insert(
                "state".into(),
                json!({"or": [
                    {"name": {"eqIgnoreCase": st}},
                    {"type": {"eq": st.to_lowercase()}}
                ]}),
            );
        }
        None if !q.include_closed => {
            f.insert("state".into(), json!({"type": {"nin": CLOSED_TYPES}}));
        }
        None => {}
    }
    if let Some(a) = &q.assignee {
        let filter = if a.eq_ignore_ascii_case("none") || a.eq_ignore_ascii_case("unassigned") {
            json!({"null": true})
        } else {
            user_filter(a)
        };
        f.insert("assignee".into(), filter);
    }
    if let Some(p) = &q.project {
        f.insert("project".into(), json!({"name": {"containsIgnoreCase": p}}));
    }
    if let Some(l) = &q.label {
        f.insert(
            "labels".into(),
            json!({"some": {"name": {"eqIgnoreCase": l}}}),
        );
    }
    if let Some(p) = q.priority {
        f.insert("priority".into(), json!({"eq": p}));
    }
    if let Some(since) = &q.updated_since {
        f.insert("updatedAt".into(), json!({"gt": since}));
    }
    if f.is_empty() {
        None
    } else {
        Some(Value::Object(f))
    }
}

/// Issues matching a query: full-text search when there is text, otherwise
/// the most recently updated.
pub async fn issues(linear: &Linear, q: &IssueQuery) -> Result<Vec<IssueRow>> {
    let first = q.limit.clamp(1, 250);
    let filter = issue_filter(q);
    let (doc, vars, path) = match q.text.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(term) => (
            Q_SEARCH,
            json!({"term": term, "filter": filter, "first": first}),
            "/searchIssues/nodes",
        ),
        None => (
            Q_ISSUES,
            json!({"filter": filter, "first": first}),
            "/issues/nodes",
        ),
    };
    let d = linear.query(doc, vars).await?;
    Ok(nodes(&d, path).into_iter().map(issue_row_from).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    pub created: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IssueDetail {
    pub row: IssueRow,
    pub description: String,
    pub creator: String,
    pub team_name: String,
    pub project: String,
    pub cycle: String,
    pub labels: Vec<String>,
    pub due: String,
    pub estimate: Option<f64>,
    pub created: String,
    pub parent: Option<(String, String)>,
    pub children: Vec<(String, String, String)>,
    pub comments: Vec<Comment>,
    pub attachments: Vec<IssueAttachment>,
}

pub(crate) fn issue_detail_from(v: &Value) -> IssueDetail {
    let mut comments: Vec<Comment> = nodes(v, "/comments/nodes")
        .into_iter()
        .map(|c| Comment {
            author: {
                let p = person(c, "/user");
                if p.is_empty() {
                    "(integration)".into()
                } else {
                    p
                }
            },
            created: s(c, "/createdAt"),
            body: s(c, "/body"),
        })
        .collect();
    // Linear does not promise an order; a conversation reads oldest first.
    comments.sort_by(|a, b| a.created.cmp(&b.created));
    let cycle = match v.pointer("/cycle/number").and_then(|n| n.as_f64()) {
        Some(n) => {
            let name = s(v, "/cycle/name");
            if name.is_empty() {
                format!("{n}")
            } else {
                format!("{n} ({name})")
            }
        }
        None => String::new(),
    };
    IssueDetail {
        row: issue_row_from(v),
        description: s(v, "/description"),
        creator: person(v, "/creator"),
        team_name: s(v, "/team/name"),
        project: s(v, "/project/name"),
        cycle,
        labels: nodes(v, "/labels/nodes")
            .into_iter()
            .map(|l| s(l, "/name"))
            .collect(),
        due: s(v, "/dueDate"),
        estimate: v.get("estimate").and_then(|e| e.as_f64()),
        created: s(v, "/createdAt"),
        parent: v
            .get("parent")
            .filter(|p| !p.is_null())
            .map(|p| (s(p, "/identifier"), s(p, "/title"))),
        children: nodes(v, "/children/nodes")
            .into_iter()
            .map(|c| (s(c, "/identifier"), s(c, "/state/name"), s(c, "/title")))
            .collect(),
        comments,
        attachments: nodes(v, "/attachments/nodes")
            .into_iter()
            .map(attachment_from)
            .collect(),
    }
}

/// One issue, by identifier (`ENG-123`) or id, with its comments.
pub async fn issue(linear: &Linear, id: &str) -> Result<IssueDetail> {
    let d = linear.query(Q_ISSUE, json!({"id": id})).await?;
    match d.get("issue").filter(|i| !i.is_null()) {
        Some(i) => Ok(issue_detail_from(i)),
        None => bail!("no issue {id}"),
    }
}

/// An issue rendered for reading in a terminal.
pub fn render_issue(i: &IssueDetail) -> String {
    let mut out = format!("{}: {}\n", i.row.identifier, i.row.title);
    let mut meta = vec![
        format!("State: {}", i.row.state),
        format!("Priority: {}", i.row.priority),
        format!(
            "Assignee: {}",
            if i.row.assignee.is_empty() {
                "unassigned"
            } else {
                &i.row.assignee
            }
        ),
        format!("Team: {}", i.row.team),
    ];
    if !i.project.is_empty() {
        meta.push(format!("Project: {}", i.project));
    }
    if !i.cycle.is_empty() {
        meta.push(format!("Cycle: {}", i.cycle));
    }
    if !i.labels.is_empty() {
        meta.push(format!("Labels: {}", i.labels.join(", ")));
    }
    if !i.due.is_empty() {
        meta.push(format!("Due: {}", i.due));
    }
    if let Some(e) = i.estimate {
        meta.push(format!("Estimate: {e}"));
    }
    out.push_str(&meta.join(" · "));
    out.push('\n');
    if !i.creator.is_empty() {
        out.push_str(&format!("Created by {} at {}\n", i.creator, i.created));
    }
    out.push_str(&format!("{}\n", i.row.url));
    if let Some((pid, ptitle)) = &i.parent {
        out.push_str(&format!("Parent: {pid} {ptitle}\n"));
    }
    out.push('\n');
    if i.description.trim().is_empty() {
        out.push_str("(no description)\n");
    } else {
        out.push_str(i.description.trim_end());
        out.push('\n');
    }
    if !i.children.is_empty() {
        out.push_str(&format!("\nSub-issues ({}):\n", i.children.len()));
        for (id, state, title) in &i.children {
            out.push_str(&format!("  {id}\t{state}\t{title}\n"));
        }
    }
    if !i.attachments.is_empty() {
        out.push_str(&format!("\nAttachments ({}):\n", i.attachments.len()));
        for a in &i.attachments {
            out.push_str(&format!("  {}\n", a.line()));
        }
    }
    let files = embedded_files(i);
    if !files.is_empty() {
        out.push_str(&format!(
            "\nUploaded files in the text ({}) — `sidekar linear download {}`:\n",
            files.len(),
            i.row.identifier
        ));
        for f in &files {
            out.push_str(&format!("  {}\t{}\t{}\n", f.name, f.place, f.url));
        }
    }
    out.push_str(&format!("\nComments ({}):\n", i.comments.len()));
    for c in &i.comments {
        out.push_str(&format!(
            "--- {} · {}\n{}\n",
            c.author,
            c.created,
            c.body.trim_end()
        ));
    }
    out.trim_end().to_string()
}

/// Linear priorities, by number or by the names its UI uses.
pub fn parse_priority(p: &str) -> Result<i64> {
    Ok(match p.trim().to_lowercase().as_str() {
        "0" | "none" | "no" => 0,
        "1" | "urgent" => 1,
        "2" | "high" => 2,
        "3" | "medium" | "normal" => 3,
        "4" | "low" => 4,
        other => bail!("unknown priority {other}; use urgent, high, medium, low, none or 0-4"),
    })
}

#[derive(Debug, Clone)]
pub struct WorkflowState {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub position: f64,
}

pub async fn states(linear: &Linear, team_id: &str) -> Result<Vec<WorkflowState>> {
    let d = linear.query(Q_STATES, json!({"teamId": team_id})).await?;
    let mut out: Vec<WorkflowState> = nodes(&d, "/workflowStates/nodes")
        .into_iter()
        .map(|n| WorkflowState {
            id: s(n, "/id"),
            name: s(n, "/name"),
            kind: s(n, "/type"),
            position: n.get("position").and_then(|p| p.as_f64()).unwrap_or(0.0),
        })
        .collect();
    out.sort_by(|a, b| {
        state_order(&a.kind)
            .cmp(&state_order(&b.kind))
            .then(a.position.total_cmp(&b.position))
    });
    Ok(out)
}

/// The order Linear's board shows state types in.
fn state_order(kind: &str) -> u8 {
    match kind {
        "triage" => 0,
        "backlog" => 1,
        "unstarted" => 2,
        "started" => 3,
        "completed" => 4,
        "canceled" => 5,
        _ => 6,
    }
}

/// A state by name, or by type when exactly one state has it ("started").
pub(crate) fn pick_state<'a>(all: &'a [WorkflowState], wanted: &str) -> Result<&'a WorkflowState> {
    if let Some(st) = all.iter().find(|s| s.name.eq_ignore_ascii_case(wanted)) {
        return Ok(st);
    }
    let by_type: Vec<&WorkflowState> = all
        .iter()
        .filter(|s| s.kind.eq_ignore_ascii_case(wanted))
        .collect();
    if by_type.len() == 1 {
        return Ok(by_type[0]);
    }
    bail!(
        "no state {wanted} in this team. States: {}",
        all.iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

#[derive(Debug, Clone)]
pub struct Label {
    pub id: String,
    pub name: String,
    /// Empty for a workspace label.
    pub team: String,
    pub is_group: bool,
}

pub async fn labels(linear: &Linear, team_id: Option<&str>) -> Result<Vec<Label>> {
    let d = match team_id {
        Some(t) => {
            linear
                .query(Q_LABELS_FOR_TEAM, json!({"teamId": t}))
                .await?
        }
        None => linear.query(Q_LABELS, json!({})).await?,
    };
    Ok(nodes(&d, "/issueLabels/nodes")
        .into_iter()
        .map(|n| Label {
            id: s(n, "/id"),
            name: s(n, "/name"),
            team: s(n, "/team/key"),
            is_group: n.get("isGroup").and_then(|g| g.as_bool()).unwrap_or(false),
        })
        .collect())
}

/// Label ids for names. A team label wins over a workspace label of the same
/// name, because that is the one the team's UI offers.
pub(crate) fn pick_labels(all: &[Label], names: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for n in names {
        let mut found: Vec<&Label> = all
            .iter()
            .filter(|l| !l.is_group && l.name.eq_ignore_ascii_case(n.trim()))
            .collect();
        found.sort_by_key(|l| l.team.is_empty());
        match found.first() {
            Some(l) => out.push(l.id.clone()),
            None => bail!(
                "no label {n}. Labels: {}",
                all.iter()
                    .filter(|l| !l.is_group)
                    .map(|l| l.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub status: String,
    pub progress: f64,
    pub start: String,
    pub target: String,
    pub health: String,
    pub lead: String,
    pub teams: Vec<String>,
    pub url: String,
}

pub(crate) fn project_from(n: &Value) -> Project {
    Project {
        id: s(n, "/id"),
        name: s(n, "/name"),
        status: s(n, "/status/name"),
        progress: n.get("progress").and_then(|p| p.as_f64()).unwrap_or(0.0),
        start: s(n, "/startDate"),
        target: s(n, "/targetDate"),
        health: s(n, "/health"),
        lead: person(n, "/lead"),
        teams: nodes(n, "/teams/nodes")
            .into_iter()
            .map(|t| s(t, "/key"))
            .collect(),
        url: s(n, "/url"),
    }
}

pub async fn projects(
    linear: &Linear,
    team: Option<&str>,
    name: Option<&str>,
    limit: usize,
) -> Result<Vec<Project>> {
    let mut f = serde_json::Map::new();
    if let Some(t) = team {
        f.insert(
            "accessibleTeams".into(),
            json!({"some": {"key": {"eqIgnoreCase": t}}}),
        );
    }
    if let Some(n) = name {
        f.insert("name".into(), json!({"containsIgnoreCase": n}));
    }
    let filter = if f.is_empty() {
        Value::Null
    } else {
        Value::Object(f)
    };
    let d = linear
        .query(
            Q_PROJECTS,
            json!({"filter": filter, "first": limit.clamp(1, 250)}),
        )
        .await?;
    Ok(nodes(&d, "/projects/nodes")
        .into_iter()
        .map(project_from)
        .collect())
}

/// A project id from a name: exact match first, else a single partial one.
pub async fn resolve_project(linear: &Linear, name: &str) -> Result<String> {
    if is_uuid(name) {
        return Ok(name.to_string());
    }
    let found = projects(linear, None, Some(name), 25).await?;
    if let Some(p) = found.iter().find(|p| p.name.eq_ignore_ascii_case(name)) {
        return Ok(p.id.clone());
    }
    match found.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => bail!("no project matches {name}; `sidekar linear projects` lists them"),
        many => bail!(
            "{name} matches several projects: {}",
            many.iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        ),
    }
}

#[derive(Debug, Clone)]
pub struct Cycle {
    pub id: String,
    pub number: i64,
    pub name: String,
    pub starts: String,
    pub ends: String,
    pub status: &'static str,
    pub progress: f64,
    pub team: String,
}

pub async fn cycles(
    linear: &Linear,
    team: Option<&str>,
    include_past: bool,
    limit: usize,
) -> Result<Vec<Cycle>> {
    let mut f = serde_json::Map::new();
    if let Some(t) = team {
        f.insert("team".into(), json!({"key": {"eqIgnoreCase": t}}));
    }
    if !include_past {
        // Current, upcoming, and the one just finished, which is the one people
        // still ask about.
        f.insert(
            "or".into(),
            json!([{"isPast": {"eq": false}}, {"isPrevious": {"eq": true}}]),
        );
    }
    let filter = if f.is_empty() {
        Value::Null
    } else {
        Value::Object(f)
    };
    let d = linear
        .query(Q_CYCLES, json!({"filter": filter, "first": 250}))
        .await?;
    let mut out: Vec<Cycle> = nodes(&d, "/cycles/nodes")
        .into_iter()
        .map(cycle_from)
        .collect();
    out.sort_by(|a, b| a.team.cmp(&b.team).then(b.number.cmp(&a.number)));
    out.truncate(limit);
    Ok(out)
}

pub(crate) fn cycle_from(n: &Value) -> Cycle {
    let flag = |k: &str| n.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    Cycle {
        id: s(n, "/id"),
        number: n.get("number").and_then(|x| x.as_f64()).unwrap_or(0.0) as i64,
        name: s(n, "/name"),
        starts: s(n, "/startsAt"),
        ends: s(n, "/endsAt"),
        status: if flag("isActive") {
            "current"
        } else if flag("isNext") {
            "next"
        } else if flag("isPast") {
            "past"
        } else {
            "upcoming"
        },
        progress: n.get("progress").and_then(|x| x.as_f64()).unwrap_or(0.0),
        team: s(n, "/team/key"),
    }
}

/// A cycle id for `--cycle`: `current`, `next`, or a cycle number.
pub async fn resolve_cycle(linear: &Linear, team: &Team, wanted: &str) -> Result<String> {
    if is_uuid(wanted) {
        return Ok(wanted.to_string());
    }
    if wanted.eq_ignore_ascii_case("current") || wanted.eq_ignore_ascii_case("active") {
        let d = linear.query(Q_ACTIVE_CYCLE, json!({"id": team.id})).await?;
        let id = s(&d, "/team/activeCycle/id");
        if id.is_empty() {
            bail!("{} has no active cycle", team.key);
        }
        return Ok(id);
    }
    let all = cycles(linear, Some(&team.key), true, usize::MAX).await?;
    let found = if wanted.eq_ignore_ascii_case("next") {
        all.iter().find(|c| c.status == "next")
    } else {
        let n: i64 = wanted
            .parse()
            .map_err(|_| anyhow::anyhow!("--cycle takes current, next, or a cycle number"))?;
        all.iter().find(|c| c.number == n)
    };
    found
        .map(|c| c.id.clone())
        .ok_or_else(|| anyhow::anyhow!("{} has no cycle {wanted}", team.key))
}

/// The team and current labels of an issue, for resolving names in an update.
pub struct IssueContext {
    pub id: String,
    pub identifier: String,
    pub team: Team,
}

pub async fn issue_context(linear: &Linear, id: &str) -> Result<IssueContext> {
    let d = linear.query(Q_ISSUE_CONTEXT, json!({"id": id})).await?;
    let i = d
        .get("issue")
        .filter(|i| !i.is_null())
        .ok_or_else(|| anyhow::anyhow!("no issue {id}"))?;
    Ok(IssueContext {
        id: s(i, "/id"),
        identifier: s(i, "/identifier"),
        team: Team {
            id: s(i, "/team/id"),
            key: s(i, "/team/key"),
            ..Default::default()
        },
    })
}

/// Changes to apply in `issueCreate` or `issueUpdate`, by name. Resolved to
/// ids against one team by [`resolve_changes`].
#[derive(Debug, Clone, Default)]
pub struct Changes {
    pub title: Option<String>,
    pub description: Option<String>,
    pub state: Option<String>,
    pub assignee: Option<String>,
    pub unassign: bool,
    pub priority: Option<i64>,
    /// Replace every label with these.
    pub labels: Option<Vec<String>>,
    pub add_labels: Vec<String>,
    pub remove_labels: Vec<String>,
    pub project: Option<String>,
    pub cycle: Option<String>,
    pub parent: Option<String>,
    pub due: Option<String>,
    pub estimate: Option<i64>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.description.is_none()
            && self.state.is_none()
            && self.assignee.is_none()
            && !self.unassign
            && self.priority.is_none()
            && self.labels.is_none()
            && self.add_labels.is_empty()
            && self.remove_labels.is_empty()
            && self.project.is_none()
            && self.cycle.is_none()
            && self.parent.is_none()
            && self.due.is_none()
            && self.estimate.is_none()
    }
}

/// Turn names into the ids Linear's inputs take. Only looks up what changed,
/// so a title edit costs no extra queries.
pub async fn resolve_changes(linear: &Linear, team: &Team, c: &Changes) -> Result<Value> {
    let mut input = serde_json::Map::new();
    if let Some(t) = &c.title {
        input.insert("title".into(), json!(t));
    }
    if let Some(d) = &c.description {
        input.insert("description".into(), json!(d));
    }
    if let Some(p) = c.priority {
        input.insert("priority".into(), json!(p));
    }
    if let Some(d) = &c.due {
        input.insert("dueDate".into(), json!(d));
    }
    if let Some(e) = c.estimate {
        input.insert("estimate".into(), json!(e));
    }
    if let Some(st) = &c.state {
        let all = states(linear, &team.id).await?;
        input.insert("stateId".into(), json!(pick_state(&all, st)?.id));
    }
    if c.unassign {
        input.insert("assigneeId".into(), Value::Null);
    } else if let Some(a) = &c.assignee {
        input.insert(
            "assigneeId".into(),
            json!(resolve_user(linear, a).await?.id),
        );
    }
    if c.labels.is_some() || !c.add_labels.is_empty() || !c.remove_labels.is_empty() {
        let all = labels(linear, Some(&team.id)).await?;
        if let Some(ls) = &c.labels {
            input.insert("labelIds".into(), json!(pick_labels(&all, ls)?));
        }
        if !c.add_labels.is_empty() {
            input.insert(
                "addedLabelIds".into(),
                json!(pick_labels(&all, &c.add_labels)?),
            );
        }
        if !c.remove_labels.is_empty() {
            input.insert(
                "removedLabelIds".into(),
                json!(pick_labels(&all, &c.remove_labels)?),
            );
        }
    }
    if let Some(p) = &c.project {
        let id = if p.eq_ignore_ascii_case("none") {
            Value::Null
        } else {
            json!(resolve_project(linear, p).await?)
        };
        input.insert("projectId".into(), id);
    }
    if let Some(cy) = &c.cycle {
        let id = if cy.eq_ignore_ascii_case("none") {
            Value::Null
        } else {
            json!(resolve_cycle(linear, team, cy).await?)
        };
        input.insert("cycleId".into(), id);
    }
    if let Some(p) = &c.parent {
        let id = if p.eq_ignore_ascii_case("none") {
            Value::Null
        } else {
            json!(issue_context(linear, p).await?.id)
        };
        input.insert("parentId".into(), id);
    }
    Ok(Value::Object(input))
}

/// Create an issue. Returns its identifier and URL.
pub async fn create(linear: &Linear, team: &Team, c: &Changes) -> Result<(String, String)> {
    if c.title.as_deref().is_none_or(|t| t.trim().is_empty()) {
        bail!("an issue needs --title");
    }
    let mut input = resolve_changes(linear, team, c).await?;
    input["teamId"] = json!(team.id);
    let d = linear.query(M_CREATE, json!({"input": input})).await?;
    mutation_result(&d, "issueCreate")
}

/// Update an issue. Returns its identifier and URL.
pub async fn update(linear: &Linear, id: &str, c: &Changes) -> Result<(String, String)> {
    let ctx = issue_context(linear, id).await?;
    let input = resolve_changes(linear, &ctx.team, c).await?;
    let d = linear
        .query(M_UPDATE, json!({"id": ctx.id, "input": input}))
        .await?;
    mutation_result(&d, "issueUpdate")
}

fn mutation_result(d: &Value, field: &str) -> Result<(String, String)> {
    let ok = d
        .pointer(&format!("/{field}/success"))
        .and_then(|s| s.as_bool())
        .unwrap_or(false);
    if !ok {
        bail!("Linear reported {field} as unsuccessful");
    }
    Ok((
        s(d, &format!("/{field}/issue/identifier")),
        s(d, &format!("/{field}/issue/url")),
    ))
}

/// Comment on an issue. Returns the comment's URL.
pub async fn comment(linear: &Linear, id: &str, body: &str) -> Result<String> {
    let ctx = issue_context(linear, id).await?;
    let d = linear
        .query(
            M_COMMENT,
            json!({"input": {"issueId": ctx.id, "body": body}}),
        )
        .await?;
    if d.pointer("/commentCreate/success")
        .and_then(|s| s.as_bool())
        != Some(true)
    {
        bail!("Linear reported commentCreate as unsuccessful");
    }
    Ok(s(&d, "/commentCreate/comment/url"))
}

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Organization {
    pub id: String,
    pub name: String,
    pub url_key: String,
    pub users: u64,
    pub viewer: String,
    pub viewer_email: String,
}

pub(crate) fn organization_from(d: &Value) -> Organization {
    Organization {
        id: s(d, "/organization/id"),
        name: s(d, "/organization/name"),
        url_key: s(d, "/organization/urlKey"),
        users: d
            .pointer("/organization/userCount")
            .and_then(|u| u.as_u64())
            .unwrap_or(0),
        viewer: person(d, "/viewer"),
        viewer_email: s(d, "/viewer/email"),
    }
}

/// The workspace a token belongs to, and who it acts as there.
pub async fn organization(linear: &Linear) -> Result<Organization> {
    Ok(organization_from(
        &linear.query(Q_ORGANIZATION, json!({})).await?,
    ))
}

// ---------------------------------------------------------------------------
// Inbox
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notification {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub subtitle: String,
    pub url: String,
    pub created: String,
    pub read: bool,
    pub archived: bool,
    pub snoozed: bool,
    pub actor: String,
    /// `ENG-123` for an issue notification.
    pub issue: String,
    pub issue_title: String,
    pub project: String,
    /// The first line of the comment, when there is one.
    pub comment: String,
}

fn present(v: &Value, ptr: &str) -> bool {
    v.pointer(ptr).is_some_and(|x| !x.is_null())
}

pub(crate) fn notification_from(v: &Value) -> Notification {
    Notification {
        id: s(v, "/id"),
        kind: s(v, "/type"),
        title: s(v, "/title"),
        subtitle: s(v, "/subtitle"),
        url: s(v, "/url"),
        created: s(v, "/createdAt"),
        read: present(v, "/readAt"),
        archived: present(v, "/archivedAt"),
        snoozed: present(v, "/snoozedUntilAt"),
        actor: person(v, "/actor"),
        issue: s(v, "/issue/identifier"),
        issue_title: s(v, "/issue/title"),
        project: s(v, "/project/name"),
        comment: first_line(&s(v, "/comment/body"), 140),
    }
}

/// A single line of at most `max` characters.
pub(crate) fn first_line(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_string()
    }
}

/// The inbox, newest first, and the unread count Linear reports for it.
pub async fn notifications(
    linear: &Linear,
    unread_only: bool,
    include_archived: bool,
    limit: usize,
) -> Result<(u64, Vec<Notification>)> {
    let d = linear
        .query(
            Q_NOTIFICATIONS,
            json!({"first": if unread_only { 250 } else { limit.clamp(1, 250) },
                   "includeArchived": include_archived}),
        )
        .await?;
    let unread = d
        .get("notificationsUnreadCount")
        .and_then(|u| u.as_u64())
        .unwrap_or(0);
    let mut found: Vec<Notification> = nodes(&d, "/notifications/nodes")
        .into_iter()
        .map(notification_from)
        .filter(|n| !unread_only || !n.read)
        .collect();
    // ISO-8601 UTC sorts as text.
    found.sort_by(|a, b| b.created.cmp(&a.created));
    found.truncate(limit.max(1));
    Ok((unread, found))
}

/// Now as the ISO-8601 instant Linear's `DateTime` takes.
pub(crate) fn iso_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    crate::utils::epoch_to_date(secs)
        .replace(" UTC", "Z")
        .replacen(' ', "T", 1)
}

/// Mark a notification read (or unread again).
pub async fn mark_notification(linear: &Linear, id: &str, read: bool) -> Result<()> {
    let read_at = if read { json!(iso_now()) } else { Value::Null };
    let d = linear
        .query(
            M_NOTIFICATION_UPDATE,
            json!({"id": id, "input": {"readAt": read_at}}),
        )
        .await?;
    if d.pointer("/notificationUpdate/success") != Some(&json!(true)) {
        bail!("Linear did not update notification {id}");
    }
    Ok(())
}

pub async fn archive_notification(linear: &Linear, id: &str) -> Result<()> {
    let d = linear
        .query(M_NOTIFICATION_ARCHIVE, json!({"id": id}))
        .await?;
    if d.pointer("/notificationArchive/success") != Some(&json!(true)) {
        bail!("Linear did not archive notification {id}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Recent activity
// ---------------------------------------------------------------------------

/// `7d`, `2w`, `12h`, `30m` as the ISO-8601 duration Linear's date filters
/// read as "that long ago"; an ISO date or time passes through.
pub(crate) fn since(text: &str) -> Result<String> {
    let t = text.trim();
    if t.starts_with("-P")
        || (t.chars().next().is_some_and(|c| c.is_ascii_digit()) && t.contains('-'))
    {
        return Ok(t.to_string());
    }
    let (num, unit) = t.split_at(t.find(|c: char| !c.is_ascii_digit()).unwrap_or(t.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("--since takes 7d, 2w, 12h, 30m, or a date; got {text}"))?;
    Ok(match unit {
        "d" | "day" | "days" => format!("-P{n}D"),
        "w" | "week" | "weeks" => format!("-P{n}W"),
        "h" | "hour" | "hours" => format!("-PT{n}H"),
        "m" | "min" | "mins" => format!("-PT{n}M"),
        _ => bail!("--since takes 7d, 2w, 12h, 30m, or a date; got {text}"),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentComment {
    pub author: String,
    pub created: String,
    pub updated: String,
    pub issue: String,
    pub issue_title: String,
    pub body: String,
    pub url: String,
}

pub(crate) fn recent_comment_from(v: &Value) -> RecentComment {
    RecentComment {
        author: person(v, "/user"),
        created: s(v, "/createdAt"),
        updated: s(v, "/updatedAt"),
        issue: s(v, "/issue/identifier"),
        issue_title: s(v, "/issue/title"),
        body: first_line(&s(v, "/body"), 160),
        url: s(v, "/url"),
    }
}

/// The `CommentFilter` for issue comments made since `since`, optionally
/// within a team or project.
pub(crate) fn comment_filter(team: Option<&str>, project: Option<&str>, since: &str) -> Value {
    let mut issue = serde_json::Map::new();
    if let Some(t) = team {
        issue.insert("team".into(), json!({"key": {"eqIgnoreCase": t}}));
    }
    if let Some(p) = project {
        issue.insert("project".into(), json!({"name": {"containsIgnoreCase": p}}));
    }
    let mut f = json!({"createdAt": {"gt": since}});
    f["issue"] = if issue.is_empty() {
        json!({"null": false})
    } else {
        Value::Object(issue)
    };
    f
}

pub struct Activity {
    pub issues: Vec<IssueRow>,
    pub comments: Vec<RecentComment>,
}

/// What moved lately: issues updated (any state) and comments made since
/// `since`, across the workspace or within a team or project.
pub async fn activity(
    linear: &Linear,
    team: Option<&str>,
    project: Option<&str>,
    since: &str,
    limit: usize,
) -> Result<Activity> {
    let q = IssueQuery {
        team: team.map(String::from),
        project: project.map(String::from),
        include_closed: true,
        updated_since: Some(since.to_string()),
        limit,
        ..Default::default()
    };
    let issues = self::issues(linear, &q).await?;
    let d = linear
        .query(
            Q_COMMENTS,
            json!({"filter": comment_filter(team, project, since), "first": limit.clamp(1, 250)}),
        )
        .await?;
    let comments = nodes(&d, "/comments/nodes")
        .into_iter()
        .map(recent_comment_from)
        .collect();
    Ok(Activity { issues, comments })
}

/// One line per change in an issue history entry, e.g.
/// `state Todo → In Progress; assignee alice → bob`.
pub(crate) fn describe_history(h: &Value) -> String {
    let mut parts = Vec::new();
    let mut pair = |what: &str, from: String, to: String| {
        if !from.is_empty() || !to.is_empty() {
            let dash = |x: String| if x.is_empty() { "none".to_string() } else { x };
            parts.push(format!("{what} {} → {}", dash(from), dash(to)));
        }
    };
    pair("state", s(h, "/fromState/name"), s(h, "/toState/name"));
    pair(
        "assignee",
        person(h, "/fromAssignee"),
        person(h, "/toAssignee"),
    );
    let prio = |p: &str| {
        h.get(p)
            .and_then(|x| x.as_f64())
            .map(|n| priority_name(n as i64).to_string())
            .unwrap_or_default()
    };
    pair("priority", prio("fromPriority"), prio("toPriority"));
    pair("title", s(h, "/fromTitle"), s(h, "/toTitle"));
    pair(
        "project",
        s(h, "/fromProject/name"),
        s(h, "/toProject/name"),
    );
    let cycle = |p: &str| {
        h.pointer(p)
            .and_then(|x| x.as_i64())
            .map(|n| n.to_string())
            .unwrap_or_default()
    };
    pair(
        "cycle",
        cycle("/fromCycle/number"),
        cycle("/toCycle/number"),
    );
    pair(
        "parent",
        s(h, "/fromParent/identifier"),
        s(h, "/toParent/identifier"),
    );
    pair("due", s(h, "/fromDueDate"), s(h, "/toDueDate"));
    let est = |p: &str| {
        h.get(p)
            .and_then(|x| x.as_f64())
            .map(|n| n.to_string())
            .unwrap_or_default()
    };
    pair("estimate", est("fromEstimate"), est("toEstimate"));
    let names = |p: &str| {
        h.get(p)
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .map(|l| s(l, "/name"))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    let added = names("addedLabels");
    if !added.is_empty() {
        parts.push(format!("+labels {added}"));
    }
    let removed = names("removedLabels");
    if !removed.is_empty() {
        parts.push(format!("-labels {removed}"));
    }
    let flag = |k: &str| h.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    if flag("updatedDescription") {
        parts.push("edited description".into());
    }
    if flag("autoClosed") {
        parts.push("auto-closed".into());
    }
    if flag("autoArchived") {
        parts.push("auto-archived".into());
    } else if flag("archived") {
        parts.push("archived".into());
    }
    if flag("trashed") {
        parts.push("deleted".into());
    }
    if parts.is_empty() {
        "updated".into()
    } else {
        parts.join("; ")
    }
}

/// Linear's names for priority numbers.
pub(crate) fn priority_name(p: i64) -> &'static str {
    match p {
        1 => "urgent",
        2 => "high",
        3 => "medium",
        4 => "low",
        _ => "none",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub at: String,
    pub actor: String,
    pub change: String,
}

/// An issue's change log, oldest first.
pub async fn history(
    linear: &Linear,
    id: &str,
    limit: usize,
) -> Result<(String, Vec<HistoryEntry>)> {
    let d = linear
        .query(Q_HISTORY, json!({"id": id, "first": limit.clamp(1, 250)}))
        .await?;
    let title = format!("{} {}", s(&d, "/issue/identifier"), s(&d, "/issue/title"));
    let mut entries: Vec<HistoryEntry> = nodes(&d, "/issue/history/nodes")
        .into_iter()
        .map(|h| HistoryEntry {
            at: s(h, "/createdAt"),
            actor: person(h, "/actor"),
            change: describe_history(h),
        })
        .collect();
    entries.sort_by(|a, b| a.at.cmp(&b.at));
    Ok((title, entries))
}

// ---------------------------------------------------------------------------
// Attachments and uploaded files
// ---------------------------------------------------------------------------

/// Where Linear keeps uploaded files. Fetching one needs the API credential.
pub const UPLOADS_HOST: &str = "uploads.linear.app";

/// An entry in an issue's attachments list: an uploaded file, or a link
/// (GitHub PR, Slack thread, any URL).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueAttachment {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub url: String,
    pub source: String,
    pub created: String,
    pub creator: String,
}

impl IssueAttachment {
    pub fn is_upload(&self) -> bool {
        crate::attachments::host_of(&self.url).as_deref() == Some(UPLOADS_HOST)
    }

    pub fn line(&self) -> String {
        let mut parts = vec![if self.title.is_empty() {
            "(untitled)".to_string()
        } else {
            self.title.clone()
        }];
        if !self.subtitle.is_empty() {
            parts.push(self.subtitle.clone());
        }
        parts.push(if self.is_upload() {
            "file".into()
        } else if self.source.is_empty() {
            "link".into()
        } else {
            self.source.clone()
        });
        parts.push(self.url.clone());
        parts.join("\t")
    }
}

pub(crate) fn attachment_from(v: &Value) -> IssueAttachment {
    IssueAttachment {
        id: s(v, "/id"),
        title: s(v, "/title"),
        subtitle: s(v, "/subtitle"),
        url: s(v, "/url"),
        source: s(v, "/sourceType"),
        created: s(v, "/createdAt"),
        creator: person(v, "/creator"),
    }
}

/// A file uploaded into an issue's description or a comment, found by its
/// `uploads.linear.app` link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddedFile {
    pub name: String,
    pub url: String,
    /// `description`, `comment by ann 2026-10-10`, or `attachment`.
    pub place: String,
}

/// Every `https://uploads.linear.app/…` link in markdown, with the link text
/// as its name (`![shot.png](url)` → `shot.png`), else the URL's last
/// segment. In order, without repeats.
pub(crate) fn uploads_in(markdown: &str) -> Vec<(String, String)> {
    let prefix = format!("https://{UPLOADS_HOST}/");
    let mut out: Vec<(String, String)> = Vec::new();
    let mut from = 0;
    while let Some(i) = markdown[from..].find(&prefix) {
        let start = from + i;
        let end = markdown[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '>' | '"' | '\'' | '<'))
            .map(|e| start + e)
            .unwrap_or(markdown.len());
        let url = &markdown[start..end];
        let before = &markdown[..start];
        let label = before
            .strip_suffix("](")
            .and_then(|b| b.rfind('[').map(|o| b[o + 1..].to_string()))
            .filter(|l| !l.trim().is_empty() && !l.contains('\n'));
        let name = label.unwrap_or_else(|| {
            let last = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("file");
            urlencoding::decode(last)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| last.to_string())
        });
        if !out.iter().any(|(_, u)| u == url) {
            out.push((name, url.to_string()));
        }
        from = end.max(start + prefix.len());
    }
    out
}

/// Files uploaded into the description and comments of an issue.
pub fn embedded_files(i: &IssueDetail) -> Vec<EmbeddedFile> {
    let mut out: Vec<EmbeddedFile> = Vec::new();
    let mut push = |text: &str, place: String| {
        for (name, url) in uploads_in(text) {
            if !out.iter().any(|f| f.url == url) {
                out.push(EmbeddedFile {
                    name,
                    url,
                    place: place.clone(),
                });
            }
        }
    };
    push(&i.description, "description".into());
    for c in &i.comments {
        push(
            &c.body,
            format!(
                "comment by {} {}",
                c.author,
                c.created.get(..10).unwrap_or(&c.created)
            ),
        );
    }
    out
}

/// Every file on an issue that can be downloaded: uploads in the text, and
/// attachments that are Linear uploads.
pub fn downloadable_files(i: &IssueDetail) -> Vec<EmbeddedFile> {
    let mut out = embedded_files(i);
    for a in i.attachments.iter().filter(|a| a.is_upload()) {
        if !out.iter().any(|f| f.url == a.url) {
            out.push(EmbeddedFile {
                name: if a.title.is_empty() {
                    a.url.rsplit('/').next().unwrap_or("file").to_string()
                } else {
                    a.title.clone()
                },
                url: a.url.clone(),
                place: "attachment".into(),
            });
        }
    }
    out
}

/// Upload a file to Linear's storage and return its asset URL: `fileUpload`
/// for a signed URL, then the bytes PUT to it.
pub async fn upload_file(
    linear: &Linear,
    name: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> Result<String> {
    let size = i64::try_from(bytes.len())
        .ok()
        .filter(|n| *n <= i64::from(i32::MAX))
        .ok_or_else(|| anyhow::anyhow!("{name} is too large for Linear (2GB limit)"))?;
    let d = linear
        .query(
            M_FILE_UPLOAD,
            json!({"contentType": content_type, "filename": name, "size": size}),
        )
        .await?;
    if d.pointer("/fileUpload/success") != Some(&json!(true)) {
        bail!("Linear refused to start the upload of {name}");
    }
    let upload_url = s(&d, "/fileUpload/uploadFile/uploadUrl");
    let asset_url = s(&d, "/fileUpload/uploadFile/assetUrl");
    if upload_url.is_empty() || asset_url.is_empty() {
        bail!("Linear gave no upload URL for {name}");
    }
    let headers: Vec<(String, String)> = nodes(&d, "/fileUpload/uploadFile/headers")
        .into_iter()
        .map(|h| (s(h, "/key"), s(h, "/value")))
        .filter(|(k, _)| !k.is_empty())
        .collect();
    linear
        .put_signed(&upload_url, &headers, content_type, bytes)
        .await?;
    Ok(asset_url)
}

/// Markdown that shows an uploaded file the way Linear's editor does: images
/// inline, everything else as a link.
pub(crate) fn markdown_for(name: &str, content_type: &str, url: &str) -> String {
    let label = name.replace(['[', ']'], "");
    if content_type.starts_with("image/") {
        format!("![{label}]({url})")
    } else {
        format!("[{label}]({url})")
    }
}

/// Upload a local file and attach it to an issue (it shows under the
/// issue's attachments). Returns the attachment's URL.
pub async fn attach_file(
    linear: &Linear,
    issue: &str,
    path: &str,
    title: Option<&str>,
) -> Result<String> {
    let ctx = issue_context(linear, issue).await?;
    let (bytes, name, mime) = crate::attachments::read_upload(path)?;
    let size = crate::attachments::human_bytes(bytes.len() as u64);
    let asset = upload_file(linear, &name, &mime, bytes).await?;
    let d = linear
        .query(
            M_ATTACHMENT_CREATE,
            json!({"input": {
                "issueId": ctx.id,
                "title": title.unwrap_or(&name),
                "subtitle": format!("{name} · {size}"),
                "url": asset,
            }}),
        )
        .await?;
    if d.pointer("/attachmentCreate/success") != Some(&json!(true)) {
        bail!("Linear reported attachmentCreate as unsuccessful");
    }
    Ok(s(&d, "/attachmentCreate/attachment/url"))
}

/// Attach a URL to an issue. Linear recognizes GitHub, Slack, Figma and the
/// like and shows them richly; anything else is a plain link.
pub async fn link_url(
    linear: &Linear,
    issue: &str,
    url: &str,
    title: Option<&str>,
) -> Result<IssueAttachment> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        bail!("{url} is not an http(s) URL");
    }
    let ctx = issue_context(linear, issue).await?;
    let d = linear
        .query(
            M_ATTACHMENT_LINK,
            json!({"issueId": ctx.id, "url": url, "title": title}),
        )
        .await?;
    if d.pointer("/attachmentLinkURL/success") != Some(&json!(true)) {
        bail!("Linear reported attachmentLinkURL as unsuccessful");
    }
    Ok(attachment_from(
        d.pointer("/attachmentLinkURL/attachment")
            .unwrap_or(&Value::Null),
    ))
}

/// Upload local files and return the markdown to embed them in a description
/// or comment, one per line.
pub async fn upload_for_markdown(linear: &Linear, paths: &[String]) -> Result<String> {
    let mut lines = Vec::new();
    for p in paths {
        let (bytes, name, mime) = crate::attachments::read_upload(p)?;
        let url = upload_file(linear, &name, &mime, bytes).await?;
        lines.push(markdown_for(&name, &mime, &url));
    }
    Ok(lines.join("\n"))
}

/// Text plus embedded files, separated by a blank line.
pub(crate) fn with_files(text: Option<&str>, files_md: &str) -> String {
    match text.map(str::trim_end).filter(|t| !t.is_empty()) {
        Some(t) if files_md.is_empty() => t.to_string(),
        Some(t) => format!("{t}\n\n{files_md}"),
        None => files_md.to_string(),
    }
}
