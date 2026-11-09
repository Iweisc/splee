use axum::http::header;
use axum::response::{IntoResponse, Response};

use crate::error::{AppError, AppResult};

/// A CSV file download. Every cell goes through spreadsheet_safe.
pub fn download<R, C>(filename: &str, header: &[&str], rows: impl IntoIterator<Item = R>) -> AppResult<Response>
where
    R: IntoIterator<Item = C>,
    C: AsRef<str>,
{
    let invalid = |e: csv::Error| AppError::Internal(e.into());
    let mut out = csv::Writer::from_writer(Vec::new());
    out.write_record(header).map_err(invalid)?;
    for row in rows {
        out.write_record(row.into_iter().map(|cell| spreadsheet_safe(cell.as_ref()))).map_err(invalid)?;
    }
    let body = out.into_inner().map_err(|e| AppError::Internal(e.into_error().into()))?;
    let disposition = format!("attachment; filename=\"{filename}\"");
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, disposition)], body).into_response())
}

/// Cells come from the web and from imports. A cell starting with one of these runs as a formula in Excel or Sheets.
fn spreadsheet_safe(cell: &str) -> String {
    match cell.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{cell}"),
        _ => cell.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::spreadsheet_safe;

    #[test]
    fn formula_cells_are_quoted() {
        assert_eq!(spreadsheet_safe("=HYPERLINK(\"x\")"), "'=HYPERLINK(\"x\")");
        assert_eq!(spreadsheet_safe("-1+2"), "'-1+2");
        assert_eq!(spreadsheet_safe("Head of @Growth"), "Head of @Growth");
        assert_eq!(spreadsheet_safe(""), "");
    }
}
