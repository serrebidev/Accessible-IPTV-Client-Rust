//! `SQLite` EPG store (port of the Python client's guide
//! database): the loaded XMLTV guide is mirrored into a database
//! beside the config so now/next, per-channel schedules, and
//! title search survive restarts and answer from SQL.

use std::path::Path;

use rusqlite::Connection;

use crate::epg::{EpgGuide, Programme};

/// A guide database at one path.
pub struct EpgStore {
    conn: Connection,
}

impl EpgStore {
    /// Open (creating if needed) the store and ensure the schema.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when the database cannot be
    /// opened or the schema cannot be created.
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS channels (
                 id TEXT PRIMARY KEY,
                 display_name TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS programmes (
                 channel TEXT NOT NULL,
                 start INTEGER NOT NULL,
                 stop INTEGER NOT NULL,
                 title TEXT NOT NULL,
                 description TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_programmes_channel_start
                 ON programmes(channel, start);
             CREATE INDEX IF NOT EXISTS idx_programmes_title
                 ON programmes(title);",
        )
        .map_err(|e| e.to_string())?;
        Ok(Self { conn })
    }

    /// Replace the stored guide with the given one, in a single
    /// transaction. Returns the imported programme count.
    ///
    /// # Errors
    /// Returns the `SQLite` error text on any failed write (the
    /// transaction rolls back, keeping the previous guide).
    pub fn import_guide(&mut self, guide: &EpgGuide) -> Result<usize, String> {
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM channels", [])
            .map_err(|e| e.to_string())?;
        tx.execute("DELETE FROM programmes", [])
            .map_err(|e| e.to_string())?;
        let mut count = 0_usize;
        {
            let mut insert_channel = tx
                .prepare("INSERT OR REPLACE INTO channels (id, display_name) VALUES (?1, ?2)")
                .map_err(|e| e.to_string())?;
            for channel in &guide.channels {
                insert_channel
                    .execute([&channel.id, &channel.display_name])
                    .map_err(|e| e.to_string())?;
            }
        }
        {
            let mut insert_programme = tx
                .prepare(
                    "INSERT INTO programmes (channel, start, stop, title, description)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|e| e.to_string())?;
            for programmes in guide.programmes.values() {
                for programme in programmes {
                    insert_programme
                        .execute(rusqlite::params![
                            programme.channel_id,
                            programme.start,
                            programme.stop,
                            programme.title,
                            programme.description
                        ])
                        .map_err(|e| e.to_string())?;
                    count += 1;
                }
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(count)
    }

    /// Stored channel and programme counts.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when a count query fails.
    pub fn counts(&self) -> Result<(usize, usize), String> {
        let channels: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM channels", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        let programmes: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM programmes", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        Ok((
            usize::try_from(channels).unwrap_or(0),
            usize::try_from(programmes).unwrap_or(0),
        ))
    }

    /// The (current, next) programmes for a channel at a time.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when a query fails.
    pub fn now_next(
        &self,
        channel_id: &str,
        at: i64,
    ) -> Result<(Option<Programme>, Option<Programme>), String> {
        let current = self.query_one(
            "SELECT channel, start, stop, title, description FROM programmes
             WHERE channel = ?1 AND start <= ?2 AND stop > ?2
             ORDER BY start DESC LIMIT 1",
            channel_id,
            at,
        )?;
        let next = self.query_one(
            "SELECT channel, start, stop, title, description FROM programmes
             WHERE channel = ?1 AND start > ?2
             ORDER BY start ASC LIMIT 1",
            channel_id,
            at,
        )?;
        Ok((current, next))
    }

    /// Programmes for a channel in [from, to), by start.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when the query fails.
    pub fn programmes_between(
        &self,
        channel_id: &str,
        from: i64,
        to: i64,
    ) -> Result<Vec<Programme>, String> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT channel, start, stop, title, description FROM programmes
                 WHERE channel = ?1 AND stop > ?2 AND start < ?3
                 ORDER BY start ASC",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(rusqlite::params![channel_id, from, to], row_programme)
            .map_err(|e| e.to_string())?;
        collect_rows(rows)
    }

    /// Title search across all channels: (channel display name,
    /// programme) pairs by start, capped at `limit`.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when the query fails.
    pub fn search_titles(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<(String, Programme)>, String> {
        let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));
        let mut statement = self
            .conn
            .prepare(
                "SELECT c.display_name, p.channel, p.start, p.stop, p.title, p.description
                 FROM programmes p JOIN channels c ON c.id = p.channel
                 WHERE p.title LIKE ?1 ESCAPE '\\'
                 ORDER BY p.start ASC LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(
                rusqlite::params![pattern, i64::try_from(limit).unwrap_or(500)],
                |row| {
                    let programme = Programme {
                        channel_id: row.get(1)?,
                        start: row.get(2)?,
                        stop: row.get(3)?,
                        title: row.get(4)?,
                        description: row.get(5)?,
                    };
                    Ok((row.get::<_, String>(0)?, programme))
                },
            )
            .map_err(|e| e.to_string())?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| e.to_string())?);
        }
        Ok(results)
    }

    fn query_one(&self, sql: &str, channel_id: &str, at: i64) -> Result<Option<Programme>, String> {
        let mut statement = self.conn.prepare(sql).map_err(|e| e.to_string())?;
        let mut rows = statement
            .query_map(rusqlite::params![channel_id, at], row_programme)
            .map_err(|e| e.to_string())?;
        match rows.next() {
            Some(row) => Ok(Some(row.map_err(|e| e.to_string())?)),
            None => Ok(None),
        }
    }
}

