//! Google Places API (New) Text Search (https://developers.google.com/maps/documentation/places/web-service/text-search), for
//! finding local businesses by what they are and where. Each organization pastes its own Google Maps Platform key.

use std::time::Duration;

use serde_json::{Value, json};

pub const API_URL: &str = "https://places.googleapis.com";

/// What a search asks for. Phone, website, rating and review count make each call the Text Search Enterprise SKU, and
/// businessStatus lets closed places be left out.
const FIELDS: &str = "places.id,places.displayName,places.types,places.primaryTypeDisplayName,places.formattedAddress,places.addressComponents,\
places.location,places.internationalPhoneNumber,places.websiteUri,places.rating,places.userRatingCount,places.googleMapsUri,places.businessStatus,nextPageToken";

/// Text Search answers at most 20 places a page and 3 pages a query.
pub const MAX_PAGES: usize = 3;

/// US cents per 1,000 Text Search calls at the fields above: the Text Search Enterprise SKU's $35.00 per 1,000 for the first
/// 100,000 calls a month, from https://developers.google.com/maps/billing-and-pricing/pricing (Places API, checked 2026-11).
/// Google's monthly free calls aren't taken off, so the cost shown is at most what Google bills.
pub const PRICE_PER_1000_CENTS: i64 = 3500;

pub struct Places {
    client: reqwest::Client,
    base: String,
    key: String,
}

/// A place as Google described it, with the parts Splee keeps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Place {
    pub place_id: String,
    pub name: String,
    pub category: String,
    pub types: Vec<String>,
    pub address: String,
    pub city: String,
    pub region: String,
    pub country_code: String,
    pub lat: Option<f64>,
    pub lng: Option<f64>,
    pub phone: String,
    pub website: String,
    pub rating: Option<f32>,
    pub review_count: i32,
    pub maps_url: String,
}

pub struct Page {
    pub places: Vec<Place>,
    pub next: Option<String>,
}

impl Places {
    pub fn new(base: &str, key: &str) -> Self {
        let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("reqwest client builds");
        Self { client, base: base.trim_end_matches('/').into(), key: key.trim().into() }
    }

    async fn call(&self, body: Value, fields: &str) -> anyhow::Result<Value> {
        let res = self
            .client
            .post(format!("{}/v1/places:searchText", self.base))
            .header("X-Goog-Api-Key", &self.key)
            .header("X-Goog-FieldMask", fields)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("Couldn't reach Google: {}", e.without_url()))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!(explain(status.as_u16(), &body));
        }
        Ok(body)
    }

    /// One cheap call to see whether Google takes the key. Asking for place ids only is the Text Search Essentials (IDs Only)
    /// SKU, which Google doesn't charge for.
    pub async fn check(&self) -> anyhow::Result<()> {
        self.call(json!({ "textQuery": "coffee in London", "pageSize": 1 }), "places.id").await.map(|_| ())
    }

    /// One page of up to 20 places for "{what} in {place}". `token` is the previous page's `next`.
    pub async fn search(&self, query: &str, token: Option<&str>) -> anyhow::Result<Page> {
        let mut body = json!({ "textQuery": query, "pageSize": 20 });
        if let Some(token) = token {
            body["pageToken"] = json!(token);
        }
        let body = self.call(body, FIELDS).await?;
        let places = body["places"].as_array().into_iter().flatten().filter(|p| p["businessStatus"] != "CLOSED_PERMANENTLY").filter_map(place).collect();
        let next = body["nextPageToken"].as_str().filter(|t| !t.is_empty()).map(String::from);
        Ok(Page { places, next })
    }
}

