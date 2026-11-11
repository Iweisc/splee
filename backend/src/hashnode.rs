//! Hashnode through its GraphQL API, with a personal access token from the user's Hashnode settings. Articles go in one of the
//! account's publications, and comments on anyone's article. A Hashnode article can live on any domain, so the link Splee
//! keeps for one carries its id as ?hashnode=, which the article's page ignores.

use std::time::Duration;

use reqwest::{StatusCode, Url};
use serde_json::{Value, json};

use crate::social::Failure;

const API: &str = "https://gql.hashnode.com";
pub const TITLE_CHARS: usize = 250;
pub const MAX_TAGS: usize = 5;
const PARAM: &str = "hashnode";

/// Tags as Hashnode slugs: lowercase letters, digits and hyphens, at most 5 without repeats.
pub fn tags(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in raw {
        let slug: String = tag.trim().trim_start_matches('#').to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-");
        if !slug.is_empty() && slug.chars().count() <= 50 && !out.contains(&slug) {
            out.push(slug);
        }
    }
    out.truncate(MAX_TAGS);
    out
}

/// Hashnode's ids are 24 hex characters.
fn is_id(id: &str) -> bool {
    id.len() == 24 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// The link Splee keeps for an article: its URL, without query or fragment, plus its id.
pub fn post_link(url: &str, id: &str) -> Option<String> {
    let mut url = Url::parse(url.trim()).ok()?;
    if url.scheme() != "https" || !is_id(id) {
        return None;
    }
    url.set_fragment(None);
    url.set_query(Some(&format!("{PARAM}={id}")));
    Some(url.into())
}

/// The article's id, from a link in the form post_link gives.
pub fn post_id(link: &str) -> Option<String> {
    let url = Url::parse(link.trim()).ok()?;
    let id = url.query_pairs().find(|(k, _)| k == PARAM)?.1.into_owned();
    (url.scheme() == "https" && is_id(&id)).then_some(id)
}

/// The account a token belongs to and where it can publish.
pub struct Account {
    pub username: String,
    /// [{id, title, url}], as hashnode_publications keeps them.
    pub publications: Vec<Value>,
}

pub struct Hashnode {
    client: reqwest::Client,
    api: String,
    token: String,
}

impl Hashnode {
    pub fn new(token: &str) -> Result<Self, Failure> {
        Self::at(API, token)
    }

    fn at(api: &str, token: &str) -> Result<Self, Failure> {
        let client = reqwest::Client::builder().user_agent("Splee").timeout(Duration::from_secs(30)).build().map_err(|e| Failure::Account(e.to_string()))?;
        Ok(Self { client, api: api.into(), token: token.into() })
    }

    /// A bad token, a missing permission and rate limits are about the account. Anything else, like a deleted article or a
    /// request that timed out and may have gone through, is about the post. GraphQL errors come with HTTP 200 and a code.
    async fn call(&self, query: &str, variables: Value) -> Result<Value, Failure> {
        let request = self.client.post(&self.api).header("Authorization", &self.token).json(&json!({ "query": query, "variables": variables }));
        let res = request.send().await.map_err(|e| Failure::Post(format!("Hashnode didn't answer: {}", e.without_url())))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        let error = &body["errors"][0];
        let said = error["message"].as_str().unwrap_or_default();
        let paren = if said.is_empty() { String::new() } else { format!(" ({said})") };
        let refused = || Failure::Account("Hashnode didn't accept the access token. Make a new one and connect Hashnode again".into());
        Err(match (status, error["extensions"]["code"].as_str().unwrap_or_default()) {
            // A server error may come after the article was saved, so it never goes back in the queue.
            (s, _) if s.is_server_error() => Failure::Post(format!("Hashnode answered {s}{paren}")),
            (StatusCode::UNAUTHORIZED, _) | (_, "UNAUTHENTICATED") => refused(),
            (StatusCode::FORBIDDEN, _) | (_, "FORBIDDEN") => Failure::Account(format!("Hashnode won't let the account do that{paren}")),
            (StatusCode::TOO_MANY_REQUESTS, _) | (_, "TOO_MANY_REQUESTS" | "RATE_LIMITED") => Failure::Account("Hashnode is rate limiting the account".into()),
            (_, "NOT_FOUND") => Failure::Post("Hashnode couldn't find it. It may have been deleted".into()),
            (s, _) if s.is_success() && error.is_null() => return Ok(body["data"].clone()),
            (_, _) if !said.is_empty() => Failure::Post(said.into()),
            (s, _) => Failure::Post(format!("Hashnode answered {s}")),
        })
    }

    pub async fn me(&self) -> Result<Account, Failure> {
        let data = self.call("query { me { username publications(first: 20) { edges { node { id title url } } } } }", json!({})).await?;
        let username = data["me"]["username"].as_str().filter(|u| !u.is_empty()).ok_or_else(|| Failure::Account("Hashnode didn't say which account this is".into()))?;
        let edges = data["me"]["publications"]["edges"].as_array().cloned().unwrap_or_default();
        let publications = edges.iter().map(|e| json!({ "id": e["node"]["id"], "title": e["node"]["title"], "url": e["node"]["url"] })).collect();
        Ok(Account { username: username.into(), publications })
    }

    /// The newest articles with the tag, keeping those whose title or summary has every word of `words` when there are any.
    /// Hashnode's API can't search all of Hashnode by keyword.
    pub async fn search(&self, tag: &str, words: &str, limit: usize) -> Result<Vec<Value>, Failure> {
        let query = "query Tagged($slug: String!, $first: Int!) { tag(slug: $slug) { posts(first: $first, filter: { sortBy: recent }) { edges { node {
            id title brief url publishedAt responseCount author { username } } } } } }";
        let first = if words.trim().is_empty() { limit } else { 50 };
        let slug = tags(&[tag.to_string()]).pop().unwrap_or_default();
        let data = self.call(query, json!({ "slug": slug, "first": first })).await?;
        let words: Vec<String> = words.split_whitespace().map(str::to_lowercase).collect();
        let has = |post: &Value| {
            let text = format!("{} {}", post["title"].as_str().unwrap_or_default(), post["brief"].as_str().unwrap_or_default()).to_lowercase();
            words.iter().all(|w| text.contains(w))
        };
        let edges = data["tag"]["posts"]["edges"].as_array().cloned().unwrap_or_default();
        Ok(edges.into_iter().map(|e| e["node"].clone()).filter(has).take(limit).collect())
    }

    /// An article with its text in Markdown and up to 20 comments.
    pub async fn post(&self, id: &str) -> Result<Value, Failure> {
        let query = "query Post($id: ID!) { post(id: $id) { id title url publishedAt responseCount author { username } content { markdown }
            preferences { disableComments } comments(first: 20) { edges { node { id dateAdded author { username } content { markdown } } } } } }";
        let data = self.call(query, json!({ "id": id })).await?;
        if data["post"].is_null() {
            return Err(Failure::Post("Hashnode couldn't find it. It may have been deleted".into()));
        }
        Ok(data["post"].clone())
    }

    /// Comments on the article at `link`, in the form post_link gives, and returns the article's link.
    pub async fn comment(&self, link: &str, markdown: &str) -> Result<String, Failure> {
        let id = post_id(link).ok_or_else(|| Failure::Post("That isn't a link to a Hashnode article".into()))?;
        let query = "mutation AddComment($input: AddCommentInput!) { addComment(input: $input) { comment { id } } }";
        self.call(query, json!({ "input": { "postId": id, "contentMarkdown": markdown } })).await?;
        let mut url = Url::parse(link).map_err(|e| Failure::Post(e.to_string()))?;
        url.set_query(None);
        Ok(url.into())
    }

    /// Publishes an article in the publication and returns its link.
    pub async fn publish(&self, publication: &str, title: &str, markdown: &str, tags: &[String]) -> Result<String, Failure> {
        let query = "mutation PublishPost($input: PublishPostInput!) { publishPost(input: $input) { post { id url } } }";
        let tags: Vec<Value> = tags.iter().map(|t| json!({ "slug": t, "name": t })).collect();
        let input = json!({ "title": title, "contentMarkdown": markdown, "publicationId": publication, "tags": tags });
        let made = self.call(query, json!({ "input": input })).await?;
        // It's out now, so not getting its link back doesn't fail it.
        Ok(made["publishPost"]["post"]["url"].as_str().unwrap_or_default().into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::routing::post;
    use axum::{Json, Router};

    use super::*;

    const ID: &str = "66f0c1a2b3c4d5e6f7a8b9c0";

    #[test]
    fn links_and_tags_get_one_form() {
        let link = post_link("https://blog.acme.dev/warming-up-a-domain?source=feed#top", ID).unwrap();
        assert_eq!(link, format!("https://blog.acme.dev/warming-up-a-domain?hashnode={ID}"));
        assert_eq!(post_id(&link).as_deref(), Some(ID));
        assert_eq!(post_link("http://blog.acme.dev/x", ID), None);
        assert_eq!(post_link("https://blog.acme.dev/x", "123"), None);
        assert_eq!(post_id("https://blog.acme.dev/x"), None);
        let raw = ["Cold Email", "#sales", "sales", "B2B SaaS!", "", "growth", "devops", "extra"].map(String::from);
        assert_eq!(tags(&raw), ["cold-email", "sales", "b2b-saas", "growth", "devops"]);
    }

    type Sent = Arc<Mutex<Vec<Value>>>;

    /// Hashnode's GraphQL API as its docs describe it. The token picks whether the account works, and the text how posting goes.
    async fn fake_hashnode() -> (String, Sent) {
        fn error(code: &str, message: &str) -> Json<Value> {
            Json(json!({ "data": null, "errors": [{ "message": message, "extensions": { "code": code } }] }))
        }
        async fn graphql(State(sent): State<Sent>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
            let ok = |data: Value| (StatusCode::OK, Json(json!({ "data": data })));
            if headers["authorization"] != "GOOD" {
                return (StatusCode::OK, error("UNAUTHENTICATED", "Invalid token"));
            }
            let (query, vars) = (body["query"].as_str().unwrap().to_string(), body["variables"].clone());
            if query.starts_with("query { me") {
                let node = json!({ "node": { "id": "pub1", "title": "Acme notes", "url": "https://blog.acme.dev", "followers": 9 } });
                return ok(json!({ "me": { "username": "acme", "publications": { "edges": [node] } } }));
            }
            if query.contains("tag(slug") {
                assert_eq!(vars["slug"], "cold-email");
                let post = |id: &str, title: &str| json!({ "node": { "id": id, "title": title, "brief": "", "url": format!("https://bo.dev/{id}") } });
                return ok(json!({ "tag": { "posts": { "edges": [post("a", "Warming up a domain"), post("b", "Writing subject lines")] } } }));
            }
            let text = vars["input"]["contentMarkdown"].as_str().unwrap_or_default();
            match text {
                "fast" => (StatusCode::TOO_MANY_REQUESTS, Json(json!({ "errors": [{ "message": "Too many requests" }] }))),
                "gone" => (StatusCode::OK, error("NOT_FOUND", "Post not found")),
                "bad" => (StatusCode::OK, error("BAD_USER_INPUT", "Title is required")),
                "nope" => (StatusCode::OK, error("FORBIDDEN", "You are not a member of this publication")),
                "down" => (StatusCode::BAD_GATEWAY, Json(json!({}))),
                _ => {
                    sent.lock().unwrap().push(vars);
                    if query.contains("addComment") {
                        ok(json!({ "addComment": { "comment": { "id": "c1" } } }))
                    } else {
                        ok(json!({ "publishPost": { "post": { "id": ID, "url": "https://blog.acme.dev/how-we-warm-up" } } }))
                    }
                }
            }
        }
        let sent = Sent::default();
        let app = Router::new().route("/", post(graphql)).with_state(sent.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, sent)
    }

    #[tokio::test]
    async fn comments_and_articles_go_out_and_failures_say_whose_problem_it_is() {
        let (base, sent) = fake_hashnode().await;
        let me = Hashnode::at(&base, "GOOD").unwrap().me().await.ok().unwrap();
        assert_eq!(me.username, "acme");
        assert_eq!(me.publications, [json!({ "id": "pub1", "title": "Acme notes", "url": "https://blog.acme.dev" })]);
        assert!(matches!(Hashnode::at(&base, "OLD").unwrap().me().await, Err(Failure::Account(e)) if e.contains("connect Hashnode again")));

        let h = Hashnode::at(&base, "GOOD").unwrap();
        let found = h.search("#Cold Email", "warming", 10).await.ok().unwrap();
        assert_eq!(found.iter().map(|p| p["id"].as_str().unwrap()).collect::<Vec<_>>(), ["a"]);

        let link = format!("https://bo.dev/warming-up?hashnode={ID}");
        assert_eq!(h.comment(&link, "Four weeks, 20 a day").await.ok().unwrap(), "https://bo.dev/warming-up");
        let tags = ["cold-email".to_string()];
        assert_eq!(h.publish("pub1", "How we warm up", "# Day one", &tags).await.ok().unwrap(), "https://blog.acme.dev/how-we-warm-up");
        assert_eq!(*sent.lock().unwrap(), [
            json!({ "input": { "postId": ID, "contentMarkdown": "Four weeks, 20 a day" } }),
            json!({ "input": { "title": "How we warm up", "contentMarkdown": "# Day one", "publicationId": "pub1", "tags": [{ "slug": "cold-email", "name": "cold-email" }] } }),
        ]);

        assert!(matches!(h.comment(&link, "gone").await, Err(Failure::Post(e)) if e.contains("deleted")));
        assert!(matches!(h.comment(&link, "bad").await, Err(Failure::Post(e)) if e == "Title is required"));
        assert!(matches!(h.comment(&link, "fast").await, Err(Failure::Account(e)) if e.contains("rate limiting")));
        assert!(matches!(h.publish("pub2", "Hi", "nope", &[]).await, Err(Failure::Account(e)) if e.contains("not a member")));
        assert!(matches!(h.publish("pub1", "Hi", "down", &[]).await, Err(Failure::Post(_))));
        assert!(matches!(h.comment("https://bo.dev/warming-up", "Hi").await, Err(Failure::Post(_))));
        assert!(matches!(Hashnode::at(&base, "OLD").unwrap().comment(&link, "Hi").await, Err(Failure::Account(_))));
    }
}
