use std::{
    env::var,
    ffi::OsStr,
    fs::{
        OpenOptions,
        canonicalize,
    },
    path::{
        Path,
        PathBuf,
    },
    process::Command,
};

use misstep::{
    OptionExt as _,
    Result,
    ResultExt as _,
    report,
};

use crate::{
    cade_file::effective_config,
    core::{
        Cade,
        participants::find_cade_root,
        permissions::set_permission,
    },
    shells::quote::split_words,
};

pub(super) fn resolve_project(
    cade: &Cade,
    path: Option<&Path>,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let requested = path.map_or_else(|| cade.cwd.clone(), |target| cade.cwd.join(target));
    if requested.is_dir() {
        let dir = canonicalize(&requested).context("resolve project directory")?;
        let root = find_cade_root(&dir).unwrap_or(dir);
        let config = effective_config(&root)?;
        return Ok((root, config));
    }

    resolve_config_file(&requested)
}

fn resolve_config_file(requested: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let name = requested
        .file_name()
        .context("find configuration filename")?;
    if name != ".cade" && name != ".envrc" {
        return Err(report!("expected a project directory, .cade, or .envrc"));
    }
    let root = canonicalize(requested.parent().context("find project directory")?)
        .context("resolve project directory")?;
    let config = effective_config(&root)?;
    validate_requested_config(requested, config.as_deref())?;
    Ok((root, config))
}

fn validate_requested_config(requested: &Path, effective: Option<&Path>) -> Result<()> {
    let matches = match effective {
        Some(selected) => same_config_file(requested, selected)?,
        None => {
            requested.file_name() == Some(OsStr::new(".cade"))
                && !requested.try_exists().context("check configuration path")?
        },
    };
    if !matches {
        return Err(report!(
            "requested file is not the effective project configuration"
        ));
    }
    Ok(())
}

fn same_config_file(requested: &Path, effective: &Path) -> Result<bool> {
    Ok(effective.file_name() == requested.file_name()
        && canonicalize(effective).context("resolve effective configuration")?
            == canonicalize(requested).context("resolve requested configuration")?)
}

pub fn edit_project(cade: &Cade, path: Option<&Path>) -> Result<()> {
    let (root, config) = resolve_project(cade, path)?;
    let file = config.unwrap_or_else(|| root.join(".cade"));
    if !file.try_exists().context("check project configuration")? {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .context("create project configuration")?;
    }
    run_editor(&root, &file)?;
    let effective =
        effective_config(&root)?.context("editor did not leave a project configuration")?;
    if effective != file {
        return Err(report!(
            "edited file is no longer the effective project configuration"
        ));
    }
    set_permission(cade, &root, true)
}

fn run_editor(root: &Path, file: &Path) -> Result<()> {
    let editor = var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| var("EDITOR").ok().filter(|value| !value.trim().is_empty()))
        .unwrap_or_else(|| "vi".to_owned());
    let parts = split_words(&editor).context("parse editor command")?;
    let (program, args) = parts.split_first().context("editor command is empty")?;
    let status = Command::new(program)
        .args(args)
        .arg(file)
        .current_dir(root)
        .status()
        .context("run editor process")?;
    if !status.success() {
        return Err(report!("editor exited unsuccessfully: {status}"));
    }
    Ok(())
}
