use rusqlite::types::ToSql;
use rusqlite::Error as RusqliteError;

/// Convert a `String` to `Option<String>`, treating empty as None.
pub fn nullable_string(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Convert `Option<String>` for SQL: `None` maps to empty string for TEXT columns that accept empty.
/// Most of our schema has nullable TEXT columns, so we use `Option` on the Rust side.
pub fn opt_to_sql(val: &Option<String>) -> String {
    val.as_deref().unwrap_or("").to_string()
}

/// Convert `Option<i64>` for SQL: `None` maps to NULL sentinel 0.
/// The consumer must handle 0-as-null where applicable.
pub fn opt_i64_to_sql(val: &Option<i64>) -> i64 {
    val.unwrap_or(0)
}

/// Convert `bool` to 0/1 INTEGER for SQLite.
pub fn bool_to_int(val: bool) -> i32 {
    if val {
        1
    } else {
        0
    }
}

/// Convert 0/1 INTEGER from SQLite to `bool`.
pub fn int_to_bool(val: i32) -> bool {
    val != 0
}

/// Generate SQL placeholder string: `?,?,?` for `count` items.
pub fn sql_placeholders(count: usize) -> String {
    (0..count).map(|_| "?").collect::<Vec<_>>().join(",")
}

/// Split a slice into chunks of `chunk_size` (for SQL IN clauses with variable limits).
/// Returns a Vec of owned Vecs.
pub fn chunk_strings(values: &[String], chunk_size: usize) -> Vec<Vec<String>> {
    values.chunks(chunk_size).map(|c| c.to_vec()).collect()
}

/// SQLite max variables per query.
pub const SQLITE_MAX_VARIABLES: usize = 900;

/// Check whether a rusqlite error is a UNIQUE constraint violation on
/// the `idx_queue_items_one_active_dedupe` partial index.
pub fn is_queue_active_dedupe_constraint_error(err: &RusqliteError) -> bool {
    match err {
        RusqliteError::SqliteFailure(e, _) => {
            e.code == rusqlite::ErrorCode::ConstraintViolation && e.extended_code == 2067
            /* SQLITE_CONSTRAINT_UNIQUE */
        }
        _ => false,
    }
}

/// Helper to create a vec of boxed `ToSql` values from optional params.
/// Usage: `params_as_refs(&[&s, &n])` to pass to rusqlite methods.
pub fn params_as_refs<'a>(params: &'a [&'a dyn ToSql]) -> Vec<&'a dyn ToSql> {
    params.to_vec()
}
