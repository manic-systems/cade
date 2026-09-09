use crate::cade_file::load_dir;
use crate::core::Cade;
use crate::core::participants::find_cade_root;
use crate::core::participants::participant_dirs;
use crate::types::keyword::Keyword;
use crate::verbosity::{self, Verbosity};
use anyhow::{Context as _, Result};
use rusqlite::named_params;
use std::path::{Path, PathBuf};

pub fn allow_here(cade: &Cade, permission: bool) -> Result<()> {
    let root = find_cade_root(&cade.cwd).unwrap_or_else(|| cade.cwd.clone());
    if !permission {
        return set_permission(cade, &root, false);
    }

    let chain = participant_dirs(&root);
    if chain.is_empty() {
        return Ok(());
    }
    let mut base = None;
    for (idx, dir) in chain.iter().enumerate() {
        if get_permission(cade, dir)? {
            base = Some(idx);
            break;
        }
    }
    let upto = base.unwrap_or(1);
    for dir in &chain[0..upto] {
        record_permission(cade, dir, true)?;
    }
    verbosity::log(
        Verbosity::Normal,
        format_args!(
            "cade is now allowed in {}{}.",
            root.display(),
            if upto > 1 {
                format!(" (+{} parent layer(s), up to the approved base)", upto - 1)
            } else {
                String::new()
            }
        ),
    );
    Ok(())
}

fn record_permission(cade: &Cade, path: &Path, permission: bool) -> Result<()> {
    cade.db.execute(
        "INSERT OR REPLACE INTO WorkingPaths (Path, Permission) VALUES (:path, :perm);",
        named_params! {
                ":path": path.to_str().context("parse path as unicode")?,
                ":perm": permission,
        },
    )?;
    Ok(())
}

pub fn set_permission(cade: &Cade, path: &Path, permission: bool) -> Result<()> {
    record_permission(cade, path, permission)?;
    verbosity::log(
        Verbosity::Normal,
        format_args!(
            "cade is now {} in {}.",
            if permission { "allowed" } else { "disallowed" },
            path.display()
        ),
    );
    Ok(())
}

pub(super) fn get_permission(cade: &Cade, path: &Path) -> Result<bool> {
    let path_str = path.to_str().context("parse path as unicode")?;
    match cade.db.query_one(
        "SELECT Permission FROM WorkingPaths WHERE Path=(:path)",
        &[(":path", &path_str)],
        |row| row.get::<_, bool>(0),
    ) {
        Ok(allowed) => Ok(allowed),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn approved_participants(cade: &Cade, participants: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut active = Vec::new();
    let mut anchored = false;
    for participant in participants {
        if get_permission(cade, participant)? {
            anchored = true;
            active.push(participant.clone());
        } else if anchored {
            break;
        }
    }
    Ok(active)
}

pub(super) fn approved_chain(cade: &Cade, root: &Path) -> Result<Vec<(PathBuf, Vec<Keyword>)>> {
    let approved = approved_participants(cade, &participant_dirs(root))?;
    let mut chain = Vec::with_capacity(approved.len());
    for path in approved {
        let keywords = load_dir(&path)?;
        chain.push((path, keywords));
    }
    chain.reverse();
    Ok(chain)
}

pub(super) fn resolve_active(cade: &Cade, cwd: &Path) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
    let participants = participant_dirs(cwd);
    let active = approved_participants(cade, &participants)?;
    let disallowed_tip = match participants.first() {
        Some(tip) if active.first() != Some(tip) => Some(tip.clone()),
        _ => None,
    };
    Ok((active, disallowed_tip))
}
