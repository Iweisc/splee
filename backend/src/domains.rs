//! Sending domains: lookalikes of a company's domain, bought through the org's Porkbun account and set up for cold email,
//! so the main domain's reputation stays out of it.

use std::time::Duration;

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::models::SendingDomain;
use crate::porkbun::{Failure, Porkbun, Record};

/// Public suffixes with two labels that companies commonly sit under. Others are taken as one label.
const TWO_LEVEL: &[&str] = &["co.uk", "org.uk", "com.au", "net.au", "co.nz", "com.br", "co.jp", "co.in", "co.za", "com.mx", "com.sg", "com.tr"];

/// "acme" from "https://www.acme.co.uk/about".
pub fn brand(domain: &str) -> Option<String> {
    let host = domain.trim().to_lowercase();
    let host = host.split("://").last()?.split(['/', '?', '#']).next()?.trim_end_matches('.');
    let host = host.strip_prefix("www.").unwrap_or(host);
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return None;
    }
    let two_level = labels.len() >= 3 && TWO_LEVEL.contains(&labels[labels.len() - 2..].join(".").as_str());
    let label = labels[labels.len() - if two_level { 3 } else { 2 }];
    let valid = (1..=50).contains(&label.len()) && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') && !label.starts_with('-');
    valid.then(|| label.to_string())
}

/// About 20 lookalikes of the domain that read as the same company, which Porkbun checks in one call (25 at most).
pub fn suggestions(domain: &str) -> Vec<String> {
    let Some(b) = brand(domain) else { return vec![] };
    let mut out: Vec<String> = ["get", "try", "use", "join", "meet", "hello", "go", "with"].iter().map(|p| format!("{p}{b}.com")).collect();
    out.extend(["hq", "app", "team", "mail", "labs", "-mail", "-team", "-hq"].iter().map(|s| format!("{b}{s}.com")));
    out.extend([format!("{b}.co"), format!("{b}.io"), format!("get{b}.co"), format!("try{b}.io"), format!("{b}hq.co"), format!("{b}team.io")]);
    let own = domain.trim().to_lowercase();
    let mut seen = std::collections::HashSet::new();
    out.retain(|d| !own.ends_with(d.as_str()) && seen.insert(d.clone()));
    out
}

pub const PROVIDERS: &[&str] = &["google", "microsoft", "zoho", "zoho_eu", "migadu"];

/// The records Splee sets for a provider: what's known before the user has an account there. DKIM keys are made per
/// account, so the user pastes those, except Migadu's, whose CNAMEs follow from the domain.
pub fn plan(domain: &str, provider: &str, forward_to: &str, dmarc_email: &str) -> Vec<Record> {
    let mx = |prio: i32, host: &str| Record::new("MX", "", host, Some(prio));
    let (mut records, spf) = match provider {
        "google" => (vec![mx(1, "smtp.google.com")], Some("_spf.google.com")),
        // Microsoft 365 names the host after the domain. Its admin center shows it once the domain is added there.
        "microsoft" => (vec![mx(0, &format!("{}.mail.protection.outlook.com", domain.replace('.', "-")))], Some("spf.protection.outlook.com")),
        "zoho" => (vec![mx(10, "mx.zoho.com"), mx(20, "mx2.zoho.com"), mx(50, "mx3.zoho.com")], Some("zohomail.com")),
        "zoho_eu" => (vec![mx(10, "mx.zoho.eu"), mx(20, "mx2.zoho.eu"), mx(50, "mx3.zoho.eu")], Some("zohomail.eu")),
        "migadu" => {
            let mut r = vec![mx(10, "aspmx1.migadu.com"), mx(20, "aspmx2.migadu.com")];
            r.extend((1..=3).map(|n| Record::new("CNAME", &format!("key{n}._domainkey"), format!("key{n}.{domain}._domainkey.migadu.com"), None)));
            (r, Some("spf.migadu.com"))
        }
        _ => (vec![], None),
    };
    if let Some(include) = spf {
        records.push(Record::new("TXT", "", format!("v=spf1 include:{include} ~all"), None));
    }
    let rua = if dmarc_email.is_empty() { String::new() } else { format!("; rua=mailto:{dmarc_email}") };
    records.push(Record::new("TXT", "_dmarc", format!("v=DMARC1; p=none{rua}"), None));
    if !forward_to.is_empty() {
        records.push(Record::new("URL", "", forward_to, None));
    }
    records
}

