//! `PRAGMA` statements issued as bare SQL.
//!
//! rusqlite's pragma helpers (`pragma_update`, `pragma_query_value`,
//! `pragma_update_and_check`) first look the name up with
//! `SELECT 1 FROM pragma_pragma_list WHERE name = ?`. That `SELECT` reads the
//! schema, which a connection-scoped pragma such as `busy_timeout` never does:
//! on a WAL database whose directory is read-only (no `-shm` can be created)
//! it fails with `attempt to write a readonly database`, and under a zero busy
//! timeout it can fail on another connection's lock. Orbit names its pragmas
//! as literals, so these helpers skip the lookup and check the name and value
//! locally instead.

use rusqlite::types::{FromSql, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, Result};

/// Run `PRAGMA <name> = <value>`, stepping past any row it reports.
///
/// `value` must be an integer (`bool` binds as `0`/`1`) or a bare keyword
/// such as `ON`, `WAL` or `NORMAL`; anything else is refused before SQLite
/// sees it.
pub fn set(conn: &Connection, name: &str, value: &dyn ToSql) -> Result<()> {
    conn.execute_batch(&statement(name, Some(value))?)
}

/// Run `PRAGMA <name> = <value>` and return the value SQLite reports back,
/// for pragmas such as `journal_mode` that answer with the setting in force.
pub fn set_and_check<T: FromSql>(conn: &Connection, name: &str, value: &dyn ToSql) -> Result<T> {
    conn.query_row(&statement(name, Some(value))?, [], |row| row.get(0))
}

/// Read `PRAGMA <name>`.
pub fn get<T: FromSql>(conn: &Connection, name: &str) -> Result<T> {
    conn.query_row(&statement(name, None)?, [], |row| row.get(0))
}

fn statement(name: &str, value: Option<&dyn ToSql>) -> Result<String> {
    if !is_bare_word(name) {
        return Err(misuse(format!("invalid pragma name {name:?}")));
    }
    let Some(value) = value else {
        return Ok(format!("PRAGMA {name}"));
    };
    let output = value.to_sql()?;
    let rendered = match &output {
        ToSqlOutput::Borrowed(value) => render(*value),
        ToSqlOutput::Owned(value) => render(ValueRef::from(value)),
        _ => None,
    };
    let rendered =
        rendered.ok_or_else(|| misuse(format!("unsupported value for pragma {name}")))?;
    Ok(format!("PRAGMA {name} = {rendered}"))
}

fn render(value: ValueRef<'_>) -> Option<String> {
    match value {
        ValueRef::Integer(value) => Some(value.to_string()),
        ValueRef::Text(text) => std::str::from_utf8(text)
            .ok()
            .filter(|text| is_bare_word(text))
            .map(str::to_owned),
        _ => None,
    }
}

fn is_bare_word(word: &str) -> bool {
    !word.is_empty()
        && word
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn misuse(message: String) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_MISUSE),
        Some(message),
    )
}
