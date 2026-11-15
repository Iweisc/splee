//! Requests to the organization's bucket on AWS S3 or a service that speaks its API: Cloudflare R2, Google Cloud Storage's
//! interoperability, Backblaze B2 and MinIO. rusty-s3 signs each request with AWS Signature Version 4 in the query string, as
//! a presigned URL, and reqwest sends it. An admin types the endpoint, so requests only go to a public address, pinned to the
//! one that was checked (hooks::public_client).
//!
//! Docs:
//! https://docs.aws.amazon.com/AmazonS3/latest/API/sigv4-query-string-auth.html
//! https://docs.aws.amazon.com/AmazonS3/latest/userguide/mpuoverview.html
//! https://docs.aws.amazon.com/AmazonS3/latest/API/API_CompleteMultipartUpload.html
//! https://docs.aws.amazon.com/AmazonS3/latest/API/ErrorResponses.html

use std::io::Write;
use std::time::Duration;

use flate2::Compression;
use flate2::write::GzEncoder;
use reqwest::header::{CONTENT_TYPE, ETAG, HOST};
use reqwest::{Method, Url};
use rusty_s3::actions::CreateMultipartUpload;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};

use crate::config::Config;
use crate::hooks;

/// How long a signed URL is good for. Each is used right away.
const EXPIRES: Duration = Duration::from_secs(15 * 60);
/// Long enough for a part on a slow link.
const TIMEOUT: Duration = if cfg!(test) { Duration::from_secs(5) } else { Duration::from_secs(5 * 60) };
/// The size a file's gzipped bytes reach before they go up as a part of a multipart upload. S3 takes parts of 5 MiB and up,
/// apart from the last. Small in tests, so a few hundred rows make a multipart upload.
pub const PART_SIZE: usize = if cfg!(test) { 16 * 1024 } else { 8 * 1024 * 1024 };

/// Where the files go and the key that may write them, as the settings hold them with the secret decrypted.
#[derive(Clone, Debug)]
pub struct Target {
    /// Empty for AWS S3 in the region.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub path_style: bool,
    pub access_key_id: String,
    pub secret: String,
}

/// The endpoint as a URL: AWS S3 in the region when it's empty, and https:// in front when it has no scheme.
pub fn endpoint_url(endpoint: &str, region: &str) -> Result<Url, String> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    if endpoint.is_empty() {
        return Url::parse(&format!("https://s3.{region}.amazonaws.com")).map_err(|_| format!("{region} isn't a region name."));
    }
    let full = if endpoint.contains("://") { endpoint.to_string() } else { format!("https://{endpoint}") };
    let bad = || format!("{endpoint} isn't an endpoint address. It looks like https://<account id>.r2.cloudflarestorage.com.");
    let url = Url::parse(&full).map_err(|_| bad())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() || url.query().is_some() {
        return Err(bad());
    }
    if url.path() != "/" {
        return Err("Leave the bucket and folders out of the endpoint. They have their own fields.".into());
    }
    Ok(url)
}

/// The bucket at its endpoint, addressed in the path or in the host.
pub fn bucket(target: &Target) -> Result<Bucket, String> {
    let style = if target.path_style { UrlStyle::Path } else { UrlStyle::VirtualHost };
    let endpoint = endpoint_url(&target.endpoint, &target.region)?;
    Bucket::new(endpoint, style, target.bucket.clone(), target.region.clone()).map_err(|e| format!("That bucket can't be used: {e:?}"))
}

pub struct Client {
    bucket: Bucket,
    credentials: Credentials,
    /// Config::s3_api_url, where every request goes instead, keeping the Host it was signed for.
    base: Option<Url>,
}

impl Client {
    pub fn new(cfg: &Config, target: &Target) -> Result<Self, String> {
        let bucket = bucket(target)?;
        let base = (!cfg.s3_api_url.is_empty()).then(|| Url::parse(&cfg.s3_api_url)).transpose().map_err(|e| format!("S3_API_URL: {e}"))?;
        Ok(Self { bucket, credentials: Credentials::new(target.access_key_id.clone(), target.secret.clone()), base })
    }

