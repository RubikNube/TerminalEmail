use crate::{
    config::private_dir,
    model::{Folder, Message},
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
            tx.execute("INSERT INTO folders(name, unread, role) VALUES (?1, ?2, ?3) ON CONFLICT(name) DO UPDATE SET unread=excluded.unread, role=excluded.role", params![folder.name, folder.unread, folder.role])?;
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

    pub fn check_validity(&mut self, folder: &str, validity: u32) -> Result<()> {
        let previous: Option<u32> = self.db.query_row(
            "SELECT validity FROM folders WHERE name=?1",
            [folder],
            |r| r.get(0),
        )?;
        let tx = self.db.transaction()?;
        if previous != Some(validity) {
            tx.execute("DELETE FROM messages WHERE folder=?1", [folder])?;
            tx.execute("DELETE FROM search WHERE folder=?1", [folder])?;
        }
        tx.execute(
            "UPDATE folders SET validity=?2 WHERE name=?1",
            params![folder, validity],
        )?;
        tx.commit()?;
        Ok(())
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

    pub fn put(&mut self, message: &Message) -> Result<()> {
        let tx = self.db.transaction()?;
        tx.execute("INSERT INTO messages(folder, uid, payload) VALUES (?1, ?2, ?3) ON CONFLICT(folder, uid) DO UPDATE SET payload=excluded.payload", params![message.folder, message.uid, serde_json::to_string(message)?])?;
        tx.execute(
            "DELETE FROM search WHERE folder=?1 AND uid=?2",
            params![message.folder, message.uid],
        )?;
        tx.execute("INSERT INTO search(folder, uid, sender, recipients, subject, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![message.folder, message.uid, message.from, format!("{} {}", message.to, message.cc), message.subject, message.body.as_deref().unwrap_or_default()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn reconcile(
        &mut self,
        folder: &str,
        remote: &std::collections::HashSet<u32>,
    ) -> Result<()> {
        let tx = self.db.transaction()?;
        let existing: Vec<u32> = tx
            .prepare("SELECT uid FROM messages WHERE folder=?1")?
            .query_map([folder], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for uid in existing {
            if !remote.contains(&uid) {
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
        Ok(())
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
