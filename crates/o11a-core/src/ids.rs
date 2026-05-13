use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Shared counter for `S`-prefixed topic IDs. One counter backs every
/// entity kind in the security-model spec family — `FeatureTopic`,
/// `RequirementTopic`, `BehaviorTopic`, and `CharacteristicTopic`. The
/// kind distinction lives on the `TopicMetadata` variant; the numeric
/// suffix is allocated from this single sequence so that no two entities
/// in the family ever collide on `i32`.
static NEXT_SPEC_ID: AtomicI32 = AtomicI32::new(1);
static NEXT_FUNCTIONAL_PROPERTY_ID: AtomicI32 = AtomicI32::new(1);
static NEXT_ADVERSARIAL_PROPERTY_ID: AtomicI32 = AtomicI32::new(1);

/// Allocate an `S`-prefixed topic ID. Shared across `FeatureTopic`,
/// `RequirementTopic`, `BehaviorTopic`, and `CharacteristicTopic` — all
/// four entity kinds in the security-model spec family.
pub fn allocate_spec_id() -> i32 {
  NEXT_SPEC_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn reseed_spec_id(max_loaded: i32) {
  NEXT_SPEC_ID.store(max_loaded + 1, Ordering::Relaxed);
}

/// Allocate a `P`-prefixed topic ID. Shared across `FunctionalSemanticTopic`,
/// `FunctionalPurposeTopic`, and `PlacementRationaleTopic` — all three are
/// "functional properties" in the security model.
pub fn allocate_functional_property_id() -> i32 {
  NEXT_FUNCTIONAL_PROPERTY_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn reseed_functional_property_id(max_loaded: i32) {
  NEXT_FUNCTIONAL_PROPERTY_ID.store(max_loaded + 1, Ordering::Relaxed);
}

/// Allocate an `A`-prefixed topic ID. Shared across `ConditionTopic`,
/// `ThreatTopic`, and `InvariantTopic` — all "adversarial properties"
/// in the security model.
pub fn allocate_adversarial_property_id() -> i32 {
  NEXT_ADVERSARIAL_PROPERTY_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn reseed_adversarial_property_id(max_loaded: i32) {
  NEXT_ADVERSARIAL_PROPERTY_ID.store(max_loaded + 1, Ordering::Relaxed);
}

/// UTC ISO-8601 timestamp with seconds precision (`YYYY-MM-DDTHH:MM:SSZ`).
/// Uses the civil-from-days algorithm from Howard Hinnant's date library
/// (public-domain) so we stay dependency-free.
pub fn now_iso8601() -> String {
  let secs = SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|d| d.as_secs() as i64)
    .unwrap_or(0);
  let z = secs.div_euclid(86_400) + 719_468;
  let era = z.div_euclid(146_097);
  let doe = (z - era * 146_097) as u64;
  let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
  let y = (yoe as i64) + era * 400;
  let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
  let mp = (5 * doy + 2) / 153;
  let d = doy - (153 * mp + 2) / 5 + 1;
  let m = if mp < 10 { mp + 3 } else { mp - 9 };
  let y = if m <= 2 { y + 1 } else { y };

  let rem = secs.rem_euclid(86_400) as u32;
  let h = rem / 3600;
  let mi = (rem % 3600) / 60;
  let s = rem % 60;

  format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, h, mi, s)
}

/// Per-counter serialization locks for tests that mutate the process-wide
/// `NEXT_*_ID` atomics. Exposed crate-internally so integration tests in
/// other modules (e.g. `report::apply_report`) can synchronize against the
/// counter tests in this module — without sharing the lock, parallel test
/// runs across modules would race on the global state.
#[cfg(test)]
pub(crate) static SPEC_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(test)]
pub(crate) static FUNCTIONAL_PROPERTY_LOCK: std::sync::Mutex<()> =
  std::sync::Mutex::new(());
#[cfg(test)]
pub(crate) static ADVERSARIAL_PROPERTY_LOCK: std::sync::Mutex<()> =
  std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn spec_allocation_is_monotonic() {
    let _guard = SPEC_LOCK.lock().unwrap();
    reseed_spec_id(0);
    let a = allocate_spec_id();
    let b = allocate_spec_id();
    let c = allocate_spec_id();
    assert_eq!(a, 1);
    assert_eq!(b, 2);
    assert_eq!(c, 3);
  }

  #[test]
  fn spec_reseed_advances_past_max() {
    let _guard = SPEC_LOCK.lock().unwrap();
    reseed_spec_id(42);
    assert_eq!(allocate_spec_id(), 43);
    assert_eq!(allocate_spec_id(), 44);
  }

  #[test]
  fn spec_reseed_with_lower_value_still_stores() {
    let _guard = SPEC_LOCK.lock().unwrap();
    reseed_spec_id(100);
    assert_eq!(allocate_spec_id(), 101);
    reseed_spec_id(5);
    assert_eq!(allocate_spec_id(), 6);
  }

  #[test]
  fn functional_property_allocation_is_monotonic() {
    let _guard = FUNCTIONAL_PROPERTY_LOCK.lock().unwrap();
    reseed_functional_property_id(0);
    assert_eq!(allocate_functional_property_id(), 1);
    assert_eq!(allocate_functional_property_id(), 2);
  }

  #[test]
  fn functional_property_reseed_advances_past_max() {
    let _guard = FUNCTIONAL_PROPERTY_LOCK.lock().unwrap();
    reseed_functional_property_id(20);
    assert_eq!(allocate_functional_property_id(), 21);
  }

  #[test]
  fn functional_property_reseed_with_lower_value_still_stores() {
    let _guard = FUNCTIONAL_PROPERTY_LOCK.lock().unwrap();
    reseed_functional_property_id(75);
    reseed_functional_property_id(3);
    assert_eq!(allocate_functional_property_id(), 4);
  }

  #[test]
  fn adversarial_property_allocation_is_monotonic() {
    let _guard = ADVERSARIAL_PROPERTY_LOCK.lock().unwrap();
    reseed_adversarial_property_id(0);
    assert_eq!(allocate_adversarial_property_id(), 1);
    assert_eq!(allocate_adversarial_property_id(), 2);
  }

  #[test]
  fn adversarial_property_reseed_advances_past_max() {
    let _guard = ADVERSARIAL_PROPERTY_LOCK.lock().unwrap();
    reseed_adversarial_property_id(20);
    assert_eq!(allocate_adversarial_property_id(), 21);
  }

  #[test]
  fn adversarial_property_reseed_with_lower_value_still_stores() {
    let _guard = ADVERSARIAL_PROPERTY_LOCK.lock().unwrap();
    reseed_adversarial_property_id(75);
    reseed_adversarial_property_id(3);
    assert_eq!(allocate_adversarial_property_id(), 4);
  }
}
