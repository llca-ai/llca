//! The few SQLite calls llca needs, bound to the system libsqlite3
//! (shipped with macOS and every common Linux distribution).

use std::ffi::{c_char, c_int, CStr, CString};
use std::path::Path;
use std::ptr;

#[allow(non_camel_case_types)]
enum sqlite3 {}
#[allow(non_camel_case_types)]
enum sqlite3_stmt {}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_INTEGER: c_int = 1;
const SQLITE_NULL: c_int = 5;
const SQLITE_OPEN_READWRITE: c_int = 0x0000_0002;
const SQLITE_OPEN_CREATE: c_int = 0x0000_0004;
// SQLITE_TRANSIENT: SQLite copies bound text before the call returns.
const SQLITE_TRANSIENT: isize = -1;

#[link(name = "sqlite3")]
extern "C" {
    fn sqlite3_open_v2(filename: *const c_char, db: *mut *mut sqlite3, flags: c_int, vfs: *const c_char) -> c_int;
    fn sqlite3_close_v2(db: *mut sqlite3) -> c_int;
    fn sqlite3_busy_timeout(db: *mut sqlite3, ms: c_int) -> c_int;
    fn sqlite3_errmsg(db: *mut sqlite3) -> *const c_char;
    fn sqlite3_prepare_v2(db: *mut sqlite3, sql: *const c_char, n: c_int, stmt: *mut *mut sqlite3_stmt, tail: *mut *const c_char) -> c_int;
    fn sqlite3_bind_text(stmt: *mut sqlite3_stmt, i: c_int, text: *const c_char, n: c_int, destructor: isize) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut sqlite3_stmt, i: c_int, value: i64) -> c_int;
    fn sqlite3_step(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_column_count(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_column_type(stmt: *mut sqlite3_stmt, i: c_int) -> c_int;
    fn sqlite3_column_int64(stmt: *mut sqlite3_stmt, i: c_int) -> i64;
    fn sqlite3_column_text(stmt: *mut sqlite3_stmt, i: c_int) -> *const u8;
    fn sqlite3_column_bytes(stmt: *mut sqlite3_stmt, i: c_int) -> c_int;
    fn sqlite3_finalize(stmt: *mut sqlite3_stmt) -> c_int;
    fn sqlite3_last_insert_rowid(db: *mut sqlite3) -> i64;
}

#[derive(Debug)]
pub struct Error(pub String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "database error: {}", self.0)
    }
}
impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub enum Param<'a> {
    Text(&'a str),
    Int(i64),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Int(i64),
    Text(String),
}

impl Cell {
    pub fn text(&self) -> Option<&str> {
        match self {
            Cell::Text(s) => Some(s),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match self {
            Cell::Int(n) => Some(*n),
            _ => None,
        }
    }
}

pub struct Connection {
    db: *mut sqlite3,
}

// A connection is used from one thread at a time; each request opens its own.
unsafe impl Send for Connection {}

impl Connection {
    pub fn open(path: &Path, busy_timeout_ms: i32) -> Result<Connection> {
        let c_path = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| Error("path contains NUL".into()))?;
        let mut db = ptr::null_mut();
        let rc = unsafe { sqlite3_open_v2(c_path.as_ptr(), &mut db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, ptr::null()) };
        let conn = Connection { db };
        if rc != SQLITE_OK {
            return Err(conn.error());
        }
        unsafe { sqlite3_busy_timeout(db, busy_timeout_ms) };
        Ok(conn)
    }

    fn error(&self) -> Error {
        if self.db.is_null() {
            return Error("cannot open database".into());
        }
        let msg = unsafe { CStr::from_ptr(sqlite3_errmsg(self.db)) };
        Error(msg.to_string_lossy().into_owned())
    }

    fn prepare(&self, sql: &str, params: &[Param]) -> Result<Statement<'_>> {
        let c_sql = CString::new(sql).map_err(|_| Error("SQL contains NUL".into()))?;
        let mut stmt = ptr::null_mut();
        let rc = unsafe { sqlite3_prepare_v2(self.db, c_sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
        if rc != SQLITE_OK {
            return Err(self.error());
        }
        let statement = Statement { conn: self, stmt };
        for (i, param) in params.iter().enumerate() {
            let index = (i + 1) as c_int;
            let rc = match param {
                Param::Text(s) => unsafe {
                    sqlite3_bind_text(stmt, index, s.as_ptr() as *const c_char, s.len() as c_int, SQLITE_TRANSIENT)
                },
                Param::Int(n) => unsafe { sqlite3_bind_int64(stmt, index, *n) },
            };
            if rc != SQLITE_OK {
                return Err(self.error());
            }
        }
        Ok(statement)
    }

    /// Run a statement that returns no rows; returns the last inserted row id.
    pub fn execute(&self, sql: &str, params: &[Param]) -> Result<i64> {
        let stmt = self.prepare(sql, params)?;
        loop {
            match unsafe { sqlite3_step(stmt.stmt) } {
                SQLITE_DONE => return Ok(unsafe { sqlite3_last_insert_rowid(self.db) }),
                SQLITE_ROW => continue, // e.g. PRAGMA journal_mode returns a row
                _ => return Err(self.error()),
            }
        }
    }

    pub fn query(&self, sql: &str, params: &[Param]) -> Result<Vec<Vec<Cell>>> {
        let stmt = self.prepare(sql, params)?;
        let mut rows = Vec::new();
        loop {
            match unsafe { sqlite3_step(stmt.stmt) } {
                SQLITE_ROW => {
                    let n = unsafe { sqlite3_column_count(stmt.stmt) };
                    let mut row = Vec::with_capacity(n as usize);
                    for i in 0..n {
                        row.push(match unsafe { sqlite3_column_type(stmt.stmt, i) } {
                            SQLITE_NULL => Cell::Null,
                            SQLITE_INTEGER => Cell::Int(unsafe { sqlite3_column_int64(stmt.stmt, i) }),
                            _ => unsafe {
                                let p = sqlite3_column_text(stmt.stmt, i);
                                let len = sqlite3_column_bytes(stmt.stmt, i) as usize;
                                if p.is_null() {
                                    Cell::Null
                                } else {
                                    Cell::Text(String::from_utf8_lossy(std::slice::from_raw_parts(p, len)).into_owned())
                                }
                            },
                        });
                    }
                    rows.push(row);
                }
                SQLITE_DONE => return Ok(rows),
                _ => return Err(self.error()),
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if !self.db.is_null() {
            unsafe { sqlite3_close_v2(self.db) };
        }
    }
}

struct Statement<'a> {
    #[allow(dead_code)]
    conn: &'a Connection,
    stmt: *mut sqlite3_stmt,
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        unsafe { sqlite3_finalize(self.stmt) };
    }
}

