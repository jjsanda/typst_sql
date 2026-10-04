//! SQL canonicalization for cache keys (F-31).
//!
//! This is *not* `sqlite3_normalized_sql`: that API anonymizes literals (every
//! literal becomes `?`), which would make different queries collide on one
//! cache key — exactly wrong for content addressing. What the cache key needs
//! is insensitivity to formatting only: comments stripped, whitespace
//! collapsed, string/blob/identifier literals preserved byte-for-byte.
//!
//! The same code runs in the plugin (`cache_key`/`normalize_sql` exports) and
//! in the sidecar CLI, so both sides of the cache compute identical keys.

/// Canonicalize SQL text: strip `--` and `/* */` comments, collapse runs of
/// whitespace to a single space, trim, and preserve everything inside
/// `'…'`, `"…"`, `` `…` `` and `[…]` exactly.
pub fn canonicalize_sql(sql: &str) -> String {
    let b = sql.as_bytes();
    let mut out = String::with_capacity(sql.len());
    let mut i = 0;
    let mut pending_space = false;

    fn push_pending(out: &mut String, pending: &mut bool) {
        if *pending && !out.is_empty() {
            out.push(' ');
        }
        *pending = false;
    }

    while i < b.len() {
        let c = b[i];
        match c {
            b'\'' | b'"' | b'`' => {
                push_pending(&mut out, &mut pending_space);
                let quote = c;
                let start = i;
                i += 1;
                while i < b.len() {
                    if b[i] == quote {
                        // doubled quote is an escaped quote inside the literal
                        if i + 1 < b.len() && b[i + 1] == quote {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                out.push_str(&sql[start..i]);
            }
            b'[' => {
                push_pending(&mut out, &mut pending_space);
                let start = i;
                while i < b.len() && b[i] != b']' {
                    i += 1;
                }
                if i < b.len() {
                    i += 1;
                }
                out.push_str(&sql[start..i]);
            }
            b'-' if i + 1 < b.len() && b[i + 1] == b'-' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                pending_space = true;
            }
            b'/' if i + 1 < b.len() && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                pending_space = true;
            }
            _ if c.is_ascii_whitespace() => {
                pending_space = true;
                i += 1;
            }
            _ => {
                push_pending(&mut out, &mut pending_space);
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::canonicalize_sql;

    #[test]
    fn collapses_whitespace_and_comments() {
        let a = canonicalize_sql("SELECT  *\n  FROM t -- trailing\n WHERE x = 1");
        assert_eq!(a, "SELECT * FROM t WHERE x = 1");
        let b = canonicalize_sql("SELECT /* c */ * FROM t WHERE x = 1");
        assert_eq!(b, "SELECT * FROM t WHERE x = 1");
    }

    #[test]
    fn formatting_insensitive_but_literal_sensitive() {
        let a = canonicalize_sql("SELECT * FROM t WHERE region = 'EMEA'");
        let b = canonicalize_sql("SELECT   *   FROM t\nWHERE region = 'EMEA'");
        let c = canonicalize_sql("SELECT * FROM t WHERE region = 'APAC'");
        assert_eq!(a, b);
        assert_ne!(
            a, c,
            "different literals must produce different canonical SQL"
        );
    }

    #[test]
    fn preserves_string_contents() {
        let sql = "SELECT 'a  -- not a comment /* neither */  b'";
        assert_eq!(canonicalize_sql(sql), sql);
        let doubled = "SELECT 'O''Brien  Ltd'";
        assert_eq!(canonicalize_sql(doubled), doubled);
    }

    #[test]
    fn preserves_quoted_identifiers() {
        let sql = "SELECT \"weird  name\", [an other], `back  tick` FROM t";
        assert_eq!(
            canonicalize_sql(sql),
            "SELECT \"weird  name\", [an other], `back  tick` FROM t"
        );
    }
}
