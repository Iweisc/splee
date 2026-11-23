//! Who on the team works a lead. A lead has at most one owner, a member of its organization. Someone sets it by hand, or the
//! project's assignment rule does when the lead first replies, books or is captured by the website chat: round robin among
//! picked members, the owner of the mailbox that emailed it, or whoever's territory has the lead's country. The rule looks at a lead once, at the first of those, so an
//! owner cleared by hand stays cleared and a rule turned on later leaves earlier leads alone.
//! A deal's owner is its lead's. The owner gets the lead's hot-lead email instead of the whole team, and Slack mentions them.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::error::{AppError, AppResult};

pub const RULES: &[&str] = &["off", "round_robin", "mailbox", "territory"];

/// Who covers which countries, as project_settings.assign_territories keeps it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Territory {
    pub owner: Uuid,
    /// ISO 3166-1 alpha-2 codes, upper case.
    pub countries: Vec<String>,
}
/// The pages with an Everyone, Mine or Unassigned filter, each remembered per member in memberships.owner_filters.
pub const VIEWS: &[&str] = &["inbox", "leads", "pipeline"];
pub const VIEW_FILTERS: &[&str] = &["everyone", "mine", "unassigned"];

/// Gives an unowned lead an owner by its project's rule the first time the lead replies, books or is captured, and returns
/// who it got. Errors are logged here, because a reply,
/// booking or chat that brought the lead in mustn't fail over it.
pub async fn claim(db: &PgPool, lead_id: Uuid) -> Option<Uuid> {
    match try_claim(db, lead_id).await {
        Ok(owner) => owner,
        Err(e) => {
            tracing::warn!(lead_id = %lead_id, error = %e, "couldn't assign the lead");
            None
        }
    }
}

