pub use parser::{DocumentationAST, DocumentationNode};

pub mod analyzer;
pub mod parser;

// Re-export analyzer function
pub use analyzer::analyze;