fn row_programme(row: &rusqlite::Row<'_>) -> rusqlite::Result<Programme> {
    Ok(Programme {
        channel_id: row.get(0)?,
        start: row.get(1)?,
        stop: row.get(2)?,
        title: row.get(3)?,
        description: row.get(4)?,
    })
}

fn collect_rows(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<Programme>>,
) -> Result<Vec<Programme>, String> {
    let mut programmes = Vec::new();
    for row in rows {
        programmes.push(row.map_err(|e| e.to_string())?);
    }
    Ok(programmes)
}

#[cfg(test)]
mod tests {
    use super::EpgStore;
    use crate::epg::{EpgChannel, EpgGuide, Programme};
    use std::collections::HashMap;

    fn sample_guide() -> EpgGuide {
        let programmes = vec![
            Programme {
                channel_id: "c1".into(),
                start: 100,
                stop: 200,
                title: "Morning Show".into(),
                description: "Wake up.".into(),
            },
            Programme {
                channel_id: "c1".into(),
                start: 200,
                stop: 300,
                title: "Midday News".into(),
                description: String::new(),
            },
        ];
        EpgGuide {
            channels: vec![EpgChannel {
                id: "c1".into(),
                display_name: "Channel One".into(),
            }],
            programmes: HashMap::from([("c1".to_string(), programmes)]),
        }
    }

    #[test]
    fn import_and_query_round_trip() {
        let mut store = EpgStore::open(std::path::Path::new(":memory:")).unwrap();
        assert_eq!(store.import_guide(&sample_guide()).unwrap(), 2);
        assert_eq!(store.counts().unwrap(), (1, 2));
        let (current, next) = store.now_next("c1", 150).unwrap();
        assert_eq!(current.unwrap().title, "Morning Show");
        assert_eq!(next.unwrap().title, "Midday News");
        let window = store.programmes_between("c1", 150, 250).unwrap();
        assert_eq!(window.len(), 2);
        let hits = store.search_titles("news", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0, "Channel One");
        // Re-import replaces rather than duplicating.
        assert_eq!(store.import_guide(&sample_guide()).unwrap(), 2);
        assert_eq!(store.counts().unwrap(), (1, 2));
    }
}