async fn try_claim(db: &PgPool, lead_id: Uuid) -> sqlx::Result<Option<Uuid>> {
    let mut tx = db.begin().await?;
    // The lead's lock makes a reply and a booking that land together give it one owner. The settings row's lock makes two
    // leads replying at once take turns, so each sees the turn the other took.
    let lead: Option<(Option<Uuid>, Uuid, Uuid)> = sqlx::query_as(
        "select l.owner_id, l.project_id, p.org_id from leads l join projects p on p.id = l.project_id
         where l.id = $1 and l.assign_ran_at is null for update of l",
    )
    .bind(lead_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((current, project_id, org_id)) = lead else { return Ok(None) };
    let owner = if current.is_some() { None } else { pick(&mut tx, project_id, org_id, lead_id).await? };
    sqlx::query("update leads set assign_ran_at = now(), owner_id = coalesce(owner_id, $2) where id = $1")
        .bind(lead_id)
        .bind(owner)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(owner)
}

#[derive(sqlx::FromRow)]
struct Rule {
    assign_rule: String,
    assign_members: Vec<Uuid>,
    assign_last: Option<Uuid>,
    assign_territories: Json<Vec<Territory>>,
}

/// Who the project's rule gives the lead to, taking that member's turn when it goes round robin.
async fn pick(tx: &mut PgConnection, project_id: Uuid, org_id: Uuid, lead_id: Uuid) -> sqlx::Result<Option<Uuid>> {
    let rule: Option<Rule> =
        sqlx::query_as("select assign_rule, assign_members, assign_last, assign_territories from project_settings where project_id = $1 for update")
            .bind(project_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(Rule { assign_rule: rule, assign_members: members, assign_last: last, assign_territories: Json(territories) }) = rule else { return Ok(None) };
    let (owner, took_turn) = match rule.as_str() {
        "round_robin" => (next_in_turn(&members, last, &available(&mut *tx, org_id, &members).await?), true),
        // A lead from no one's territory, or from the territory of someone away or gone, takes its turn like round robin.
        "territory" => {
            let country = lead_country(&mut *tx, lead_id).await?;
            let covering = country.and_then(|c| territories.iter().find(|t| t.countries.contains(&c))).map(|t| t.owner);
            let available = available(&mut *tx, org_id, &[members.as_slice(), covering.as_slice()].concat()).await?;
            match covering.filter(|o| available.contains(o)) {
                Some(owner) => (Some(owner), false),
                None => (next_in_turn(&members, last, &available), true),
            }
        }
        "mailbox" => (mailbox_owner(&mut *tx, org_id, lead_id).await?, false),
        _ => (None, false),
    };
    if let (Some(owner), true) = (owner, took_turn) {
        sqlx::query("update project_settings set assign_last = $2 where project_id = $1").bind(project_id).bind(owner).execute(&mut *tx).await?;
    }
    Ok(owner)
}

/// Those among `users` who are on the team and not away.
async fn available(conn: &mut PgConnection, org_id: Uuid, users: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar("select user_id from memberships where org_id = $1 and user_id = any($2) and not away").bind(org_id).bind(users).fetch_all(conn).await
}

/// The lead's country for territories: its own, else its company's, else its phone number's, where +1 reads as the US.
async fn lead_country(conn: &mut PgConnection, lead_id: Uuid) -> sqlx::Result<Option<String>> {
    let (own, phone): (String, String) = sqlx::query_as(
        "select coalesce(nullif(trim(l.country_code), ''), trim(c.country_code), ''), l.phone from leads l left join companies c on c.id = l.company_id
         where l.id = $1",
    )
    .bind(lead_id)
    .fetch_one(conn)
    .await?;
    Ok(Some(own.to_ascii_uppercase()).filter(|c| !c.is_empty()).or_else(|| crate::phone::country(&phone).map(str::to_string)))
}

/// Who the project's settings give leads to now, before a save.
#[derive(sqlx::FromRow)]
pub struct Assignment {
    pub assign_members: Vec<Uuid>,
    pub assign_territories: Json<Vec<Territory>>,
}

/// Those among `users` who left the team. Someone who was never on it fails the save. The settings drop someone they named
/// before who left since, because every settings page sends the whole form back and it would fail otherwise.
async fn departed(db: &PgPool, org_id: Uuid, users: &[Uuid], saved: &Assignment) -> AppResult<HashSet<Uuid>> {
    let off: Vec<Uuid> =
        sqlx::query_scalar("select u from unnest($2::uuid[]) u where not exists (select 1 from memberships m where m.org_id = $1 and m.user_id = u)")
            .bind(org_id)
            .bind(users)
            .fetch_all(db)
            .await?;
    let named = |u: &Uuid| saved.assign_members.contains(u) || saved.assign_territories.iter().any(|t| t.owner == *u);
    if !off.iter().all(named) {
        return Err(AppError::BadRequest("The owner has to be on your team".into()));
    }
    Ok(off.into_iter().collect())
}

/// Round robin's members as settings save them: on the team, each once, in the order given.
pub async fn check_members(db: &PgPool, org_id: Uuid, mut members: Vec<Uuid>, saved: &Assignment) -> AppResult<Vec<Uuid>> {
    let mut seen = HashSet::new();
    members.retain(|m| seen.insert(*m));
    let gone = departed(db, org_id, &members, saved).await?;
    members.retain(|m| !gone.contains(m));
    Ok(members)
}

/// Territories as settings save them: codes in upper case, each owner on the team and in one territory, and each country in
/// one territory, so a lead never has two people who cover it. A territory without countries is dropped.
pub async fn check_territories(db: &PgPool, org_id: Uuid, territories: Vec<Territory>, before: &Assignment) -> AppResult<Vec<Territory>> {
    let gone = departed(db, org_id, &territories.iter().map(|t| t.owner).collect::<Vec<_>>(), before).await?;
    let (mut owners, mut taken, mut saved) = (HashSet::new(), HashSet::new(), vec![]);
    for t in territories.into_iter().filter(|t| !gone.contains(&t.owner)) {
        let mut countries: Vec<String> = vec![];
        for code in t.countries.iter().map(|c| c.trim().to_ascii_uppercase()).filter(|c| !c.is_empty()) {
            if code.len() != 2 || !code.bytes().all(|b| b.is_ascii_uppercase()) {
                return Err(AppError::BadRequest(format!("{code} isn't a two-letter country code, like US or DE")));
            }
            if !countries.contains(&code) {
                countries.push(code);
            }
        }
        if countries.is_empty() {
            continue;
        }
        if !owners.insert(t.owner) {
            return Err(AppError::BadRequest("Give each person one territory, with all their countries in it".into()));
        }
        if let Some(code) = countries.iter().find(|c| !taken.insert((*c).clone())) {
            return Err(AppError::BadRequest(format!("{code} is in two territories. Give each country to one person")));
        }
        saved.push(Territory { owner: t.owner, countries });
    }
    Ok(saved)
}

/// The member after `last` in `members`, going round, who is in `available`. The order is the one picked in settings, and
/// someone away keeps their place, so they're next in line again once they're back.
fn next_in_turn(members: &[Uuid], last: Option<Uuid>, available: &[Uuid]) -> Option<Uuid> {
    let start = last.and_then(|l| members.iter().position(|m| *m == l)).map_or(0, |i| i + 1);
    (0..members.len()).map(|i| members[(start + i) % members.len()]).find(|m| available.contains(m))
}

/// The owner of the mailbox that last emailed the lead, while they're on the team.
async fn mailbox_owner(conn: &mut PgConnection, org_id: Uuid, lead_id: Uuid) -> sqlx::Result<Option<Uuid>> {
    sqlx::query_scalar(
        "select b.owner_id from (select mailbox_id from email_messages where lead_id = $2 and direction = 'outbound' and status = 'sent'
                                 order by coalesce(sent_at, created_at) desc limit 1) e
         join mailboxes b on b.id = e.mailbox_id
         join memberships m on m.org_id = $1 and m.user_id = b.owner_id",
    )
    .bind(org_id)
    .bind(lead_id)
    .fetch_optional(conn)
    .await
}

/// Fails unless `user_id` is a member of the organization. None, which clears an owner, always passes.
pub async fn check_member(db: &PgPool, org_id: Uuid, user_id: Option<Uuid>) -> AppResult<()> {
    let Some(user_id) = user_id else { return Ok(()) };
    let member: bool = sqlx::query_scalar("select exists (select 1 from memberships where org_id = $1 and user_id = $2)")
        .bind(org_id)
        .bind(user_id)
        .fetch_one(db)
        .await?;
    if !member {
        return Err(AppError::BadRequest("The owner has to be on your team".into()));
    }
    Ok(())
}

/// Sets or clears the owner of the organization's leads among `lead_ids`, for anyone on the team. Leads of other
/// organizations are skipped. Returns how many changed.
pub async fn assign(db: &PgPool, org_id: Uuid, lead_ids: &[Uuid], owner: Option<Uuid>) -> AppResult<u64> {
    check_member(db, org_id, owner).await?;
    let changed = sqlx::query(
        "update leads l set owner_id = $3 from projects p
         where p.id = l.project_id and p.org_id = $1 and l.id = any($2) and l.owner_id is distinct from $3",
    )
    .bind(org_id)
    .bind(lead_ids)
    .bind(owner)
    .execute(db)
    .await?
    .rows_affected();
    Ok(changed)
}

/// An owner given by id or by a member's email, which reads better from the API and MCP. Null and "" clear it.
pub async fn resolve_owner(db: &PgPool, org_id: Uuid, raw: &Value) -> AppResult<Option<Uuid>> {
    let raw = match raw {
        Value::Null => return Ok(None),
        Value::String(s) if s.trim().is_empty() => return Ok(None),
        Value::String(s) => s.trim(),
        _ => return Err(AppError::BadRequest("Give the owner as a user id or a team member's email, or null for nobody".into())),
    };
    if let Ok(id) = raw.parse::<Uuid>() {
        return Ok(Some(id));
    }
    let id: Option<Uuid> = sqlx::query_scalar("select m.user_id from memberships m join users u on u.id = m.user_id where m.org_id = $1 and u.email = $2::citext")
        .bind(org_id)
        .bind(raw)
        .fetch_optional(db)
        .await?;
    id.map(Some).ok_or_else(|| AppError::BadRequest(format!("Nobody on your team has the email {raw}")))
}

/// Which leads a list keeps by owner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OwnerFilter {
    Everyone,
    Is(Uuid),
    Unassigned,
    /// The sidebar's Inbox and Calls counts: yours and the ones nobody has.
    IsOrUnassigned(Uuid),
}

