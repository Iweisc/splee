use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct PageQuery {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

impl PageQuery {
    pub fn limit(&self) -> i64 {
        self.per_page.unwrap_or(50).clamp(1, 200)
    }

    pub fn offset(&self) -> i64 {
        (self.page.unwrap_or(1).max(1) - 1) * self.limit()
    }
}

#[derive(Debug, Serialize)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: i64,
}
