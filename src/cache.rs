use crate::{
    config::private_dir,
    model::{Folder, Message, MessageFlags},
};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, params};
use std::path::Path;

pub struct Cache {
    db: Connection,
}

impl Cache {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            private_dir(parent)?;
        }
        let db = Connection::open(path).context("Cannot open mail cache")?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        let version: i32 = db.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > 1 {
            bail!("Cache was created by a newer version of TerminalEmail");
        }
        db.execute_batch("BEGIN;
            CREATE TABLE IF NOT EXISTS folders (name TEXT PRIMARY KEY, unread INTEGER NOT NULL, role TEXT, validity INTEGER);
            CREATE TABLE IF NOT EXISTS messages (folder TEXT NOT NULL, uid INTEGER NOT NULL, payload TEXT NOT NULL, PRIMARY KEY(folder, uid));
            CREATE VIRTUAL TABLE IF NOT EXISTS search USING fts5(folder UNINDEXED, uid UNINDEXED, sender, recipients, subject, body);
            PRAGMA user_version=1;
            COMMIT;")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(Self { db })
    }

    #[cfg(test)]
    pub fn total_changes(&self) -> u64 {
        self.db.total_changes()
    }

    pub fn folders(&self) -> Result<Vec<Folder>> {
        let mut query = self
            .db
            .prepare("SELECT name, unread, role FROM folders ORDER BY name != 'INBOX', name")?;
        Ok(query
            .query_map([], |r| {
                Ok(Folder {
                    name: r.get(0)?,
                    unread: r.get(1)?,
                    role: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn set_folders(&mut self, folders: &[Folder]) -> Result<()> {
        let tx = self.db.transaction()?;
        for folder in folders {
            tx.execute("INSERT INTO folders(name, unread, role) VALUES (?1, ?2, ?3) ON CONFLICT(name) DO UPDATE SET unread=excluded.unread, role=excluded.role WHERE unread != excluded.unread OR role IS NOT excluded.role", params![folder.name, folder.unread, folder.role])?;
        }
        // Remove mailboxes which no longer exist, including their searchable data.
        let names: std::collections::HashSet<_> = folders.iter().map(|f| f.name.as_str()).collect();
        let old: Vec<String> = tx
            .prepare("SELECT name FROM folders")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for name in old {
            if !names.contains(name.as_str()) {
                tx.execute("DELETE FROM folders WHERE name=?1", [&name])?;
                tx.execute("DELETE FROM messages WHERE folder=?1", [&name])?;
                tx.execute("DELETE FROM search WHERE folder=?1", [&name])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn check_validity(&mut self, folder: &str, validity: u32) -> Result<bool> {
        let previous: Option<u32> = self.db.query_row(
            "SELECT validity FROM folders WHERE name=?1",
            [folder],
            |r| r.get(0),
        )?;
        if previous == Some(validity) {
            return Ok(false);
        }
        let tx = self.db.transaction()?;
        tx.execute("DELETE FROM messages WHERE folder=?1", [folder])?;
        tx.execute("DELETE FROM search WHERE folder=?1", [folder])?;
        tx.execute(
            "UPDATE folders SET validity=?2 WHERE name=?1",
            params![folder, validity],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn messages(&self, folder: &str) -> Result<Vec<Message>> {
        let mut query = self
            .db
            .prepare("SELECT payload FROM messages WHERE folder=?1 ORDER BY uid DESC")?;
        let rows = query.query_map([folder], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }

    pub fn message(&self, folder: &str, uid: u32) -> Result<Option<Message>> {
        use rusqlite::OptionalExtension;
        let payload: Option<String> = self
            .db
            .query_row(
                "SELECT payload FROM messages WHERE folder=?1 AND uid=?2",
                params![folder, uid],
                |r| r.get(0),
            )
            .optional()?;
        payload.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn uids(&self, folder: &str) -> Result<std::collections::HashSet<u32>> {
        let mut query = self
            .db
            .prepare("SELECT uid FROM messages WHERE folder=?1")?;
        Ok(query
            .query_map([folder], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn put(&mut self, message: &Message) -> Result<()> {
        self.put_batch(std::slice::from_ref(message))
    }

    pub fn put_batch(&mut self, messages: &[Message]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut put = tx.prepare_cached("INSERT INTO messages(folder, uid, payload) VALUES (?1, ?2, ?3) ON CONFLICT(folder, uid) DO UPDATE SET payload=excluded.payload")?;
            let mut delete = tx.prepare_cached("DELETE FROM search WHERE folder=?1 AND uid=?2")?;
            let mut index = tx.prepare_cached("INSERT INTO search(folder, uid, sender, recipients, subject, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)")?;
            for message in messages {
                put.execute(params![
                    message.folder,
                    message.uid,
                    serde_json::to_string(message)?
                ])?;
                delete.execute(params![message.folder, message.uid])?;
                index.execute(params![
                    message.folder,
                    message.uid,
                    message.from,
                    format!("{} {}", message.to, message.cc),
                    message.subject,
                    message.body.as_deref().unwrap_or_default()
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn update_flags(
        &mut self,
        folder: &str,
        validity: u32,
        flags: &[MessageFlags],
    ) -> Result<Vec<MessageFlags>> {
        let tx = self.db.transaction()?;
        let mut changed = Vec::new();
        {
            // Update just the JSON flags; bodies and the full-text index are untouched.
            let mut update = tx.prepare_cached("UPDATE messages SET payload=json_set(payload, '$.seen', json(?4), '$.starred', json(?5)) WHERE folder=?1 AND uid=?2 AND json_extract(payload, '$.uid_validity')=?3 AND (json_extract(payload, '$.seen') != ?6 OR json_extract(payload, '$.starred') != ?7)")?;
            for flag in flags {
                if update.execute(params![
                    folder,
                    flag.uid,
                    validity,
                    if flag.seen { "true" } else { "false" },
                    if flag.starred { "true" } else { "false" },
                    flag.seen,
                    flag.starred
                ])? > 0
                {
                    changed.push(*flag);
                }
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    pub fn reconcile(
        &mut self,
        folder: &str,
        remote: &std::collections::HashSet<u32>,
    ) -> Result<Vec<u32>> {
        let tx = self.db.transaction()?;
        let existing: Vec<u32> = tx
            .prepare("SELECT uid FROM messages WHERE folder=?1")?
            .query_map([folder], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut removed = Vec::new();
        for uid in existing {
            if !remote.contains(&uid) {
                removed.push(uid);
                tx.execute(
                    "DELETE FROM messages WHERE folder=?1 AND uid=?2",
                    params![folder, uid],
                )?;
                tx.execute(
                    "DELETE FROM search WHERE folder=?1 AND uid=?2",
                    params![folder, uid],
                )?;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    pub fn search(&self, folder: &str, query: &str) -> Result<Vec<Message>> {
        // Treat user input as literal terms, not arbitrary FTS syntax.
        let terms = query
            .split_whitespace()
            .map(|s| format!("\"{}\"", s.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        if terms.is_empty() {
            return self.messages(folder);
        }
        let mut statement = self.db.prepare("SELECT m.payload FROM search s JOIN messages m ON m.folder=s.folder AND m.uid=s.uid WHERE search MATCH ?1 AND s.folder=?2 ORDER BY rank")?;
        let rows = statement.query_map(params![terms, folder], |r| r.get::<_, String>(0))?;
        rows.map(|row| Ok(serde_json::from_str(&row?)?)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flag_updates_preserve_content_and_search_without_index_writes() {
        let temp = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(&temp.path().join("mail.db")).unwrap();
        let message = Message {
            uid: 1,
            folder: "INBOX".into(),
            uid_validity: 7,
            body: Some("searchable body".into()),
            html_body: Some("<p>body</p>".into()),
            html_loaded: true,
            attachments: vec![crate::model::Attachment {
                name: "file".into(),
                data: vec![1, 2, 3],
                ..Default::default()
            }],
            ..Default::default()
        };
        cache.put_batch(&[message]).unwrap();
        // Any attempt to rebuild the index makes this test fail.
        cache.db.execute_batch("CREATE TRIGGER reject_index_delete BEFORE DELETE ON search_content BEGIN SELECT RAISE(ABORT, 'index changed'); END; CREATE TRIGGER reject_index_insert BEFORE INSERT ON search_content BEGIN SELECT RAISE(ABORT, 'index changed'); END;").unwrap();
        let flags = [MessageFlags {
            uid: 1,
            seen: true,
            starred: true,
        }];
        assert_eq!(cache.update_flags("INBOX", 7, &flags).unwrap(), flags);
        let loaded = cache.message("INBOX", 1).unwrap().unwrap();
        assert!(loaded.seen && loaded.starred && loaded.html_loaded);
        assert_eq!(loaded.body.as_deref(), Some("searchable body"));
        assert_eq!(loaded.html_body.as_deref(), Some("<p>body</p>"));
        assert_eq!(loaded.attachments[0].data, vec![1, 2, 3]);
        assert_eq!(cache.search("INBOX", "searchable").unwrap().len(), 1);
        let writes = cache.total_changes();
        assert!(cache.update_flags("INBOX", 7, &flags).unwrap().is_empty());
        assert!(
            cache
                .update_flags(
                    "INBOX",
                    8,
                    &[MessageFlags {
                        uid: 1,
                        seen: false,
                        starred: false
                    }]
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(cache.total_changes(), writes);
    }

    #[test]
    fn cache_search_reconcile_and_uidvalidity() {
        let temp = tempfile::tempdir().unwrap();
        let mut cache = Cache::open(&temp.path().join("mail.db")).unwrap();
        cache
            .set_folders(&[Folder {
                name: "INBOX".into(),
                ..Default::default()
            }])
            .unwrap();
        cache.check_validity("INBOX", 1).unwrap();
        let mut message = Message {
            uid: 42,
            folder: "INBOX".into(),
            uid_validity: 1,
            subject: "Rust newsletter".into(),
            ..Default::default()
        };
        cache.put(&message).unwrap();
        assert_eq!(cache.search("INBOX", "rust").unwrap().len(), 1);
        assert!(cache.search("INBOX", "hello").unwrap().is_empty());
        message.body = Some("hello from cache".into());
        message.html_body = Some("<p>hello from cache</p>".into());
        message.html_loaded = true;
        cache.put(&message).unwrap();
        let cached = cache.message("INBOX", 42).unwrap().unwrap();
        assert_eq!(cached.html_body, message.html_body);
        assert!(cached.html_loaded);
        assert_eq!(cache.search("INBOX", "hello").unwrap().len(), 1);
        cache.check_validity("INBOX", 1).unwrap();
        assert!(cache.message("INBOX", 42).unwrap().unwrap().body.is_some());
        cache
            .reconcile("INBOX", &std::collections::HashSet::new())
            .unwrap();
        assert!(cache.messages("INBOX").unwrap().is_empty());
        cache.put(&message).unwrap();
        cache.check_validity("INBOX", 2).unwrap();
        assert!(cache.search("INBOX", "rust").unwrap().is_empty());
    }
}
