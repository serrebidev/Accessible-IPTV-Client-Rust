//! `SQLite` EPG store (port of the Python client's guide
//! database): the loaded XMLTV guide is mirrored into a database
//! beside the config so now/next, per-channel schedules, and
//! programme search survive restarts and answer from SQL.
//!
//! Imports are per source, like Python's: each source's channels
//! are tracked under the source's key, a re-import replaces only
//! that source's contribution, and a source that fails or is left
//! out of a run keeps its previously stored guide (audit E04).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _};

use crate::epg::{canonicalize_name, strip_noise_words, EpgChannel, EpgGuide, NowNext, Programme};

/// A guide database at one path.
pub struct EpgStore {
    conn: Connection,
}

/// The normalized channel name Python stores beside each guide
/// channel (`canonicalize_name(strip_noise_words(name))`): the
/// main-window EPG search matches channel names against it.
fn norm_name(display_name: &str) -> String {
    canonicalize_name(&strip_noise_words(display_name))
}

impl EpgStore {
    /// Open (creating if needed) the store and ensure the schema,
    /// migrating databases written before per-source tracking.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when the database cannot be
    /// opened or the schema cannot be created.
    pub fn open(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        let _ = conn.busy_timeout(Duration::from_secs(2));
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
                 ON programmes(title);
             CREATE TABLE IF NOT EXISTS channel_sources (
                 channel_id TEXT NOT NULL,
                 source_key TEXT NOT NULL,
                 PRIMARY KEY (channel_id, source_key)
             );",
        )
        .map_err(|e| e.to_string())?;
        let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    /// Add the columns per-source imports need to databases from
    /// older builds, backfill normalized names once, and make
    /// programme slots unique so the upsert can conflict on them.
    fn migrate(&mut self) -> Result<(), String> {
        let columns: Vec<String> = {
            let mut statement = self
                .conn
                .prepare("PRAGMA table_info(channels)")
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| e.to_string())?;
            let mut names = Vec::new();
            for row in rows {
                names.push(row.map_err(|e| e.to_string())?);
            }
            names
        };
        let mut added_norm = false;
        if !columns.iter().any(|c| c == "norm_name") {
            self.conn
                .execute(
                    "ALTER TABLE channels ADD COLUMN norm_name TEXT NOT NULL DEFAULT ''",
                    [],
                )
                .map_err(|e| e.to_string())?;
            added_norm = true;
        }
        if !columns.iter().any(|c| c == "last_seen") {
            self.conn
                .execute("ALTER TABLE channels ADD COLUMN last_seen INTEGER", [])
                .map_err(|e| e.to_string())?;
        }
        if added_norm {
            let rows: Vec<(String, String)> = {
                let mut statement = self
                    .conn
                    .prepare("SELECT id, display_name FROM channels")
                    .map_err(|e| e.to_string())?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })
                    .map_err(|e| e.to_string())?;
                let mut out = Vec::new();
                for row in rows {
                    out.push(row.map_err(|e| e.to_string())?);
                }
                out
            };
            let tx = self.conn.transaction().map_err(|e| e.to_string())?;
            {
                let mut update = tx
                    .prepare("UPDATE channels SET norm_name = ?2 WHERE id = ?1")
                    .map_err(|e| e.to_string())?;
                for (id, display) in &rows {
                    update
                        .execute([id, &norm_name(display)])
                        .map_err(|e| e.to_string())?;
                }
            }
            tx.commit().map_err(|e| e.to_string())?;
        }
        // Python's upsert keys programmes by (channel, start, stop);
        // older databases can hold duplicate slots, so keep the
        // newest row per slot before enforcing uniqueness.
        self.conn
            .execute(
                "DELETE FROM programmes WHERE rowid NOT IN (
                     SELECT MAX(rowid) FROM programmes GROUP BY channel, start, stop
                 )",
                [],
            )
            .map_err(|e| e.to_string())?;
        self.conn
            .execute_batch(
                "CREATE UNIQUE INDEX IF NOT EXISTS idx_programmes_slot
                     ON programmes(channel, start, stop);",
            )
            .map_err(|e| e.to_string())?;
        Ok(())
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
        tx.execute("DELETE FROM channel_sources", [])
            .map_err(|e| e.to_string())?;
        let count = insert_guide_rows(&tx, guide, None, 0)?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(count)
    }

    /// Import one source's guide under its key (Python's
    /// per-source import): this source's channels and programmes
    /// are upserted, channels it no longer lists are forgotten
    /// (deleted only when no other source lists them), programmes
    /// over 14 days old are pruned, and channels no import has
    /// listed for 14 days are retired. Other sources' rows are
    /// never touched. Returns the imported programme count.
    ///
    /// # Errors
    /// Returns the `SQLite` error text on any failed write (the
    /// transaction rolls back, keeping the previous guide).
    pub fn import_source(
        &mut self,
        source_key: &str,
        guide: &EpgGuide,
        now: i64,
    ) -> Result<usize, String> {
        let keep: Vec<String> = guide.channels.iter().map(|c| c.id.clone()).collect();
        let listed: Vec<String> = {
            let mut statement = self
                .conn
                .prepare("SELECT channel_id FROM channel_sources WHERE source_key = ?1")
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([source_key], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| e.to_string())?);
            }
            out
        };
        let tx = self.conn.transaction().map_err(|e| e.to_string())?;
        let count = insert_guide_rows(&tx, guide, Some(source_key), now)?;
        // Forget this source's listing of channels it no longer
        // defines; delete a channel only when no source lists it
        // any more (Python's `_drop_channels_missing_from`).
        for id in listed.iter().filter(|id| !keep.contains(id)) {
            tx.execute(
                "DELETE FROM channel_sources WHERE channel_id = ?1 AND source_key = ?2",
                [id, source_key],
            )
            .map_err(|e| e.to_string())?;
            let remaining: i64 = tx
                .query_row(
                    "SELECT COUNT(*) FROM channel_sources WHERE channel_id = ?1",
                    [id],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            if remaining == 0 {
                delete_channel(&tx, id)?;
            }
        }
        // Retire channels no import has listed for 14 days, and
        // prune programmes that ended over 14 days ago (Python's
        // post-import maintenance).
        let cutoff = now - 14 * 86_400;
        let stale: Vec<String> = {
            let mut statement = tx
                .prepare("SELECT id FROM channels WHERE last_seen IS NOT NULL AND last_seen < ?1")
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map([cutoff], |row| row.get::<_, String>(0))
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| e.to_string())?);
            }
            out
        };
        for id in &stale {
            delete_channel(&tx, id)?;
        }
        tx.execute("DELETE FROM programmes WHERE stop < ?1", [cutoff])
            .map_err(|e| e.to_string())?;
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

    /// Whether the store holds any programme at all (Python's
    /// `epg_database_has_programmes`).
    ///
    /// # Errors
    /// Returns the `SQLite` error text when the query fails.
    pub fn has_programmes(&self) -> Result<bool, String> {
        let found: Option<i64> = self
            .conn
            .query_row("SELECT 1 FROM programmes LIMIT 1", [], |row| row.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(found.is_some())
    }

    /// Whether the store holds a programme that has not ended,
    /// joined to a known channel (Python's
    /// `epg_database_has_usable_data`): the guide-freshness input
    /// to the automatic-import decision.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when the query fails.
    pub fn has_usable_data(&self, now: i64) -> Result<bool, String> {
        let found: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM programmes p JOIN channels c ON c.id = p.channel
                 WHERE p.stop > ?1 LIMIT 1",
                [now],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(found.is_some())
    }

    /// Every stored guide channel, in first-import order.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when the query fails.
    pub fn channels(&self) -> Result<Vec<EpgChannel>, String> {
        let mut statement = self
            .conn
            .prepare("SELECT id, display_name FROM channels ORDER BY rowid")
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok(EpgChannel {
                    id: row.get(0)?,
                    display_name: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(|e| e.to_string())?);
        }
        Ok(out)
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

    /// Now/next for every stored channel in one range scan (the
    /// store twin of `EpgGuide::bulk_now_next`, for guides that
    /// live only in the database after a restart): rows arrive
    /// ordered by channel and start, so each channel's current
    /// programme is its last row started by `at` and its next is
    /// the first row starting after `at`.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when the query fails.
    pub fn all_now_next(&self, at: i64) -> Result<Vec<(EpgChannel, NowNext)>, String> {
        let horizon = at + 31 * 86_400;
        let mut statement = self
            .conn
            .prepare(
                "SELECT p.channel, p.start, p.stop, p.title, p.description, c.display_name
                 FROM programmes p JOIN channels c ON c.id = p.channel
                 WHERE p.stop > ?1 AND p.start < ?2
                 ORDER BY p.channel, p.start",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(rusqlite::params![at, horizon], |row| {
                let programme = Programme {
                    channel_id: row.get(0)?,
                    start: row.get(1)?,
                    stop: row.get(2)?,
                    title: row.get(3)?,
                    description: row.get(4)?,
                };
                Ok((programme, row.get::<_, String>(5)?))
            })
            .map_err(|e| e.to_string())?;
        let mut out: Vec<(EpgChannel, NowNext)> = Vec::new();
        let mut group: Option<(EpgChannel, NowNext)> = None;
        for row in rows {
            let (programme, display_name) = row.map_err(|e| e.to_string())?;
            if group.as_ref().map(|(c, _)| c.id.as_str()) != Some(programme.channel_id.as_str()) {
                if let Some(done) = group.take() {
                    out.push(done);
                }
                group = Some((
                    EpgChannel {
                        id: programme.channel_id.clone(),
                        display_name,
                    },
                    NowNext::default(),
                ));
            }
            if let Some((_, entry)) = &mut group {
                if programme.start <= at {
                    entry.current = Some(programme);
                } else if entry.next.is_none() {
                    entry.next = Some(programme);
                }
            }
        }
        if let Some(done) = group.take() {
            out.push(done);
        }
        Ok(out)
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

    /// Main-window EPG search (port of `get_channels_with_show`
    /// with `include_title_search=False`): programmes on channels
    /// whose normalized name matches the query — never a broad
    /// title scan. On-air rows come first, then future rows,
    /// deduplicated per channel and title, capped at `limit`.
    /// Returns (channel display name, programme) pairs.
    ///
    /// # Errors
    /// Returns the `SQLite` error text when a query fails.
    pub fn channels_with_show(
        &self,
        query: &str,
        now: i64,
        limit: usize,
    ) -> Result<Vec<(String, Programme)>, String> {
        let normalized = norm_name(query);
        if normalized.is_empty() {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit.max(1)).unwrap_or(100);
        let pattern = format!(
            "%{}%",
            normalized
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let channel_ids: Vec<String> = {
            let mut statement = self
                .conn
                .prepare("SELECT id FROM channels WHERE norm_name LIKE ?1 ESCAPE '\\' LIMIT ?2")
                .map_err(|e| e.to_string())?;
            let rows = statement
                .query_map(rusqlite::params![pattern, limit], |row| {
                    row.get::<_, String>(0)
                })
                .map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row.map_err(|e| e.to_string())?);
            }
            out
        };
        if channel_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = vec!["?"; channel_ids.len()].join(",");
        let sql = format!(
            "SELECT c.display_name, p.channel, p.start, p.stop, p.title, p.description
             FROM programmes p JOIN channels c ON c.id = p.channel
             WHERE p.channel IN ({placeholders}) AND p.stop >= ?{}
             ORDER BY p.start ASC LIMIT ?{}",
            channel_ids.len() + 1,
            channel_ids.len() + 2
        );
        let mut statement = self.conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut params: Vec<rusqlite::types::Value> =
            channel_ids.iter().cloned().map(Into::into).collect();
        params.push(now.into());
        params.push(limit.into());
        let rows = statement
            .query_map(rusqlite::params_from_iter(params), |row| {
                let programme = Programme {
                    channel_id: row.get(1)?,
                    start: row.get(2)?,
                    stop: row.get(3)?,
                    title: row.get(4)?,
                    description: row.get(5)?,
                };
                Ok((row.get::<_, String>(0)?, programme))
            })
            .map_err(|e| e.to_string())?;
        let mut found: Vec<(String, Programme)> = Vec::new();
        let mut seen_slots: std::collections::HashSet<(String, i64, i64, String)> =
            std::collections::HashSet::new();
        for row in rows {
            let (name, programme) = row.map_err(|e| e.to_string())?;
            let slot = (
                programme.channel_id.clone(),
                programme.start,
                programme.stop,
                programme.title.clone(),
            );
            if seen_slots.insert(slot) {
                found.push((name, programme));
            }
        }
        // On-air first, then future; one row per (channel, title).
        let mut ordered: Vec<(String, Programme)> = Vec::new();
        let mut seen_titles: std::collections::HashSet<(String, String)> =
            std::collections::HashSet::new();
        for want_future in [false, true] {
            for (name, programme) in &found {
                let is_future = programme.start > now;
                if is_future != want_future {
                    continue;
                }
                if !(want_future || (programme.start <= now && now < programme.stop)) {
                    continue; // already over: neither on-air nor future
                }
                if seen_titles.insert((programme.channel_id.clone(), programme.title.clone())) {
                    ordered.push((name.clone(), programme.clone()));
                }
            }
        }
        ordered.truncate(usize::try_from(limit).unwrap_or(100));
        Ok(ordered)
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

/// Insert one guide's channels and programmes inside `tx`, with
/// Python's upserts: a channel whose normalized name changed was
/// renumbered (its stored programmes go), programmes delete the
/// stored rows they overlap, and an exact slot keeps the newer
/// title and the longer description. When `source_key` is given,
/// the channels are recorded under it and stamped `last_seen`.
fn insert_guide_rows(
    tx: &rusqlite::Transaction<'_>,
    guide: &EpgGuide,
    source_key: Option<&str>,
    now: i64,
) -> Result<usize, String> {
    {
        let mut read_norm = tx
            .prepare("SELECT norm_name FROM channels WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        let mut insert_channel = tx
            .prepare(
                "INSERT OR REPLACE INTO channels (id, display_name, norm_name, last_seen)
                 VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(|e| e.to_string())?;
        let mut insert_source = tx
            .prepare(
                "INSERT OR IGNORE INTO channel_sources (channel_id, source_key)
                 VALUES (?1, ?2)",
            )
            .map_err(|e| e.to_string())?;
        for channel in &guide.channels {
            let new_norm = norm_name(&channel.display_name);
            let stored: Option<String> = read_norm
                .query_row([&channel.id], |row| row.get::<_, String>(0))
                .optional()
                .map_err(|e| e.to_string())?;
            if let Some(stored) = stored {
                if !stored.is_empty() && !new_norm.is_empty() && stored != new_norm {
                    tx.execute("DELETE FROM programmes WHERE channel = ?1", [&channel.id])
                        .map_err(|e| e.to_string())?;
                }
            }
            let seen: Option<i64> = source_key.map(|_| now);
            insert_channel
                .execute(rusqlite::params![
                    channel.id,
                    channel.display_name,
                    new_norm,
                    seen
                ])
                .map_err(|e| e.to_string())?;
            if let Some(key) = source_key {
                insert_source
                    .execute([&channel.id, key])
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    let mut count = 0_usize;
    {
        let mut delete_overlap = tx
            .prepare(
                "DELETE FROM programmes
                 WHERE channel = ?1 AND start < ?3 AND stop > ?2
                   AND NOT (start = ?2 AND stop = ?3)",
            )
            .map_err(|e| e.to_string())?;
        let mut upsert = tx
            .prepare(
                "INSERT INTO programmes (channel, start, stop, title, description)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(channel, start, stop) DO UPDATE SET
                   title = excluded.title,
                   description = CASE
                     WHEN programmes.title != excluded.title THEN excluded.description
                     WHEN length(excluded.description) > length(coalesce(programmes.description, ''))
                       THEN excluded.description
                     ELSE programmes.description END
                 WHERE programmes.title != excluded.title
                    OR length(excluded.description) > length(coalesce(programmes.description, ''))",
            )
            .map_err(|e| e.to_string())?;
        for programmes in guide.programmes.values() {
            for programme in programmes {
                delete_overlap
                    .execute(rusqlite::params![
                        programme.channel_id,
                        programme.start,
                        programme.stop
                    ])
                    .map_err(|e| e.to_string())?;
                upsert
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
    Ok(count)
}

/// Delete one channel with its programmes and source listings.
fn delete_channel(tx: &rusqlite::Transaction<'_>, id: &str) -> Result<(), String> {
    tx.execute("DELETE FROM programmes WHERE channel = ?1", [id])
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM channel_sources WHERE channel_id = ?1", [id])
        .map_err(|e| e.to_string())?;
    tx.execute("DELETE FROM channels WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
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

    fn guide_with(channel: &str, name: &str, programmes: Vec<Programme>) -> EpgGuide {
        EpgGuide {
            channels: vec![EpgChannel {
                id: channel.into(),
                display_name: name.into(),
            }],
            programmes: HashMap::from([(channel.to_string(), programmes)]),
        }
    }

    fn prog(channel: &str, start: i64, stop: i64, title: &str, description: &str) -> Programme {
        Programme {
            channel_id: channel.into(),
            start,
            stop,
            title: title.into(),
            description: description.into(),
        }
    }

    #[test]
    fn partial_imports_preserve_other_sources() {
        // Audit E04: import A and B, re-import only A, then let B
        // fail (no run at all): B's stored guide must survive.
        let mut store = EpgStore::open(std::path::Path::new(":memory:")).unwrap();
        let a = guide_with("a1", "Alpha", vec![prog("a1", 100, 200, "A Show", "")]);
        let b = guide_with("b1", "Beta", vec![prog("b1", 100, 200, "B Show", "")]);
        store.import_source("key-a", &a, 1_000).unwrap();
        store.import_source("key-b", &b, 1_000).unwrap();
        assert_eq!(store.counts().unwrap(), (2, 2));
        // Re-import A with a corrected slot; B is untouched.
        let a2 = guide_with("a1", "Alpha", vec![prog("a1", 150, 250, "A Show 2", "")]);
        store.import_source("key-a", &a2, 2_000).unwrap();
        let (current, _) = store.now_next("b1", 150).unwrap();
        assert_eq!(current.unwrap().title, "B Show");
        let (current, _) = store.now_next("a1", 160).unwrap();
        assert_eq!(current.unwrap().title, "A Show 2");
        // A drops its channel entirely: only A's rows go.
        let a3 = guide_with("a2", "Alpha Two", vec![prog("a2", 100, 200, "New", "")]);
        store.import_source("key-a", &a3, 3_000).unwrap();
        assert_eq!(store.now_next("a1", 150).unwrap(), (None, None));
        let (current, _) = store.now_next("b1", 150).unwrap();
        assert_eq!(current.unwrap().title, "B Show");
    }

    #[test]
    fn overlapping_slots_replace_and_descriptions_enrich() {
        // Audit E07: a corrected overlapping slot replaces the old
        // row; an exact slot keeps the longer description.
        let mut store = EpgStore::open(std::path::Path::new(":memory:")).unwrap();
        let first = guide_with(
            "c1",
            "One",
            vec![
                prog("c1", 100, 200, "Show", "Short."),
                prog("c1", 200, 300, "Next", ""),
            ],
        );
        store.import_source("k", &first, 1_000).unwrap();
        let second = guide_with(
            "c1",
            "One",
            vec![
                prog("c1", 100, 200, "Show", "A much longer synopsis."),
                prog("c1", 150, 250, "Corrected", ""),
            ],
        );
        store.import_source("k", &second, 2_000).unwrap();
        let rows = store.programmes_between("c1", 0, 400).unwrap();
        let titles: Vec<&str> = rows.iter().map(|p| p.title.as_str()).collect();
        assert_eq!(titles, ["Corrected"], "{rows:?}");
        let third = guide_with(
            "c1",
            "One",
            vec![prog("c1", 150, 250, "Corrected", "Full synopsis here.")],
        );
        store.import_source("k", &third, 3_000).unwrap();
        let rows = store.programmes_between("c1", 0, 400).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].description, "Full synopsis here.");
    }

    #[test]
    fn usable_data_and_channels_with_show() {
        // Audits E02/E03/E08: freshness checks and the bounded
        // channel-name programme search over the stored guide.
        let mut store = EpgStore::open(std::path::Path::new(":memory:")).unwrap();
        assert!(!store.has_usable_data(1_000).unwrap());
        assert!(!store.has_programmes().unwrap());
        let guide = guide_with(
            "n1",
            "News Channel HD",
            vec![
                prog("n1", 100, 200, "Old News", ""),
                prog("n1", 900, 1_100, "News at Nine", ""),
                prog("n1", 1_100, 1_200, "News at Nine", ""),
                prog("n1", 1_200, 1_300, "Later Show", ""),
            ],
        );
        store.import_source("k", &guide, 500).unwrap();
        assert!(store.has_programmes().unwrap());
        assert!(store.has_usable_data(1_000).unwrap());
        assert!(!store.has_usable_data(99_999).unwrap());
        // "news" matches the channel name; on-air first, the repeat
        // collapses to one row per (channel, title).
        let hits = store.channels_with_show("news", 1_000, 100).unwrap();
        let titles: Vec<&str> = hits.iter().map(|(_, p)| p.title.as_str()).collect();
        assert_eq!(titles, ["News at Nine", "Later Show"], "{hits:?}");
        assert_eq!(hits[0].0, "News Channel HD");
        // A title-only query finds nothing on this route.
        assert!(store
            .channels_with_show("later", 1_000, 100)
            .unwrap()
            .is_empty());
        // Bulk now/next from the store alone: next is the first
        // programme starting after now (Python's get_all_now_next),
        // here the same-title continuation.
        let bulk = store.all_now_next(1_000).unwrap();
        assert_eq!(bulk.len(), 1);
        assert_eq!(bulk[0].1.current.as_ref().unwrap().title, "News at Nine");
        assert_eq!(bulk[0].1.next.as_ref().unwrap().title, "News at Nine");
        assert_eq!(bulk[0].1.next.as_ref().unwrap().start, 1_100);
        let bulk = store.all_now_next(1_150).unwrap();
        assert_eq!(bulk[0].1.current.as_ref().unwrap().start, 1_100);
        assert_eq!(bulk[0].1.next.as_ref().unwrap().title, "Later Show");
    }
}
