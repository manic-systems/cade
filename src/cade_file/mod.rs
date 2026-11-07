use std::{
    ffi::OsStr,
    fs::read as fs_read,
    path::{
        Path,
        PathBuf,
    },
    str::from_utf8,
};

use misstep::{
    Result,
    ResultExt as _,
    report,
};

use crate::{
    config,
    expand::walk::expand_keyword,
    types::keyword::{
        Keyword,
        Loadable,
    },
};

mod parse;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigKind {
    Cade,
    Envrc,
}

impl ConfigKind {
    pub const fn filename(self) -> &'static str {
        match self {
            Self::Cade => ".cade",
            Self::Envrc => ".envrc",
        }
    }
}

#[derive(Debug)]
pub struct ConfigSnapshot {
    pub kind:     ConfigKind,
    pub contents: Vec<u8>,
}

pub fn effective_config(dir: &Path) -> Result<Option<PathBuf>> {
    let cade = dir.join(".cade");
    if cade.try_exists().context("check .cade path")? {
        return Ok(Some(cade));
    }
    let envrc = dir.join(".envrc");
    if config::direnv_mode().loads_envrc() && envrc.try_exists().context("check .envrc path")? {
        return Ok(Some(envrc));
    }
    Ok(None)
}

impl ConfigSnapshot {
    pub const fn filename(&self) -> &'static str {
        self.kind.filename()
    }

    pub fn read_effective(dir: &Path) -> Result<Option<Self>> {
        let Some(path) = effective_config(dir)? else {
            return Ok(None);
        };
        let kind = if path.file_name() == Some(OsStr::new(".cade")) {
            ConfigKind::Cade
        } else {
            ConfigKind::Envrc
        };
        let contents = fs_read(&path).context("read effective project configuration")?;
        Ok(Some(Self { kind, contents }))
    }

    pub fn keywords(&self, dir: &Path) -> Result<Vec<Keyword>> {
        let mut keywords = match self.kind {
            ConfigKind::Cade => parse_contents(&dir.join(".cade"), &self.contents)?,
            ConfigKind::Envrc => vec![Keyword::Load(Loadable::Envrc(String::new()))],
        };
        for keyword in &mut keywords {
            expand_keyword(keyword);
        }
        Ok(keywords)
    }
}

pub fn read(path: &Path) -> Result<Vec<Keyword>> {
    parse_contents(path, &fs_read(path).context("reading cade file")?)
}

fn parse_contents(path: &Path, contents: &[u8]) -> Result<Vec<Keyword>> {
    let mut accum = Vec::new();
    for (line_idx, raw_line) in contents.split(|&byte| byte == b'\n').enumerate() {
        let no_cr = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let line = from_utf8(no_cr).map_err(|utf8_err| {
            report!(
                "parse cade file at {}: line {} is not valid UTF-8: {utf8_err}",
                path.display(),
                line_idx + 1
            )
        })?;
        match line.parse::<Keyword>() {
            Ok(kw) => accum.push(kw),
            Err(parse::ParseError::EmptyLine) => {},
            Err(parse_err) => {
                return Err(report!(
                    "parse cade file at {}: line {}: {parse_err}",
                    path.display(),
                    line_idx + 1
                ));
            },
        }
    }
    Ok(accum)
}

#[cfg(test)]
mod tests {
    use std::{
        env::temp_dir,
        fs::{
            remove_file,
            write,
        },
        process::id,
    };

    use crate::cade_file::read;

    #[test]
    fn read_errors_on_invalid_utf8_instead_of_truncating() {
        let path = temp_dir().join(format!("cade-badutf8-{}", id()));
        let mut body = b"FOO=bar\n".to_vec();
        body.extend_from_slice(&[0xFF, b'\n']);
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
