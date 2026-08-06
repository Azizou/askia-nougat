use rusqlite::Connection;

// --- §8.1: units sold by month ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlyUnits {
    pub month: String,
    pub units_sold: i64,
    pub revenue_minor: i64,
}

pub fn units_sold_by_month(conn: &Connection, item_id: &str, anchor: &str) -> rusqlite::Result<Vec<MonthlyUnits>> {
    let mut stmt = conn.prepare(
        "SELECT strftime('%Y-%m', sl.date) AS month,
                SUM(sl.qty) AS units_sold, SUM(sl.revenue_minor) AS revenue_minor
         FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
         WHERE sl.item_id = ?1 AND s.reversed = 0
           AND sl.date >= date(?2, 'start of month', '-1 month')
         GROUP BY month ORDER BY month")?;
    let rows = stmt.query_map(rusqlite::params![item_id, anchor], |r| {
        Ok(MonthlyUnits { month: r.get(0)?, units_sold: r.get(1)?, revenue_minor: r.get(2)? })
    })?;
    rows.collect()
}

// --- §8.2: gross & net profit ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrossProfit {
    pub revenue_minor: i64,
    pub cogs_minor: i64,
    pub gross_profit_minor: i64,
}

pub fn gross_profit(conn: &Connection, anchor: &str) -> rusqlite::Result<GrossProfit> {
    conn.query_row(
        "SELECT COALESCE(SUM(sl.revenue_minor), 0),
                COALESCE(SUM(sl.cogs_minor), 0),
                COALESCE(SUM(sl.revenue_minor - sl.cogs_minor), 0)
         FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
         WHERE s.reversed = 0 AND sl.date >= date(?1, '-6 months')",
        rusqlite::params![anchor], |r| {
            Ok(GrossProfit { revenue_minor: r.get(0)?, cogs_minor: r.get(1)?, gross_profit_minor: r.get(2)? })
        })
}

pub fn net_profit(conn: &Connection, anchor: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT
          (SELECT COALESCE(SUM(sl.revenue_minor - sl.cogs_minor), 0)
             FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
             WHERE s.reversed = 0 AND sl.date >= date(?1, '-6 months'))
          -
          (SELECT COALESCE(SUM(debit_minor - credit_minor), 0) FROM journal_lines jl
             JOIN accounts a ON a.id = jl.account_id
             WHERE a.type = 'expense' AND a.system_role IS NOT 'cogs'
               AND jl.date >= date(?1, '-6 months'))",
        rusqlite::params![anchor], |r| r.get(0))
}

// --- §8.3: inventory age per lot + aging buckets ---

/// Age and on-hand value of one open lot (spec §8.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LotAge {
    pub lot_id: String,
    pub qty_remaining: i64,
    pub unit_cost_minor: i64,
    pub age_days: i64,
    pub value_on_hand_minor: i64,
}

/// One aging bucket across inventory (spec §8.3 dead-stock detector).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgingBucket {
    pub bucket: String,
    pub units: i64,
    pub value_minor: i64,
}

const LOT_AGES_SQL: &str = "\
SELECT id AS lot_id, qty_remaining, unit_cost_minor,
       CAST(julianday(?2) - julianday(acquired_at) AS INT) AS age_days,
       qty_remaining * unit_cost_minor AS value_on_hand_minor
FROM inventory_lots
WHERE item_id = ?1 AND qty_remaining > 0
ORDER BY acquired_at";

