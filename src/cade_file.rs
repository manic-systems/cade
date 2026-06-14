use crate::types::{Keyword, Loadable};
use anyhow::{Context, Result, anyhow};
use std::path::Path;

mod parse;

pub(crate) fn read(path: &Path) -> Result<Vec<Keyword>> {
    let contents = std::fs::read(path).context("reading cade file")?;
    let mut accum = Vec::new();
    for (n, raw) in contents.split(|&b| b == b'\n').enumerate() {
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        let line = std::str::from_utf8(raw).map_err(|e| {
            anyhow!(
                "parse cade file at {}: line {} is not valid UTF-8: {e}",
                path.display(),
                n + 1
            )
        })?;
        match line.parse::<Keyword>() {
            Ok(kw) => accum.push(kw),
            Err(parse::ParseError::EmptyLine) => continue,
            Err(e) => {
                return Err(anyhow!(
                    "parse cade file at {}: line {}: {e}",
                    path.display(),
                    n + 1
                ));
            }
        }
    }
    Ok(accum)
}

pub(crate) fn load_dir(dir: &Path) -> Result<Vec<Keyword>> {
    let mut keywords = if std::fs::exists(dir.join(".cade")).unwrap_or(false) {
        read(&dir.join(".cade")).context("reading cade file")?
    } else {
        vec![Keyword::Load(Loadable::Envrc(String::new()))]
    };
    for keyword in &mut keywords {
        crate::expand::expand_keyword(keyword);
    }
    Ok(keywords)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_errors_on_invalid_utf8_instead_of_truncating() {
        let path = std::env::temp_dir().join(format!("cade-badutf8-{}", std::process::id()));
        let mut body = b"FOO=bar\n".to_vec();
        body.extend_from_slice(&[0xff, b'\n']);
        body.extend_from_slice(b"pure\n");
        std::fs::write(&path, &body).unwrap();

        let err = read(&path).expect_err("invalid UTF-8 must be an error");
        assert!(
            err.to_string().contains("line 2"),
            "error should point at the bad line: {err}"
        );

        std::fs::remove_file(&path).ok();
    }
}