/// Google's error in plain words for the ones people can fix, like a key without Places API (New) turned on.
fn explain(status: u16, body: &Value) -> String {
    let error = &body["error"];
    let message = error["message"].as_str().unwrap_or_default().trim();
    let reason = error["details"].as_array().into_iter().flatten().find_map(|d| d["reason"].as_str()).unwrap_or_default();
    match reason {
        "API_KEY_INVALID" => "Google didn't accept this API key. Copy it again from Google Cloud under APIs & Services, then Credentials.".into(),
        "SERVICE_DISABLED" => "Turn on Places API (New) in the key's Google Cloud project, then try again.".into(),
        "BILLING_DISABLED" => "Google needs billing turned on in the key's Google Cloud project before it answers searches.".into(),
        "API_KEY_SERVICE_BLOCKED" | "API_KEY_HTTP_REFERRER_BLOCKED" | "API_KEY_IP_ADDRESS_BLOCKED" => {
            "This key's restrictions block Places API (New) from this server. Allow it under the key's API restrictions.".into()
        }
        _ if status == 429 => "Google is limiting this key's searches. Wait a minute and try again.".into(),
        _ if message.is_empty() => format!("Google answered {status} without saying why."),
        _ => format!("Google refused the search: {message}"),
    }
}

fn place(p: &Value) -> Option<Place> {
    let text = |v: &Value| v.as_str().unwrap_or_default().trim().to_string();
    let component = |kind: &str, short: bool| {
        p["addressComponents"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["types"].as_array().is_some_and(|t| t.iter().any(|t| t == kind)))
            .map(|c| text(if short { &c["shortText"] } else { &c["longText"] }))
    };
    let country_code = component("country", true).unwrap_or_default().to_uppercase();
    let raw_phone = text(&p["internationalPhoneNumber"]);
    Some(Place {
        place_id: text(&p["id"]),
        name: text(&p["displayName"]["text"]),
        category: text(&p["primaryTypeDisplayName"]["text"]),
        types: p["types"].as_array().into_iter().flatten().filter_map(|t| t.as_str().map(String::from)).collect(),
        address: text(&p["formattedAddress"]),
        city: component("locality", false).or_else(|| component("postal_town", false)).unwrap_or_default(),
        region: component("administrative_area_level_1", true).unwrap_or_default(),
        lat: p["location"]["latitude"].as_f64(),
        lng: p["location"]["longitude"].as_f64(),
        phone: crate::phone::normalize(&raw_phone, &country_code).unwrap_or(raw_phone),
        country_code,
        website: text(&p["websiteUri"]),
        rating: p["rating"].as_f64().map(|r| r as f32),
        review_count: p["userRatingCount"].as_i64().unwrap_or(0) as i32,
        maps_url: text(&p["googleMapsUri"]),
    })
    .filter(|p| !p.place_id.is_empty() && !p.name.is_empty())
}

#[cfg(test)]
pub mod fake {
    //! A stand-in for Text Search. Every place whose name holds the first four letters of the query is a match, so "dentists"
    //! finds Smile Dental, 20 to a page, and
    //! `calls` counts the requests. The key "bad" is refused the way Google refuses an unknown key.

    use std::sync::{Arc, Mutex};

    use axum::extract::State;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::{Value, json};

    /// The text query and page token of a call.
    type Call = (String, Option<String>);

    #[derive(Clone, Default)]
    pub struct Fake {
        pub places: Arc<Mutex<Vec<Value>>>,
        /// The searches. Key checks, which ask for ids only and are free, are counted in `checks` instead.
        pub calls: Arc<Mutex<Vec<Call>>>,
        pub checks: Arc<Mutex<usize>>,
    }

