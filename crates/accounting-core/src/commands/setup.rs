use crate::commands::{commit_event, reject, CommandContext, CommandError};
use crate::genesis::{ANON_SUPPLIER_PARTY_ID, WALKIN_PARTY_ID};
use crate::refs::{count_references, ITEM_REFS, PARTY_REFS};
use serde_json::json;

fn ensure_absent(ctx: &CommandContext, table: &str, id: &str) -> Result<(), CommandError> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE id = ?1");
    let n: i64 = ctx.conn.query_row(&sql, [id], |r| r.get(0))?;
    if n > 0 { Err(reject(format!("{table} id already exists: {id}"))) } else { Ok(()) }
}

fn ensure_present(ctx: &CommandContext, table: &str, id: &str) -> Result<(), CommandError> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE id = ?1");
    let n: i64 = ctx.conn.query_row(&sql, [id], |r| r.get(0))?;
    if n == 0 { Err(reject(format!("{table} id not found: {id}"))) } else { Ok(()) }
}

/// The seeded parties the UI selects automatically for cash trade. Neither may
/// be archived or deleted — doing so breaks the default path on both the sales
/// and purchases forms.
fn ensure_not_seeded_party(party_id: &str) -> Result<(), CommandError> {
    if party_id == WALKIN_PARTY_ID || party_id == ANON_SUPPLIER_PARTY_ID {
        return Err(reject(format!(
            "{party_id} is a built-in party used for cash trade and cannot be archived or deleted"
        )));
    }
    Ok(())
}

/// Refuse a hard delete once anything in the read model points at the row.
///
/// Deliberately strict: the alternative is a `DELETE` the foreign keys reject
/// during replay, and startup treats a failed rebuild as fatal.
fn ensure_unreferenced(
    ctx: &CommandContext,
    refs: &[(&str, &str)],
    id: &str,
    noun: &str,
) -> Result<(), CommandError> {
    let n = count_references(ctx.conn, refs, id)?;
    if n > 0 {
        return Err(reject(format!(
            "{noun} {id} is used by {n} existing record(s); archive it instead of deleting it"
        )));
    }
    Ok(())
}

pub fn handle_user_registered(ctx: &mut CommandContext, user_id: &str, name: &str, role: Option<&str>)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_absent(ctx, "users", user_id)?;
    let mut p = json!({ "userId": user_id, "name": name });
    if let Some(r) = role { p["role"] = json!(r); }
    commit_event(ctx, "UserRegistered", p)
}

pub fn handle_account_opened(
    ctx: &mut CommandContext, account_id: &str, name: &str, acct_type: &str,
    normal_side: &str, system_role: Option<&str>,
) -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_absent(ctx, "accounts", account_id)?;
    if !matches!(acct_type, "asset"|"liability"|"equity"|"income"|"expense") {
        return Err(reject(format!("invalid account type: {acct_type}")));
    }
    if !matches!(normal_side, "debit"|"credit") {
        return Err(reject(format!("invalid normal side: {normal_side}")));
    }
    let mut p = json!({ "accountId": account_id, "name": name, "type": acct_type, "normal": normal_side });
    if let Some(r) = system_role { p["system_role"] = json!(r); }
    commit_event(ctx, "AccountOpened", p)
}

pub fn handle_item_defined(ctx: &mut CommandContext, item_id: &str, sku: &str, name: &str, unit: &str)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_absent(ctx, "items", item_id)?;
    commit_event(ctx, "ItemDefined",
        json!({ "itemId": item_id, "sku": sku, "name": name, "unit": unit, "active": true }))
}

pub fn handle_party_created(ctx: &mut CommandContext, party_id: &str, name: &str, kind: &str)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_absent(ctx, "parties", party_id)?;
    if !matches!(kind, "supplier"|"customer"|"both") {
        return Err(reject(format!("invalid party kind: {kind}")));
    }
    commit_event(ctx, "PartyCreated", json!({ "partyId": party_id, "name": name, "kind": kind }))
}

pub fn handle_user_updated(ctx: &mut CommandContext, user_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "users", user_id)?;
    commit_event(ctx, "UserUpdated", json!({ "userId": user_id, "changes": changes }))
}

pub fn handle_account_updated(ctx: &mut CommandContext, account_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "accounts", account_id)?;
    for immutable in ["type", "normal", "normalSide", "system_role"] {
        if changes.get(immutable).is_some() {
            return Err(reject(format!("account field '{immutable}' is immutable")));
        }
    }
    commit_event(ctx, "AccountUpdated", json!({ "accountId": account_id, "changes": changes }))
}

