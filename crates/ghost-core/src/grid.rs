//! Grid Planner: upstream only ever sees grid-aligned `GetBlockRange` requests.
//!
//! The grid rule is Nym's network-wide one from `nym-swizzle-zcash` (`grid::quantize`): spacing
//! `max(1152, 144·2^j)`, start rounded down, end rounded up and capped at the tip, emitted on the
//! wire as ascending one-day (1152-block) cells. Every wallet resuming in the same cell sends
//! identical requests. We never invent our own boundaries.
//!
//! The wallet still gets exactly the blocks it asked for: cells are cached (sqlite) and the
//! requested window is served from them. A cell is reused from cache only when it is complete
//! and at least [`REORG_SAFETY`] blocks below the tip; otherwise the whole cell is re-fetched
//! (never a partial, unaligned request), which also refreshes it after a reorg.

use std::path::Path;
use std::sync::Mutex;

use nym_swizzle_zcash::grid::{self, QueuedRange};
use rusqlite::{Connection, params};

pub use nym_swizzle_zcash::grid::{S_FLOOR, SHARD};

/// Cells this close to the tip are never served from cache (reorg margin).
pub const REORG_SAFETY: u64 = 100;

/// Upstream requests for a wallet request, as inclusive `[start, end]` heights, ascending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub cells: Vec<(u64, u64)>,
    /// The emitted (quantized) range, inclusive.
    pub emitted: (u64, u64),
    pub spacing: u64,
}

/// Plans the grid-aligned upstream requests covering the wallet's inclusive `[lo, hi]`.
/// `hi` must be `<= tip`.
pub fn plan(lo: u64, hi: u64, tip: u64) -> Plan {
    assert!(lo <= hi && hi <= tip, "bad range {lo}..={hi} (tip {tip})");
    let q = grid::quantize(QueuedRange::scan(lo, hi + 1), tip);
    let (es, ee) = q.emitted();
    Plan {
        cells: q.requests().map(|(s, e)| (s, e - 1)).collect(),
        emitted: (es, ee - 1),
        spacing: q.spacing(),
    }
}

/// True if an inclusive range sits on the grid (what the upstream log is checked against).
pub fn is_aligned(start: u64, end: u64, tip: u64) -> bool {
    start % S_FLOOR == 0 && (end + 1 == start + S_FLOOR || end == tip)
}

pub struct BlockCache {
    db: Mutex<Connection>,
}

impl BlockCache {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS blocks (
               kind TEXT NOT NULL, height INTEGER NOT NULL, data BLOB NOT NULL,
               PRIMARY KEY (kind, height));",
        )?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// A cell is served from cache only if complete and safely below the tip.
    pub fn has_cell(&self, kind: &str, (s, e): (u64, u64), tip: u64) -> anyhow::Result<bool> {
        if e + REORG_SAFETY > tip {
            return Ok(false);
        }
        let n: u64 = self.db.lock().unwrap().query_row(
            "SELECT COUNT(*) FROM blocks WHERE kind=?1 AND height BETWEEN ?2 AND ?3",
            params![kind, s as i64, e as i64],
            |r| r.get::<_, i64>(0),
        )? as u64;
        Ok(n == e - s + 1)
    }

    /// Replaces a whole cell's contents (drops stale blocks, e.g. after a reorg).
    pub fn put_cell(&self, kind: &str, (s, e): (u64, u64), blocks: &[(u64, Vec<u8>)]) -> anyhow::Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute("DELETE FROM blocks WHERE kind=?1 AND height BETWEEN ?2 AND ?3", params![kind, s as i64, e as i64])?;
        {
            let mut st = tx.prepare("INSERT INTO blocks (kind,height,data) VALUES (?1,?2,?3)")?;
            for (h, d) in blocks {
                st.execute(params![kind, *h as i64, d])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get(&self, kind: &str, lo: u64, hi: u64) -> anyhow::Result<Vec<(u64, Vec<u8>)>> {
        let db = self.db.lock().unwrap();
        let mut st = db.prepare("SELECT height,data FROM blocks WHERE kind=?1 AND height BETWEEN ?2 AND ?3 ORDER BY height")?;
        let rows = st.query_map(params![kind, lo as i64, hi as i64], |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_tip_sync_is_one_aligned_cell() {
        // Wallet 120 blocks behind: one day-cell from the floor, capped at the tip.
        let tip = 4_581_920;
        let p = plan(4_581_800, tip, tip);
        let floor = 4_581_800 - 4_581_800 % S_FLOOR;
        assert_eq!(p.cells, vec![(floor, tip)]);
        assert!(p.cells.iter().all(|&(s, e)| is_aligned(s, e, tip)));
    }

    #[test]
    fn same_cell_wallets_collide() {
        // Two wallets with different resume points in the same cell emit identical requests.
        let tip = 4_581_920;
        // both resume points lie in the cell starting at 3977 * 1152 = 4_581_504
        assert_eq!(plan(4_581_600, tip, tip).cells, plan(4_581_800, tip, tip).cells);
        assert_ne!(plan(4_581_200, tip, tip).cells, plan(4_581_800, tip, tip).cells);
    }

    #[test]
    fn long_range_splits_into_full_cells() {
        let tip = 4_600_000;
        let p = plan(4_500_001, 4_510_000, tip);
        assert!(p.cells.iter().all(|&(s, e)| s % S_FLOOR == 0 && e - s + 1 == S_FLOOR));
        assert!(p.emitted.0 <= 4_500_001 && p.emitted.1 >= 4_510_000);
    }

    #[test]
    fn cache_respects_reorg_margin() {
        let dir = std::env::temp_dir().join(format!("ghost-grid-test-{}", std::process::id()));
        let c = BlockCache::open(&dir.join("c.sqlite")).unwrap();
        let cell = (0, 9);
        let blocks: Vec<_> = (0..=9).map(|h| (h, vec![h as u8])).collect();
        c.put_cell("k", cell, &blocks).unwrap();
        assert!(c.has_cell("k", cell, 1000).unwrap());
        assert!(!c.has_cell("k", cell, 50).unwrap()); // too close to tip
        assert!(!c.has_cell("other", cell, 1000).unwrap());
        assert_eq!(c.get("k", 3, 5).unwrap().len(), 3);
        let _ = std::fs::remove_dir_all(dir);
    }
}
