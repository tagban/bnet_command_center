//! Reading a PvPGN server's accounts.
//!
//! PvPGN keeps an account as a bag of Battle.net-style keys (`BNET\acct\username`,
//! `Record\SEXP\0\wins`, `profile\location`), the same model Command Center stores. Two of its
//! storage back ends are read here, both from what an operator can hand over without PvPGN
//! running:
//!
//! - **Plain files** (`storage_path = file:mode=plain;dir=…`): one file per account, one
//!   `"key"="value"` line per attribute, backslashes and quotes escaped with a backslash.
//! - **SQL** (MySQL, PostgreSQL, SQLite), as a dump (`mysqldump`, `pg_dump --inserts`,
//!   `sqlite3 … .dump`): each key's first segment is a table (`BNET`, `Record`, `profile`,
//!   `friend`), the rest a column with `_` for `\` (and for the spaces in record keys), and the
//!   `uid` column ties an account's rows together.
//!
//! The binary `cdb` back end is not read; PvPGN can write its accounts out as plain files first.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// One PvPGN account: its attributes by key, as PvPGN spells them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PvpgnAccount {
    /// Where it came from, for the report: a file, or a dump's `uid`.
    pub origin: String,
    /// Every attribute, keys with single backslashes (`BNET\acct\username`).
    pub attrs: BTreeMap<String, String>,
}

impl PvpgnAccount {
    /// An attribute by key, any case.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v.as_str())
    }
}

/// Why a source could not be read.
#[derive(Debug)]
pub enum SourceError {
    /// A file or directory could not be read.
    Io(PathBuf, std::io::Error),
    /// Not a PvPGN plain-file account, or a broken dump.
    Format(PathBuf, String),
}

impl fmt::Display for SourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, e) => write!(f, "{}: {e}", path.display()),
            Self::Format(path, why) => write!(f, "{}: {why}", path.display()),
        }
    }
}

impl std::error::Error for SourceError {}

/// A quoted string with backslash escapes, from the front of `s`: the text and what follows.
fn quoted(s: &str) -> Option<(String, &str)> {
    let rest = s.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &rest[i + 1..])),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                't' => out.push('\t'),
                other => out.push(other),
            },
            other => out.push(other),
        }
    }
    None
}

/// One plain-file account (`"key"="value"` lines; blank lines and `#` comments skipped).
///
/// # Errors
///
/// A line that is not a quoted key, `=`, and a quoted value.
pub fn parse_plain(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut attrs = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bad = || format!("line {}: expected \"key\"=\"value\"", n + 1);
        let (key, rest) = quoted(line).ok_or_else(bad)?;
        let rest = rest.trim_start().strip_prefix('=').ok_or_else(bad)?;
        let (value, rest) = quoted(rest.trim_start()).ok_or_else(bad)?;
        if !rest.trim().is_empty() {
            return Err(bad());
        }
        attrs.insert(key, value);
    }
    Ok(attrs)
}

/// Every account in a plain-file `users` directory. Files that hold no `BNET\acct\username`
/// are left out (PvPGN's default-user template, stray files).
///
/// # Errors
///
/// [`SourceError`] when the directory cannot be listed or a file cannot be read or parsed.
pub fn read_plain_dir(dir: &Path) -> Result<Vec<PvpgnAccount>, SourceError> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| SourceError::Io(dir.to_path_buf(), e))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    let mut accounts = Vec::new();
    for path in entries {
        let bytes = std::fs::read(&path).map_err(|e| SourceError::Io(path.clone(), e))?;
        let text = String::from_utf8_lossy(&bytes);
        let attrs = parse_plain(&text).map_err(|why| SourceError::Format(path.clone(), why))?;
        let account = PvpgnAccount { origin: path.display().to_string(), attrs };
        if account.get(r"BNET\acct\username").is_some() {
            accounts.push(account);
        }
    }
    Ok(accounts)
}

/// A SQL value as a dump writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Value {
    Null,
    Text(String),
}

/// One inserted row: its column names and values.
type Row = (Vec<String>, Vec<Value>);