pub fn handle_item_updated(ctx: &mut CommandContext, item_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "items", item_id)?;
    if let Some(a) = changes.get("active") {
        if !a.is_boolean() { return Err(reject("item 'active' must be a boolean")); }
    }
    // `items_sku` is UNIQUE, so a colliding rename fails inside the projector.
    // The commit rolls back either way; checking here turns an opaque database
    // error into something the user can act on.
    if let Some(sku) = changes.get("sku") {
        let sku = sku.as_str().ok_or_else(|| reject("item 'sku' must be a string"))?;
        let clash: i64 = ctx.conn.query_row(
            "SELECT COUNT(*) FROM items WHERE sku = ?1 AND id != ?2",
            rusqlite::params![sku, item_id],
            |r| r.get(0),
        )?;
        if clash > 0 {
            return Err(reject(format!("another item already uses SKU '{sku}'")));
        }
    }
    commit_event(ctx, "ItemUpdated", json!({ "itemId": item_id, "changes": changes }))
}

pub fn handle_party_updated(ctx: &mut CommandContext, party_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "parties", party_id)?;
    if let Some(k) = changes.get("kind") {
        let ok = k.as_str().map(|s| matches!(s, "supplier"|"customer"|"both")).unwrap_or(false);
        if !ok { return Err(reject(format!("invalid party kind in changes: {k}"))); }
    }
    match changes.get("active") {
        Some(a) if !a.is_boolean() => return Err(reject("party 'active' must be a boolean")),
        // Renaming a seeded party is fine; hiding it is not.
        Some(a) if a == &json!(false) => ensure_not_seeded_party(party_id)?,
        _ => {}
    }
    commit_event(ctx, "PartyUpdated", json!({ "partyId": party_id, "changes": changes }))
}

pub fn handle_item_deleted(ctx: &mut CommandContext, item_id: &str)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "items", item_id)?;
    ensure_unreferenced(ctx, ITEM_REFS, item_id, "item")?;
    commit_event(ctx, "ItemDeleted", json!({ "itemId": item_id }))
}