/// The DKIM selector the DNS check tries first: the one the user added, else the provider's own.
pub fn dkim_selector(d: &SendingDomain) -> Option<String> {
    let records: Vec<Record> = serde_json::from_value(d.records.clone()).unwrap_or_default();
    let added = records.iter().find_map(|r| r.name.strip_suffix("._domainkey").map(str::to_string));
    added.or_else(|| match d.provider.as_str() {
        "google" => Some("google".into()),
        "microsoft" => Some("selector1".into()),
        "migadu" => Some("key1".into()),
        _ => None,
    })
}

const SELECT: &str = "select d.*,
       (select count(*) from mailboxes m where m.org_id = d.org_id and lower(split_part(m.email::text, '@', 2)) = d.domain) as mailboxes,
       (select count(*) from mailboxes m where m.org_id = d.org_id and lower(split_part(m.email::text, '@', 2)) = d.domain and m.warmup) as warming
  from sending_domains d";

pub async fn list(db: &PgPool, org_id: Uuid) -> sqlx::Result<Vec<SendingDomain>> {
    sqlx::query_as(&format!("{SELECT} where d.org_id = $1 order by d.created_at, d.domain")).bind(org_id).fetch_all(db).await
}

pub async fn load(db: &PgPool, id: Uuid, org_id: Uuid) -> sqlx::Result<Option<SendingDomain>> {
    sqlx::query_as(&format!("{SELECT} where d.id = $1 and d.org_id = $2")).bind(id).bind(org_id).fetch_optional(db).await
}

pub struct Order {
    pub domain: String,
    pub price_micros: i64,
}

pub struct Setup {
    pub provider: String,
    pub forward_to: String,
    pub dmarc_email: String,
}

/// Writes a queued row per domain before anything is bought. A domain the org already has, bought, being bought or
/// unclear, is skipped, so a repeated request can't buy it twice. One that failed is queued again with a new attempt key.
pub async fn queue(db: &PgPool, org_id: Uuid, orders: &[Order], setup: &Setup) -> sqlx::Result<Vec<Uuid>> {
    let mut ids = vec![];
    for o in orders {
        let id: Option<Uuid> = sqlx::query_scalar(
            "insert into sending_domains (org_id, domain, price_micros, provider, forward_to, dmarc_email) values ($1, $2, $3, $4, $5, $6)
             on conflict (org_id, domain) do update
               set status = 'queued', attempt_key = gen_random_uuid(), price_micros = excluded.price_micros, provider = excluded.provider,
                   forward_to = excluded.forward_to, dmarc_email = excluded.dmarc_email, error = '', records = '[]', dns_status = '',
                   created_at = now()
               where sending_domains.status = 'failed'
             returning id",
        )
        .bind(org_id)
        .bind(&o.domain)
        .bind(o.price_micros)
        .bind(&setup.provider)
        .bind(&setup.forward_to)
        .bind(&setup.dmarc_email)
        .fetch_optional(db)
        .await?;
        ids.extend(id);
    }
    Ok(ids)
}

/// Buys the queued domains one at a time and sets up each one bought. Each row moves to registering before its create
/// call, so a restart mid-call leaves it there for `recover` to mark unclear instead of trying again.
pub async fn buy(db: &PgPool, pb: &Porkbun, ids: &[Uuid]) {
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            // Porkbun allows one create a second.
            tokio::time::sleep(Duration::from_millis(1100)).await;
        }
        if let Err(e) = buy_one(db, pb, *id).await {
            tracing::error!(error = ?e, domain_id = %id, "buying a sending domain");
        }
    }
}

