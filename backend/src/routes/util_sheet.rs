//! Reads a Google Sheet shared by link as CSV, so leads import straight from it without a download and upload.
//! Only the sheet's id and tab are taken from the link, so the request always goes to Google, never a host someone typed.

use std::time::Duration;

use reqwest::header::{CONTENT_DISPOSITION, CONTENT_TYPE};

use crate::error::{AppError, AppResult};

/// The same cap as an uploaded file.
const MAX_BYTES: usize = 20 * 1024 * 1024;

/// The sheet's name and its CSV.
pub async fn fetch(link: &str) -> AppResult<(String, Vec<u8>)> {
    let bad = |m: &str| AppError::BadRequest(m.into());
    let url = export_url(link).ok_or_else(|| bad("Paste the link to a Google Sheet, like https://docs.google.com/spreadsheets/d/…"))?;
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| AppError::Internal(e.into()))?;
    let mut res = client.get(url).send().await.map_err(|_| bad("Couldn't reach Google Sheets. Try again in a moment."))?;
    // A sheet that isn't shared answers with a sign-in page, not an error.
    let is_csv = res.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/csv"));
    if !res.status().is_success() || !is_csv {
        return Err(bad("Couldn't open that sheet. Share it so anyone with the link can view it, then try again."));
    }
    let name = res.headers().get(CONTENT_DISPOSITION).and_then(|v| v.to_str().ok()).and_then(filename).unwrap_or_else(|| "Google Sheet.csv".into());
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|_| bad("Google Sheets stopped sending the sheet. Try again in a moment."))? {
        bytes.extend_from_slice(&chunk);
        if bytes.len() > MAX_BYTES {
            return Err(bad("The sheet is over 20 MB. Split it, or download it as CSV and upload that."));
        }
    }
    Ok((name, bytes))
}

/// https://docs.google.com/spreadsheets/d/{id}/edit#gid={tab} becomes its CSV export, keeping the tab that was open.
fn export_url(link: &str) -> Option<String> {
    let crate::sheets::Link { id, gid } = crate::sheets::link(link)?;
    Some(format!("https://docs.google.com/spreadsheets/d/{id}/export?format=csv{}", gid.map(|gid| format!("&gid={gid}")).unwrap_or_default()))
}

/// The UTF-8 name Google gives the export, like "Leads - Sheet1.csv".
fn filename(disposition: &str) -> Option<String> {
    let encoded = disposition.split(';').find_map(|part| part.trim().strip_prefix("filename*=UTF-8''"))?;
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%').then(|| encoded.get(i + 1..i + 3)).flatten().and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    Some(String::from_utf8_lossy(&out).into_owned()).filter(|name| !name.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::{export_url, filename};

    #[test]
    fn a_share_link_becomes_the_csv_of_its_tab() {
        let csv = "https://docs.google.com/spreadsheets/d/1Bxi_M-9/export?format=csv";
        assert_eq!(export_url("https://docs.google.com/spreadsheets/d/1Bxi_M-9/edit?usp=sharing").as_deref(), Some(csv));
        assert_eq!(export_url(" https://docs.google.com/spreadsheets/d/1Bxi_M-9/edit#gid=42 ").as_deref(), Some(&*format!("{csv}&gid=42")));
        assert_eq!(export_url("https://docs.google.com/spreadsheets/d/1Bxi_M-9/edit?gid=7#gid=7").as_deref(), Some(&*format!("{csv}&gid=7")));
        for other in [
            "https://evil.example/spreadsheets/d/1Bxi/edit",
            "https://docs.google.com/document/d/1Bxi/edit",
            "https://docs.google.com/spreadsheets/d/e/2PACX/pubhtml",
            "https://docs.google.com/spreadsheets/d/1Bxi%2F..%2F/edit",
            "not a link",
        ] {
            assert_eq!(export_url(other), None, "{other}");
        }
    }

    #[test]
    fn the_name_is_the_sheets_title() {
        let header = "attachment; filename=\"Leads-Sheet1.csv\"; filename*=UTF-8''Leads%20%E2%80%94%20Sheet1.csv";
        assert_eq!(filename(header).as_deref(), Some("Leads — Sheet1.csv"));
        assert_eq!(filename("attachment; filename=\"x.csv\""), None);
    }
}