// A parallel `sort_key` (0..3) orders the buckets by age band; ordering by the
// text label would sort '180d+' before '31-90d'. GROUP BY both so the label and
// its key stay paired.
const AGING_BUCKETS_SQL: &str = "\
SELECT CASE
    WHEN julianday(?1) - julianday(acquired_at) <= 30  THEN '0-30d'
    WHEN julianday(?1) - julianday(acquired_at) <= 90  THEN '31-90d'
    WHEN julianday(?1) - julianday(acquired_at) <= 180 THEN '91-180d'
    ELSE '180d+' END AS bucket,
  CASE
    WHEN julianday(?1) - julianday(acquired_at) <= 30  THEN 0
    WHEN julianday(?1) - julianday(acquired_at) <= 90  THEN 1
    WHEN julianday(?1) - julianday(acquired_at) <= 180 THEN 2
    ELSE 3 END AS sort_key,
  SUM(qty_remaining) AS units, SUM(qty_remaining * unit_cost_minor) AS value_minor
FROM inventory_lots WHERE qty_remaining > 0
GROUP BY bucket, sort_key ORDER BY sort_key";

pub fn lot_ages(conn: &Connection, item_id: &str, anchor: &str) -> rusqlite::Result<Vec<LotAge>> {
    let mut stmt = conn.prepare(LOT_AGES_SQL)?;
    let rows = stmt.query_map(rusqlite::params![item_id, anchor], |r| {
        Ok(LotAge {
            lot_id: r.get(0)?,
            qty_remaining: r.get(1)?,
            unit_cost_minor: r.get(2)?,
            age_days: r.get(3)?,
            value_on_hand_minor: r.get(4)?,
        })
    })?;
    rows.collect()
}

pub fn aging_buckets(conn: &Connection, anchor: &str) -> rusqlite::Result<Vec<AgingBucket>> {
    let mut stmt = conn.prepare(AGING_BUCKETS_SQL)?;
    let rows = stmt.query_map(rusqlite::params![anchor], |r| {
        Ok(AgingBucket { bucket: r.get(0)?, units: r.get(2)?, value_minor: r.get(3)? })
    })?;
    rows.collect()
}

// --- §8.4: stock on hand + inventory valuation ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StockOnHand {
    pub item_id: String,
    pub qty: i64,
}

const STOCK_ON_HAND_SQL: &str = "\
SELECT item_id, SUM(qty_remaining) AS qty
FROM inventory_lots WHERE qty_remaining > 0
GROUP BY item_id ORDER BY item_id";

const INVENTORY_VALUATION_SQL: &str =
    "SELECT COALESCE(SUM(qty_remaining * unit_cost_minor), 0) FROM inventory_lots";

pub fn stock_on_hand(conn: &Connection) -> rusqlite::Result<Vec<StockOnHand>> {
    let mut stmt = conn.prepare(STOCK_ON_HAND_SQL)?;
    let rows = stmt.query_map([], |r| Ok(StockOnHand { item_id: r.get(0)?, qty: r.get(1)? }))?;
    rows.collect()
}

pub fn inventory_valuation(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(INVENTORY_VALUATION_SQL, [], |r| r.get(0))
}

// --- §8.4: gross margin % per item + best/worst sellers ---

