pub mod analysis;

pub mod documentation {
  pub mod analyzer;
  pub mod parser;
  pub mod resolution_pass;
}

pub mod solidity {
  pub mod analyzer;
  pub mod dev_doc_resolution_pass;
  pub mod parser;
  pub mod transform;
}