    /// Sends a signed request, giving the answer when it's a success and what went wrong, in plain words, when it isn't.
    async fn send(&self, method: Method, url: Url, content_type: Option<&str>, body: Vec<u8>) -> Result<reqwest::Response, String> {
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (h, _) => h.unwrap_or_default().to_string(),
        };
        let (client, to) = match &self.base {
            Some(base) => {
                let mut to = url.clone();
                // Both are http or https, so these can't fail.
                let _ = to.set_scheme(base.scheme());
                let _ = to.set_host(base.host_str());
                let _ = to.set_port(base.port());
                (reqwest::Client::new(), to)
            }
            None => (hooks::public_client(&url).await.map_err(|e| format!("Couldn't reach {host}: {e:#}. Check the endpoint."))?, url),
        };
        let mut request = client.request(method, to).header(HOST, &host).timeout(TIMEOUT).body(body);
        if let Some(content_type) = content_type {
            request = request.header(CONTENT_TYPE, content_type);
        }
        let res = request.send().await.map_err(|e| format!("Couldn't reach {host}: {}. Check the endpoint.", plain(e)))?;
        if res.status().is_success() {
            return Ok(res);
        }
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        Err(refused(status, &body, self.bucket.name()))
    }

    pub async fn put(&self, key: &str, content_type: &str, body: Vec<u8>) -> Result<(), String> {
        let url = self.bucket.put_object(Some(&self.credentials), key).sign(EXPIRES);
        self.send(Method::PUT, url, Some(content_type), body).await.map(drop)
    }

    pub async fn delete(&self, key: &str) -> Result<(), String> {
        let url = self.bucket.delete_object(Some(&self.credentials), key).sign(EXPIRES);
        self.send(Method::DELETE, url, None, Vec::new()).await.map(drop)
    }

    async fn start_multipart(&self, key: &str, content_type: &str) -> Result<String, String> {
        let url = self.bucket.create_multipart_upload(Some(&self.credentials), key).sign(EXPIRES);
        let body = self.send(Method::POST, url, Some(content_type), Vec::new()).await?.text().await.map_err(plain)?;
        let started = CreateMultipartUpload::parse_response(&body).map_err(|_| "The storage service didn't start the upload. Try again.".to_string())?;
        Ok(started.upload_id().to_string())
    }

    async fn upload_part(&self, key: &str, upload_id: &str, number: u16, body: Vec<u8>) -> Result<String, String> {
        let url = self.bucket.upload_part(Some(&self.credentials), key, number, upload_id).sign(EXPIRES);
        let res = self.send(Method::PUT, url, None, body).await?;
        let etag = res.headers().get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
        etag.ok_or_else(|| "The storage service took a part of the file but didn't say what it got (no ETag).".into())
    }

    async fn complete(&self, key: &str, upload_id: &str, etags: &[String]) -> Result<(), String> {
        let action = self.bucket.complete_multipart_upload(Some(&self.credentials), key, upload_id, etags.iter().map(String::as_str));
        let url = action.sign(EXPIRES);
        let res = self.send(Method::POST, url, Some("application/xml"), action.body().into_bytes()).await?;
        // S3 can answer 200 and still fail, with the error in the body.
        let body = res.text().await.unwrap_or_default();
        if body.contains("<Error>") {
            return Err(refused(200, &body, self.bucket.name()));
        }
        Ok(())
    }

    async fn abort(&self, key: &str, upload_id: &str) {
        let url = self.bucket.abort_multipart_upload(Some(&self.credentials), key, upload_id).sign(EXPIRES);
        if let Err(e) = self.send(Method::DELETE, url, None, Vec::new()).await {
            tracing::info!(error = e, "data export: couldn't abort a multipart upload");
        }
    }
}

/// A request error without its URL, which carries the signature.
fn plain(e: reqwest::Error) -> String {
    if e.is_timeout() {
        return "it didn't answer in time".into();
    }
    format!("{:#}", anyhow::Error::from(e.without_url()))
}

/// The text of the first <tag> in an S3 error body.
fn tag<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = body.find(&open)? + open.len();
    let len = body[start..].find(&format!("</{name}>"))?;
    Some(body[start..start + len].trim()).filter(|t| !t.is_empty())
}