#[derive(Debug, Clone, PartialEq)]
pub struct ItemMargin {
    pub item_id: String,
    pub revenue_minor: i64,
    pub cogs_minor: i64,
    pub margin_pct: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SellerRow {
    pub item_id: String,
    pub units: i64,
    pub profit_minor: i64,
}

const GROSS_MARGIN_SQL: &str = "\
SELECT sl.item_id,
  COALESCE(SUM(sl.revenue_minor), 0) AS revenue,
  COALESCE(SUM(sl.cogs_minor), 0) AS cogs,
  CASE WHEN SUM(sl.revenue_minor) = 0 THEN NULL
       ELSE ROUND(100.0 * SUM(sl.revenue_minor - sl.cogs_minor) / SUM(sl.revenue_minor), 2)
  END AS margin_pct
FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
WHERE s.reversed = 0
GROUP BY sl.item_id ORDER BY sl.item_id";

const SELLERS_SQL: &str = "\
SELECT sl.item_id, SUM(sl.qty) AS units, SUM(sl.revenue_minor - sl.cogs_minor) AS profit_minor
FROM sale_lines sl JOIN sales s ON s.id = sl.sale_id
WHERE s.reversed = 0
GROUP BY sl.item_id ORDER BY units DESC, sl.item_id";

pub fn gross_margin_per_item(conn: &Connection) -> rusqlite::Result<Vec<ItemMargin>> {
    let mut stmt = conn.prepare(GROSS_MARGIN_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(ItemMargin {
            item_id: r.get(0)?,
            revenue_minor: r.get(1)?,
            cogs_minor: r.get(2)?,
            margin_pct: r.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn sellers_by_units(conn: &Connection) -> rusqlite::Result<Vec<SellerRow>> {
    let mut stmt = conn.prepare(SELLERS_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(SellerRow { item_id: r.get(0)?, units: r.get(1)?, profit_minor: r.get(2)? })
    })?;
    rows.collect()
}

// --- §8.4: party balances + A/R & A/P aging ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartyBalance {
    pub party_id: String,
    pub receivable_minor: i64,
    pub payable_minor: i64,
    pub unallocated_cr_minor: i64,
    pub unallocated_dr_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgingInvoice {
    pub invoice_id: String,
    pub date: String,
    pub outstanding_minor: i64,
    pub age_days: i64,
    pub bucket: String,
}

const PARTY_BALANCES_SQL: &str = "\
SELECT party_id, receivable_minor, payable_minor, unallocated_cr_minor, unallocated_dr_minor
FROM party_balances ORDER BY party_id";

const RECEIVABLE_AGING_SQL: &str = "\
SELECT id AS invoice_id, date, outstanding_minor,
  CAST(julianday(?1) - julianday(date) AS INT) AS age_days,
  CASE
    WHEN julianday(?1) - julianday(date) <= 30 THEN '0-30d'
    WHEN julianday(?1) - julianday(date) <= 60 THEN '31-60d'
    WHEN julianday(?1) - julianday(date) <= 90 THEN '61-90d'
    ELSE '90d+' END AS bucket
FROM sales WHERE terms = 'credit' AND outstanding_minor > 0 ORDER BY date";
const PAYABLE_AGING_SQL: &str = "\
SELECT id AS invoice_id, date, outstanding_minor,
  CAST(julianday(?1) - julianday(date) AS INT) AS age_days,
  CASE
    WHEN julianday(?1) - julianday(date) <= 30 THEN '0-30d'
    WHEN julianday(?1) - julianday(date) <= 60 THEN '31-60d'
    WHEN julianday(?1) - julianday(date) <= 90 THEN '61-90d'
    ELSE '90d+' END AS bucket
FROM purchases WHERE terms = 'credit' AND outstanding_minor > 0 ORDER BY date";

pub fn party_balances(conn: &Connection) -> rusqlite::Result<Vec<PartyBalance>> {
    let mut stmt = conn.prepare(PARTY_BALANCES_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(PartyBalance {
            party_id: r.get(0)?,
            receivable_minor: r.get(1)?,
            payable_minor: r.get(2)?,
            unallocated_cr_minor: r.get(3)?,
            unallocated_dr_minor: r.get(4)?,
        })
    })?;
    rows.collect()
}

// --- parties eligible for a payment ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayableParty {
    pub id: String,
    pub name: String,
    pub kind: String,
    /// True when the party is archived and appears only because it still owes or
    /// is owed something. The form uses this to label the row, so an archived
    /// party showing up in the dropdown reads as deliberate rather than as a
    /// filter that failed.
    pub archived: bool,
}

/// Parties the payments form may offer for `direction`: every active party of a
/// matching kind, plus any archived party that still has an unsettled invoice or
/// an unallocated balance.
///
/// The second clause is the point. Archiving is explicitly allowed while an
/// invoice is open — that is what archive is for, retiring a party without
/// erasing its history. But the form filtered on `active`, so archiving a
/// customer mid-collection removed the only screen that could record their
/// payment: the debt stayed on the books, visible in aging, with no way to
/// settle it. Un-archiving as a workaround is not discoverable, and a user who
/// found it would still have to remember to re-archive.
///
/// `COALESCE(active, 1)` per D3: rows written before the column existed project
/// NULL, and a bare `active = 1` would hide every legacy party.
pub fn payable_parties(conn: &Connection, direction: &str) -> rusqlite::Result<Vec<PayableParty>> {
    // Money in settles sales owed by customers; money out settles purchases owed
    // to suppliers. `both` qualifies either way.
    let (kind, invoice_table, party_col, owed_cols) = match direction {
        "in" => ("customer", "sales", "customer_id", ("receivable_minor", "unallocated_cr_minor")),
        "out" => ("supplier", "purchases", "supplier_id", ("payable_minor", "unallocated_dr_minor")),
        other => {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "invalid direction: {other}"
            )))
        }
    };
    let (owed_a, owed_b) = owed_cols;
    let sql = format!(
        "SELECT p.id, p.name, p.kind, COALESCE(p.active, 1) AS act
         FROM parties p
         WHERE (p.kind = ?1 OR p.kind = 'both')
           AND (
             COALESCE(p.active, 1) = 1
             OR EXISTS (SELECT 1 FROM {invoice_table} inv
                        WHERE inv.{party_col} = p.id
                          AND inv.reversed = 0 AND inv.outstanding_minor > 0)
             OR EXISTS (SELECT 1 FROM party_balances pb
                        WHERE pb.party_id = p.id
                          AND (COALESCE(pb.{owed_a}, 0) <> 0
                               OR COALESCE(pb.{owed_b}, 0) <> 0))
           )
         ORDER BY act DESC, p.name"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([kind], |r| {
        Ok(PayableParty {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            archived: r.get::<_, i64>(3)? == 0,
        })
    })?;
    rows.collect()
}

