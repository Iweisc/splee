//! S3 as its docs describe it, on a local port, for the export tests: PUT and DELETE of an object, and multipart uploads. Every
//! request goes here through Config::s3_api_url, with the Host it was signed for, so the bucket comes from the Host's first
//! label for a virtual-hosted request and from the path otherwise. It checks that a request is presigned with a known access
//! key but not the signature itself, which the AWS example in tests.rs covers.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use reqwest::Url;

pub const KEY_ID: &str = "AKIDSPLEETEST";

#[derive(Clone, Debug)]
pub struct Seen {
    pub method: Method,
    pub host: String,
    /// As sent, still percent-encoded.
    pub path: String,
    pub query: HashMap<String, String>,
    pub bytes: usize,
}

/// A multipart upload in progress: the bucket, the key and the parts by number.
type Upload = (String, String, BTreeMap<u16, Vec<u8>>);

#[derive(Default)]
pub struct World {
    pub requests: Vec<Seen>,
    /// By bucket, its objects by key.
    pub buckets: BTreeMap<String, BTreeMap<String, Vec<u8>>>,
    /// Buckets that turn every request away with AccessDenied.
    pub denied: HashSet<String>,
    uploads: HashMap<String, Upload>,
    /// Requests whose path contains this answer 500 with an InternalError.
    pub broken: Option<String>,
}

#[derive(Clone)]
pub struct Fake {
    pub base: String,
    pub world: Arc<Mutex<World>>,
}

impl Fake {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake = Fake { base: format!("http://{}", listener.local_addr().unwrap()), world: Arc::default() };
        let app = Router::new().fallback(handle).with_state(fake.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        fake
    }

    pub fn bucket(&self, name: &str) {
        self.world.lock().unwrap().buckets.entry(name.into()).or_default();
    }

    pub fn object(&self, bucket: &str, key: &str) -> Option<Vec<u8>> {
        self.world.lock().unwrap().buckets.get(bucket)?.get(key).cloned()
    }

    pub fn keys(&self, bucket: &str) -> Vec<String> {
        self.world.lock().unwrap().buckets.get(bucket).map(|b| b.keys().cloned().collect()).unwrap_or_default()
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.world.lock().unwrap().requests.clone()
    }
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    let xml = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>{message}</Message></Error>");
    (status, [(header::CONTENT_TYPE, "application/xml")], xml).into_response()
}

fn decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() && let Ok(b) = u8::from_str_radix(&path[i + 1..i + 3], 16) {
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

async fn handle(State(fake): State<Fake>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default().to_string();
    let query: HashMap<String, String> = Url::parse(&format!("http://x/?{}", uri.query().unwrap_or_default())).unwrap().query_pairs().into_owned().collect();
    let mut world = fake.world.lock().unwrap();
    world.requests.push(Seen { method: method.clone(), host: host.clone(), path: uri.path().into(), query: query.clone(), bytes: body.len() });

    let key_id = query.get("X-Amz-Credential").and_then(|c| c.split('/').next()).unwrap_or_default();
    if !query.contains_key("X-Amz-Signature") || query.get("X-Amz-Algorithm").map(String::as_str) != Some("AWS4-HMAC-SHA256") {
        return error(StatusCode::FORBIDDEN, "AccessDenied", "Request is not signed");
    }
    if key_id != KEY_ID {
        return error(StatusCode::FORBIDDEN, "InvalidAccessKeyId", "The AWS Access Key Id you provided does not exist in our records.");
    }
    if world.broken.as_ref().is_some_and(|b| uri.path().contains(b.as_str())) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "InternalError", "We encountered an internal error. Please try again.");
    }
    let label = host.split('.').next().unwrap_or_default();
    let (bucket, key) = if host.contains('.') && world.buckets.contains_key(label) {
        (label.to_string(), decode(uri.path().trim_start_matches('/')))
    } else {
        let path = uri.path().trim_start_matches('/');
        let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
        (decode(bucket), decode(key))
    };
    if !world.buckets.contains_key(&bucket) {
        return error(StatusCode::NOT_FOUND, "NoSuchBucket", "The specified bucket does not exist");
    }
    if world.denied.contains(&bucket) {
        return error(StatusCode::FORBIDDEN, "AccessDenied", "Access Denied");
    }
    let etag = |bytes: &[u8]| format!("\"{}\"", crate::crypto::sha256_hex(&String::from_utf8_lossy(bytes)));
    match (method, query.get("uploadId").cloned()) {
        (Method::POST, None) if query.contains_key("uploads") => {
            let id = uuid::Uuid::new_v4().to_string();
            world.uploads.insert(id.clone(), (bucket.clone(), key.clone(), BTreeMap::new()));
            let ns = "http://s3.amazonaws.com/doc/2006-03-01/";
            let xml = format!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><InitiateMultipartUploadResult xmlns=\"{ns}\"><Bucket>{bucket}</Bucket><Key>{key}</Key><UploadId>{id}</UploadId></InitiateMultipartUploadResult>"
            );
            xml.into_response()
        }
        (Method::PUT, Some(id)) => {
            let number: u16 = query.get("partNumber").and_then(|n| n.parse().ok()).unwrap_or(0);
            let Some((_, _, parts)) = world.uploads.get_mut(&id) else { return error(StatusCode::NOT_FOUND, "NoSuchUpload", "No such upload") };
            parts.insert(number, body.to_vec());
            (StatusCode::OK, [(header::ETAG, etag(&body))]).into_response()
        }
        (Method::POST, Some(id)) => {
            let Some((bucket, key, parts)) = world.uploads.remove(&id) else { return error(StatusCode::NOT_FOUND, "NoSuchUpload", "No such upload") };
            let listed = String::from_utf8_lossy(&body).matches("<PartNumber>").count();
            if listed != parts.len() {
                return error(StatusCode::BAD_REQUEST, "InvalidPart", "One or more of the specified parts could not be found.");
            }
            let whole: Vec<u8> = parts.into_values().flatten().collect();
            world.buckets.get_mut(&bucket).unwrap().insert(key.clone(), whole);
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><CompleteMultipartUploadResult><Bucket>{bucket}</Bucket><Key>{key}</Key><ETag>\"x\"</ETag></CompleteMultipartUploadResult>")
                .into_response()
        }
        (Method::DELETE, Some(id)) => {
            world.uploads.remove(&id);
            StatusCode::NO_CONTENT.into_response()
        }
        (Method::PUT, None) => {
            world.buckets.get_mut(&bucket).unwrap().insert(key, body.to_vec());
            (StatusCode::OK, [(header::ETAG, etag(&body))]).into_response()
        }
        (Method::DELETE, None) => {
            world.buckets.get_mut(&bucket).unwrap().remove(&key);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => error(StatusCode::METHOD_NOT_ALLOWED, "MethodNotAllowed", "The specified method is not allowed against this resource."),
    }
}
