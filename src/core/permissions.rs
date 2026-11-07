use std::{
    fs::canonicalize,
    path::{
        Path,
        PathBuf,
    },
};

use misstep::{
    OptionExt as _,
    Result,
    ResultExt as _,
};
use rusqlite::{
    OptionalExtension as _,
    Transaction,
    TransactionBehavior,
    named_params,
};

use crate::{
    cade_file::ConfigSnapshot,
    core::{
        Cade,
        participants::{
            find_cade_root,
            participant_dirs,
        },
    },
    verbosity::{
        self,
        Verbosity,
    },
};

pub fn allow_here(cade: &Cade, permission: bool) -> Result<()> {
    let Some(root) = find_cade_root(&cade.cwd) else {
        return Ok(());
    };
    if !permission {
        return set_permission(cade, &root, false);
    }

    let chain = participant_dirs(&root);
    let upto = approval_gap_end(cade, &chain)?;
    for dir in chain.iter().take(upto) {
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

fn approval_gap_end(cade: &Cade, chain: &[PathBuf]) -> Result<usize> {
    for (idx, dir) in chain.iter().enumerate().skip(1) {
        if get_permission(cade, dir)? {
            return Ok(idx);
        }
        // New gaps can be approved, previously rejected or changed layers
        // cannot.
        if has_permission_record(cade, dir)? {
            break;
        }
    }
    Ok(1)
}

pub(super) fn ensure_permission_schema(conn: &rusqlite::Connection) -> Result<()> {
    if permission_columns(conn)? == [true, true] {
        return Ok(());
    }
    let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let columns = permission_columns(&transaction)?;
    for ((name, kind), present) in [("ConfigFile", "TEXT"), ("ConfigContents", "BLOB")]
        .into_iter()
        .zip(columns)
    {
        if !present {
            transaction.execute(
                &format!("ALTER TABLE WorkingPaths ADD COLUMN {name} {kind}"),
                [],
            )?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn permission_columns(conn: &rusqlite::Connection) -> Result<[bool; 2]> {
    let mut statement = conn.prepare("PRAGMA table_info(WorkingPaths)")?;
    let mut rows = statement.query([])?;
    let mut present = [false, false];
    while let Some(row) = rows.next()? {
        match row.get_ref(1)?.as_str()? {
            "ConfigFile" => present[0] = true,
            "ConfigContents" => present[1] = true,
            _ => {},
        }
    }
    Ok(present)
}

fn has_permission_record(cade: &Cade, path: &Path) -> Result<bool> {
    let root = canonicalize(path).context("resolve permission directory")?;
    Ok(cade.db.query_one(
        "SELECT EXISTS(SELECT 1 FROM WorkingPaths WHERE Path = ?1)",
        [root.to_str().context("parse path as unicode")?],
        |row| row.get(0),
    )?)
}

fn record_permission(cade: &Cade, path: &Path, permission: bool) -> Result<()> {
    let root = canonicalize(path).context("resolve permission directory")?;
    let config = if permission {
        Some(
            ConfigSnapshot::read_effective(&root)?
                .context("no effective project configuration to approve")?,
        )
    } else {
        None
    };
    let contents = config.as_ref().map(|snapshot| snapshot.contents.as_slice());
    let filename = config.as_ref().map(|snapshot| snapshot.kind.filename());
    cade.db.execute(
        "INSERT OR REPLACE INTO WorkingPaths (Path, Permission, ConfigFile, ConfigContents) \
         VALUES (:path, :perm, :file, :contents);",
        named_params! {
            ":path": root.to_str().context("parse path as unicode")?,
            ":perm": permission,
            ":file": filename,
            ":contents": contents,
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
    Ok(approved_config(cade, path)?.is_some())
}

fn recorded_approval(cade: &Cade, root: &Path) -> Result<Option<(String, Vec<u8>)>> {
    let record = cade
        .db
        .query_one(
            "SELECT Permission, ConfigFile, ConfigContents FROM WorkingPaths WHERE Path = ?1",
            [root.to_str().context("parse path as unicode")?],
            |row| {
                Ok((
                    row.get::<_, bool>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                ))
            },
        )
        .optional()?;
    Ok(match record {
        Some((true, Some(filename), Some(contents))) => Some((filename, contents)),
        _ => None,
    })
}

fn approved_config(cade: &Cade, path: &Path) -> Result<Option<ConfigSnapshot>> {
    let root = canonicalize(path).context("resolve permission directory")?;
    let Some((filename, contents)) = recorded_approval(cade, &root)? else {
        return Ok(None);
    };
    let Some(snapshot) = ConfigSnapshot::read_effective(&root)? else {
        return Ok(None);
    };
    if snapshot.kind.filename() == filename && snapshot.contents == contents {
        return Ok(Some(snapshot));
    }
    Ok(None)
}

fn approved_participants(
    cade: &Cade,
    participants: &[PathBuf],
) -> Result<Vec<(PathBuf, ConfigSnapshot)>> {
    let mut active = Vec::new();
    let mut anchored = false;
    for participant in participants {
        if let Some(snapshot) = approved_config(cade, participant)? {
            anchored = true;
            active.push((participant.clone(), snapshot));
        } else if anchored {
            break;
        }
    }
    Ok(active)
}

pub(super) fn approved_chain(cade: &Cade, root: &Path) -> Result<Vec<(PathBuf, ConfigSnapshot)>> {
    let mut chain = approved_participants(cade, &participant_dirs(root))?;
    chain.reverse();
    Ok(chain)
}

pub(super) fn resolve_active(cade: &Cade, cwd: &Path) -> Result<(Vec<PathBuf>, Option<PathBuf>)> {
    let participants = participant_dirs(cwd);
    let active: Vec<_> = approved_participants(cade, &participants)?
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    let disallowed_tip = match participants.first() {
        Some(tip) if active.first() != Some(tip) => Some(tip.clone()),
        _ => None,
    };
    Ok((active, disallowed_tip))
}

#[cfg(test)]
mod tests {
    use std::{
        env::temp_dir,
        fs::{
            create_dir_all,
            remove_dir_all,
            write,
        },
        process::id,
        sync::{
            OnceLock,
            mpsc,
        },
        thread,
        time::Duration,
    };

    use rusqlite::{
        Connection,
        Transaction,
        TransactionBehavior,
    };

    use super::{
        approved_chain,
        ensure_permission_schema,
        set_permission,
    };
    use crate::{
        core::{
            Cade,
            layer::load_single_layer,
        },
        env::rollup::rollup_envs,
    };

    fn test_cade(name: &str) -> Cade {
        let cwd = temp_dir().join(format!("cade-trust-{name}-{}", id()));
        create_dir_all(&cwd).unwrap();
        let db = Connection::open_in_memory().unwrap();
        Cade::ensure_db(&db).unwrap();
        Cade {
            db,
            state_dir: cwd.join("state"),
            cwd,
        }
    }

    #[test]
    fn approved_cade_snapshot_is_used_after_configuration_replacement() {
        let cade = test_cade("cade-snapshot");
        write(cade.cwd.join(".cade"), "TRUST_VALUE=approved\n").unwrap();
        set_permission(&cade, &cade.cwd, true).unwrap();
        let chain = approved_chain(&cade, &cade.cwd).unwrap();
        write(cade.cwd.join(".cade"), "TRUST_VALUE=replaced\n").unwrap();

        let approved = chain.last().unwrap();
        let path = &approved.0;
        let snapshot = &approved.1;
        let keywords = snapshot.keywords(path).unwrap();
        let (_, layer) = load_single_layer(0, path, &keywords, snapshot, &cade, "test").unwrap();
        let rolled = rollup_envs(vec![layer]);
        assert_eq!(rolled.env()["TRUST_VALUE"].as_slice(), ["approved"]);
        assert!(!super::get_permission(&cade, &cade.cwd).unwrap());
        remove_dir_all(&cade.cwd).unwrap();
    }

    #[test]
    fn approved_implicit_envrc_snapshot_survives_replacement_and_config_kind_switch() {
        let cade = test_cade("envrc-snapshot");
        write(cade.cwd.join(".envrc"), "export TRUST_VALUE=approved\n").unwrap();
        set_permission(&cade, &cade.cwd, true).unwrap();
        let chain = approved_chain(&cade, &cade.cwd).unwrap();
        write(cade.cwd.join(".envrc"), "export TRUST_VALUE=replaced\n").unwrap();
        write(cade.cwd.join(".cade"), "TRUST_VALUE=shadowed\n").unwrap();

        let approved = chain.last().unwrap();
        let path = &approved.0;
        let snapshot = &approved.1;
        let keywords = snapshot.keywords(path).unwrap();
        let (_, layer) = load_single_layer(0, path, &keywords, snapshot, &cade, "test").unwrap();
        let rolled = rollup_envs(vec![layer]);
        assert_eq!(rolled.env()["TRUST_VALUE"].as_slice(), ["approved"]);
        assert!(!super::get_permission(&cade, &cade.cwd).unwrap());
        remove_dir_all(&cade.cwd).unwrap();
    }

    #[test]
    fn migrated_permission_schema_does_not_contend_with_active_writer() {
        let cade = test_cade("migrated-schema-reader");
        let path = cade.cwd.join("migrated.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE WorkingPaths (Path TEXT PRIMARY KEY, Permission INTEGER NOT NULL \
                 DEFAULT 0)",
            )
            .unwrap();
        ensure_permission_schema(&writer).unwrap();
        let transaction =
            Transaction::new_unchecked(&writer, TransactionBehavior::Immediate).unwrap();
        let reader = Connection::open(path).unwrap();
        reader.busy_timeout(Duration::ZERO).unwrap();

        ensure_permission_schema(&reader).unwrap();
        let columns = super::permission_columns(&reader).unwrap();
        assert_eq!(columns, [true, true]);
        transaction.commit().unwrap();
        remove_dir_all(&cade.cwd).unwrap();
    }

    static MIGRATION_BLOCKED: OnceLock<mpsc::Sender<()>> = OnceLock::new();

    fn wait_for_schema_writer(attempt: i32) -> bool {
        if attempt == 0_i32 {
            MIGRATION_BLOCKED.get().unwrap().send(()).unwrap();
        }
        thread::sleep(Duration::from_millis(1));
        attempt < 5_000
    }

    #[test]
    fn concurrent_permission_migration_rechecks_columns_after_acquiring_writer() {
        let cade = test_cade("concurrent-migration");
        let path = cade.cwd.join("legacy.db");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE WorkingPaths (Path TEXT PRIMARY KEY, Permission INTEGER NOT NULL \
                 DEFAULT 0)",
            )
            .unwrap();
        let transaction =
            Transaction::new_unchecked(&writer, TransactionBehavior::Immediate).unwrap();
        let (blocked, observed) = mpsc::channel();
        MIGRATION_BLOCKED.set(blocked).unwrap();
        let migration = thread::spawn(move || {
            let mut conn = Connection::open(path).unwrap();
            conn.busy_handler(Some(wait_for_schema_writer)).unwrap();
            ensure_permission_schema(&conn).unwrap();
        });

        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        transaction
            .execute("ALTER TABLE WorkingPaths ADD COLUMN ConfigFile TEXT", [])
            .unwrap();
        transaction
            .execute("ALTER TABLE WorkingPaths ADD COLUMN ConfigContents BLOB", [
            ])
            .unwrap();
        transaction.commit().unwrap();
        migration.join().unwrap();
        writer
            .execute(
                "INSERT INTO WorkingPaths (Path, Permission, ConfigFile, ConfigContents) VALUES \
                 ('project', 1, '.cade', X'616263')",
                [],
            )
            .unwrap();
        let record = writer
            .query_one(
                "SELECT ConfigFile, ConfigContents FROM WorkingPaths WHERE Path = 'project'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .unwrap();
        assert_eq!(record, (".cade".to_owned(), b"abc".to_vec()));
        remove_dir_all(&cade.cwd).unwrap();
    }
}