fn run_aging(conn: &Connection, sql: &str, anchor: &str) -> rusqlite::Result<Vec<AgingInvoice>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params![anchor], |r| {
        Ok(AgingInvoice {
            invoice_id: r.get(0)?,
            date: r.get(1)?,
            outstanding_minor: r.get(2)?,
            age_days: r.get(3)?,
            bucket: r.get(4)?,
        })
    })?;
    rows.collect()
}

pub fn receivable_aging(conn: &Connection, anchor: &str) -> rusqlite::Result<Vec<AgingInvoice>> {
    run_aging(conn, RECEIVABLE_AGING_SQL, anchor)
}

pub fn payable_aging(conn: &Connection, anchor: &str) -> rusqlite::Result<Vec<AgingInvoice>> {
    run_aging(conn, PAYABLE_AGING_SQL, anchor)
}

// --- §8.4: return rate per item + age-at-sale / turnover ---

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnRate {
    pub item_id: String,
    pub sold_qty: i64,
    pub returned_qty: i64,
    pub return_rate: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgeAtSale {
    pub item_id: String,
    pub sale_date: String,
    pub acquired_at: String,
    pub age_at_sale_days: i64,
    pub qty_taken: i64,
}

const RETURN_RATE_SQL: &str = "\
SELECT sl.item_id,
  COALESCE(SUM(sl.qty), 0) AS sold_qty,
  COALESCE((SELECT SUM(rl.qty) FROM return_lines rl
              JOIN returns r ON r.id = rl.return_id
              WHERE rl.item_id = sl.item_id AND r.return_type = 'sale_return'), 0) AS returned_qty,
  CASE WHEN SUM(sl.qty) = 0 THEN NULL
       ELSE ROUND(1.0 * COALESCE((SELECT SUM(rl.qty) FROM return_lines rl
                                    JOIN returns r ON r.id = rl.return_id
                                    WHERE rl.item_id = sl.item_id AND r.return_type = 'sale_return'), 0)
                  / SUM(sl.qty), 4)
  END AS return_rate
FROM sale_lines sl GROUP BY sl.item_id ORDER BY sl.item_id";

const AGE_AT_SALE_SQL: &str = "\
SELECT sl.item_id, s.date AS sale_date, il.acquired_at,
  CAST(julianday(s.date) - julianday(il.acquired_at) AS INT) AS age_at_sale_days,
  lc.qty_taken
FROM lot_consumptions lc
JOIN sale_lines sl ON sl.id = lc.sale_line_id
JOIN sales s ON s.id = sl.sale_id
JOIN inventory_lots il ON il.id = lc.lot_id
ORDER BY s.date, sl.item_id";

pub fn return_rate_per_item(conn: &Connection) -> rusqlite::Result<Vec<ReturnRate>> {
    let mut stmt = conn.prepare(RETURN_RATE_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(ReturnRate {
            item_id: r.get(0)?,
            sold_qty: r.get(1)?,
            returned_qty: r.get(2)?,
            return_rate: r.get(3)?,
        })
    })?;
    rows.collect()
}