async fn buy_one(db: &PgPool, pb: &Porkbun, id: Uuid) -> sqlx::Result<()> {
    let claimed: Option<(String, i64, Uuid)> =
        sqlx::query_as("update sending_domains set status = 'registering' where id = $1 and status = 'queued' returning domain, price_micros, attempt_key")
            .bind(id)
            .fetch_optional(db)
            .await?;
    let Some((domain, price, key)) = claimed else { return Ok(()) };
    match pb.create(&domain, price, &key.to_string()).await {
        Ok(order) => {
            sqlx::query("update sending_domains set status = 'registered', order_id = $2, error = '', registered_at = now() where id = $1")
                .bind(id)
                .bind(order)
                .execute(db)
                .await?;
            registered(db, pb, id, &domain).await
        }
        Err(Failure::Refused { message, .. }) => {
            sqlx::query("update sending_domains set status = 'failed', error = $2 where id = $1").bind(id).bind(message).execute(db).await?;
            Ok(())
        }
        Err(Failure::Unknown(why)) => {
            let error = format!("{why}. It may have been bought. Look for it in your Porkbun account, then press Check Porkbun.");
            sqlx::query("update sending_domains set status = 'uncertain', error = $2 where id = $1").bind(id).bind(error).execute(db).await?;
            Ok(())
        }
    }
}

async fn registered(db: &PgPool, pb: &Porkbun, id: Uuid, domain: &str) -> sqlx::Result<()> {
    if let Ok(Some(Some(expires))) = pb.find(domain).await {
        sqlx::query("update sending_domains set expires_on = $2 where id = $1").bind(id).bind(expires).execute(db).await?;
    }
    set_dns(db, pb, id).await
}

/// For a purchase that ended unclear: asks Porkbun whether the domain is in the account. Never buys.
pub async fn confirm(db: &PgPool, pb: &Porkbun, d: &SendingDomain) -> Result<bool, Failure> {
    let Some(expires) = pb.find(&d.domain).await? else {
        // A bought domain shows up in the account within minutes, so after a day it's safe to call it never bought.
        sqlx::query(
            "update sending_domains set
               status = case when created_at < now() - interval '1 day' then 'failed' else status end,
               error = case when created_at < now() - interval '1 day' then 'Porkbun never bought it, so you can buy it again.'
                            else 'Porkbun doesn''t list it yet. Check again in a few minutes.' end
             where id = $1 and status = 'uncertain'",
        )
        .bind(d.id)
        .execute(db)
        .await
        .map_err(|e| Failure::Unknown(e.to_string()))?;
        return Ok(false);
    };
    sqlx::query("update sending_domains set status = 'registered', error = '', registered_at = coalesce(registered_at, now()), expires_on = $2 where id = $1 and status = 'uncertain'")
        .bind(d.id)
        .bind(expires)
        .execute(db)
        .await
        .map_err(|e| Failure::Unknown(e.to_string()))?;
    set_dns(db, pb, d.id).await.map_err(|e| Failure::Unknown(e.to_string()))?;
    Ok(true)
}