/// What a refusal means for the person setting up the export.
pub fn refused(status: u16, body: &str, bucket: &str) -> String {
    let message = tag(body, "Message");
    match (status, tag(body, "Code")) {
        (_, Some("InvalidAccessKeyId")) => "The storage service doesn't know this access key ID. Check it, or make a new key.".into(),
        (_, Some("SignatureDoesNotMatch")) => "The secret access key doesn't match the access key ID. Paste the secret again.".into(),
        (_, Some("NoSuchBucket")) | (404, None) => {
            format!("There's no bucket called {bucket} at this endpoint. Check the bucket name, the endpoint and the region.")
        }
        (_, Some("AuthorizationHeaderMalformed" | "AuthorizationQueryParametersError" | "PermanentRedirect" | "IllegalLocationConstraintException"))
        | (301, _) => match tag(body, "Region") {
            Some(region) => format!("The bucket is in {region}. Set the region to {region}."),
            None => format!("The bucket is in another region. Check the region.{}", message.map(|m| format!(" The storage service said: {m}")).unwrap_or_default()),
        },
        (_, Some("AccessDenied" | "AllAccessDisabled")) | (403, _) => {
            format!("Access denied: this key isn't allowed to write to {bucket}. Give it permission to put and delete objects there.")
        }
        (_, Some("RequestTimeTooSkewed")) => "The storage service says this server's clock is off. Tell whoever runs Splee.".into(),
        (s, code) => {
            let said = message.or(code).map(|m| format!(": {m}")).unwrap_or_default();
            format!("The storage service answered {s}{said}")
        }
    }
}

/// One gzipped file of newline-delimited JSON, sent as a single PUT when it stays under a part and as a multipart upload once
/// it grows past one, so only about a part's worth is ever held in memory.
pub struct FileWriter<'a> {
    client: &'a Client,
    key: String,
    gz: GzEncoder<Vec<u8>>,
    /// The multipart upload's id and the ETags of the parts sent so far.
    upload: Option<(String, Vec<String>)>,
    pub rows: i64,
    pub bytes: i64,
}

pub const NDJSON_GZ: &str = "application/x-ndjson";

impl<'a> FileWriter<'a> {
    pub fn new(client: &'a Client, key: String) -> Self {
        Self { client, key, gz: GzEncoder::new(Vec::new(), Compression::default()), upload: None, rows: 0, bytes: 0 }
    }

    pub async fn line(&mut self, line: &str) -> Result<(), String> {
        let written = self.gz.write_all(line.as_bytes()).and_then(|()| self.gz.write_all(b"\n"));
        written.map_err(|e| format!("Couldn't compress the file: {e}"))?;
        self.rows += 1;
        if self.gz.get_ref().len() >= PART_SIZE {
            let part = std::mem::take(self.gz.get_mut());
            self.part(part).await?;
        }
        Ok(())
    }

    async fn part(&mut self, part: Vec<u8>) -> Result<(), String> {
        if self.upload.is_none() {
            self.upload = Some((self.client.start_multipart(&self.key, NDJSON_GZ).await?, Vec::new()));
        }
        let (id, etags) = self.upload.as_mut().expect("started above");
        let number = u16::try_from(etags.len() + 1).map_err(|_| "The file has more than 65,535 parts.".to_string())?;
        self.bytes += part.len() as i64;
        etags.push(self.client.upload_part(&self.key, id, number, part).await?);
        Ok(())
    }

    /// Sends what's left and closes the file. An upload that fails is aborted, so the bucket keeps no stray parts.
    pub async fn finish(mut self) -> Result<(String, i64, i64), String> {
        let rest = std::mem::replace(&mut self.gz, GzEncoder::new(Vec::new(), Compression::default()));
        let rest = rest.finish().map_err(|e| format!("Couldn't compress the file: {e}"))?;
        let done = match self.upload.take() {
            None => {
                self.bytes += rest.len() as i64;
                self.client.put(&self.key, NDJSON_GZ, rest).await
            }
            Some((id, etags)) => {
                self.upload = Some((id.clone(), etags));
                let sent = self.part(rest).await;
                let (_, etags) = self.upload.as_ref().expect("put back above");
                match sent {
                    Ok(()) => self.client.complete(&self.key, &id, etags).await,
                    Err(e) => Err(e),
                }
            }
        };
        if done.is_err() {
            self.abort().await;
        }
        done.map(|()| (std::mem::take(&mut self.key), self.rows, self.bytes))
    }

    /// Drops a multipart upload that won't be finished.
    pub async fn abort(&mut self) {
        if let Some((id, _)) = self.upload.take() {
            self.client.abort(&self.key, &id).await;
        }
    }
}