pub fn age_at_sale(conn: &Connection) -> rusqlite::Result<Vec<AgeAtSale>> {
    let mut stmt = conn.prepare(AGE_AT_SALE_SQL)?;
    let rows = stmt.query_map([], |r| {
        Ok(AgeAtSale {
            item_id: r.get(0)?,
            sale_date: r.get(1)?,
            acquired_at: r.get(2)?,
            age_at_sale_days: r.get(3)?,
            qty_taken: r.get(4)?,
        })
    })?;
    rows.collect()
}

// --- §8.4: P&L + balance sheet ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfitAndLoss {
    pub income_minor: i64,
    pub expense_minor: i64,
    pub net_profit_minor: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BalanceSheet {
    pub assets_minor: i64,
    pub liabilities_minor: i64,
    pub equity_minor: i64,
}

const PL_SQL: &str = "\
SELECT
  (SELECT COALESCE(SUM(jl.credit_minor - jl.debit_minor), 0)
     FROM journal_lines jl JOIN accounts a ON a.id = jl.account_id
     WHERE a.type = 'income' AND jl.date BETWEEN ?1 AND ?2) AS income_minor,
  (SELECT COALESCE(SUM(jl.debit_minor - jl.credit_minor), 0)
     FROM journal_lines jl JOIN accounts a ON a.id = jl.account_id
     WHERE a.type = 'expense' AND jl.date BETWEEN ?1 AND ?2) AS expense_minor";

const BS_SQL: &str = "\
SELECT
  COALESCE((SELECT SUM(balance_minor) FROM accounts WHERE type = 'asset'), 0)     AS assets_minor,
  COALESCE((SELECT SUM(balance_minor) FROM accounts WHERE type = 'liability'), 0) AS liabilities_minor,
  COALESCE((SELECT SUM(balance_minor) FROM accounts WHERE type = 'equity'), 0)    AS equity_minor";