/// A dump split into what the import needs: each table's column names (from `CREATE TABLE` or
/// an `INSERT`'s own list) and rows.
#[derive(Debug, Default)]
struct Dump {
    columns: BTreeMap<String, Vec<String>>,
    rows: BTreeMap<String, Vec<Row>>,
}

/// A small tokenizer for dump statements: identifiers (bare or quoted with `` ` ``, `"` or
/// `[]`), strings (`'…'` with `''` or backslash escapes), numbers and punctuation.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Ident(String),
    Str(String),
    Punct(char),
}

fn tokenize(sql: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let b: Vec<char> = sql.chars().collect();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '-' && b.get(i + 1) == Some(&'-') {
            while i < b.len() && b[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && b.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '\'' {
            let mut s = String::new();
            i += 1;
            while i < b.len() {
                match b[i] {
                    '\'' if b.get(i + 1) == Some(&'\'') => {
                        s.push('\'');
                        i += 2;
                    }
                    '\'' => {
                        i += 1;
                        break;
                    }
                    '\\' if i + 1 < b.len() => {
                        s.push(match b[i + 1] {
                            'n' => '\n',
                            'r' => '\r',
                            't' => '\t',
                            '0' => '\0',
                            other => other,
                        });
                        i += 2;
                    }
                    other => {
                        s.push(other);
                        i += 1;
                    }
                }
            }
            out.push(Token::Str(s));
        } else if c == '`' || c == '"' || c == '[' {
            let close = if c == '[' { ']' } else { c };
            let mut s = String::new();
            i += 1;
            while i < b.len() && b[i] != close {
                s.push(b[i]);
                i += 1;
            }
            i += 1;
            out.push(Token::Ident(s));
        } else if c.is_alphanumeric() || c == '_' || ((c == '-' || c == '.') && b.get(i + 1).is_some_and(char::is_ascii_digit)) {
            let mut s = String::new();
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_' || b[i] == '.' || b[i] == '-' && s.is_empty()) {
                s.push(b[i]);
                i += 1;
            }
            out.push(Token::Word(s));
        } else {
            out.push(Token::Punct(c));
            i += 1;
        }
    }
    out
}

fn name_of(t: &Token) -> Option<&str> {
    match t {
        Token::Word(w) | Token::Ident(w) => Some(w),
        _ => None,
    }
}

fn is_word(t: Option<&Token>, word: &str) -> bool {
    matches!(t, Some(Token::Word(w)) if w.eq_ignore_ascii_case(word))
}

/// A table name, schema prefix (`public.`) dropped.
fn table_name(tokens: &[Token], at: &mut usize) -> Option<String> {
    let mut name = name_of(tokens.get(*at)?)?.to_string();
    *at += 1;
    while tokens.get(*at) == Some(&Token::Punct('.')) {
        name = name_of(tokens.get(*at + 1)?)?.to_string();
        *at += 2;
    }
    Some(name.rsplit('.').next().unwrap_or(&name).to_string())
}

