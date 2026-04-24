use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_FEATURE_ID: AtomicI32 = AtomicI32::new(1);
static NEXT_REQUIREMENT_ID: AtomicI32 = AtomicI32::new(1);
static NEXT_BEHAVIOR_ID: AtomicI32 = AtomicI32::new(1);
static NEXT_FUNCTIONAL_SEMANTIC_ID: AtomicI32 = AtomicI32::new(1);

pub fn allocate_feature_id() -> i32 {
  NEXT_FEATURE_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn reseed_feature_id(max_loaded: i32) {
  NEXT_FEATURE_ID.store(max_loaded + 1, Ordering::Relaxed);
}

pub fn allocate_requirement_id() -> i32 {
  NEXT_REQUIREMENT_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn reseed_requirement_id(max_loaded: i32) {
  NEXT_REQUIREMENT_ID.store(max_loaded + 1, Ordering::Relaxed);
}

pub fn allocate_behavior_id() -> i32 {
  NEXT_BEHAVIOR_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn reseed_behavior_id(max_loaded: i32) {
  NEXT_BEHAVIOR_ID.store(max_loaded + 1, Ordering::Relaxed);
}

pub fn allocate_functional_semantic_id() -> i32 {
  NEXT_FUNCTIONAL_SEMANTIC_ID.fetch_add(1, Ordering::Relaxed)
}

pub fn reseed_functional_semantic_id(max_loaded: i32) {
  NEXT_FUNCTIONAL_SEMANTIC_ID.store(max_loaded + 1, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
  use super::*;
  use std::sync::Mutex;

  // Each counter has a mutex to serialize the tests that touch it, since
  // the counters are process-wide state.
  static FEATURE_LOCK: Mutex<()> = Mutex::new(());
  static REQUIREMENT_LOCK: Mutex<()> = Mutex::new(());
  static BEHAVIOR_LOCK: Mutex<()> = Mutex::new(());
  static FUNCTIONAL_SEMANTIC_LOCK: Mutex<()> = Mutex::new(());

  #[test]
  fn feature_allocation_is_monotonic() {
    let _guard = FEATURE_LOCK.lock().unwrap();
    reseed_feature_id(0);
    let a = allocate_feature_id();
    let b = allocate_feature_id();
    let c = allocate_feature_id();
    assert_eq!(a, 1);
    assert_eq!(b, 2);
    assert_eq!(c, 3);
  }

  #[test]
  fn feature_reseed_advances_past_max() {
    let _guard = FEATURE_LOCK.lock().unwrap();
    reseed_feature_id(42);
    assert_eq!(allocate_feature_id(), 43);
    assert_eq!(allocate_feature_id(), 44);
  }

  #[test]
  fn feature_reseed_with_lower_value_still_stores() {
    let _guard = FEATURE_LOCK.lock().unwrap();
    reseed_feature_id(100);
    assert_eq!(allocate_feature_id(), 101);
    reseed_feature_id(5);
    assert_eq!(allocate_feature_id(), 6);
  }

  #[test]
  fn requirement_allocation_is_monotonic() {
    let _guard = REQUIREMENT_LOCK.lock().unwrap();
    reseed_requirement_id(0);
    assert_eq!(allocate_requirement_id(), 1);
    assert_eq!(allocate_requirement_id(), 2);
  }

  #[test]
  fn requirement_reseed_advances_past_max() {
    let _guard = REQUIREMENT_LOCK.lock().unwrap();
    reseed_requirement_id(10);
    assert_eq!(allocate_requirement_id(), 11);
  }

  #[test]
  fn requirement_reseed_with_lower_value_still_stores() {
    let _guard = REQUIREMENT_LOCK.lock().unwrap();
    reseed_requirement_id(50);
    reseed_requirement_id(2);
    assert_eq!(allocate_requirement_id(), 3);
  }

  #[test]
  fn behavior_allocation_is_monotonic() {
    let _guard = BEHAVIOR_LOCK.lock().unwrap();
    reseed_behavior_id(0);
    assert_eq!(allocate_behavior_id(), 1);
    assert_eq!(allocate_behavior_id(), 2);
  }

  #[test]
  fn behavior_reseed_advances_past_max() {
    let _guard = BEHAVIOR_LOCK.lock().unwrap();
    reseed_behavior_id(7);
    assert_eq!(allocate_behavior_id(), 8);
  }

  #[test]
  fn behavior_reseed_with_lower_value_still_stores() {
    let _guard = BEHAVIOR_LOCK.lock().unwrap();
    reseed_behavior_id(99);
    reseed_behavior_id(1);
    assert_eq!(allocate_behavior_id(), 2);
  }

  #[test]
  fn functional_semantic_allocation_is_monotonic() {
    let _guard = FUNCTIONAL_SEMANTIC_LOCK.lock().unwrap();
    reseed_functional_semantic_id(0);
    assert_eq!(allocate_functional_semantic_id(), 1);
    assert_eq!(allocate_functional_semantic_id(), 2);
  }

  #[test]
  fn functional_semantic_reseed_advances_past_max() {
    let _guard = FUNCTIONAL_SEMANTIC_LOCK.lock().unwrap();
    reseed_functional_semantic_id(20);
    assert_eq!(allocate_functional_semantic_id(), 21);
  }

  #[test]
  fn functional_semantic_reseed_with_lower_value_still_stores() {
    let _guard = FUNCTIONAL_SEMANTIC_LOCK.lock().unwrap();
    reseed_functional_semantic_id(75);
    reseed_functional_semantic_id(3);
    assert_eq!(allocate_functional_semantic_id(), 4);
  }
}
