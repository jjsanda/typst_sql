//! `quarry lint` (F-27): the two footguns worth catching mechanically.
//!
//! 1. String concatenation feeding SQL — `qr.query(db, "…" + variable)` is an
//!    injection vector and breaks on the first apostrophe; binding is the
//!    supported path (D-14).
//! 2. Literal secrets — connection strings with embedded passwords in
//!    quarry.toml or documents. Credentials belong in the environment (F-26).
//!
//! Heuristic by design: linting Typst source without evaluating it cannot be
//! sound; false negatives are possible and findings are worded as advice.

use anyhow::Result;
use std::path::Path;

#[derive(Debug)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub rule: &'static str,
    pub message: String,
}

fn scan_typ(path: &Path, text: &str, findings: &mut Vec<Finding>) {
    let query_fns = [
        "query(",
        "try-query(",
        "value(",
        "column(",
        "row(",
        "describe(",
    ];
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") {
            continue;
        }
        // concatenation inside a quarry query call on the same line
        let calls_query = query_fns
            .iter()
            .any(|f| line.contains(&format!("qr.{f}")) || line.contains(&format!("quarry.{f}")));
        if calls_query && (line.contains("\" + ") || line.contains(" + \"")) {
            findings.push(Finding {
                file: path.display().to_string(),
                line: idx + 1,
                rule: "sql-concat",
                message: "SQL built by string concatenation — bind values instead: \
                          qr.query(db, \"… WHERE x = :v\", v: value) (D-14)"
                    .into(),
            });
        }
        // password-bearing URLs anywhere in the document
        for scheme in ["postgres://", "postgresql://", "mysql://"] {
            if let Some(pos) = line.find(scheme) {
                let rest = &line[pos..];
                if rest.contains('@') && rest[..rest.find('@').unwrap()].contains(':') {
                    findings.push(Finding {
                        file: path.display().to_string(),
                        line: idx + 1,
                        rule: "literal-secret",
                        message: "connection URL with embedded credentials in a document — \
                                  use quarry.toml with url = \"env:VAR\" (F-26)"
                            .into(),
                    });
                }
            }
        }
    }
}

fn scan_toml(path: &Path, text: &str, findings: &mut Vec<Finding>) {
    for (idx, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("url") && trimmed.contains('=') {
            let value = trimmed
                .split('=')
                .nth(1)
                .unwrap_or("")
                .trim()
                .trim_matches('"');
            if !(value.starts_with("env:") || value.starts_with("file:") || value.is_empty()) {
                findings.push(Finding {
                    file: path.display().to_string(),
                    line: idx + 1,
                    rule: "literal-secret",
                    message: format!(
                        "literal connection string in quarry.toml — use url = \"env:VAR\" \
                         (found {:?}…)",
                        &value[..value.len().min(24)]
                    ),
                });
            }
        }
    }
}

pub fn lint(project_dir: &Path) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    let mut stack = vec![project_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if path.is_dir() {
                if !name.starts_with('.') && name != "target" && name != "node_modules" {
                    stack.push(path);
                }
                continue;
            }
            match path.extension().and_then(|e| e.to_str()) {
                Some("typ") => {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        scan_typ(&path, &text, &mut findings);
                    }
                }
                _ if name == "quarry.toml" => {
                    if let Ok(text) = std::fs::read_to_string(&path) {
                        scan_toml(&path, &text, &mut findings);
                    }
                }
                _ => {}
            }
        }
    }
    findings.sort_by(|a, b| (&a.file, a.line).cmp(&(&b.file, b.line)));
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_concat_and_secrets() {
        let mut findings = Vec::new();
        scan_typ(
            Path::new("doc.typ"),
            "#let r = qr.query(db, \"SELECT * FROM t WHERE x = '\" + name + \"'\")\n\
             #let ok = qr.query(db, \"SELECT 1\")\n\
             // qr.query(db, \"commented \" + out)\n",
            &mut findings,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "sql-concat");
        assert_eq!(findings[0].line, 1);

        let mut findings = Vec::new();
        scan_toml(
            Path::new("quarry.toml"),
            "[sources.wh]\nurl = \"postgres://user:hunter2@db/prod\"\n",
            &mut findings,
        );
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, "literal-secret");

        let mut findings = Vec::new();
        scan_toml(
            Path::new("quarry.toml"),
            "[sources.wh]\nurl = \"env:WAREHOUSE_URL\"\n",
            &mut findings,
        );
        assert!(findings.is_empty());
    }
}
