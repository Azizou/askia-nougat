#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventCategory {
    LotCreating,
    LotConsuming,
    LotRestoring,
    AllocationBearing,
    Transactional,
}

pub fn categories_of(event_type: &str) -> Vec<EventCategory> {
    use EventCategory::*;
    match event_type {
        "UserRegistered" | "AccountOpened" | "ItemDefined" | "PartyCreated"
        | "UserUpdated" | "AccountUpdated" | "ItemUpdated" | "PartyUpdated" => vec![],

        "PurchaseRecorded" => vec![LotCreating, Transactional],
        "InventoryFound"   => vec![LotCreating, Transactional],
        "OpeningBalancesRecorded" => vec![LotCreating],

        "SaleRecorded"           => vec![LotConsuming, Transactional],
        "PurchaseReturnRecorded" => vec![LotConsuming, Transactional],
        "InventoryAdjusted"      => vec![LotConsuming, Transactional],

        "SaleReturnRecorded" => vec![LotRestoring, Transactional],

        "PaymentMade"     => vec![AllocationBearing, Transactional],
        "PaymentReceived" => vec![AllocationBearing, Transactional],
        "PaymentAllocated"=> vec![AllocationBearing, Transactional],

        "ExpenseRecorded"  => vec![Transactional],
        "TransferRecorded" => vec![Transactional],

        "TransactionReversed" => vec![],

        _ => vec![],
    }
}

pub fn is_transactional(event_type: &str) -> bool {
    categories_of(event_type).contains(&EventCategory::Transactional)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lot_creating_membership_matches_spec() {
        for t in ["PurchaseRecorded", "OpeningBalancesRecorded", "InventoryFound"] {
            assert!(categories_of(t).contains(&EventCategory::LotCreating), "{t}");
        }
        assert!(!categories_of("SaleRecorded").contains(&EventCategory::LotCreating));
    }

    #[test]
    fn lot_consuming_and_restoring_membership() {
        for t in ["SaleRecorded", "PurchaseReturnRecorded", "InventoryAdjusted"] {
            assert!(categories_of(t).contains(&EventCategory::LotConsuming), "{t}");
        }
        assert!(categories_of("SaleReturnRecorded").contains(&EventCategory::LotRestoring));
    }

    #[test]
    fn allocation_bearing_and_transactional_membership() {
        for t in ["PaymentMade", "PaymentReceived", "PaymentAllocated"] {
            assert!(categories_of(t).contains(&EventCategory::AllocationBearing), "{t}");
        }
        assert!(categories_of("SaleRecorded").contains(&EventCategory::Transactional));
        assert!(!categories_of("ItemDefined").contains(&EventCategory::Transactional));
        assert!(!categories_of("OpeningBalancesRecorded").contains(&EventCategory::Transactional));
        assert!(categories_of("PaymentAllocated").contains(&EventCategory::Transactional));
    }
}
