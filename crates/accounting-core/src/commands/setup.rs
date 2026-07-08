use crate::commands::{commit_event, reject, CommandContext, CommandError};
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
    commit_event(ctx, "ItemUpdated", json!({ "itemId": item_id, "changes": changes }))
}

pub fn handle_party_updated(ctx: &mut CommandContext, party_id: &str, changes: serde_json::Value)
    -> Result<crate::events::LedgerEvent, CommandError> {
    ensure_present(ctx, "parties", party_id)?;
    if let Some(k) = changes.get("kind") {
        let ok = k.as_str().map(|s| matches!(s, "supplier"|"customer"|"both")).unwrap_or(false);
        if !ok { return Err(reject(format!("invalid party kind in changes: {k}"))); }
    }
    commit_event(ctx, "PartyUpdated", json!({ "partyId": party_id, "changes": changes }))
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
}