impl Dump {
    fn parse(sql: &str) -> Self {
        let tokens = tokenize(sql);
        let mut dump = Self::default();
        let mut i = 0;
        while i < tokens.len() {
            if is_word(tokens.get(i), "CREATE") && is_word(tokens.get(i + 1), "TABLE") {
                i += 2;
                if is_word(tokens.get(i), "IF") {
                    i += 3;
                }
                let Some(table) = table_name(&tokens, &mut i) else { continue };
                if tokens.get(i) != Some(&Token::Punct('(')) {
                    continue;
                }
                i += 1;
                let mut depth = 1;
                let mut columns = Vec::new();
                let mut at_start = true;
                while i < tokens.len() && depth > 0 {
                    match &tokens[i] {
                        Token::Punct('(') => depth += 1,
                        Token::Punct(')') => depth -= 1,
                        Token::Punct(',') if depth == 1 => at_start = true,
                        t if depth == 1 && at_start => {
                            at_start = false;
                            if let Some(name) = name_of(t) {
                                let keyword = ["PRIMARY", "KEY", "UNIQUE", "CONSTRAINT", "INDEX", "FOREIGN", "CHECK"];
                                if !(matches!(t, Token::Word(_)) && keyword.iter().any(|k| name.eq_ignore_ascii_case(k))) {
                                    columns.push(name.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                dump.columns.insert(table.to_ascii_lowercase(), columns);
            } else if is_word(tokens.get(i), "INSERT") {
                i += 1;
                while i < tokens.len() && !is_word(tokens.get(i), "INTO") {
                    i += 1;
                }
                i += 1;
                let Some(table) = table_name(&tokens, &mut i) else { continue };
                let table = table.to_ascii_lowercase();
                let mut columns = Vec::new();
                if tokens.get(i) == Some(&Token::Punct('(')) {
                    i += 1;
                    while i < tokens.len() && tokens[i] != Token::Punct(')') {
                        if let Some(name) = name_of(&tokens[i]) {
                            columns.push(name.to_string());
                        }
                        i += 1;
                    }
                    i += 1;
                }
                if columns.is_empty() {
                    columns = dump.columns.get(&table).cloned().unwrap_or_default();
                }
                if !is_word(tokens.get(i), "VALUES") {
                    continue;
                }
                i += 1;
                while tokens.get(i) == Some(&Token::Punct('(')) {
                    i += 1;
                    let mut values = Vec::new();
                    while i < tokens.len() && tokens[i] != Token::Punct(')') {
                        match &tokens[i] {
                            Token::Str(s) => values.push(Value::Text(s.clone())),
                            Token::Word(w) if w.eq_ignore_ascii_case("NULL") => values.push(Value::Null),
                            Token::Word(w) => values.push(Value::Text(w.clone())),
                            _ => {}
                        }
                        i += 1;
                    }
                    i += 1;
                    dump.rows.entry(table.clone()).or_default().push((columns.clone(), values));
                    if tokens.get(i) == Some(&Token::Punct(',')) {
                        i += 1;
                    }
                }
            } else {
                i += 1;
            }
        }
        dump
    }
}

/// The PvPGN key a SQL column stands for in `table` (`BNET`.`acct_username` →
/// `BNET\acct\username`, `Record`.`SEXP_0_last_game_result` → `Record\SEXP\0\last game result`,
/// `friend`.`0_uid` → `friend\0\uid`). `None` for `uid`, which only ties rows together.
#[must_use]
pub fn column_key(table: &str, column: &str) -> Option<String> {
    if column.eq_ignore_ascii_case("uid") {
        return None;
    }
    let table_lower = table.to_ascii_lowercase();
    Some(match table_lower.as_str() {
        // `acct_lastlogin_time`: the group, then the rest as one segment.
        "bnet" => match column.split_once('_') {
            Some((group, rest)) => format!(r"BNET\{group}\{rest}"),
            None => format!(r"BNET\{column}"),
        },
        // `SEXP_0_last_game_result`: product, ladder, then a leaf whose spaces became `_`.
        "record" => {
            let mut parts = column.splitn(3, '_');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(product), Some(ladder), Some(leaf)) => format!(r"Record\{product}\{ladder}\{}", leaf.replace('_', " ")),
                _ => format!(r"Record\{column}"),
            }
        }
        "friend" => format!(r"friend\{}", column.replacen('_', "\\", 1)),
        "profile" => format!(r"profile\{column}"),
        _ => format!("{table}\\{}", column.replace('_', "\\")),
    })
}

/// Every account in a SQL dump, from its `BNET`, `Record`, `profile` and `friend` tables joined
/// on `uid`. Accounts without `acct_username` are left out.
///
/// # Errors
///
/// [`SourceError::Io`] when the file cannot be read.
pub fn read_sql_dump(path: &Path) -> Result<Vec<PvpgnAccount>, SourceError> {
    let bytes = std::fs::read(path).map_err(|e| SourceError::Io(path.to_path_buf(), e))?;
    Ok(accounts_from_dump(&String::from_utf8_lossy(&bytes)))
}

fn accounts_from_dump(sql: &str) -> Vec<PvpgnAccount> {
    let dump = Dump::parse(sql);
    let mut by_uid: BTreeMap<String, PvpgnAccount> = BTreeMap::new();
    for (table, rows) in &dump.rows {
        let table_name = match table.as_str() {
            "bnet" => "BNET",
            "record" => "Record",
            "profile" => "profile",
            "friend" => "friend",
            _ => continue,
        };
        for (columns, values) in rows {
            let Some(uid) = columns.iter().position(|c| c.eq_ignore_ascii_case("uid")).and_then(|at| values.get(at)) else { continue };
            let Value::Text(uid) = uid else { continue };
            let account = by_uid.entry(uid.clone()).or_insert_with(|| PvpgnAccount { origin: format!("uid {uid}"), attrs: BTreeMap::new() });
            for (column, value) in columns.iter().zip(values) {
                if let (Some(key), Value::Text(v)) = (column_key(table_name, column), value) {
                    account.attrs.insert(key, v.clone());
                }
            }
        }
    }
    by_uid.into_values().filter(|a| a.get(r"BNET\acct\username").is_some()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_files_unescape_keys_and_values() {
        let text = "\"BNET\\\\acct\\\\username\"=\"Zeratul\"\n\n# comment\n\"profile\\\\description\"=\"say \\\"hi\\\"\\nbye\"\n";
        let attrs = parse_plain(text).unwrap();
        assert_eq!(attrs[r"BNET\acct\username"], "Zeratul");
        assert_eq!(attrs[r"profile\description"], "say \"hi\"\nbye");
        assert!(parse_plain("\"key\" \"value\"").is_err());
    }

    #[test]
    fn columns_map_back_to_keys() {
        assert_eq!(column_key("BNET", "acct_lastlogin_time").as_deref(), Some(r"BNET\acct\lastlogin_time"));
        assert_eq!(column_key("Record", "SEXP_0_last_game_result").as_deref(), Some(r"Record\SEXP\0\last game result"));
        assert_eq!(column_key("friend", "3_uid").as_deref(), Some(r"friend\3\uid"));
        assert_eq!(column_key("profile", "location").as_deref(), Some(r"profile\location"));
        assert_eq!(column_key("BNET", "uid"), None);
    }

    #[test]
    fn a_mysql_dump_without_column_lists_joins_tables_on_uid() {
        let sql = r"
-- MySQL dump
CREATE TABLE `BNET` (
  `uid` int NOT NULL default '0',
  `acct_username` varchar(32) default NULL,
  `acct_passhash1` varchar(128) default NULL,
  `auth_admin` varchar(6) default 'false',
  PRIMARY KEY  (`uid`)
);
INSERT INTO `BNET` VALUES (1,'O\'Neil','00112233445566778899aabbccddeeff00112233','true'),(2,'Tassadar',NULL,'false');
CREATE TABLE `profile` (`uid` int, `location` varchar(128));
INSERT INTO `profile` VALUES (1,'Mar Sara');
INSERT INTO public.record (uid, SEXP_0_wins) VALUES (2, '12');
";
        let accounts = accounts_from_dump(sql);
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].get(r"BNET\acct\username"), Some("O'Neil"));
        assert_eq!(accounts[0].get(r"bnet\AUTH\admin"), Some("true"));
        assert_eq!(accounts[0].get(r"profile\location"), Some("Mar Sara"));
        assert_eq!(accounts[1].get(r"BNET\acct\passhash1"), None, "NULL is no value");
        assert_eq!(accounts[1].get(r"Record\SEXP\0\wins"), Some("12"));
    }

    #[test]
    fn a_sqlite_dump_reads_too() {
        let sql = "CREATE TABLE BNET (uid INTEGER PRIMARY KEY, acct_username TEXT, acct_ctime TEXT);\nINSERT INTO BNET VALUES(7,'Fenix','1136073600');\n";
        let accounts = accounts_from_dump(sql);
        assert_eq!((accounts[0].get(r"BNET\acct\username"), accounts[0].get(r"BNET\acct\ctime")), (Some("Fenix"), Some("1136073600")));
    }
}
