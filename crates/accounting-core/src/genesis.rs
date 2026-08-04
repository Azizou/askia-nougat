use crate::events::append_event;
use crate::hlc::Hlc;
use rusqlite::Connection;
use serde_json::json;

/// The reserved system-user id used to author genesis events.
pub const SYSTEM_USER_ID: &str = "system";

/// The shared, always-present customer used to record cash sales to unknown
/// walk-in buyers.
pub const WALKIN_PARTY_ID: &str = "party_walkin";

/// The shared, always-present supplier used to record cash purchases from an
/// unrecorded seller — the buy-side counterpart of [`WALKIN_PARTY_ID`].
pub const ANON_SUPPLIER_PARTY_ID: &str = "party_anon_supplier";

/// Seeded chart of accounts: (system_role, display name, type, normal side).
const SEEDED_ACCOUNTS: &[(&str, &str, &str, &str)] = &[
    ("cash",                "Cash",                     "asset",     "debit"),
    ("bank",                "Bank",                     "asset",     "debit"),
    ("inventory",           "Inventory",                "asset",     "debit"),
    ("accounts_receivable", "Accounts Receivable",      "asset",     "debit"),
    ("accounts_payable",    "Accounts Payable",         "liability", "credit"),
    ("tax_payable",         "Tax Payable",              "liability", "credit"),
    ("owner_capital",       "Owner Capital",            "equity",    "credit"),
    ("retained_earnings",   "Retained Earnings",        "equity",    "credit"),
    ("sales",               "Sales",                    "income",    "credit"),
    ("cogs",                "Cost of Goods Sold",       "expense",   "debit"),
    ("shrinkage",           "Inventory Shrinkage",      "expense",   "debit"),
    ("inventory_gain",      "Inventory Gain (overage)", "income",    "credit"),
    ("rent",                "Rent",                     "expense",   "debit"),
    ("wages",               "Wages",                    "expense",   "debit"),
];

/// Emit the first-run genesis event sequence.
pub fn run_genesis(
    conn: &Connection,
    hlc: &mut Hlc,
    physical_now: u64,
    device_id: &str,
    owner_user_id: &str,
    owner_name: &str,
) -> rusqlite::Result<()> {
    let existing: i64 = conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))?;
    if existing > 0 {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            Some("run_genesis called on a non-empty event log".to_string()),
        ));
    }

    append_event(
        conn, hlc, physical_now, device_id, SYSTEM_USER_ID,
        "UserRegistered",
        &json!({ "userId": owner_user_id, "name": owner_name }),
    )?;

    for (role, name, acct_type, normal) in SEEDED_ACCOUNTS {
        append_event(
            conn, hlc, physical_now, device_id, SYSTEM_USER_ID,
            "AccountOpened",
            &json!({
                "accountId": format!("acct_{role}"),
                "name": name,
                "type": acct_type,
                "normal": normal,
                "system_role": role,
            }),
        )?;
    }
    Ok(())
}

/// Idempotently ensure the walk-in customer party exists.
///
/// Safe to call on every startup: it checks the immutable event log (not the
/// `parties` projection, which is empty until `rebuild()` runs) and only
/// appends a `PartyCreated` event when none exists yet. Covers installs whose
/// genesis predates the walk-in party.
pub fn ensure_walkin_party(
    conn: &Connection,
    hlc: &mut Hlc,
    physical_now: u64,
    device_id: &str,
) -> rusqlite::Result<()> {
    let exists: i64 = conn.query_row(
        "SELECT COUNT(*) FROM events WHERE type = 'PartyCreated' \
         AND json_extract(payload, '$.partyId') = ?1",
        [WALKIN_PARTY_ID],
        |r| r.get(0),
    )?;
    if exists > 0 {
        return Ok(());
    }
    append_event(
        conn, hlc, physical_now, device_id, SYSTEM_USER_ID,
        "PartyCreated",
        &json!({ "partyId": WALKIN_PARTY_ID, "name": "Walk-in Customer", "kind": "customer" }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::open_in_memory_with_schema;
    use crate::events::read_events;

    #[test]
    fn genesis_emits_user_then_accounts_all_system_authored() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane Owner").unwrap();

        let events = read_events(&conn).unwrap();
        assert_eq!(events[0].event_type, "UserRegistered");
        assert_eq!(events[0].payload["userId"], "owner-1");
        assert!(events.iter().all(|e| e.user_id == "system"));
        let account_events = events.iter().filter(|e| e.event_type == "AccountOpened").count();
        assert_eq!(account_events, 14);
        let inv = events.iter().find(|e| e.payload["system_role"] == "inventory").unwrap();
        assert_eq!(inv.payload["type"], "asset");
        assert_eq!(inv.payload["normal"], "debit");
    }

    #[test]
    fn genesis_seeds_unique_system_roles() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();
        let events = read_events(&conn).unwrap();
        let mut roles: Vec<String> = events
            .iter()
            .filter(|e| e.event_type == "AccountOpened")
            .map(|e| e.payload["system_role"].as_str().unwrap().to_string())
            .collect();
        let before = roles.len();
        roles.sort();
        roles.dedup();
        assert_eq!(before, roles.len(), "system_roles must be unique");
    }

    #[test]
    fn genesis_rejects_second_run_and_leaves_log_unchanged() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();
        let count_after_first: i64 =
            conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();

        let second = run_genesis(&conn, &mut hlc, 2000, "deviceA", "owner-1", "Jane");
        assert!(second.is_err(), "second genesis must be rejected");

        let count_after_second: i64 =
            conn.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0)).unwrap();
        assert_eq!(count_after_first, count_after_second, "log must be unchanged");
    }

    #[test]
    fn ensure_walkin_seeds_once_and_is_idempotent() {
        let conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();

        ensure_walkin_party(&conn, &mut hlc, 2000, "deviceA").unwrap();
        ensure_walkin_party(&conn, &mut hlc, 3000, "deviceA").unwrap();

        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM events WHERE type='PartyCreated' \
                 AND json_extract(payload, '$.partyId') = ?1",
                [WALKIN_PARTY_ID],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "walk-in party must be seeded exactly once");
    }

    #[test]
    fn ensure_walkin_projects_a_customer_party() {
        let mut conn = open_in_memory_with_schema().unwrap();
        let mut hlc = Hlc::new("deviceA");
        run_genesis(&conn, &mut hlc, 1000, "deviceA", "owner-1", "Jane").unwrap();
        ensure_walkin_party(&conn, &mut hlc, 2000, "deviceA").unwrap();
        crate::projectors::rebuild(&mut conn).unwrap();
        let kind: String = conn
            .query_row("SELECT kind FROM parties WHERE id = ?1", [WALKIN_PARTY_ID], |r| r.get(0))
            .unwrap();
        assert_eq!(kind, "customer");
    }
}