impl OwnerFilter {
    /// "mine", "unassigned", "mine_or_unassigned", a member's id, or blank and "everyone" for all. `me` is None for an API key,
    /// which isn't anyone, so it can't ask for mine.
    pub fn parse(raw: Option<&str>, me: Option<Uuid>) -> AppResult<Self> {
        let mine = || me.ok_or_else(|| AppError::BadRequest("An API key isn't a team member. Give the owner's user id instead of mine".into()));
        Ok(match raw.map(str::trim).unwrap_or_default() {
            "" | "everyone" => Self::Everyone,
            "unassigned" | "none" => Self::Unassigned,
            "mine" => Self::Is(mine()?),
            "mine_or_unassigned" => Self::IsOrUnassigned(mine()?),
            id => Self::Is(id.parse().map_err(|_| AppError::BadRequest("owner is mine, unassigned, everyone or a user id".into()))?),
        })
    }

    /// What `sql`'s mode parameter takes.
    pub fn mode(&self) -> &'static str {
        match self {
            Self::Everyone => "everyone",
            Self::Is(_) => "is",
            Self::Unassigned => "none",
            Self::IsOrUnassigned(_) => "is_or_none",
        }
    }

    pub fn user(&self) -> Option<Uuid> {
        match self {
            Self::Is(id) | Self::IsOrUnassigned(id) => Some(*id),
            _ => None,
        }
    }
}

/// The condition on `{alias}.owner_id` for an OwnerFilter bound as its mode in `$m` and its user in `$u`. Two parameters
/// rather than one text, because casting "none" to a uuid fails even in a case branch that isn't taken.
pub fn sql(alias: &str, m: u8, u: u8) -> String {
    format!(
        "(case ${m}::text when 'is' then {alias}.owner_id = ${u}::uuid when 'none' then {alias}.owner_id is null
           when 'is_or_none' then {alias}.owner_id is null or {alias}.owner_id = ${u}::uuid else true end)"
    )
}

