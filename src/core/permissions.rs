use super::{Cade, Keyword, find_cade_root, participant_dirs};
use crate::verbosity::{self, Verbosity};
use anyhow::{Context, Result};
use rusqlite::named_params;
use std::path::{Path, PathBuf};

impl Cade {
    pub fn allow_here(&mut self, permission: bool) -> Result<()> {
        let root = find_cade_root(&self.cwd).unwrap_or_else(|| self.cwd.clone());
        if !permission {
            return self.set_permission(&root, false);
        }
        // gap-fill follows participants not parents
        let chain = participant_dirs(&root);
        if chain.is_empty() {
            return Ok(());
        }
        let mut base = None;
        for (i, dir) in chain.iter().enumerate() {
            if self.get_permission(dir)? {
                base = Some(i);
                break;
            }
        }
        let upto = base.unwrap_or(1);
        for dir in &chain[0..upto] {
            self.record_permission(dir, true)?;
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

    // silent db write
    fn record_permission(&self, path: &Path, permission: bool) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO WorkingPaths (Path, Permission) VALUES (:path, :perm);",
            named_params! {
                    ":path": path.to_str().context("parse path as unicode")?,
                    ":perm": permission,
            },
        )?;
        Ok(())
    }

    pub fn set_permission(&mut self, path: &Path, permission: bool) -> Result<()> {
        self.record_permission(path, permission)?;
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

    pub fn get_permission(&mut self, path: &Path) -> Result<bool> {
        let path_str = path.to_str().context("parse path as unicode")?;
        match self.db.query_one(
            "SELECT Permission FROM WorkingPaths WHERE Path=(:path)",
            &[(":path", &path_str)],
            |row| row.get::<_, bool>(0),
        ) {
            Ok(allowed) => Ok(allowed),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    // tip first run anchored at the deepest approval
    fn approved_participants(&mut self, participants: &[PathBuf]) -> Result<Vec<PathBuf>> {
        let mut active = Vec::new();
        let mut anchored = false;
        for p in participants {
            if self.get_permission(p)? {
                anchored = true;
                active.push(p.clone());
            } else if anchored {
                break;
            }
        }
        Ok(active)
    }

    pub(super) fn approved_chain(&mut self, root: &Path) -> Result<Vec<(PathBuf, Vec<Keyword>)>> {
        let approved = self.approved_participants(&participant_dirs(root))?;
        let mut chain = Vec::with_capacity(approved.len());
        for path in approved {
            let keywords = crate::cade_file::load_dir(&path)?;
            chain.push((path, keywords));
        }
        chain.reverse();
        Ok(chain)
    }

    pub(super) fn resolve_active(&mut self, cwd: &Path) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
        let participants = participant_dirs(cwd);
        let active = self.approved_participants(&participants)?;
        let disallowed_tip = match participants.first() {
            Some(tip) if active.first() != Some(tip) => Some(tip.clone()),
            _ => None,
        };
        Ok((active, disallowed_tip))
    }
}
