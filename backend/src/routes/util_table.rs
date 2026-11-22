//! Uploaded tables, from lead imports and questionnaires: a CSV, or the first sheet of an Excel or OpenDocument file.

use std::io::Cursor;

use calamine::Reader;

use crate::error::{AppError, AppResult};

/// A table read from a file, a sheet or a database.
pub(super) struct Csv {
    pub(super) headers: Vec<String>,
    /// (line number in the file, cells)
    pub(super) rows: Vec<(usize, Vec<String>)>,
}

pub(super) fn parse_csv(bytes: &[u8]) -> AppResult<Csv> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut reader = csv::ReaderBuilder::new().flexible(true).trim(csv::Trim::All).from_reader(bytes);
    let to_strings = |r: &csv::ByteRecord| r.iter().map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>();
    let invalid = |e: csv::Error| AppError::BadRequest(format!("Couldn't read the CSV: {e}"));
    let headers = to_strings(reader.byte_headers().map_err(invalid)?);
    if headers.iter().all(String::is_empty) {
        return Err(AppError::BadRequest("The CSV has no header row".into()));
    }
    let mut rows = Vec::new();
    for record in reader.byte_records() {
        let record = record.map_err(invalid)?;
        let line = record.position().map_or(0, |p| p.line() as usize);
        rows.push((line, to_strings(&record)));
    }
    Ok(Csv { headers, rows })
}

/// How much an .xlsx or .ods may unzip to. Its sheet is read into memory whole, and a small file can unzip to gigabytes.
const MAX_UNZIPPED: u64 = 200 * 1024 * 1024;

/// The first sheet of an Excel or OpenDocument file, read like a CSV: its first row is the header and line numbers are
/// the sheet's row numbers. Blank rows are skipped, as the CSV reader skips blank lines.
pub(super) fn parse_spreadsheet(bytes: Vec<u8>) -> AppResult<Csv> {
    read_sheet(bytes).map(|(_, csv)| csv)
}

/// A spreadsheet's header, and its rows with the blank ones below the header that `parse_spreadsheet` skips put back, for a
/// file that goes back with every row where it was. Only for spreadsheets, since a CSV row can span lines. Errs past `max` rows.
pub(super) fn parse_spreadsheet_with_blank_rows(bytes: Vec<u8>, max: usize) -> AppResult<(Vec<String>, Vec<Vec<String>>)> {
    let (header_line, csv) = read_sheet(bytes)?;
    Ok((csv.headers, with_blank_rows(header_line, csv.rows, max)?))
}

/// The sheet as `parse_spreadsheet` reads it, and the header's row number.
fn read_sheet(bytes: Vec<u8>) -> AppResult<(usize, Csv)> {
    let unreadable = |e: &dyn std::fmt::Display| AppError::BadRequest(format!("Couldn't read the spreadsheet: {e}"));
    // .xls isn't a zip, and only a zip can unzip to more than it is.
    if let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(&bytes)) {
        let unzipped: u64 = (0..zip.len()).filter_map(|i| zip.by_index_raw(i).ok().map(|f| f.size())).sum();
        if unzipped > MAX_UNZIPPED {
            return Err(AppError::BadRequest("The spreadsheet is too big. Save its first sheet as a CSV and import that.".into()));
        }
    }
    let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|e| unreadable(&e))?;
    let sheet = book.worksheet_range_at(0).ok_or_else(|| AppError::BadRequest("The spreadsheet has no sheets".into()))?;
    let sheet = sheet.map_err(|e| unreadable(&e))?;
    let first_row = sheet.start().map_or(0, |(row, _)| row as usize);
    let mut rows = sheet
        .rows()
        .enumerate()
        .map(|(i, cells)| (first_row + i + 1, cells.iter().map(|c| c.to_string().trim().to_string()).collect::<Vec<_>>()))
        .filter(|(_, cells)| cells.iter().any(|c| !c.is_empty()));
    let (header_line, headers) = rows.next().ok_or_else(|| AppError::BadRequest("The spreadsheet has no header row".into()))?;
    Ok((header_line, Csv { headers, rows: rows.collect() }))
}

/// The rows below the header with blank ones in the gaps of their row numbers, so each lands where it was in the file.
fn with_blank_rows(header_line: usize, rows: Vec<(usize, Vec<String>)>, max: usize) -> AppResult<Vec<Vec<String>>> {
    let span = rows.last().map_or(0, |(last, _)| last - header_line);
    if span > max {
        return Err(AppError::BadRequest(format!("It has {span} rows. Split it into parts of up to {max}")));
    }
    let mut out = Vec::with_capacity(span);
    for (line, cells) in rows {
        out.resize(line - header_line - 1, Vec::new());
        out.push(cells);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_row_between_two_comes_back_in_its_place() {
        // The fixture has a blank row 1, the header on row 2, rows 3 and 5, and a blank row 4.
        let (_, rows) = parse_spreadsheet_with_blank_rows(include_bytes!("testdata/leads.xlsx").to_vec(), 10).unwrap();
        assert_eq!(rows.iter().map(|r| r.first().map_or("", String::as_str)).collect::<Vec<_>>(), ["ada@acme.com", "", "bob@acme.com"]);
    }

    #[test]
    fn a_blank_row_under_the_header_comes_back_and_counts() {
        let first = |rows: Vec<Vec<String>>| rows.iter().map(|r| r.first().cloned().unwrap_or_default()).collect::<Vec<_>>();
        assert_eq!(first(with_blank_rows(1, vec![(3, vec!["q".into()])], 10).unwrap()), ["", "q"]);
        assert_eq!(with_blank_rows(1, vec![(11, vec![])], 10).unwrap().len(), 10);
        assert!(with_blank_rows(1, vec![(12, vec![])], 10).is_err(), "11 rows below the header");
    }
}
