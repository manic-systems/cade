use crate::expand::walk::expand_keyword;
use crate::types::keyword::{Keyword, Loadable};
use anyhow::{Context as _, Result, anyhow};
use std::fs::{exists, read as fs_read};
use std::path::Path;
use std::str::from_utf8;

mod parse;

pub fn read(path: &Path) -> Result<Vec<Keyword>> {
    let contents = fs_read(path).context("reading cade file")?;
    let mut accum = Vec::new();
    for (line_idx, raw_line) in contents.split(|&byte| byte == b'\n').enumerate() {
        let no_cr = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let line = from_utf8(no_cr).map_err(|utf8_err| {
            anyhow!(
                "parse cade file at {}: line {} is not valid UTF-8: {utf8_err}",
                path.display(),
                line_idx + 1
            )
        })?;
        match line.parse::<Keyword>() {
            Ok(kw) => accum.push(kw),
            Err(parse::ParseError::EmptyLine) => {}
            Err(parse_err) => {
                return Err(anyhow!(
                    "parse cade file at {}: line {}: {parse_err}",
                    path.display(),
                    line_idx + 1
                ));
            }
        }
    }
    Ok(accum)
}

pub fn load_dir(dir: &Path) -> Result<Vec<Keyword>> {
    let mut keywords = if exists(dir.join(".cade")).unwrap_or(false) {
        read(&dir.join(".cade")).context("reading cade file")?
    } else {
        vec![Keyword::Load(Loadable::Envrc(String::new()))]
    };
    for keyword in &mut keywords {
        expand_keyword(keyword);
    }
    Ok(keywords)
}

#[cfg(test)]
mod tests {
    use crate::cade_file::read;
    use std::env::temp_dir;
    use std::fs::{remove_file, write};
    use std::process::id;

    #[test]
    fn read_errors_on_invalid_utf8_instead_of_truncating() {
        let path = temp_dir().join(format!("cade-badutf8-{}", id()));
        let mut body = b"FOO=bar\n".to_vec();
        body.extend_from_slice(&[0xff, b'\n']);
        body.extend_from_slice(b"pure\n");
        write(&path, &body).unwrap();

        let err = read(&path).expect_err("invalid UTF-8 must be an error");
        assert!(
            err.to_string().contains("line 2"),
            "error should point at the bad line: {err}"
        );

        let _ = remove_file(&path);
    }
}
