use rusqlite::Connection;

/// A Hybrid Logical Clock. Produces lexically-sortable timestamp strings of the
/// form `{physical:015}:{counter:06}:{device_id}`.
pub struct Hlc {
    device_id: String,
    last_physical: u64,
    counter: u64,
}

impl Hlc {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self { device_id: device_id.into(), last_physical: 0, counter: 0 }
    }

    /// Advance the clock for a locally-authored event and return the stamp.
    pub fn tick(&mut self, physical_now: u64) -> String {
        if physical_now > self.last_physical {
            self.last_physical = physical_now;
            self.counter = 0;
        } else {
            self.counter += 1;
        }
        self.encode()
    }

    fn encode(&self) -> String {
        format!("{:015}:{:06}:{}", self.last_physical, self.counter, self.device_id)
    }

    /// Merge a remote event's stamp, keeping this clock ahead of it.
    pub fn observe(&mut self, remote_hlc: &str, physical_now: u64) {
        let (r_phys, r_ctr) = Self::decode(remote_hlc);
        let max_phys = physical_now.max(self.last_physical).max(r_phys);
        if max_phys == self.last_physical && max_phys == r_phys {
            self.counter = self.counter.max(r_ctr) + 1;
        } else if max_phys == self.last_physical {
            self.counter += 1;
        } else if max_phys == r_phys {
            self.counter = r_ctr + 1;
        } else {
            self.counter = 0;
        }
        self.last_physical = max_phys;
    }

    fn decode(hlc: &str) -> (u64, u64) {
        let mut parts = hlc.splitn(3, ':');
        let phys = parts.next().unwrap_or("0").parse().unwrap_or(0);
        let ctr = parts.next().unwrap_or("0").parse().unwrap_or(0);
        (phys, ctr)
    }
}

/// Seed an in-memory clock from the persisted log on open.
pub fn rehydrate_from_log(
    conn: &Connection,
    hlc: &mut Hlc,
    physical_now: u64,
) -> rusqlite::Result<()> {
    let max_hlc: Option<String> =
        conn.query_row("SELECT MAX(hlc) FROM events", [], |r| r.get(0))?;
    if let Some(stamp) = max_hlc {
        hlc.observe(&stamp, physical_now);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_is_strictly_increasing_lexically() {
        let mut hlc = Hlc::new("deviceA");
        let a = hlc.tick(1000);
        let b = hlc.tick(1000);
        let c = hlc.tick(1000);
        assert!(a < b, "{a} should sort before {b}");
        assert!(b < c, "{b} should sort before {c}");
    }

    #[test]
    fn tick_resets_counter_when_physical_advances() {
        let mut hlc = Hlc::new("deviceA");
        let _ = hlc.tick(1000);
        let _ = hlc.tick(1000);
        let later = hlc.tick(2000);
        assert!(later.starts_with("000000000002000:000000:"), "got {later}");
    }

    #[test]
    fn observe_pulls_clock_ahead_of_remote() {
        let mut local = Hlc::new("deviceA");
        local.observe("000000000005000:000003:deviceB", 1000);
        let next = local.tick(1000);
        assert!(
            next.as_str() > "000000000005000:000003:deviceB",
            "local tick {next} should sort after observed remote"
        );
    }

    #[test]
    fn rehydrate_from_log_orders_after_last_persisted_stamp() {
        use crate::db::open_in_memory_with_schema;
        use crate::events::append_event;
        use serde_json::json;

        let conn = open_in_memory_with_schema().unwrap();

        let mut hlc1 = Hlc::new("deviceA");
        let e1 = append_event(&conn, &mut hlc1, 5000, "deviceA", "u", "A", &json!({})).unwrap();
        drop(hlc1);

        let mut hlc2 = Hlc::new("deviceA");
        rehydrate_from_log(&conn, &mut hlc2, 1000).unwrap();
        let e2 = append_event(&conn, &mut hlc2, 1000, "deviceA", "u", "B", &json!({})).unwrap();

        assert!(
            e2.hlc > e1.hlc,
            "post-restart event {} must sort after pre-restart {}",
            e2.hlc, e1.hlc
        );
    }
}
