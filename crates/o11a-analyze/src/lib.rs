pub mod analysis;

pub mod documentation {
  pub mod analyzer;
  pub mod parser;
  pub mod resolution_pass;
}

pub mod solidity {
  pub mod analyzer;
  pub mod dev_doc_resolution_pass;
  pub mod effective_properties;
  pub mod parser;
  pub mod transform;
}

/// Skeleton mirror of `solidity` for the Rust language. Every entry
/// point is a no-op until the Rust parser/analyzer lands; the modules
/// exist so `crate::analysis::run_analysis` can be wired symmetrically
/// across languages.
pub mod rust {
  pub mod analyzer;
  pub mod dev_doc_resolution_pass;
  pub mod parser;
  pub mod transform;
}
