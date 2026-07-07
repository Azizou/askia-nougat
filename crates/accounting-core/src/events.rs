use crate::hlc::Hlc;
use rusqlite::Connection;

/// The immutable event envelope.
#[derive(Debug, Clone)]
pub struct LedgerEvent {
    pub id: String,
    pub hlc: String,
    pub device_id: String,
    pub user_id: String,
    pub seq: i64,
    pub event_type: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
}

/// Append one event to the log.
pub fn append_event(
    conn: &Connection,
    hlc: &mut Hlc,
    physical_now: u64,
    device_id: &str,
    user_id: &str,
    event_type: &str,
    payload: &serde_json::Value,
) -> rusqlite::Result<LedgerEvent> {
    let next_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE device_id = ?1",
        [device_id],
        |r| r.get(0),
    )?;
    let stamp = hlc.tick(physical_now);
    let id = stamp.clone();
    let payload_str = payload.to_string();

    conn.execute(
        "INSERT INTO events (id, hlc, device_id, user_id, seq, type, payload, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, jsonb(?7), ?8)",
        rusqlite::params![
            id, stamp, device_id, user_id, next_seq, event_type,
            payload_str, physical_now as i64
        ],
    )?;

    Ok(LedgerEvent {
        id,
        hlc: stamp,
        device_id: device_id.to_string(),
        user_id: user_id.to_string(),
        seq: next_seq,
        event_type: event_type.to_string(),
        payload: payload.clone(),
        created_at: physical_now as i64,
    })
}

/// Read every event in deterministic replay order (by HLC ascending).
pub fn read_events(conn: &Connection) -> rusqlite::Result<Vec<LedgerEvent>> {
    let mut stmt = conn.prepare(
        "SELECT id, hlc, device_id, user_id, seq, type, json(payload), created_at
         FROM events ORDER BY hlc ASC",
    )?;
    let rows = stmt.query_map([], |r| {
        let payload_text: String = r.get(6)?;
        let payload = serde_json::from_str(&payload_text).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(6, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(LedgerEvent {
            id: r.get(0)?,
            hlc: r.get(1)?,
            device_id: r.get(2)?,
            user_id: r.get(3)?,
            seq: r.get(4)?,
            event_type: r.get(5)?,
            payload,
            created_at: r.get(7)?,
        })
    })?;
    rows.collect()
}

/// Return the list of missing `seq` values for a device (gap detection for sync).
pub fn missing_seqs(conn: &Connection, device_id: &str) -> rusqlite::Result<Vec<i64>> {
    let max_seq: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq), 0) FROM events WHERE device_id = ?1",
        [device_id],
        |r| r.get(0),
    )?;
    let mut present = std::collections::HashSet::new();
    let mut stmt = conn.prepare("SELECT seq FROM events WHERE device_id = ?1")?;
    let rows = stmt.query_map([device_id], |r| r.get::<_, i64>(0))?;
    for s in rows {
        present.insert(s?);
    }
    Ok((1..=max_seq).filter(|s| !present.contains(s)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use serde_json::json;

    #[test]
    fn append_assigns_incrementing_seq_per_device() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");

        let e1 = append_event(&conn, &mut hlc, 1000, "deviceA", "userX",
            "ItemDefined", &json!({"itemId": "i1"})).unwrap();
        let e2 = append_event(&conn, &mut hlc, 1000, "deviceA", "userX",
            "ItemDefined", &json!({"itemId": "i2"})).unwrap();

        assert_eq!(e1.seq, 1);
        assert_eq!(e2.seq, 2);
        assert_ne!(e1.id, e2.id);
        assert_eq!(e1.id, e1.hlc);
        assert!(e1.id.ends_with(":deviceA"));
    }

    #[test]
    fn payload_round_trips_as_json() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        append_event(&conn, &mut hlc, 1000, "deviceA", "userX",
            "ItemDefined", &json!({"itemId": "i1", "sku": "SKU-1"})).unwrap();

        let sku: String = conn
            .query_row("SELECT payload ->> 'sku' FROM events LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(sku, "SKU-1");
    }

    #[test]
    fn read_events_returns_hlc_order() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        append_event(&conn, &mut hlc, 1000, "deviceA", "u", "A", &json!({})).unwrap();
        append_event(&conn, &mut hlc, 1000, "deviceA", "u", "B", &json!({})).unwrap();
        append_event(&conn, &mut hlc, 2000, "deviceA", "u", "C", &json!({})).unwrap();

        let events = read_events(&conn).unwrap();
        let types: Vec<&str> = events.iter().map(|e| e.event_type.as_str()).collect();
        assert_eq!(types, vec!["A", "B", "C"], "must be in HLC order");
    }

    #[test]
    fn missing_seq_reports_gap() {
        let conn = open_in_memory_with_schema().unwrap();
        for (id, seq, t) in [("h1-deviceA", 1, "A"), ("h3-deviceA", 3, "C")] {
            conn.execute(
                "INSERT INTO events (id, hlc, device_id, user_id, seq, type, payload, created_at)
                 VALUES (?1, ?1, 'deviceA', 'u', ?2, ?3, jsonb('{}'), 0)",
                rusqlite::params![id, seq, t],
            ).unwrap();
        }
        let gaps = missing_seqs(&conn, "deviceA").unwrap();
        assert_eq!(gaps, vec![2], "seq 2 should be reported missing");
    }
}
