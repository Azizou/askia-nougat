pub mod db;
pub mod hlc;
pub mod events;
pub mod genesis;
pub mod projectors;
pub mod commands;
pub mod reconciliation;
pub mod queries;
pub mod settings;
#[cfg(test)]
mod test_support;

pub use db::{apply_schema, open_in_memory, open_in_memory_with_schema};
pub use hlc::{rehydrate_from_log, Hlc};
pub use events::{append_event, missing_seqs, read_events, LedgerEvent};
pub use genesis::{run_genesis, SYSTEM_USER_ID};
pub use projectors::{apply_event, rebuild};
pub use commands::{CommandContext, CommandError};
pub use commands::setup::{
    handle_account_opened, handle_account_updated, handle_item_defined, handle_item_updated,
    handle_party_created, handle_party_updated, handle_user_registered, handle_user_updated,
};
pub use commands::purchase::{handle_purchase_recorded, handle_purchase_return_recorded, PurchaseLineInput, PurchaseReturnLineInput};
pub use commands::sale::{handle_sale_recorded, handle_sale_return_recorded, SaleLineInput, SaleReturnItemInput};
pub use commands::payment::{handle_payment_allocated, handle_payment_made, handle_payment_received, AllocInput};
pub use commands::movement::{handle_expense_recorded, handle_inventory_adjusted,
    handle_inventory_found, handle_transfer_recorded, AdjustLineInput, FoundLineInput};
pub use commands::reversal::handle_transaction_reversed;
pub use reconciliation::{all_passed, run_all_checks, Check, CheckOutcome};
pub use queries::{
    age_at_sale, aging_buckets, balance_sheet, gross_margin_per_item, gross_profit,
    inventory_valuation, lot_ages, net_profit, party_balances, payable_aging, profit_and_loss,
    receivable_aging, return_rate_per_item, sellers_by_units, stock_on_hand, units_sold_by_month,
    AgeAtSale, AgingBucket, AgingInvoice, BalanceSheet, GrossProfit, ItemMargin, LotAge,
    MonthlyUnits, PartyBalance, ProfitAndLoss, ReturnRate, SellerRow, StockOnHand,
};
pub use settings::{get_settings, set_setting, SETTING_KEYS};
