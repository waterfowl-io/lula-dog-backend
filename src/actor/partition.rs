//! Key-partitioned storage inside one workspace (spike).
//!
//! Every workspace has one **shared** partition (today's
//! `workspaces/<ws>.db`). A table group can also be partitioned by a key
//! (for example `user` by user id): each key value gets its own SQLite file
//! and actor under `workspaces/<ws>.parts/<group>/<key>.db`.
//!
//! Hard invariant: the workspace id is the outer boundary. Paths are derived
//! only from a validated [`PartitionKey`], so nothing can escape its
//! workspace's directory, and the read fan-out refuses to attach any
//! partition from another workspace.

use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Upper bound on how many partitions one fan-out read may attach.
pub const DEFAULT_FANOUT_LIMIT: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Partition {
    /// The workspace-wide partition; transactions across users happen here.
    Shared,
    /// One partition per key value of a partitioned table group.
    Keyed { group: String, key: String },
}

/// Which actor/file a command or read targets. Always carries its workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PartitionKey {
    pub workspace_id: Uuid,
    pub partition: Partition,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PartitionError {
    #[error("invalid table group name {0:?}: use [a-z][a-z0-9_]{{0,31}}")]
    InvalidGroup(String),
    #[error("invalid partition key {0:?}: use [A-Za-z0-9_-]{{1,64}}")]
    InvalidKey(String),
    #[error("partition belongs to another workspace; cross-workspace access is never allowed")]
    CrossWorkspace,
    #[error("fan-out over {requested} partitions exceeds the limit of {limit}")]
    FanoutTooLarge { requested: usize, limit: usize },
}

fn valid_group(group: &str) -> bool {
    let mut chars = group.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && group.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl PartitionKey {
    pub fn shared(workspace_id: Uuid) -> Self {
        Self {
            workspace_id,
            partition: Partition::Shared,
        }
    }

    /// A keyed partition, validating `group` and `key` so neither can carry
    /// path separators, dots or anything else that could escape the
    /// workspace directory.
    pub fn keyed(workspace_id: Uuid, group: &str, key: &str) -> Result<Self, PartitionError> {
        if !valid_group(group) {
            return Err(PartitionError::InvalidGroup(group.to_owned()));
        }
        if !valid_key(key) {
            return Err(PartitionError::InvalidKey(key.to_owned()));
        }
        Ok(Self {
            workspace_id,
            partition: Partition::Keyed {
                group: group.to_owned(),
                key: key.to_owned(),
            },
        })
    }

    /// The SQLite file for this partition. The shared partition keeps
    /// today's path, so existing workspaces need no file move.
    pub fn path(&self, data_dir: &Path) -> PathBuf {
        let workspaces = data_dir.join("workspaces");
        match &self.partition {
            Partition::Shared => workspaces.join(format!("{}.db", self.workspace_id)),
            Partition::Keyed { group, key } => workspaces
                .join(format!("{}.parts", self.workspace_id))
                .join(group)
                .join(format!("{key}.db")),
        }
    }
}

/// DuckDB `ATTACH` statements for a read fan-out across `partitions` of
/// `workspace_id`, all `READ_ONLY`, as `(alias, statement)` pairs; the
/// aliases are `p0`, `p1`, ... in input order. Refuses any partition from
/// another workspace and any fan-out larger than `limit`.
pub fn fanout_attach_sql(
    data_dir: &Path,
    workspace_id: Uuid,
    partitions: &[PartitionKey],
    limit: usize,
) -> Result<Vec<(String, String)>, PartitionError> {
    if partitions.len() > limit {
        return Err(PartitionError::FanoutTooLarge {
            requested: partitions.len(),
            limit,
        });
    }
    if partitions.iter().any(|p| p.workspace_id != workspace_id) {
        return Err(PartitionError::CrossWorkspace);
    }
    Ok(partitions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let alias = format!("p{i}");
            let path = p.path(data_dir).display().to_string().replace('\'', "''");
            let sql = format!("ATTACH '{path}' AS {alias} (TYPE SQLITE, READ_ONLY);");
            (alias, sql)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_cannot_escape_the_workspace_directory() -> Result<(), PartitionError> {
        let ws = Uuid::new_v4();
        for bad in ["../x", "a/b", "a.b", "", "..", "x y"] {
            assert_eq!(
                PartitionKey::keyed(ws, "user", bad),
                Err(PartitionError::InvalidKey(bad.to_owned()))
            );
        }
        assert!(PartitionKey::keyed(ws, "User", "k").is_err());
        assert!(PartitionKey::keyed(ws, "../g", "k").is_err());
        let dir = Path::new("/data");
        let p = PartitionKey::keyed(ws, "user", "u1")?.path(dir);
        assert!(p.starts_with(dir.join("workspaces").join(format!("{ws}.parts"))));
        assert_eq!(
            PartitionKey::shared(ws).path(dir),
            dir.join("workspaces").join(format!("{ws}.db"))
        );
        Ok(())
    }

    #[test]
    fn same_key_in_two_workspaces_is_two_files() -> Result<(), PartitionError> {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let dir = Path::new("/data");
        let pa = PartitionKey::keyed(a, "user", "u1")?.path(dir);
        let pb = PartitionKey::keyed(b, "user", "u1")?.path(dir);
        assert_ne!(pa, pb);
        Ok(())
    }

    #[test]
    fn fanout_refuses_other_workspaces_and_oversized_reads() -> Result<(), PartitionError> {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let dir = Path::new("/data");
        let mine = vec![
            PartitionKey::shared(a),
            PartitionKey::keyed(a, "user", "u1")?,
        ];
        let sql = fanout_attach_sql(dir, a, &mine, 8)?;
        let aliases: Vec<&str> = sql.iter().map(|(alias, _)| alias.as_str()).collect();
        assert_eq!(aliases, ["p0", "p1"]);
        assert!(sql.iter().all(|(_, s)| s.contains("READ_ONLY")));

        let mut mixed = mine.clone();
        mixed.push(PartitionKey::keyed(b, "user", "u1")?);
        assert_eq!(
            fanout_attach_sql(dir, a, &mixed, 8),
            Err(PartitionError::CrossWorkspace)
        );
        assert_eq!(
            fanout_attach_sql(dir, a, &mine, 1),
            Err(PartitionError::FanoutTooLarge {
                requested: 2,
                limit: 1
            })
        );
        Ok(())
    }
}
