//! Section documents and their merge.
//!
//! A section (accounts, prefs, templates…) is a map of records. Each record
//! holds last-write-wins fields and add/remove sets, plus an optional
//! tombstone. Merging is commutative, associative and idempotent, so two
//! devices that edited offline converge whatever order their writes reach the
//! server in. The server never merges: a client that gets `412` merges the
//! current document into its own and writes again.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Tombstones (record deletes and set removals) are dropped after this long.
/// A device offline for longer resets the section from the server.
pub const TOMBSTONE_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// Record id for sections that hold a single object (`prefs`, `shortcuts`).
pub const SINGLETON: &str = "_";

/// When and where a write happened. Ordered by time, then device id, so two
/// writes in the same millisecond still have a fixed winner.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Stamp {
    pub ms: i64,
    pub device: String,
}

impl Stamp {
    pub fn new(ms: i64, device: impl Into<String>) -> Self {
        Stamp { ms, device: device.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lww {
    pub value: Value,
    pub at: Stamp,
}

/// Add-wins-if-later set: an item is present when its latest add is newer than
/// its latest remove.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrSet {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub adds: BTreeMap<String, Stamp>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub removes: BTreeMap<String, Stamp>,
}

impl OrSet {
    pub fn contains(&self, item: &str) -> bool {
        match (self.adds.get(item), self.removes.get(item)) {
            (Some(a), Some(r)) => a > r,
            (Some(_), None) => true,
            _ => false,
        }
    }

    /// Present items, sorted.
    pub fn items(&self) -> Vec<String> {
        self.adds.keys().filter(|k| self.contains(k)).cloned().collect()
    }

    /// Use [`Record::assign_set`] on records: it respects the tombstone.
    pub fn add(&mut self, item: impl Into<String>, at: Stamp) {
        keep_max(&mut self.adds, item.into(), at);
    }

    pub fn remove(&mut self, item: impl Into<String>, at: Stamp) {
        keep_max(&mut self.removes, item.into(), at);
    }

    /// Make the set equal `want`: adds what's missing, removes what's extra.
    pub fn assign(&mut self, want: &[String], at: &Stamp) {
        let current = self.items();
        for item in want {
            if !self.contains(item) {
                self.add(item.clone(), at.clone());
            }
        }
        for item in current {
            if !want.contains(&item) {
                self.remove(item, at.clone());
            }
        }
    }

    pub fn merge(&mut self, other: &OrSet) {
        for (k, v) in &other.adds {
            keep_max(&mut self.adds, k.clone(), v.clone());
        }
        for (k, v) in &other.removes {
            keep_max(&mut self.removes, k.clone(), v.clone());
        }
    }

    fn gc(&mut self, before_ms: i64) {
        let adds = &self.adds;
        // A removal that's old and not overtaken can go together with its add.
        let dead: Vec<String> = self
            .removes
            .iter()
            .filter(|(k, r)| r.ms < before_ms && adds.get(*k).is_none_or(|a| a < *r))
            .map(|(k, _)| k.clone())
            .collect();
        for k in dead {
            self.removes.remove(&k);
            self.adds.remove(&k);
        }
        // A removal overtaken by a later add is no longer needed once old.
        let adds = &self.adds;
        let stale: Vec<String> = self
            .removes
            .iter()
            .filter(|(k, r)| r.ms < before_ms && adds.get(*k).is_some_and(|a| a > *r))
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            self.removes.remove(&k);
        }
    }
}

fn keep_max(map: &mut BTreeMap<String, Stamp>, key: String, at: Stamp) {
    match map.get(&key) {
        Some(cur) if *cur >= at => {}
        _ => {
            map.insert(key, at);
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, Lww>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sets: BTreeMap<String, OrSet>,
    /// Set when the record was deleted. Edits stamped later bring it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted: Option<Stamp>,
}

impl Record {
    /// True when no live edit is newer than the tombstone.
    pub fn is_deleted(&self) -> bool {
        match &self.deleted {
            None => false,
            Some(d) => self.latest().is_none_or(|l| l <= d),
        }
    }

    /// The newest stamp among fields and set operations.
    pub fn latest(&self) -> Option<&Stamp> {
        let f = self.fields.values().map(|l| &l.at);
        let s = self.sets.values().flat_map(|s| s.adds.values().chain(s.removes.values()));
        f.chain(s).max()
    }

    pub fn get(&self, field: &str) -> Option<&Value> {
        self.fields.get(field).map(|l| &l.value)
    }

    /// Sets `field` if the value changed (an unchanged value keeps its stamp,
    /// so re-saving a whole form doesn't win over another device's edit).
    pub fn set(&mut self, field: &str, value: Value, at: &Stamp) -> bool {
        if self.fields.get(field).is_some_and(|l| l.value == value) {
            return false;
        }
        let newer = self.fields.get(field).is_none_or(|l| l.at < *at)
            && self.deleted.as_ref().is_none_or(|d| d < at);
        if newer {
            self.fields.insert(field.into(), Lww { value, at: at.clone() });
        }
        newer
    }

    pub fn set_items(&self, field: &str) -> Vec<String> {
        self.sets.get(field).map(OrSet::items).unwrap_or_default()
    }

    pub fn assign_set(&mut self, field: &str, items: &[String], at: &Stamp) {
        self.sets.entry(field.into()).or_default().assign(items, at);
        self.prune();
    }

    pub fn merge(&mut self, other: &Record) {
        for (k, v) in &other.fields {
            match self.fields.get(k) {
                Some(cur) if cur.at > v.at => {}
                // Same stamp, different value can only come from a buggy
                // writer; pick deterministically so replicas still agree.
                Some(cur) if cur.at == v.at && value_key(&cur.value) >= value_key(&v.value) => {}
                _ => {
                    self.fields.insert(k.clone(), v.clone());
                }
            }
        }
        for (k, v) in &other.sets {
            self.sets.entry(k.clone()).or_default().merge(v);
        }
        self.deleted = match (self.deleted.take(), other.deleted.clone()) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        self.prune();
    }

    /// Drops everything the tombstone covers, so a record brought back by a
    /// later edit starts clean on every replica.
    fn prune(&mut self) {
        let Some(d) = self.deleted.clone() else { return };
        self.fields.retain(|_, l| l.at > d);
        for s in self.sets.values_mut() {
            s.adds.retain(|_, a| *a > d);
            s.removes.retain(|_, a| *a > d);
        }
        self.sets.retain(|_, s| !s.adds.is_empty() || !s.removes.is_empty());
    }
}

fn value_key(v: &Value) -> String {
    v.to_string()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SectionDoc {
    #[serde(default)]
    pub records: BTreeMap<String, Record>,
}

impl SectionDoc {
    pub fn merge(&mut self, other: &SectionDoc) {
        for (id, rec) in &other.records {
            self.records.entry(id.clone()).or_default().merge(rec);
        }
    }

    pub fn merged(mut self, other: &SectionDoc) -> SectionDoc {
        self.merge(other);
        self
    }

    /// Live records (not deleted), by id.
    pub fn live(&self) -> impl Iterator<Item = (&String, &Record)> {
        self.records.iter().filter(|(_, r)| !r.is_deleted())
    }

    pub fn record(&self, id: &str) -> Option<&Record> {
        self.records.get(id).filter(|r| !r.is_deleted())
    }

    pub fn record_mut(&mut self, id: &str) -> &mut Record {
        self.records.entry(id.into()).or_default()
    }

    /// Deletes a record (tombstone; the fields are dropped so a deleted
    /// credential doesn't linger on the server).
    pub fn delete(&mut self, id: &str, at: &Stamp) {
        let tomb = Record { deleted: Some(at.clone()), ..Default::default() };
        self.records.entry(id.into()).or_default().merge(&tomb);
    }

    /// Drops tombstones older than [`TOMBSTONE_TTL_MS`] before `now_ms`.
    pub fn gc(&mut self, now_ms: i64) {
        let before = now_ms - TOMBSTONE_TTL_MS;
        self.records.retain(|_, r| !(r.is_deleted() && r.deleted.as_ref().is_some_and(|d| d.ms < before)));
        for r in self.records.values_mut() {
            for s in r.sets.values_mut() {
                s.gc(before);
            }
            r.sets.retain(|_, s| !s.adds.is_empty() || !s.removes.is_empty());
        }
    }

    /// The oldest write still represented: a device whose last sync is older
    /// than `now - TTL` may have missed a dropped tombstone and must reset.
    pub fn needs_reset(last_sync_ms: i64, now_ms: i64) -> bool {
        last_sync_ms > 0 && now_ms - last_sync_ms > TOMBSTONE_TTL_MS
    }
}

/// Monotonic stamp source for one device: never goes backwards, and never
/// behind the latest server time seen, so a device with a slow clock can't lose
/// every edit to devices with correct clocks.
#[derive(Debug, Clone)]
pub struct Clock {
    pub device: String,
    last: i64,
}

impl Clock {
    pub fn new(device: impl Into<String>) -> Self {
        Clock { device: device.into(), last: 0 }
    }

    /// Note a time seen elsewhere (server `Date`, a merged stamp).
    pub fn observe(&mut self, ms: i64) {
        self.last = self.last.max(ms);
    }

    pub fn stamp(&mut self, now_ms: i64) -> Stamp {
        self.last = now_ms.max(self.last + 1);
        Stamp::new(self.last, self.device.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;

    fn st(ms: i64, d: &str) -> Stamp {
        Stamp::new(ms, d)
    }

    #[test]
    fn different_fields_both_survive() {
        let mut a = SectionDoc::default();
        a.record_mut("acc_1").set("color", json!("#f00"), &st(10, "A"));
        let mut b = SectionDoc::default();
        b.record_mut("acc_1").set("displayName", json!("Work"), &st(11, "B"));
        let m = a.clone().merged(&b);
        let r = m.record("acc_1").unwrap();
        assert_eq!(r.get("color"), Some(&json!("#f00")));
        assert_eq!(r.get("displayName"), Some(&json!("Work")));
        assert_eq!(m, b.merged(&a));
    }

    #[test]
    fn same_field_latest_wins_with_device_tiebreak() {
        let mut a = SectionDoc::default();
        a.record_mut("_").set("theme", json!("dark"), &st(10, "A"));
        let mut b = SectionDoc::default();
        b.record_mut("_").set("theme", json!("light"), &st(10, "B"));
        let m = a.clone().merged(&b);
        assert_eq!(m.record("_").unwrap().get("theme"), Some(&json!("light")));
        assert_eq!(m, b.merged(&a));
    }

    #[test]
    fn delete_beats_older_offline_edit_and_later_edit_resurrects() {
        let mut server = SectionDoc::default();
        server.record_mut("acc_1").set("email", json!("a@x"), &st(1, "A"));
        let mut offline = server.clone();
        server.delete("acc_1", &st(20, "A"));
        offline.record_mut("acc_1").set("color", json!("#0f0"), &st(15, "B"));
        let m = server.clone().merged(&offline);
        assert!(m.record("acc_1").is_none(), "removed account stays removed");
        assert_eq!(m, offline.clone().merged(&server));
        offline.record_mut("acc_1").set("color", json!("#00f"), &st(30, "B"));
        assert!(server.merged(&offline).record("acc_1").is_some());
    }

    #[test]
    fn set_union_with_tombstones() {
        let mut base = SectionDoc::default();
        base.record_mut("acc_1").assign_set("autoBcc", &["a@x".into(), "b@x".into()], &st(1, "A"));
        let mut a = base.clone();
        let mut b = base.clone();
        a.record_mut("acc_1").assign_set("autoBcc", &["a@x".into()], &st(5, "A"));
        b.record_mut("acc_1").assign_set("autoBcc", &["a@x".into(), "b@x".into(), "c@x".into()], &st(6, "B"));
        let m = a.clone().merged(&b);
        assert_eq!(m.record("acc_1").unwrap().set_items("autoBcc"), vec!["a@x", "c@x"]);
        assert_eq!(m, b.merged(&a));
    }

    #[test]
    fn unchanged_value_keeps_its_stamp() {
        let mut d = SectionDoc::default();
        d.record_mut("_").set("theme", json!("dark"), &st(5, "A"));
        assert!(!d.record_mut("_").set("theme", json!("dark"), &st(9, "A")));
        assert_eq!(d.record("_").unwrap().fields["theme"].at, st(5, "A"));
    }

    #[test]
    fn gc_drops_old_tombstones_only() {
        let mut d = SectionDoc::default();
        d.record_mut("old").set("x", json!(1), &st(1, "A"));
        d.delete("old", &st(2, "A"));
        d.record_mut("new").set("x", json!(1), &st(1, "A"));
        d.delete("new", &st(TOMBSTONE_TTL_MS + 50, "A"));
        d.record_mut("keep").assign_set("s", &["a".into()], &st(1, "A"));
        d.record_mut("keep").assign_set("s", &[], &st(2, "A"));
        d.gc(TOMBSTONE_TTL_MS + 100);
        assert!(!d.records.contains_key("old"));
        assert!(d.records.contains_key("new"));
        assert!(d.records["keep"].sets.is_empty());
    }

    #[test]
    fn delete_drops_secret_fields() {
        let mut d = SectionDoc::default();
        d.record_mut("_").set("refreshToken", json!("secret"), &st(1, "A"));
        d.delete("_", &st(2, "A"));
        assert!(d.records["_"].fields.is_empty());
    }

    #[test]
    fn clock_is_monotonic_and_respects_observed_time() {
        let mut c = Clock::new("A");
        c.observe(1_000);
        assert_eq!(c.stamp(10).ms, 1_001);
        assert_eq!(c.stamp(5_000).ms, 5_000);
        assert_eq!(c.stamp(4_000).ms, 5_001);
    }

    // ---- property tests: merge is a join-semilattice ----

    fn arb_stamp() -> impl Strategy<Value = Stamp> {
        (0i64..20, prop_oneof![Just("A"), Just("B"), Just("C")]).prop_map(|(ms, d)| st(ms, d))
    }

    #[derive(Debug, Clone)]
    enum Op {
        Set(String, String, i64, Stamp),
        Add(String, String, Stamp),
        Remove(String, String, Stamp),
        Delete(String, Stamp),
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        let rec = prop_oneof![Just("r1".to_string()), Just("r2".to_string())];
        let field = prop_oneof![Just("f".to_string()), Just("g".to_string())];
        prop_oneof![
            (rec.clone(), field, 0i64..3, arb_stamp()).prop_map(|(r, f, v, s)| Op::Set(r, f, v, s)),
            (rec.clone(), prop_oneof![Just("x".to_string()), Just("y".to_string())], arb_stamp())
                .prop_map(|(r, i, s)| Op::Add(r, i, s)),
            (rec.clone(), prop_oneof![Just("x".to_string()), Just("y".to_string())], arb_stamp())
                .prop_map(|(r, i, s)| Op::Remove(r, i, s)),
            (rec, arb_stamp()).prop_map(|(r, s)| Op::Delete(r, s)),
        ]
    }

    fn apply(ops: &[Op]) -> SectionDoc {
        let mut d = SectionDoc::default();
        for op in ops {
            match op {
                Op::Set(r, f, v, s) => {
                    let rec = d.record_mut(r);
                    // Writers only ever move forward, like Record::set; model
                    // raw stamped writes through merge to cover equal stamps.
                    let mut one = Record::default();
                    one.fields.insert(f.clone(), Lww { value: json!(v), at: s.clone() });
                    rec.merge(&one);
                }
                Op::Add(r, i, s) | Op::Remove(r, i, s) => {
                    let mut set = OrSet::default();
                    if matches!(op, Op::Add(..)) {
                        set.add(i.clone(), s.clone());
                    } else {
                        set.remove(i.clone(), s.clone());
                    }
                    let mut one = Record::default();
                    one.sets.insert("s".into(), set);
                    d.record_mut(r).merge(&one);
                }
                Op::Delete(r, s) => d.delete(r, s),
            }
        }
        d
    }

    /// What a reader sees: live records and their values / set items.
    fn view(d: &SectionDoc) -> BTreeMap<String, (BTreeMap<String, Value>, Vec<String>)> {
        d.live()
            .map(|(k, r)| {
                let f = r.fields.iter().map(|(k, l)| (k.clone(), l.value.clone())).collect();
                (k.clone(), (f, r.set_items("s")))
            })
            .collect()
    }

    proptest! {
        #[test]
        fn merge_commutative(a in prop::collection::vec(arb_op(), 0..12), b in prop::collection::vec(arb_op(), 0..12)) {
            let (da, db) = (apply(&a), apply(&b));
            prop_assert_eq!(da.clone().merged(&db), db.merged(&da));
        }

        #[test]
        fn merge_associative(a in prop::collection::vec(arb_op(), 0..8), b in prop::collection::vec(arb_op(), 0..8), c in prop::collection::vec(arb_op(), 0..8)) {
            let (da, db, dc) = (apply(&a), apply(&b), apply(&c));
            prop_assert_eq!(da.clone().merged(&db).merged(&dc), da.merged(&db.merged(&dc)));
        }

        #[test]
        fn merge_idempotent(a in prop::collection::vec(arb_op(), 0..12)) {
            let da = apply(&a);
            prop_assert_eq!(da.clone().merged(&da), da);
        }

        #[test]
        fn replicas_converge(ops in prop::collection::vec(arb_op(), 0..16), split in 0usize..16) {
            // Two devices see the same ops split differently, then sync both ways.
            let k = split.min(ops.len());
            let (a, b) = (apply(&ops[..k]), apply(&ops[k..]));
            let all = apply(&ops);
            prop_assert_eq!(view(&a.clone().merged(&b)), view(&all));
        }
    }
}