/// Where a hot lead's email goes: its owner alone while they're on the team, or else everyone. Either way only those
/// whose notification settings ask for it.
pub async fn hot_lead_recipients(db: &PgPool, org_id: Uuid, owner: Option<Uuid>) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "select u.email::text from memberships m join users u on u.id = m.user_id
         left join notification_prefs n on n.user_id = u.id
         where m.org_id = $1 and coalesce(n.hot_lead_email, true)
           and (m.user_id = $2 or not exists (select 1 from memberships o where o.org_id = $1 and o.user_id = $2))
         order by u.email",
    )
    .bind(org_id)
    .bind(owner)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;
    use crate::config::Config;
    use crate::state::AppState;

    #[test]
    fn turns_go_round_in_order_and_skip_whoever_is_away() {
        let [a, b, c] = [Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)];
        let all = [a, b, c];
        assert_eq!(next_in_turn(&all, None, &all), Some(a));
        assert_eq!(next_in_turn(&all, Some(a), &all), Some(b));
        assert_eq!(next_in_turn(&all, Some(c), &all), Some(a), "it wraps round");
        assert_eq!(next_in_turn(&all, Some(a), &[a, c]), Some(c), "b is away");
        assert_eq!(next_in_turn(&all, Some(Uuid::from_u128(9)), &all), Some(a), "the last one left the list");
        assert_eq!(next_in_turn(&all, Some(a), &[]), None, "everyone is away");
        assert_eq!(next_in_turn(&[], None, &[]), None);
    }

    #[test]
    fn owner_filters_parse_for_people_and_for_api_keys() {
        let me = Uuid::from_u128(7);
        assert_eq!(OwnerFilter::parse(None, Some(me)).unwrap(), OwnerFilter::Everyone);
        assert_eq!(OwnerFilter::parse(Some("everyone"), None).unwrap(), OwnerFilter::Everyone);
        assert_eq!(OwnerFilter::parse(Some("mine"), Some(me)).unwrap(), OwnerFilter::Is(me));
        assert_eq!(OwnerFilter::parse(Some("unassigned"), None).unwrap(), OwnerFilter::Unassigned);
        assert_eq!(OwnerFilter::parse(Some("mine_or_unassigned"), Some(me)).unwrap(), OwnerFilter::IsOrUnassigned(me));
        assert_eq!(OwnerFilter::parse(Some(&me.to_string()), None).unwrap(), OwnerFilter::Is(me));
        assert!(OwnerFilter::parse(Some("mine"), None).is_err(), "an API key isn't anyone");
        assert!(OwnerFilter::parse(Some("someone"), Some(me)).is_err());
    }

    struct Team {
        db: PgPool,
        acme: Uuid,
        project: Uuid,
        /// Ana, Ben and Cy, in that order. Ana is the owner.
        people: [Uuid; 3],
        /// Eve is only in Beta.
        eve: Uuid,
        beta_project: Uuid,
    }

    async fn user(db: &PgPool, org: Uuid, email: &str, role: &str) -> Uuid {
        let name = email.split('@').next().unwrap();
        let id: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, initcap($2), 'x') returning id")
            .bind(email)
            .bind(name)
            .fetch_one(db)
            .await
            .unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, $3)").bind(org).bind(id).bind(role).execute(db).await.unwrap();
        crate::auth::sign_in(db, id, org, &format!("session-{name}")).await;
        id
    }

    async fn project(db: &PgPool, org: Uuid) -> Uuid {
        let id: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1) on conflict do nothing").bind(id).execute(db).await.unwrap();
        id
    }

    impl Team {
        async fn new(db: PgPool) -> Self {
            let org = |name: &'static str| sqlx::query_scalar::<_, Uuid>("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db);
            let (acme, beta) = (org("Acme").await.unwrap(), org("Beta").await.unwrap());
            let people = [
                user(&db, acme, "ana@acme.test", "owner").await,
                user(&db, acme, "ben@acme.test", "member").await,
                user(&db, acme, "cy@acme.test", "member").await,
            ];
            let eve = user(&db, beta, "eve@beta.test", "owner").await;
            let (project, beta_project) = (project(&db, acme).await, project(&db, beta).await);
            Self { db, acme, project, people, eve, beta_project }
        }

        async fn lead(&self, project: Uuid, name: &str) -> Uuid {
            sqlx::query_scalar("insert into leads (project_id, full_name, email) values ($1, $2, $3) returning id")
                .bind(project)
                .bind(name)
                .bind(format!("{}@lead.test", name.to_lowercase().replace(' ', ".")))
                .fetch_one(&self.db)
                .await
                .unwrap()
        }

        async fn rule(&self, rule: &str, members: &[Uuid]) {
            sqlx::query("update project_settings set assign_rule = $2, assign_members = $3 where project_id = $1")
                .bind(self.project)
                .bind(rule)
                .bind(members)
                .execute(&self.db)
                .await
                .unwrap();
        }

        async fn owner(&self, lead: Uuid) -> Option<Uuid> {
            sqlx::query_scalar("select owner_id from leads where id = $1").bind(lead).fetch_one(&self.db).await.unwrap()
        }

        async fn away(&self, user: Uuid, away: bool) {
            sqlx::query("update memberships set away = $2 where user_id = $1").bind(user).bind(away).execute(&self.db).await.unwrap();
        }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn round_robin_takes_turns_skips_the_away_and_never_moves_an_owned_lead(db: PgPool) {
        let t = Team::new(db).await;
        let [ana, ben, cy] = t.people;
        let off = t.lead(t.project, "Off Lead").await;
        assert_eq!(claim(&t.db, off).await, None, "off by default, as before");

        t.rule("round_robin", &[ben, ana, cy]).await;
        let mut got = vec![];
        for i in 0..4 {
            got.push(claim(&t.db, t.lead(t.project, &format!("Lead {i}")).await).await);
        }
        assert_eq!(got, [Some(ben), Some(ana), Some(cy), Some(ben)], "in the picked order, round again");

        t.away(ana, true).await;
        assert_eq!(claim(&t.db, t.lead(t.project, "While Away").await).await, Some(cy), "Ana is away");
        t.away(ana, false).await;
        assert_eq!(claim(&t.db, t.lead(t.project, "Back").await).await, Some(ben));
        assert_eq!(claim(&t.db, t.lead(t.project, "Her Turn").await).await, Some(ana));

        // A lead that has an owner keeps them, and doesn't use up anyone's turn.
        let kept = t.lead(t.project, "Kept").await;
        assign(&t.db, t.acme, &[kept], Some(ana)).await.unwrap();
        assert_eq!(claim(&t.db, kept).await, None);
        assert_eq!(t.owner(kept).await, Some(ana));
        assert_eq!(claim(&t.db, t.lead(t.project, "Next").await).await, Some(cy), "Cy was next before Kept");

        // A member who left keeps their place in the list, but gets no leads.
        sqlx::query("delete from memberships where user_id = $1").bind(ben).execute(&t.db).await.unwrap();
        assert_eq!(claim(&t.db, t.lead(t.project, "After Ben").await).await, Some(ana));
        t.away(ana, true).await;
        t.away(cy, true).await;
        let nobody = t.lead(t.project, "Nobody In").await;
        assert_eq!(claim(&t.db, nobody).await, None, "everyone is away, so it waits for someone to take it");
        assert_eq!(t.owner(nobody).await, None);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn territories_get_their_countries_leads_and_the_rest_take_turns(db: PgPool) {
        let t = Team::new(db).await;
        let [ana, ben, cy] = t.people;
        t.rule("territory", &[ana, cy]).await;
        let territories = json!([{ "owner": ben, "countries": ["DE", "AT"] }, { "owner": cy, "countries": ["FR"] }]);
        sqlx::query("update project_settings set assign_territories = $2 where project_id = $1").bind(t.project).bind(&territories).execute(&t.db).await.unwrap();
        // A lead with its own country, a company's or a phone number's, as each may be all Splee knows.
        let lead = |name: &'static str, country: &'static str, company_country: &'static str, phone: &'static str| {
            let (db, project) = (t.db.clone(), t.project);
            async move {
                let company: Option<Uuid> = if company_country.is_empty() {
                    None
                } else {
                    let domain = format!("{}.test", name.to_lowercase().replace(' ', "-"));
                    let insert = "insert into companies (project_id, domain, country_code) values ($1, $2, $3) returning id";
                    Some(sqlx::query_scalar(insert).bind(project).bind(domain).bind(company_country).fetch_one(&db).await.unwrap())
                };
                let id: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, country_code, company_id, phone) values ($1, $2, $3, $4, $5) returning id")
                    .bind(project)
                    .bind(name)
                    .bind(country)
                    .bind(company)
                    .bind(phone)
                    .fetch_one(&db)
                    .await
                    .unwrap();
                claim(&db, id).await
            }
        };
        assert_eq!(lead("Berlin", "de", "", "").await, Some(ben), "the lead's own country, in any case");
        assert_eq!(lead("Boston", "US", "", "").await, Some(ana), "nobody covers the US, so it's Ana's turn");
        assert_eq!(lead("Vienna", "", "AT", "").await, Some(ben), "the company's country");
        assert_eq!(lead("Nowhere", "", "", "").await, Some(cy), "Ben's leads didn't use a turn");
        assert_eq!(lead("Paris", "", "", "+33612345678").await, Some(cy), "the phone number's country");
        assert_eq!(lead("Leeds", "GB", "FR", "+33612345678").await, Some(ana), "the lead's own country comes first");

        // Someone away, or gone, has their territory's leads take turns until they're back.
        t.away(ben, true).await;
        assert_eq!(lead("Munich", "DE", "", "").await, Some(cy));
        t.away(ben, false).await;
        assert_eq!(lead("Hamburg", "DE", "", "").await, Some(ben));
        sqlx::query("delete from memberships where user_id = $1").bind(ben).execute(&t.db).await.unwrap();
        assert_eq!(lead("Graz", "AT", "", "").await, Some(ana));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_rule_only_looks_at_a_lead_once(db: PgPool) {
        let t = Team::new(db).await;
        let [ana, ben, _] = t.people;
        // A lead that replied while the rule was off isn't picked up by a later reply once it's on.
        let early = t.lead(t.project, "Early").await;
        assert_eq!(claim(&t.db, early).await, None);
        t.rule("round_robin", &[ana, ben]).await;
        assert_eq!(claim(&t.db, early).await, None);
        // An owner cleared by hand stays cleared when the lead replies again.
        let cleared = t.lead(t.project, "Cleared").await;
        assert_eq!(claim(&t.db, cleared).await, Some(ana));
        assign(&t.db, t.acme, &[cleared], None).await.unwrap();
        assert_eq!(claim(&t.db, cleared).await, None);
        assert_eq!(t.owner(cleared).await, None);
        assert_eq!(claim(&t.db, t.lead(t.project, "Next").await).await, Some(ben), "and nobody's turn was used");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn round_robin_is_fair_when_leads_reply_at_once(db: PgPool) {
        let t = Team::new(db).await;
        t.rule("round_robin", &t.people).await;
        let mut leads = vec![];
        for i in 0..12 {
            leads.push(t.lead(t.project, &format!("Lead {i}")).await);
        }
        // Each claim on its own task and connection, so they really do race for the turn.
        let claims = leads.iter().map(|&lead| tokio::spawn({
            let db = t.db.clone();
            async move { claim(&db, lead).await }
        }));
        let got: Vec<Option<Uuid>> = futures::future::join_all(claims).await.into_iter().map(Result::unwrap).collect();
        assert!(got.iter().all(Option::is_some), "no lead was lost to the race");
        for person in t.people {
            assert_eq!(got.iter().filter(|o| **o == Some(person)).count(), 4, "each gets a third");
        }
        // The same lead claimed twice at once still gets one owner and uses one turn.
        let twice = t.lead(t.project, "Twice").await;
        let (a, b) = tokio::join!(claim(&t.db, twice), claim(&t.db, twice));
        assert_eq!([a, b].iter().flatten().count(), 1);
        let after = claim(&t.db, t.lead(t.project, "After").await).await.unwrap();
        assert_ne!(Some(after), a.or(b));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn by_mailbox_gives_the_lead_to_whoever_owns_the_mailbox_that_emailed_it(db: PgPool) {
        let t = Team::new(db).await;
        let [ana, ben, _] = t.people;
        t.rule("mailbox", &[]).await;
        let mailbox = |email: &'static str, owner: Option<Uuid>| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, owner_id) values ($1, $2, 'smtp.test', '', '', $3)
                 returning id",
            )
            .bind(t.acme)
            .bind(email)
            .bind(owner)
            .fetch_one(&t.db)
        };
        let (anas, bens, shared) = (mailbox("ana@send.test", Some(ana)).await.unwrap(), mailbox("ben@send.test", Some(ben)).await.unwrap(), mailbox("hi@send.test", None).await.unwrap());
        let emailed = |lead: Uuid, mailbox: Uuid, days_ago: i32| {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, sent_at)
                 values ($1, $2, $3, 'outbound', 'initial', 'sent', now() - make_interval(days => $4))",
            )
            .bind(t.project)
            .bind(lead)
            .bind(mailbox)
            .bind(days_ago)
            .execute(&t.db)
        };
        let sarah = t.lead(t.project, "Sarah").await;
        emailed(sarah, anas, 3).await.unwrap();
        emailed(sarah, bens, 1).await.unwrap();
        assert_eq!(claim(&t.db, sarah).await, Some(ben), "the mailbox that emailed them last");

        let sam = t.lead(t.project, "Sam").await;
        emailed(sam, shared, 1).await.unwrap();
        assert_eq!(claim(&t.db, sam).await, None, "a mailbox nobody owns");
        assert_eq!(claim(&t.db, t.lead(t.project, "Never Emailed").await).await, None);

        sqlx::query("delete from memberships where user_id = $1").bind(ben).execute(&t.db).await.unwrap();
        let late = t.lead(t.project, "Late").await;
        emailed(late, bens, 1).await.unwrap();
        assert_eq!(claim(&t.db, late).await, None, "Ben left the team");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn assigning_by_hand_stays_inside_the_team(db: PgPool) {
        let t = Team::new(db).await;
        let [_, ben, _] = t.people;
        let (ours, theirs) = (t.lead(t.project, "Ours").await, t.lead(t.beta_project, "Theirs").await);
        assert!(matches!(assign(&t.db, t.acme, &[ours], Some(t.eve)).await, Err(AppError::BadRequest(_))), "Eve is in another org");
        assert_eq!(assign(&t.db, t.acme, &[ours, theirs], Some(ben)).await.unwrap(), 1, "Beta's lead is skipped");
        assert_eq!((t.owner(ours).await, t.owner(theirs).await), (Some(ben), None));
        assert_eq!(assign(&t.db, t.acme, &[ours], Some(ben)).await.unwrap(), 0, "already his");
        assert_eq!(assign(&t.db, t.acme, &[ours], None).await.unwrap(), 1);
        assert_eq!(resolve_owner(&t.db, t.acme, &json!("BEN@acme.test")).await.unwrap(), Some(ben));
        assert!(resolve_owner(&t.db, t.acme, &json!("eve@beta.test")).await.is_err(), "not on Acme's team");
        assert_eq!(resolve_owner(&t.db, t.acme, &Value::Null).await.unwrap(), None);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_hot_lead_email_goes_to_its_owner_alone(db: PgPool) {
        let t = Team::new(db).await;
        let [ana, ben, cy] = t.people;
        let everyone = ["ana@acme.test", "ben@acme.test", "cy@acme.test"];
        assert_eq!(hot_lead_recipients(&t.db, t.acme, None).await.unwrap(), everyone);
        assert_eq!(hot_lead_recipients(&t.db, t.acme, Some(ben)).await.unwrap(), ["ben@acme.test"]);
        sqlx::query("insert into notification_prefs (user_id, hot_lead_email) values ($1, false), ($2, false)").bind(ben).bind(cy).execute(&t.db).await.unwrap();
        assert!(hot_lead_recipients(&t.db, t.acme, Some(ben)).await.unwrap().is_empty(), "Ben turned it off, and it's his lead");
        assert_eq!(hot_lead_recipients(&t.db, t.acme, None).await.unwrap(), ["ana@acme.test"]);
        assert_eq!(hot_lead_recipients(&t.db, t.acme, Some(t.eve)).await.unwrap(), ["ana@acme.test"], "an owner off the team is nobody");
        assert_eq!(hot_lead_recipients(&t.db, t.acme, Some(ana)).await.unwrap(), ["ana@acme.test"]);
    }

    fn config() -> Config {
        Config { public_url: "http://localhost:3424".into(), ..Config::test() }
    }

    struct Api {
        base: String,
        client: reqwest::Client,
    }

    impl Api {
        async fn start(db: &PgPool) -> Self {
            let state = AppState {
                db: db.clone(),
                cfg: Arc::new(config()),
                events: tokio::sync::broadcast::channel(1).0,
                rate_limits: Default::default(),
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", crate::routes::router(&state)).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { base, client: reqwest::Client::new() }
        }

        /// `who` is the part of their email before the @, which names their session.
        async fn send(&self, who: &str, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
            let mut req = self.client.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session=session-{who}"));
            if let Some(body) = body {
                req = req.json(&body);
            }
            let res = req.send().await.unwrap();
            (res.status().as_u16(), res.json().await.unwrap_or(Value::Null))
        }

        async fn get(&self, who: &str, path: &str) -> Value {
            let (status, body) = self.send(who, reqwest::Method::GET, path, None).await;
            assert_eq!(status, 200, "{path}: {body}");
            body
        }
    }

    fn names(page: &Value, key: &str) -> Vec<String> {
        let mut names: Vec<String> = page["items"].as_array().unwrap().iter().map(|i| i[key].as_str().unwrap().to_string()).collect();
        names.sort();
        names
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_app_assigns_filters_and_remembers_for_each_member(db: PgPool) {
        let t = Team::new(db).await;
        let [ana, ben, _] = t.people;
        let api = Api::start(&t.db).await;
        let put = reqwest::Method::PUT;
        let (mine, bens, nobodys, theirs) =
            (t.lead(t.project, "Ana Lead").await, t.lead(t.project, "Ben Lead").await, t.lead(t.project, "Free Lead").await, t.lead(t.beta_project, "Beta Lead").await);

        // Anyone on the team can hand out a lead, but only to someone on the team, and only their own org's leads.
        let (status, lead) = api.send("ben", put.clone(), &format!("/leads/{mine}/owner"), Some(json!({ "owner_id": ana }))).await;
        assert_eq!((status, lead["owner_id"].as_str(), lead["owner_name"].as_str()), (200, Some(ana.to_string().as_str()), Some("Ana")));
        let (status, _) = api.send("ben", put.clone(), &format!("/leads/{bens}/owner"), Some(json!({ "owner_id": t.eve }))).await;
        assert_eq!(status, 400, "Eve isn't on Acme's team");
        let (status, _) = api.send("ben", put.clone(), &format!("/leads/{theirs}/owner"), Some(json!({ "owner_id": ben }))).await;
        assert_eq!(status, 404, "Beta's lead");
        let bulk = |owner: Uuid| json!({ "ids": [bens, theirs], "owner_id": owner });
        let (status, _) = api.send("ana", reqwest::Method::POST, &format!("/projects/{}/leads/assign", t.project), Some(bulk(t.eve))).await;
        assert_eq!(status, 400);
        let (status, body) = api.send("ana", reqwest::Method::POST, &format!("/projects/{}/leads/assign", t.project), Some(bulk(ben))).await;
        assert_eq!((status, body["changed"].as_i64()), (200, Some(1)), "Beta's lead isn't in the project");
        assert_eq!(t.owner(theirs).await, None);
        let (status, _) = api.send("eve", reqwest::Method::POST, &format!("/projects/{}/leads/assign", t.project), Some(bulk(t.eve))).await;
        assert_eq!(status, 404, "Eve can't reach Acme's project");

        // Leads, Inbox and Pipeline filter by owner.
        let leads = |who: &'static str, owner: String| {
            let path = format!("/projects/{}/leads?owner={owner}", t.project);
            let api = &api;
            async move { names(&api.get(who, &path).await, "full_name") }
        };
        assert_eq!(leads("ana", "mine".into()).await, ["Ana Lead"]);
        assert_eq!(leads("ben", "mine".into()).await, ["Ben Lead"]);
        assert_eq!(leads("ben", "unassigned".into()).await, ["Free Lead"]);
        assert_eq!(leads("ben", "everyone".into()).await, ["Ana Lead", "Ben Lead", "Free Lead"]);
        assert_eq!(leads("ben", ana.to_string()).await, ["Ana Lead"]);
        assert_eq!(api.send("eve", reqwest::Method::GET, &format!("/projects/{}/leads?owner=mine", t.project), None).await.0, 404);

        for lead in [mine, bens, nobodys] {
            sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'inbound', 'inbound', 'received')")
                .bind(t.project)
                .bind(lead)
                .execute(&t.db)
                .await
                .unwrap();
        }
        let inbox = |owner: &str| format!("/inbox?filter=needs_reply&owner={owner}");
        let threads = |page: Value| -> Vec<String> {
            let mut n: Vec<String> = page["items"].as_array().unwrap().iter().map(|i| i["lead"]["full_name"].as_str().unwrap().to_string()).collect();
            n.sort();
            n
        };
        assert_eq!(threads(api.get("ben", &inbox("mine")).await), ["Ben Lead"]);
        assert_eq!(api.get("ben", &inbox("mine_or_unassigned")).await["total"], 2, "the sidebar counts Ben's and nobody's");
        assert_eq!(api.get("ben", &inbox("")).await["total"], 3);
        assert_eq!(api.get("eve", &inbox("")).await["total"], 0, "Beta sees none of Acme's");

        crate::deals::ensure_stages(&t.db, t.acme).await.unwrap();
        let stage: Uuid = sqlx::query_scalar("select id from deal_stages where org_id = $1 order by position limit 1").bind(t.acme).fetch_one(&t.db).await.unwrap();
        for lead in [mine, bens] {
            sqlx::query("insert into deals (org_id, project_id, lead_id, stage_id, value_cents) values ($1, $2, $3, $4, 100000)")
                .bind(t.acme)
                .bind(t.project)
                .bind(lead)
                .bind(stage)
                .execute(&t.db)
                .await
                .unwrap();
        }
        let board = api.get("ben", "/pipeline?owner=mine").await;
        let deals = board["deals"].as_array().unwrap();
        assert_eq!((deals.len(), deals[0]["lead_name"].as_str(), deals[0]["owner_name"].as_str()), (1, Some("Ben Lead"), Some("Ben")));
        assert_eq!(board["forecast"]["amounts"][0]["open"], 1000.0, "the forecast adds up Ben's alone");
        assert_eq!(api.get("ben", "/pipeline").await["deals"].as_array().unwrap().len(), 2);
        // A deal's owner is its lead's, so giving the deal to someone assigns the lead.
        let deal = deals[0]["id"].as_str().unwrap();
        let (status, _) = api.send("ben", reqwest::Method::PATCH, &format!("/deals/{deal}"), Some(json!({ "owner_id": ana }))).await;
        assert_eq!(status, 200);
        assert_eq!(t.owner(bens).await, Some(ana));

        // Away and each page's filter are the member's own.
        let patch = reqwest::Method::PATCH;
        let (_, me) = api.send("ben", patch.clone(), "/auth/me", Some(json!({ "away": true }))).await;
        assert_eq!(me["away"], true);
        api.send("ben", patch.clone(), "/auth/me", Some(json!({ "view": "inbox", "owner_filter": "mine" }))).await;
        let (_, me) = api.send("ben", patch.clone(), "/auth/me", Some(json!({ "view": "pipeline", "owner_filter": "unassigned" }))).await;
        assert_eq!(me["owner_filters"], json!({ "inbox": "mine", "pipeline": "unassigned" }));
        assert_eq!(api.get("ana", "/auth/me").await["owner_filters"], json!({}), "Ana's are her own");
        let (status, _) = api.send("ben", patch.clone(), "/auth/me", Some(json!({ "view": "emails", "owner_filter": "mine" }))).await;
        assert_eq!(status, 400);
        let members = api.get("ana", "/organization/users").await;
        assert_eq!(members.as_array().unwrap().iter().filter(|m| m["away"] == true).count(), 1);

        // Round robin's members have to be on the team.
        let settings = format!("/projects/{}/settings", t.project);
        let (status, _) = api.send("ana", put.clone(), &settings, Some(json!({ "assign_rule": "round_robin", "assign_members": [ana, t.eve] }))).await;
        assert_eq!(status, 400);
        let (status, saved) = api.send("ana", put.clone(), &settings, Some(json!({ "assign_rule": "round_robin", "assign_members": [ben, ana, ben] }))).await;
        assert_eq!((status, saved["assign_rule"].as_str(), saved["assign_members"].clone()), (200, Some("round_robin"), json!([ben, ana])));
        let (status, _) = api.send("ana", put.clone(), &settings, Some(json!({ "assign_rule": "random" }))).await;
        assert_eq!(status, 400);

        // Territories are kept in upper case, without empty ones, and each person and each country is in one at most.
        let territories = json!([{ "owner": ben, "countries": [" de", "AT", "DE"] }, { "owner": ana, "countries": [""] }]);
        let (status, saved) = api.send("ana", put.clone(), &settings, Some(json!({ "assign_rule": "territory", "assign_territories": territories }))).await;
        assert_eq!((status, saved["assign_rule"].as_str()), (200, Some("territory")));
        assert_eq!(saved["assign_territories"], json!([{ "owner": ben, "countries": ["DE", "AT"] }]));
        for wrong in [
            json!([{ "owner": t.eve, "countries": ["FR"] }]),
            json!([{ "owner": ben, "countries": ["DE"] }, { "owner": ana, "countries": ["de"] }]),
            json!([{ "owner": ben, "countries": ["DE"] }, { "owner": ben, "countries": ["FR"] }]),
            json!([{ "owner": ben, "countries": ["Germany"] }]),
        ] {
            assert_eq!(api.send("ana", put.clone(), &settings, Some(json!({ "assign_territories": wrong }))).await.0, 400, "{wrong}");
        }
        assert_eq!(api.get("ana", &settings).await["assign_territories"], json!([{ "owner": ben, "countries": ["DE", "AT"] }]));

        // Follow-ups saved before their check got stricter, sent back unchanged with the rest of the form, don't block a save.
        let legacy = json!([{ "delay_days": 3, "enabled": true }, { "delay_days": 45 }]);
        sqlx::query("update project_settings set follow_ups = $2 where project_id = $1").bind(t.project).bind(&legacy).execute(&t.db).await.unwrap();
        let (status, _) = api.send("ana", put.clone(), &settings, Some(json!({ "assign_rule": "off", "follow_ups": legacy }))).await;
        assert_eq!(status, 200);
        let (status, _) = api.send("ana", put.clone(), &settings, Some(json!({ "follow_ups": [{ "delay_days": 45, "enabled": true }] }))).await;
        assert_eq!(status, 400, "a change is checked");

        // Removing someone hands their leads back to nobody.
        assign(&t.db, t.acme, &[nobodys], Some(ben)).await.unwrap();
        let (status, _) = api.send("ana", reqwest::Method::DELETE, &format!("/organization/users/{ben}"), None).await;
        assert_eq!(status, 200);
        assert_eq!((t.owner(nobodys).await, t.owner(mine).await), (None, Some(ana)));
        // The settings still name him, and any settings page sends them back on save, which drops him. Someone who was never
        // on the team is still refused.
        let draft = api.get("ana", &settings).await;
        let resend = json!({ "assign_members": draft["assign_members"], "assign_territories": draft["assign_territories"], "daily_email_limit": 40 });
        let (status, saved) = api.send("ana", put.clone(), &settings, Some(resend)).await;
        assert_eq!((status, saved["assign_members"].clone(), saved["assign_territories"].clone()), (200, json!([ana]), json!([])));
        assert_eq!(api.send("ana", put.clone(), &settings, Some(json!({ "assign_members": [ana, t.eve] }))).await.0, 400);
        let territory = json!([{ "owner": t.eve, "countries": ["FR"] }]);
        assert_eq!(api.send("ana", put, &settings, Some(json!({ "assign_territories": territory }))).await.0, 400);
    }
}