/// Sets the planned records plus any the user added, and keeps how each went.
pub async fn set_dns(db: &PgPool, pb: &Porkbun, id: Uuid) -> sqlx::Result<()> {
    let row: Option<(String, String, String, String, serde_json::Value)> = sqlx::query_as(
        "update sending_domains set dns_status = 'setting' where id = $1 and status = 'registered' returning domain, provider, forward_to, dmarc_email, records",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    let Some((domain, provider, forward_to, dmarc_email, old)) = row else { return Ok(()) };
    let mut records = plan(&domain, &provider, &forward_to, &dmarc_email);
    let old: Vec<Record> = serde_json::from_value(old).unwrap_or_default();
    records.extend(old.into_iter().filter(|r| r.added));
    for r in &mut records {
        let result = pb.apply(&domain, r).await;
        r.ok = result.is_ok();
        r.error = result.err().map(|e| e.to_string()).unwrap_or_default();
    }
    let status = if records.iter().all(|r| r.ok) { "set" } else { "error" };
    sqlx::query("update sending_domains set dns_status = $2, records = $3 where id = $1").bind(id).bind(status).bind(json!(records)).execute(db).await?;
    Ok(())
}

/// Adds a record pasted from the mail provider, like its DKIM key. `name` may be given with or without the domain.
pub async fn add_record(db: &PgPool, pb: &Porkbun, d: &SendingDomain, kind: &str, name: &str, content: &str) -> Result<(), String> {
    let name = name.trim().trim_end_matches('.').to_lowercase();
    let name = if name == d.domain || name == "@" { "" } else { name.strip_suffix(&format!(".{}", d.domain)).unwrap_or(&name) };
    let content = content.trim();
    let content = if kind == "CNAME" {
        content.trim_end_matches('.').to_string()
    } else if content.contains('"') {
        // Some providers show a TXT value in quotes, or a long key split into quoted parts, which DNS joins as they are.
        content.split('"').skip(1).step_by(2).collect()
    } else {
        content.to_string()
    };
    if content.is_empty() || name.contains(char::is_whitespace) {
        return Err("Paste the record's host and value from your mail provider".into());
    }
    let mut record = Record::new(kind, name, content, None);
    record.added = true;
    let result = pb.apply(&d.domain, &record).await;
    record.ok = result.is_ok();
    record.error = result.as_ref().err().map(|e| e.to_string()).unwrap_or_default();
    let mut records: Vec<Record> = serde_json::from_value(d.records.clone()).unwrap_or_default();
    records.retain(|r| !(r.kind == record.kind && r.name == record.name && r.content == record.content));
    records.push(record);
    sqlx::query("update sending_domains set records = $2 where id = $1").bind(d.id).bind(json!(records)).execute(db).await.map_err(|e| e.to_string())?;
    result.map_err(|e| e.to_string())
}

/// Run on start, before any purchase can begin. A queued domain was never sent to Porkbun. One mid-call may have been bought.
pub async fn recover(db: &PgPool) -> sqlx::Result<()> {
    sqlx::query("update sending_domains set status = 'failed', error = 'Splee restarted before buying it. Nothing was charged.' where status = 'queued'")
        .execute(db)
        .await?;
    sqlx::query(
        "update sending_domains set status = 'uncertain',
                error = 'Splee restarted while buying it. Look for it in your Porkbun account, then press Check Porkbun.'
          where status = 'registering'",
    )
    .execute(db)
    .await?;
    sqlx::query("update sending_domains set dns_status = 'error' where dns_status = 'setting'").execute(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::porkbun::fake;

    #[test]
    fn suggestions_look_like_the_company() {
        assert_eq!(brand("https://www.Acme.com/about"), Some("acme".into()));
        assert_eq!(brand("shop.acme.co.uk"), Some("acme".into()));
        assert_eq!(brand("acme"), None);
        assert_eq!(brand("-x.com"), None);
        let s = suggestions("acme.com");
        assert!((18..=25).contains(&s.len()), "{}", s.len());
        for d in ["getacme.com", "tryacme.com", "acmehq.com", "acme-mail.com", "acmeteam.io", "acme.io"] {
            assert!(s.contains(&d.to_string()), "{d} missing from {s:?}");
        }
        assert!(!s.contains(&"acme.com".to_string()));
        // A company on .io doesn't get its own domain back.
        assert!(!suggestions("acme.io").contains(&"acme.io".to_string()));
        assert!(suggestions("localhost").is_empty());
    }

    #[test]
    fn each_provider_gets_its_known_records() {
        let line = |r: &Record| format!("{} {} {}{}", r.kind, r.name, r.prio.map(|p| format!("{p} ")).unwrap_or_default(), r.content);
        let google: Vec<String> = plan("getacme.com", "google", "https://acme.com", "dmarc@acme.com").iter().map(line).collect();
        assert_eq!(
            google,
            [
                "MX  1 smtp.google.com",
                "TXT  v=spf1 include:_spf.google.com ~all",
                "TXT _dmarc v=DMARC1; p=none; rua=mailto:dmarc@acme.com",
                "URL  https://acme.com"
            ]
        );
        let microsoft = plan("get-acme.co", "microsoft", "", "");
        assert_eq!(line(&microsoft[0]), "MX  0 get-acme-co.mail.protection.outlook.com");
        assert_eq!(line(microsoft.last().unwrap()), "TXT _dmarc v=DMARC1; p=none");
        let migadu: Vec<String> = plan("getacme.com", "migadu", "", "").iter().map(line).collect();
        assert!(migadu.contains(&"CNAME key2._domainkey key2.getacme.com._domainkey.migadu.com".to_string()), "{migadu:?}");
    }

    async fn org(db: &PgPool) -> Uuid {
        sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap()
    }

    fn orders(domains: &[&str]) -> Vec<Order> {
        domains.iter().map(|d| Order { domain: d.to_string(), price_micros: fake::PRICE_CENTS * 10_000 }).collect()
    }

    fn google() -> Setup {
        Setup { provider: "google".into(), forward_to: "https://acme.com".into(), dmarc_email: String::new() }
    }

    async fn status(db: &PgPool, org: Uuid, domain: &str) -> (String, String, String) {
        sqlx::query_as("select status, dns_status, error from sending_domains where org_id = $1 and domain = $2").bind(org).bind(domain).fetch_one(db).await.unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn bought_domains_get_their_dns_and_a_retry_never_buys_twice(db: PgPool) {
        let (base, porkbun) = fake::start().await;
        let pb = Porkbun::new(&base, "pk1_good", "sk1_good");
        let org = org(&db).await;
        let ids = queue(&db, org, &orders(&["getacme.com", "acmetaken.com"]), &google()).await.unwrap();
        buy(&db, &pb, &ids).await;

        assert_eq!(status(&db, org, "getacme.com").await, ("registered".into(), "set".into(), String::new()));
        let (taken, _, why) = status(&db, org, "acmetaken.com").await;
        assert_eq!((taken.as_str(), why.as_str()), ("failed", "Someone registered it since the search."));
        let d = list(&db, org).await.unwrap().into_iter().find(|d| d.domain == "getacme.com").unwrap();
        assert_eq!(d.expires_on, chrono::NaiveDate::from_ymd_opt(2027, 11, 10));
        assert_eq!(
            porkbun.lock().unwrap().records,
            [
                "getacme.com MX  1 smtp.google.com",
                "getacme.com TXT  v=spf1 include:_spf.google.com ~all",
                "getacme.com TXT _dmarc v=DMARC1; p=none",
                "getacme.com URL  https://acme.com"
            ]
        );

        // The same order again: the bought domain is skipped, the failed one is tried again.
        let again = queue(&db, org, &orders(&["getacme.com", "acmetaken.com"]), &google()).await.unwrap();
        assert_eq!(again.len(), 1);
        buy(&db, &pb, &again).await;
        assert_eq!(porkbun.lock().unwrap().bought, ["getacme.com"]);

        // The DKIM key pasted from Google goes in, and setting DNS again keeps it without adding anything twice.
        let d = load(&db, d.id, org).await.unwrap().unwrap();
        add_record(&db, &pb, &d, "TXT", "google._domainkey.getacme.com.", "\"v=DKIM1; k=rsa; \" \"p=MIIBIj\"").await.unwrap();
        set_dns(&db, &pb, d.id).await.unwrap();
        let d = load(&db, d.id, org).await.unwrap().unwrap();
        assert_eq!(dkim_selector(&d).as_deref(), Some("google"));
        assert_eq!(d.dns_status, "set");
        let records = porkbun.lock().unwrap().records.clone();
        assert_eq!(records.len(), 5);
        assert_eq!(records[4], "getacme.com TXT google._domainkey v=DKIM1; k=rsa; p=MIIBIj");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_unclear_purchase_is_checked_not_retried(db: PgPool) {
        let (base, porkbun) = fake::start().await;
        let impatient = crate::porkbun::with_timeout(&base, "pk1_good", "sk1_good", Duration::from_millis(500));
        let org = org(&db).await;
        let ids = queue(&db, org, &orders(&["timeoutacme.com"]), &google()).await.unwrap();
        buy(&db, &impatient, &ids).await;
        let (unclear, _, why) = status(&db, org, "timeoutacme.com").await;
        assert_eq!(unclear, "uncertain");
        assert!(why.contains("Check Porkbun"), "{why}");

        // Buying it again does nothing while it's unclear.
        assert!(queue(&db, org, &orders(&["timeoutacme.com"]), &google()).await.unwrap().is_empty());
        // Porkbun doesn't list one it never bought. It stays unclear for a day, then can be bought again.
        let never = queue(&db, org, &orders(&["nevergot.com"]), &google()).await.unwrap();
        sqlx::query("update sending_domains set status = 'uncertain' where id = $1").bind(never[0]).execute(&db).await.unwrap();
        let pb = Porkbun::new(&base, "pk1_good", "sk1_good");
        let n = load(&db, never[0], org).await.unwrap().unwrap();
        assert!(!confirm(&db, &pb, &n).await.unwrap());
        assert_eq!(status(&db, org, "nevergot.com").await.0, "uncertain");
        sqlx::query("update sending_domains set created_at = now() - interval '2 days' where id = $1").bind(never[0]).execute(&db).await.unwrap();
        assert!(!confirm(&db, &pb, &n).await.unwrap());
        assert_eq!(status(&db, org, "nevergot.com").await.0, "failed");
        assert_eq!(queue(&db, org, &orders(&["nevergot.com"]), &google()).await.unwrap(), never);

        // Porkbun did buy it, which the check finds, and then the DNS goes on.
        let d = load(&db, ids[0], org).await.unwrap().unwrap();
        assert!(confirm(&db, &pb, &d).await.unwrap());
        assert_eq!(status(&db, org, "timeoutacme.com").await, ("registered".into(), "set".into(), String::new()));
        assert_eq!(porkbun.lock().unwrap().bought, ["timeoutacme.com"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn refusals_charge_nothing_and_say_why(db: PgPool) {
        let (base, porkbun) = fake::start().await;
        let org = org(&db).await;
        porkbun.lock().unwrap().balance = 0;
        let pb = Porkbun::new(&base, "pk1_good", "sk1_good");
        let ids = queue(&db, org, &orders(&["getacme.com"]), &google()).await.unwrap();
        buy(&db, &pb, &ids).await;
        let (failed, _, why) = status(&db, org, "getacme.com").await;
        assert_eq!((failed.as_str(), why.as_str()), ("failed", "Your Porkbun credit is too low by $11.08. Add credit at porkbun.com/account/credit, then buy it again."));

        let wrong = Porkbun::new(&base, "pk1_good", "sk1_old");
        let ids = queue(&db, org, &orders(&["getacme.com"]), &google()).await.unwrap();
        buy(&db, &wrong, &ids).await;
        assert_eq!(status(&db, org, "getacme.com").await.2, "Invalid API key. (002)");

        // A domain bought with API access off is kept, with the fix next to its DNS.
        porkbun.lock().unwrap().balance = 5000;
        let ids = queue(&db, org, &orders(&["noapiacme.com"]), &google()).await.unwrap();
        buy(&db, &pb, &ids).await;
        let d = load(&db, ids[0], org).await.unwrap().unwrap();
        assert_eq!((d.status.as_str(), d.dns_status.as_str()), ("registered", "error"));
        assert!(d.records[0]["error"].as_str().unwrap().contains("Turn on API access"));
        assert_eq!(porkbun.lock().unwrap().bought, ["noapiacme.com"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn domains_belong_to_their_org(db: PgPool) {
        let (ours, theirs) = (org(&db).await, org(&db).await);
        let ids = queue(&db, ours, &orders(&["getacme.com"]), &google()).await.unwrap();
        // Another org can queue the same name, which Porkbun then refuses, and can't see or load ours.
        assert_eq!(queue(&db, theirs, &orders(&["getacme.com"]), &google()).await.unwrap().len(), 1);
        assert!(load(&db, ids[0], theirs).await.unwrap().is_none());
        assert_eq!(list(&db, theirs).await.unwrap().len(), 1);
        sqlx::query("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, warmup) values ($1, 'ana@GetAcme.com', 'smtp.test', '', '', true)")
            .bind(ours)
            .execute(&db)
            .await
            .unwrap();
        let mine = load(&db, ids[0], ours).await.unwrap().unwrap();
        assert_eq!((mine.mailboxes, mine.warming), (1, 1));
        assert_eq!(list(&db, theirs).await.unwrap()[0].mailboxes, 0);

        recover(&db).await.unwrap();
        assert_eq!(status(&db, ours, "getacme.com").await.0, "failed");
    }
}