pub fn profit_and_loss(conn: &Connection, from: &str, to: &str) -> rusqlite::Result<ProfitAndLoss> {
    let (income, expense): (i64, i64) =
        conn.query_row(PL_SQL, rusqlite::params![from, to], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(ProfitAndLoss { income_minor: income, expense_minor: expense, net_profit_minor: income - expense })
}

pub fn balance_sheet(conn: &Connection) -> rusqlite::Result<BalanceSheet> {
    conn.query_row(BS_SQL, [], |r| {
        Ok(BalanceSheet { assets_minor: r.get(0)?, liabilities_minor: r.get(1)?, equity_minor: r.get(2)? })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{open_seeded, ANCHOR, WIDGET};

    #[test]
    fn units_sold_by_month_for_widget() {
        let (conn, _) = open_seeded();
        let rows = units_sold_by_month(&conn, WIDGET, ANCHOR).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].month, "2026-07");
        assert_eq!(rows[0].units_sold, 60);
        assert_eq!(rows[0].revenue_minor, 54000);
    }

    #[test]
    fn gross_and_net_profit_over_period() {
        let (conn, _) = open_seeded();
        let g = gross_profit(&conn, ANCHOR).unwrap();
        assert_eq!(g.revenue_minor, 61500);
        assert_eq!(g.cogs_minor, 35000);
        assert_eq!(g.gross_profit_minor, 26500);

        let net = net_profit(&conn, ANCHOR).unwrap();
        assert_eq!(net, 23500);
    }

    #[test]
    fn inventory_age_per_lot_and_buckets() {
        use crate::test_support::{ANCHOR, WIDGET};
        let (conn, _hlc) = open_seeded();

        let lots = lot_ages(&conn, WIDGET, ANCHOR).unwrap();
        assert_eq!(lots.len(), 2);
        assert_eq!(lots[0].lot_id, "pur_1#lot0");
        assert_eq!(lots[0].qty_remaining, 50);
        assert_eq!(lots[0].age_days, 35);
        assert_eq!(lots[0].value_on_hand_minor, 25000);
        assert_eq!(lots[1].lot_id, "pur_2#lot0");
        assert_eq!(lots[1].age_days, 21);

        let buckets = aging_buckets(&conn, ANCHOR).unwrap();
        let find = |b: &str| buckets.iter().find(|x| x.bucket == b).cloned();
        let b0 = find("0-30d").expect("0-30d bucket");
        assert_eq!(b0.units, 65);
        assert_eq!(b0.value_minor, 45000);
        let b1 = find("31-90d").expect("31-90d bucket");
        assert_eq!(b1.units, 50);
        assert_eq!(b1.value_minor, 25000);
    }

    #[test]
    fn stock_on_hand_and_valuation() {
        use crate::test_support::{GADGET, WIDGET};
        let (conn, _hlc) = open_seeded();

        let soh = stock_on_hand(&conn).unwrap();
        let qty = |id: &str| soh.iter().find(|r| r.item_id == id).map(|r| r.qty).unwrap();
        assert_eq!(qty(WIDGET), 100);
        assert_eq!(qty(GADGET), 15);

        assert_eq!(inventory_valuation(&conn).unwrap(), 70000);
    }

    #[test]
    fn gross_margin_and_sellers() {
        use crate::test_support::{GADGET, WIDGET};
        let (conn, _hlc) = open_seeded();

        let margins = gross_margin_per_item(&conn).unwrap();
        let m = |id: &str| margins.iter().find(|r| r.item_id == id).cloned().unwrap();
        assert_eq!(m(WIDGET).margin_pct, Some(44.44));
        assert_eq!(m(GADGET).margin_pct, Some(33.33));

        let sellers = sellers_by_units(&conn).unwrap();
        assert_eq!(sellers[0].item_id, WIDGET);
        assert_eq!(sellers[0].units, 60);
        assert_eq!(sellers[0].profit_minor, 24000);
        assert_eq!(sellers[1].item_id, GADGET);
        assert_eq!(sellers[1].units, 5);
    }

    #[test]
    fn party_balances_and_aging() {
        use crate::test_support::{ANCHOR, CUST, SUPP};
        let (conn, _hlc) = open_seeded();

        let balances = party_balances(&conn).unwrap();
        let b = |id: &str| balances.iter().find(|r| r.party_id == id).cloned().unwrap();
        assert_eq!(b(CUST).receivable_minor, 5000);
        assert_eq!(b(SUPP).payable_minor, 100000);

        let ar = receivable_aging(&conn, ANCHOR).unwrap();
        assert_eq!(ar.len(), 1);
        assert_eq!(ar[0].invoice_id, "sale_1");
        assert_eq!(ar[0].outstanding_minor, 5000);
        assert_eq!(ar[0].bucket, "0-30d");

        let ap = payable_aging(&conn, ANCHOR).unwrap();
        assert_eq!(ap.len(), 3);
        assert_eq!(ap.iter().map(|r| r.outstanding_minor).sum::<i64>(), 100000);
    }

    #[test]
    fn return_rate_and_age_at_sale() {
        use crate::test_support::{GADGET, WIDGET};
        let (conn, _hlc) = open_seeded();

        let rr = return_rate_per_item(&conn).unwrap();
        let r = |id: &str| rr.iter().find(|x| x.item_id == id).cloned().unwrap();
        assert_eq!(r(WIDGET).sold_qty, 60);
        assert_eq!(r(WIDGET).returned_qty, 10);
        assert_eq!(r(WIDGET).return_rate, Some(0.1667));
        assert_eq!(r(GADGET).returned_qty, 0);
        assert_eq!(r(GADGET).return_rate, Some(0.0));

        let ages = age_at_sale(&conn).unwrap();
        let w = ages.iter().find(|x| x.item_id == WIDGET).unwrap();
        assert_eq!(w.age_at_sale_days, 31);
        assert_eq!(w.qty_taken, 60);
        let g = ages.iter().find(|x| x.item_id == GADGET).unwrap();
        assert_eq!(g.age_at_sale_days, 13);
    }

    #[test]
    fn profit_and_loss_and_balance_sheet() {
        let (conn, _hlc) = open_seeded();

        let pl = profit_and_loss(&conn, "2026-01-01", "2026-12-31").unwrap();
        assert_eq!(pl.income_minor, 52500);
        assert_eq!(pl.expense_minor, 33000);
        assert_eq!(pl.net_profit_minor, 19500);

        let bs = balance_sheet(&conn).unwrap();
        assert_eq!(bs.assets_minor, 119500);
        assert_eq!(bs.liabilities_minor, 100000);
        assert_eq!(bs.equity_minor, 0);
        assert_eq!(bs.assets_minor, bs.liabilities_minor + bs.equity_minor + pl.net_profit_minor);
    }

    /// Archiving a customer who still owes money used to strand the debt: the
    /// payments form filtered on `active`, so the only screen that could record
    /// the settlement stopped offering them, while aging kept showing the balance.
    /// They must stay payable, and be flagged as archived so the row reads as
    /// deliberate.
    #[test]
    fn an_archived_customer_with_an_open_invoice_is_still_payable() {
        use crate::commands::setup::handle_party_updated;
        use crate::commands::CommandContext;
        use crate::test_support::CUST;

        let (mut conn, mut hlc) = open_seeded();
        let owed: i64 = conn
            .query_row(
                "SELECT outstanding_minor FROM sales WHERE customer_id = ?1 AND outstanding_minor > 0",
                [CUST], |r| r.get(0))
            .expect("the fixture must leave this customer owing something");
        assert!(owed > 0);

        {
            let mut ctx = CommandContext {
                conn: &mut conn, hlc: &mut hlc, physical_now: 2000,
                device_id: "deviceA".into(), user_id: "owner-1".into(),
            };
            handle_party_updated(&mut ctx, CUST, serde_json::json!({ "active": false }))
                .expect("archiving with an open invoice is allowed — that is what archive is for");
        }

        let rows = payable_parties(&conn, "in").unwrap();
        let found = rows.iter().find(|p| p.id == CUST)
            .expect("an archived customer who still owes must remain payable");
        assert!(found.archived, "and must be flagged so the form can label the row");

        // Active parties sort first, so the archived exception never displaces the
        // ordinary choices at the top of the dropdown.
        let first_archived = rows.iter().position(|p| p.archived);
        let last_active = rows.iter().rposition(|p| !p.archived);
        if let (Some(fa), Some(la)) = (first_archived, last_active) {
            assert!(fa > la, "archived parties must sort after active ones");
        }
    }

    /// The other half of the rule: archiving is meant to remove a party from the
    /// forms. A party with nothing outstanding must actually disappear, or the
    /// exception above would swallow the feature.
    #[test]
    fn an_archived_party_with_nothing_outstanding_drops_out() {
        use crate::commands::setup::{handle_party_created, handle_party_updated};
        use crate::commands::CommandContext;

        let (mut conn, mut hlc) = open_seeded();
        {
            let mut ctx = CommandContext {
                conn: &mut conn, hlc: &mut hlc, physical_now: 2000,
                device_id: "deviceA".into(), user_id: "owner-1".into(),
            };
            handle_party_created(&mut ctx, "cust_quiet", "Quiet Co", "customer").unwrap();
            handle_party_updated(&mut ctx, "cust_quiet", serde_json::json!({ "active": false }))
                .unwrap();
        }
        let rows = payable_parties(&conn, "in").unwrap();
        assert!(
            !rows.iter().any(|p| p.id == "cust_quiet"),
            "archiving must still hide a party that owes nothing"
        );
        // While active, it was offered — so the assertion above is about the
        // archive flag and not about the kind filter.
        let all = payable_parties(&conn, "in").unwrap();
        assert!(all.iter().any(|p| p.id == crate::test_support::CUST));
    }

    /// The supplier mirror, checked in the same pass.
    #[test]
    fn an_archived_supplier_with_an_open_bill_is_still_payable() {
        use crate::commands::setup::handle_party_updated;
        use crate::commands::CommandContext;
        use crate::test_support::SUPP;

        let (mut conn, mut hlc) = open_seeded();
        let owed: i64 = conn
            .query_row(
                "SELECT outstanding_minor FROM purchases
                 WHERE supplier_id = ?1 AND outstanding_minor > 0 LIMIT 1",
                [SUPP], |r| r.get(0))
            .expect("the fixture must leave this supplier owed something");
        assert!(owed > 0);
        {
            let mut ctx = CommandContext {
                conn: &mut conn, hlc: &mut hlc, physical_now: 2000,
                device_id: "deviceA".into(), user_id: "owner-1".into(),
            };
            handle_party_updated(&mut ctx, SUPP, serde_json::json!({ "active": false })).unwrap();
        }
        let rows = payable_parties(&conn, "out").unwrap();
        let found = rows.iter().find(|p| p.id == SUPP)
            .expect("an archived supplier still owed money must remain payable");
        assert!(found.archived);
        // Direction must still partition: a supplier is not a customer.
        let money_in = payable_parties(&conn, "in").unwrap();
        assert!(!money_in.iter().any(|p| p.id == SUPP));
    }

    /// A party carrying only an unallocated balance has no open invoice, so the
    /// invoice clause alone would miss them — and an unallocated credit is exactly
    /// what `PaymentAllocated` needs a party in the form to draw down.
    #[test]
    fn an_archived_party_holding_an_unallocated_credit_is_still_payable() {
        use crate::commands::payment::handle_payment_received;
        use crate::commands::setup::{handle_party_created, handle_party_updated};
        use crate::commands::CommandContext;

        let (mut conn, mut hlc) = open_seeded();
        {
            let mut ctx = CommandContext {
                conn: &mut conn, hlc: &mut hlc, physical_now: 2000,
                device_id: "deviceA".into(), user_id: "owner-1".into(),
            };
            handle_party_created(&mut ctx, "cust_prepaid", "Prepaid Co", "customer").unwrap();
            // A pure prepayment: no invoice, just a credit held for them.
            handle_payment_received(&mut ctx, "pay_pre", "cust_prepaid", 2500, "2026-07-10", vec![])
                .unwrap();
            handle_party_updated(&mut ctx, "cust_prepaid", serde_json::json!({ "active": false }))
                .unwrap();
        }
        let rows = payable_parties(&conn, "in").unwrap();
        let found = rows.iter().find(|p| p.id == "cust_prepaid")
            .expect("an archived party still holding a credit must remain payable");
        assert!(found.archived);
    }

    #[test]
    fn payable_parties_rejects_an_unknown_direction() {
        let (conn, _) = open_seeded();
        assert!(payable_parties(&conn, "sideways").is_err());
    }
}