pub fn handle_party_deleted(ctx: &mut CommandContext, party_id: &str)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "parties", party_id)?;
    ensure_not_seeded_party(party_id)?;
    ensure_unreferenced(ctx, PARTY_REFS, party_id, "party")?;
    commit_event(ctx, "PartyDeleted", json!({ "partyId": party_id }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::tests::fixture;

    fn ctx<'a>(conn: &'a mut rusqlite::Connection, hlc: &'a mut crate::hlc::Hlc) -> CommandContext<'a> {
        CommandContext { conn, hlc, physical_now: 1000, device_id: "deviceA".into(), user_id: "owner-1".into() }
    }

    #[test]
    fn party_created_then_duplicate_rejected_and_not_written() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme", "supplier").expect("first ok");
        }
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme Dup", "supplier").unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM events WHERE type='PartyCreated'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "duplicate must not append a second event");
    }

    #[test]
    fn account_updated_rejects_immutable_type_change() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_opened(&mut c, "a1", "Misc", "expense", "debit", None).expect("open");
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_updated(&mut c, "a1", json!({"name": "Miscellaneous"})).expect("rename ok");
        }
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_account_updated(&mut c, "a1", json!({"type": "asset"})).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn item_updated_rejects_unknown_id() {
        let (mut conn, mut hlc) = fixture();
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_item_updated(&mut c, "nope", json!({"name": "X"})).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn party_updated_rejects_invalid_kind_in_changes() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme", "supplier").expect("create");
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            assert!(handle_party_updated(&mut c, "p1", json!({"name": "Acme Inc"})).is_ok());
        }
        let mut c = ctx(&mut conn, &mut hlc);
        let err = handle_party_updated(&mut c, "p1", json!({"kind": "vendor"})).unwrap_err();
        assert!(matches!(err, CommandError::Validation(_)));
    }

    fn seed_sold_item(conn: &rusqlite::Connection) {
        conn.execute(
            "INSERT INTO sales (id, event_id, date, terms, total_minor) VALUES ('s1','e1','2026-01-01','cash',100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sale_lines (id, sale_id, item_id, qty, unit_price_minor, revenue_minor, cogs_minor, date)
             VALUES ('sl1','s1','i1',1,100,100,50,'2026-01-01')",
            [],
        )
        .unwrap();
    }

    #[test]
    fn item_deleted_removes_an_item_nothing_references() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i1", "S1", "Widget", "ea").expect("define");
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_deleted(&mut c, "i1").expect("delete an unused item");
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM items WHERE id = 'i1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "the projection row must be gone");
    }

    #[test]
    fn item_deleted_is_refused_once_the_item_has_been_sold() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i1", "S1", "Widget", "ea").expect("define");
        }
        seed_sold_item(&conn);

        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_deleted(&mut c, "i1").unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));

        let events: i64 = conn
            .query_row("SELECT COUNT(*) FROM events WHERE type = 'ItemDeleted'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(events, 0, "a rejected delete must not append an event");
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM items WHERE id = 'i1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1, "the item must survive");
    }

    #[test]
    fn item_deleted_rejects_an_unknown_id() {
        let (mut conn, mut hlc) = fixture();
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(matches!(
            handle_item_deleted(&mut c, "nope").unwrap_err(),
            CommandError::Validation(_)
        ));
    }

    #[test]
    fn party_deleted_removes_a_party_nothing_references() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme", "supplier").expect("create");
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_deleted(&mut c, "p1").expect("delete an unused party");
        }
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM parties WHERE id = 'p1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn party_deleted_is_refused_once_the_party_has_traded() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, "p1", "Acme", "customer").expect("create");
        }
        conn.execute(
            "INSERT INTO sales (id, event_id, customer_id, date, terms, total_minor)
             VALUES ('s1','e1','p1','2026-01-01','credit',100)",
            [],
        )
        .unwrap();

        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_deleted(&mut c, "p1").unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
    }

    #[test]
    fn the_seeded_parties_can_be_neither_deleted_nor_archived() {
        // Cash sales auto-select the walk-in customer and cash purchases the
        // anonymous supplier. Removing or hiding either breaks the default
        // path for the very users least able to diagnose it.
        let (mut conn, mut hlc) = fixture();
        for id in [crate::genesis::WALKIN_PARTY_ID, crate::genesis::ANON_SUPPLIER_PARTY_ID] {
            {
                let mut c = ctx(&mut conn, &mut hlc);
                handle_party_created(&mut c, id, "Seeded", "both").expect("seed");
            }
            {
                let mut c = ctx(&mut conn, &mut hlc);
                assert!(
                    matches!(
                        handle_party_deleted(&mut c, id).unwrap_err(),
                        CommandError::Validation(_)
                    ),
                    "{id} must not be deletable"
                );
            }
            let mut c = ctx(&mut conn, &mut hlc);
            assert!(
                matches!(
                    handle_party_updated(&mut c, id, json!({"active": false})).unwrap_err(),
                    CommandError::Validation(_)
                ),
                "{id} must not be archivable"
            );
        }
    }

    #[test]
    fn party_updated_still_allows_renaming_a_seeded_party() {
        // Only archiving is blocked, not every edit.
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_party_created(&mut c, crate::genesis::WALKIN_PARTY_ID, "Walk-in", "customer")
                .unwrap();
        }
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_party_updated(
            &mut c,
            crate::genesis::WALKIN_PARTY_ID,
            json!({"name": "Passing trade"})
        )
        .is_ok());
    }

    #[test]
    fn item_updated_rejects_a_duplicate_sku() {
        // `items_sku` is a UNIQUE index, so a colliding rename would fail
        // inside the projector. That rolls the commit back safely, but the
        // user sees a database error instead of an explanation.
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i1", "S1", "Widget", "ea").unwrap();
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i2", "S2", "Gadget", "ea").unwrap();
        }
        let err = {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_updated(&mut c, "i2", json!({"sku": "S1"})).unwrap_err()
        };
        assert!(matches!(err, CommandError::Validation(_)));
        // Renaming to its own sku is not a collision.
        let mut c = ctx(&mut conn, &mut hlc);
        assert!(handle_item_updated(&mut c, "i2", json!({"sku": "S2", "name": "Gadget II"})).is_ok());
    }

    #[test]
    fn item_updated_archives_by_setting_active_false() {
        let (mut conn, mut hlc) = fixture();
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_defined(&mut c, "i1", "S1", "Widget", "ea").unwrap();
        }
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_updated(&mut c, "i1", json!({"active": false})).expect("archive");
        }
        let active: Option<i64> = conn
            .query_row("SELECT active FROM items WHERE id = 'i1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(active, Some(0), "archive must be visible through the generated column");

        // ...and it is reversible.
        {
            let mut c = ctx(&mut conn, &mut hlc);
            handle_item_updated(&mut c, "i1", json!({"active": true})).expect("restore");
        }
        let active: Option<i64> = conn
            .query_row("SELECT active FROM items WHERE id = 'i1'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(active, Some(1));
    }
}