    impl Fake {
        pub fn calls(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    /// A place in Google's shape, in Austin, TX. An empty website leaves websiteUri out.
    pub fn place(id: &str, name: &str, website: &str, rating: f64, reviews: i64) -> Value {
        let mut p = json!({
            "id": id,
            "displayName": { "text": name, "languageCode": "en" },
            "types": ["dentist", "health", "point_of_interest", "establishment"],
            "primaryTypeDisplayName": { "text": "Dentist", "languageCode": "en-US" },
            "formattedAddress": "100 Congress Ave, Austin, TX 78701, USA",
            "addressComponents": [
                { "longText": "Austin", "shortText": "Austin", "types": ["locality", "political"] },
                { "longText": "Texas", "shortText": "TX", "types": ["administrative_area_level_1", "political"] },
                { "longText": "United States", "shortText": "US", "types": ["country", "political"] }
            ],
            "location": { "latitude": 30.2672, "longitude": -97.7431 },
            "internationalPhoneNumber": "+1 512-555-0100",
            "rating": rating,
            "userRatingCount": reviews,
            "googleMapsUri": format!("https://maps.google.com/?cid={id}"),
            "businessStatus": "OPERATIONAL",
        });
        if !website.is_empty() {
            p["websiteUri"] = json!(website);
        }
        p
    }

    async fn search(State(fake): State<Fake>, headers: HeaderMap, Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        if headers.get("x-goog-api-key").and_then(|k| k.to_str().ok()) == Some("bad") {
            let error = json!({ "error": { "code": 400, "message": "API key not valid. Please pass a valid API key.", "status": "INVALID_ARGUMENT",
                "details": [{ "@type": "type.googleapis.com/google.rpc.ErrorInfo", "reason": "API_KEY_INVALID" }] } });
            return (StatusCode::BAD_REQUEST, Json(error));
        }
        let mask = headers.get("x-goog-fieldmask").and_then(|m| m.to_str().ok()).expect("Google refuses a search without a field mask");
        let query = body["textQuery"].as_str().unwrap_or_default().to_lowercase();
        let token = body["pageToken"].as_str().map(String::from);
        if mask == "places.id" {
            *fake.checks.lock().unwrap() += 1;
        } else {
            fake.calls.lock().unwrap().push((query.clone(), token.clone()));
        }
        let word: String = query.chars().take(4).collect();
        let found: Vec<Value> = fake
            .places
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p["displayName"]["text"].as_str().unwrap_or_default().to_lowercase().contains(&word))
            .cloned()
            .collect();
        let start: usize = token.as_deref().and_then(|t| t.strip_prefix("page-")).and_then(|n| n.parse().ok()).unwrap_or(0);
        let page: Vec<Value> = found.iter().skip(start).take(20).cloned().collect();
        let mut answer = json!({ "places": page });
        if found.len() > start + 20 && start + 20 < 60 {
            answer["nextPageToken"] = json!(format!("page-{}", start + 20));
        }
        (StatusCode::OK, Json(answer))
    }

    /// Starts the fake and returns the base URL to use as Config::places_api_url.
    pub async fn start() -> (String, Fake) {
        let fake = Fake::default();
        let app = Router::new().route("/v1/places:searchText", post(search)).with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await });
        (base, fake)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[tokio::test]
    async fn the_key_check_tells_a_good_key_from_a_rejected_one() {
        let (base, fake) = super::fake::start().await;
        assert!(super::Places::new(&base, "good").check().await.is_ok());
        let refused = super::Places::new(&base, "bad").check().await.unwrap_err().to_string();
        assert_eq!(refused, "Google didn't accept this API key. Copy it again from Google Cloud under APIs & Services, then Credentials.");
        assert_eq!((*fake.checks.lock().unwrap(), fake.calls()), (1, 0), "the refused key never got an answer, and a check is no search");
    }

    #[test]
    fn places_keep_their_parts_and_errors_read_plainly() {
        let p = super::place(&super::fake::place("p1", "Smile Dental", "https://www.smile.example/", 4.8, 120)).unwrap();
        assert_eq!((p.city.as_str(), p.region.as_str(), p.country_code.as_str(), p.phone.as_str()), ("Austin", "TX", "US", "+15125550100"));
        assert_eq!((p.category.as_str(), p.rating, p.review_count), ("Dentist", Some(4.8), 120));
        let disabled = json!({ "error": { "code": 403, "message": "Places API (New) has not been used in project 1 before or it is disabled.",
            "details": [{ "reason": "SERVICE_DISABLED" }] } });
        assert_eq!(super::explain(403, &disabled), "Turn on Places API (New) in the key's Google Cloud project, then try again.");
        assert_eq!(super::explain(500, &json!({})), "Google answered 500 without saying why.");
    }
}
