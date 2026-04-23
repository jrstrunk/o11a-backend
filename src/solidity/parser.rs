use crate::core;
use crate::core::topic;
use crate::core::{
  ContractKind, FunctionKind, ProjectPath, VariableMutability,
};
use serde_json;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;
use std::{panic, vec};

struct ParserContext {
  pub source_content: String,
  pub ast_map: BTreeMap<core::ProjectPath, Vec<SolidityAST>>,
  /// The node ID of the parent signature node, used to set `parameter_variable`
  /// on VariableDeclaration nodes within parameter lists.
  pub signature_parent_node: Cell<Option<i32>>,
}

pub fn process(project_root: &Path) -> Result<BTreeMap<core::ProjectPath, Vec<SolidityAST>>, String> {
  // Look for the "out" directory in the project root
  let out_dir = project_root.join("out");
  if !out_dir.exists() || !out_dir.is_dir() {
    return Err(format!("'out' directory not found at {:?}", out_dir));
  }

  println!("Processing JSON files in directory: {:?}", out_dir);

  let mut context = ParserContext {
    source_content: String::new(),
    ast_map: std::collections::BTreeMap::new(),
    signature_parent_node: Cell::new(None),
  };

  // Recursively traverse the out directory to find all JSON files
  traverse_and_parse_asts(&out_dir, &project_root, &mut context)?;

  let total_asts: usize = context.ast_map.values().map(|v| v.len()).sum();
  println!(
    "Successfully processed {} unique paths with {} total AST files",
    context.ast_map.len(),
    total_asts
  );

  Ok(context.ast_map)
}

fn traverse_and_parse_asts(
  dir: &Path,
  project_root: &Path,
  context: &mut ParserContext,
) -> Result<(), String> {
  let entries = std::fs::read_dir(dir)
    .map_err(|e| format!("Failed to read directory {:?}: {}", dir, e))?;

  for entry in entries {
    let entry =
      entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
    let path = entry.path();

    if path.is_dir() {
      // Skip the build-info directory
      if let Some(dir_name) = path.file_name() {
        if dir_name == "build-info" {
          continue;
        }
      }
      // Recursively traverse subdirectories
      traverse_and_parse_asts(&path, &project_root, context)?;
    } else if path.is_file() {
      if let Some(extension) = path.extension() {
        if extension == "json" {
          println!("Processing JSON file: {:?}", path);
          let ast =
            ast_from_json_file(&path.to_string_lossy(), &project_root, context)
              .map_err(|e| {
                format!("Failed to parse JSON file {:?}: {}", path, e)
              })?;

          context
            .ast_map
            .entry(ast.project_path.clone())
            .or_insert_with(Vec::new)
            .push(ast);
        }
      }
    }
  }

  Ok(())
}

fn ast_from_json_file(
  file_path: &str,
  project_root: &Path,
  context: &mut ParserContext,
) -> Result<SolidityAST, String> {
  let json = std::fs::read_to_string(file_path)
    .map_err(|e| format!("Failed to read file: {}", e))?;

  // Parse the JSON string
  let parsed: serde_json::Value = serde_json::from_str(&json)
    .map_err(|e| format!("Failed to parse JSON: {}", e))?;

  // Extract the "ast" object from the root
  let ast_obj = parsed
    .get("ast")
    .ok_or_else(|| "Missing 'ast' field in JSON".to_string())?;

  // Parse the required fields from the ast object
  let node_id = ast_obj
    .get("id")
    .and_then(|v| v.as_i64())
    .map(|v| v as i32)
    .ok_or_else(|| "Missing or invalid 'id' field in ast object".to_string())?;

  let absolute_path = ast_obj
    .get("absolutePath")
    .and_then(|v| v.as_str())
    .map(|s| s.to_string())
    .ok_or_else(|| {
      "Missing or invalid 'absolutePath' field in ast object".to_string()
    })?;
  let project_path = core::new_project_path(&absolute_path, project_root);

  // Read the original source file content
  let source_content = read_source_file(&project_path, &project_root)?;
  context.source_content = source_content;

  let nodes_array = ast_obj
    .get("nodes")
    .and_then(|v| v.as_array())
    .ok_or_else(|| {
      "Missing or invalid 'nodes' field in ast object".to_string()
    })?;

  // Parse each node in the nodes array
  let nodes: Result<Vec<ASTNode>, String> = nodes_array
    .iter()
    .map(|node_val| node_from_json(node_val, &context))
    .collect();

  let nodes = nodes?;

  Ok(SolidityAST {
    node_id,
    nodes,
    project_path,
  })
}

pub struct SolidityAST {
  pub node_id: i32,
  pub nodes: Vec<ASTNode>,
  pub project_path: ProjectPath,
}

impl SolidityAST {
  /// Get children nodes, resolving nodes that are stubs to their real nodes
  /// from the nodes map
  pub fn resolve_nodes(
    &self,
    nodes_map: &BTreeMap<topic::Topic, core::Node>,
  ) -> Vec<ASTNode> {
    self
      .nodes
      .iter()
      .map(|node| match node {
        ASTNode::Stub { topic, .. } => {
          if let Some(core::Node::Solidity(ast_node)) = nodes_map.get(topic) {
            ast_node.clone()
          } else {
            node.clone()
          }
        }
        _ => node.clone(),
      })
      .collect()
  }
}

fn read_source_file(
  project_path: &ProjectPath,
  project_root: &Path,
) -> Result<String, String> {
  // Create the absolute path to the source file
  let absolute_source_file_path =
    core::project_path_to_absolute_path(project_path, project_root);

  // Read the source file
  std::fs::read_to_string(&absolute_source_file_path).map_err(|e| {
    format!(
      "Failed to read source file {:?}: {}",
      absolute_source_file_path, e
    )
  })
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct SourceLocation {
  pub start: Option<usize>,
  pub length: Option<usize>,
  pub index: Option<usize>,
}

impl FromStr for SourceLocation {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    let invalid_location = move || format!("{s} invalid source location");

    let mut split = s.split(':');
    let start = split
      .next()
      .ok_or_else(invalid_location)?
      .parse::<isize>()
      .map_err(|_| invalid_location())?;
    let length = split
      .next()
      .ok_or_else(invalid_location)?
      .parse::<isize>()
      .map_err(|_| invalid_location())?;
    let index = split
      .next()
      .ok_or_else(invalid_location)?
      .parse::<isize>()
      .map_err(|_| invalid_location())?;

    let start = if start < 0 {
      None
    } else {
      Some(start as usize)
    };
    let length = if length < 0 {
      None
    } else {
      Some(length as usize)
    };
    let index = if index < 0 {
      None
    } else {
      Some(index as usize)
    };

    Ok(Self {
      start,
      length,
      index,
    })
  }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionStateMutability {
  Pure,
  View,
  NonPayable,
  Payable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FunctionVisibility {
  Public,
  Private,
  Internal,
  External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VariableVisibility {
  Public,
  Private,
  Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageLocation {
  Default,
  Storage,
  Memory,
  Calldata,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Copy)]
pub enum LiteralKind {
  Number,
  Bool,
  String,
  HexString,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TypeDescriptions {
  pub type_identifier: String,
  pub type_string: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum UnaryOperator {
  Increment,  // ++
  Decrement,  // --
  Plus,       // +
  Minus,      // -
  BitwiseNot, // ~
  Not,        // !
  Delete,     // delete
}

impl FromStr for UnaryOperator {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "!" => Ok(UnaryOperator::Not),
      "++" => Ok(UnaryOperator::Increment),
      "--" => Ok(UnaryOperator::Decrement),
      "+" => Ok(UnaryOperator::Plus),
      "-" => Ok(UnaryOperator::Minus),
      "~" => Ok(UnaryOperator::BitwiseNot),
      "delete" => Ok(UnaryOperator::Delete),
      _ => Err(format!("Invalid unary operator: {}", s)),
    }
  }
}

impl UnaryOperator {
  pub fn as_str(&self) -> &'static str {
    match self {
      UnaryOperator::Increment => "++",
      UnaryOperator::Decrement => "--",
      UnaryOperator::Plus => "+",
      UnaryOperator::Minus => "-",
      UnaryOperator::BitwiseNot => "~",
      UnaryOperator::Not => "!",
      UnaryOperator::Delete => "delete",
    }
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum BinaryOperator {
  // Arithmetic
  Add,      // +
  Subtract, // -
  Multiply, // *
  Divide,   // /
  Modulo,   // %
  Power,    // **

  // Comparison
  Equal,              // ==
  NotEqual,           // !=
  LessThan,           // <
  LessThanOrEqual,    // <=
  GreaterThan,        // >
  GreaterThanOrEqual, // >=

  // Logical
  And, // &&
  Or,  // ||

  // Bitwise
  BitwiseAnd, // &
  BitwiseOr,  // |
  BitwiseXor, // ^
  LeftShift,  // <<
  RightShift, // >>
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AssignmentOperator {
  Assign,           // =
  AddAssign,        // +=
  SubtractAssign,   // -=
  MultiplyAssign,   // *=
  DivideAssign,     // /=
  ModuloAssign,     // %=
  BitwiseAndAssign, // &=
  BitwiseOrAssign,  // |=
  BitwiseXorAssign, // ^=
  LeftShiftAssign,  // <<=
  RightShiftAssign, // >>=
}

impl FromStr for FunctionKind {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "constructor" => Ok(FunctionKind::Constructor),
      "function" => Ok(FunctionKind::Function),
      "fallback" => Ok(FunctionKind::Fallback),
      "receive" => Ok(FunctionKind::Receive),
      "freeFunction" => Ok(FunctionKind::FreeFunction),
      _ => Err(format!("Unknown function kind: {}", s)),
    }
  }
}

impl FromStr for FunctionStateMutability {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "pure" => Ok(FunctionStateMutability::Pure),
      "view" => Ok(FunctionStateMutability::View),
      "nonpayable" => Ok(FunctionStateMutability::NonPayable),
      "payable" => Ok(FunctionStateMutability::Payable),
      _ => Err(format!("Unknown state mutability: {}", s)),
    }
  }
}

impl FromStr for FunctionVisibility {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "public" => Ok(FunctionVisibility::Public),
      "private" => Ok(FunctionVisibility::Private),
      "internal" => Ok(FunctionVisibility::Internal),
      "external" => Ok(FunctionVisibility::External),
      _ => Err(format!("Unknown visibility: {}", s)),
    }
  }
}

impl FromStr for VariableVisibility {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "public" => Ok(VariableVisibility::Public),
      "private" => Ok(VariableVisibility::Private),
      "internal" => Ok(VariableVisibility::Internal),
      _ => Err(format!("Unknown variable visibility: {}", s)),
    }
  }
}

impl FromStr for LiteralKind {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "number" => Ok(LiteralKind::Number),
      "bool" => Ok(LiteralKind::Bool),
      "string" => Ok(LiteralKind::String),
      "hexString" => Ok(LiteralKind::HexString),
      _ => Err(format!("Unknown literal kind: {}", s)),
    }
  }
}

impl LiteralKind {
  pub fn as_str(&self) -> &'static str {
    match self {
      LiteralKind::Number => "number",
      LiteralKind::Bool => "bool",
      LiteralKind::String => "string",
      LiteralKind::HexString => "hexString",
    }
  }
}

impl FromStr for BinaryOperator {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "+" => Ok(BinaryOperator::Add),
      "-" => Ok(BinaryOperator::Subtract),
      "*" => Ok(BinaryOperator::Multiply),
      "/" => Ok(BinaryOperator::Divide),
      "%" => Ok(BinaryOperator::Modulo),
      "**" => Ok(BinaryOperator::Power),
      "==" => Ok(BinaryOperator::Equal),
      "!=" => Ok(BinaryOperator::NotEqual),
      "<" => Ok(BinaryOperator::LessThan),
      "<=" => Ok(BinaryOperator::LessThanOrEqual),
      ">" => Ok(BinaryOperator::GreaterThan),
      ">=" => Ok(BinaryOperator::GreaterThanOrEqual),
      "&&" => Ok(BinaryOperator::And),
      "||" => Ok(BinaryOperator::Or),
      "&" => Ok(BinaryOperator::BitwiseAnd),
      "|" => Ok(BinaryOperator::BitwiseOr),
      "^" => Ok(BinaryOperator::BitwiseXor),
      "<<" => Ok(BinaryOperator::LeftShift),
      ">>" => Ok(BinaryOperator::RightShift),
      _ => Err(format!("Unknown binary operator: {}", s)),
    }
  }
}

impl BinaryOperator {
  /// Returns true if this operator indicates a "relatives" relationship between operands.
  /// This includes all operators except logical And (&&) and Or (||).
  pub fn is_relative_operator(&self) -> bool {
    !matches!(self, BinaryOperator::And | BinaryOperator::Or)
  }

  pub fn as_str(&self) -> &'static str {
    match self {
      BinaryOperator::Add => "+",
      BinaryOperator::Subtract => "-",
      BinaryOperator::Multiply => "*",
      BinaryOperator::Divide => "/",
      BinaryOperator::Modulo => "%",
      BinaryOperator::Power => "**",
      BinaryOperator::Equal => "==",
      BinaryOperator::NotEqual => "!=",
      BinaryOperator::LessThan => "<",
      BinaryOperator::LessThanOrEqual => "<=",
      BinaryOperator::GreaterThan => ">",
      BinaryOperator::GreaterThanOrEqual => ">=",
      BinaryOperator::And => "&&",
      BinaryOperator::Or => "||",
      BinaryOperator::BitwiseAnd => "&",
      BinaryOperator::BitwiseOr => "|",
      BinaryOperator::BitwiseXor => "^",
      BinaryOperator::LeftShift => "<<",
      BinaryOperator::RightShift => ">>",
    }
  }
}

impl FromStr for AssignmentOperator {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "=" => Ok(AssignmentOperator::Assign),
      "+=" => Ok(AssignmentOperator::AddAssign),
      "-=" => Ok(AssignmentOperator::SubtractAssign),
      "*=" => Ok(AssignmentOperator::MultiplyAssign),
      "/=" => Ok(AssignmentOperator::DivideAssign),
      "%=" => Ok(AssignmentOperator::ModuloAssign),
      "&=" => Ok(AssignmentOperator::BitwiseAndAssign),
      "|=" => Ok(AssignmentOperator::BitwiseOrAssign),
      "^=" => Ok(AssignmentOperator::BitwiseXorAssign),
      "<<=" => Ok(AssignmentOperator::LeftShiftAssign),
      ">>=" => Ok(AssignmentOperator::RightShiftAssign),
      _ => Err(format!("Unknown assignment operator: {}", s)),
    }
  }
}

impl AssignmentOperator {
  pub fn as_str(&self) -> &'static str {
    match self {
      AssignmentOperator::Assign => "=",
      AssignmentOperator::AddAssign => "+=",
      AssignmentOperator::SubtractAssign => "-=",
      AssignmentOperator::MultiplyAssign => "*=",
      AssignmentOperator::DivideAssign => "/=",
      AssignmentOperator::ModuloAssign => "%=",
      AssignmentOperator::BitwiseAndAssign => "&=",
      AssignmentOperator::BitwiseOrAssign => "|=",
      AssignmentOperator::BitwiseXorAssign => "^=",
      AssignmentOperator::LeftShiftAssign => "<<=",
      AssignmentOperator::RightShiftAssign => ">>=",
    }
  }
}

impl FromStr for VariableMutability {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "mutable" => Ok(VariableMutability::Mutable),
      "immutable" => Ok(VariableMutability::Immutable),
      "constant" => Ok(VariableMutability::Constant),
      _ => Err(format!("Unknown mutability: {}", s)),
    }
  }
}

impl FromStr for StorageLocation {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "default" => Ok(StorageLocation::Default),
      "storage" => Ok(StorageLocation::Storage),
      "memory" => Ok(StorageLocation::Memory),
      "calldata" => Ok(StorageLocation::Calldata),
      _ => Err(format!("Invalid storage location: {}", s)),
    }
  }
}

impl FromStr for ContractKind {
  type Err = String;

  fn from_str(s: &str) -> Result<Self, Self::Err> {
    match s {
      "contract" => Ok(ContractKind::Contract),
      "library" => Ok(ContractKind::Library),
      "abstract" => Ok(ContractKind::Abstract),
      "interface" => Ok(ContractKind::Interface),
      _ => Err(format!("Invalid contract kind: {}", s)),
    }
  }
}

impl TypeDescriptions {
  pub fn from_json(value: &serde_json::Value) -> Result<Self, String> {
    let type_identifier = value
      .get("typeIdentifier")
      .and_then(|v| v.as_str())
      .ok_or_else(|| {
        format!("TypeDescriptions missing typeIdentifier: {:?}", value)
      })?
      .to_string();
    let type_string = value
      .get("typeString")
      .and_then(|v| v.as_str())
      .ok_or_else(|| {
        format!("TypeDescriptions missing typeString: {:?}", value)
      })?
      .to_string();

    Ok(TypeDescriptions {
      type_identifier,
      type_string,
    })
  }
}

/// Classification of a stub's underlying node type, used by the formatter
/// to decide whether to emit inline documentation placeholders without
/// needing to resolve the stub via a nodes_map lookup.
#[derive(Debug, Clone, PartialEq)]
pub enum StubKind {
  /// Identifier or IdentifierPath node. `referenced_topic` is the topic of
  /// the declaration this identifier references (not the identifier node
  /// itself), used for placeholder generation.
  Identifier { referenced_topic: topic::Topic },
  /// MemberAccess node. `base_kind` describes the kind of the base expression.
  MemberAccess { base_kind: Box<StubKind> },
  /// TypeConversion (@ cast) node. `argument_kind` describes the kind of the
  /// inner argument expression, and `argument_topic` is the topic of the
  /// argument (used for placeholder generation, since the placeholder should
  /// target the argument, not the cast itself).
  TypeConversion {
    argument_kind: Box<StubKind>,
    argument_topic: topic::Topic,
  },
  /// Literal node (numbers, strings, bools, hex).
  Literal,
  /// A declaration node (VariableDeclarationStatement, VariableDeclaration,
  /// ContractDefinition, FunctionDefinition, etc.). Statement containers use
  /// this to skip emitting statement-level placeholders for declarations.
  Declaration,
  /// A compound expression (FunctionCall, StructConstructor,
  /// FunctionCallOptions) that wraps an inner expression. The
  /// `expression_kind` describes the kind of the inner expression, allowing
  /// placeholder logic to recurse through stubbed compound expressions.
  CompoundExpression { expression_kind: Box<StubKind> },
  /// Everything else.
  Other,
}

impl StubKind {
  /// Returns true if this stub kind is "identifier-like" for placeholder
  /// purposes: an Identifier, a Literal, a TypeConversion whose argument is
  /// identifier-like, or a MemberAccess whose base is identifier-like.
  pub fn is_identifier_like(&self) -> bool {
    match self {
      StubKind::Identifier { .. } | StubKind::Literal => true,
      StubKind::TypeConversion { argument_kind, .. } => {
        argument_kind.is_identifier_like()
      }
      StubKind::MemberAccess { base_kind } => base_kind.is_identifier_like(),
      StubKind::CompoundExpression { expression_kind } => {
        expression_kind.is_identifier_like()
      }
      StubKind::Declaration | StubKind::Other => false,
    }
  }

  /// Returns the topic to use for placeholder generation, if the kind carries
  /// one. For Identifier this is the referenced declaration's topic; for
  /// TypeConversion this is the argument's topic. Returns None for kinds that
  /// don't carry a specific placeholder topic.
  pub fn placeholder_topic(&self) -> Option<&topic::Topic> {
    match self {
      StubKind::Identifier { referenced_topic } => Some(referenced_topic),
      StubKind::MemberAccess { base_kind } => base_kind.placeholder_topic(),
      StubKind::TypeConversion { argument_topic, .. } => Some(argument_topic),
      StubKind::CompoundExpression { expression_kind } => {
        expression_kind.placeholder_topic()
      }
      _ => None,
    }
  }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ASTNode {
  // Expression nodes
  Assignment {
    node_id: i32,
    src_location: SourceLocation,
    operator: AssignmentOperator,
    right_hand_side: Box<ASTNode>,
    left_hand_side: Box<ASTNode>,
  },
  BinaryOperation {
    node_id: i32,
    src_location: SourceLocation,
    left_expression: Box<ASTNode>,
    operator: BinaryOperator,
    right_expression: Box<ASTNode>,
    type_descriptions: TypeDescriptions,
  },
  Conditional {
    node_id: i32,
    src_location: SourceLocation,
    condition: Box<ASTNode>,
    true_expression: Box<ASTNode>,
    false_expression: Option<Box<ASTNode>>,
  },
  ElementaryTypeNameExpression {
    node_id: i32,
    src_location: SourceLocation,
    type_descriptions: TypeDescriptions,
    type_name: Box<ASTNode>,
  },
  FunctionCall {
    node_id: i32,
    src_location: SourceLocation,
    arguments: Vec<ASTNode>,
    expression: Box<ASTNode>,
    name_locations: Vec<SourceLocation>,
    names: Vec<String>,
    try_call: bool,
    type_descriptions: TypeDescriptions,
    /// Node IDs of the return parameter VariableDeclarations from the called function.
    /// Populated during the transform phase; empty when initially parsed.
    referenced_return_declarations: Vec<i32>,
  },
  /// Wrapper node for function call arguments that links the argument to its
  /// corresponding parameter declaration. Generated during post-processing,
  /// not from the Solidity compiler AST.
  Argument {
    node_id: i32,
    src_location: SourceLocation,
    /// The node ID of the VariableDeclaration parameter this argument maps to.
    /// None if the called function is a built-in or couldn't be resolved.
    parameter: Option<Box<ASTNode>>,
    /// The original argument expression from the compiler AST.
    argument: Box<ASTNode>,
  },
  TypeConversion {
    node_id: i32,
    src_location: SourceLocation,
    argument: Box<ASTNode>,
    expression: Box<ASTNode>,
    name_locations: Vec<SourceLocation>,
    names: Vec<String>,
    try_call: bool,
    type_descriptions: TypeDescriptions,
  },
  StructConstructor {
    node_id: i32,
    src_location: SourceLocation,
    arguments: Vec<ASTNode>,
    expression: Box<ASTNode>,
    name_locations: Vec<SourceLocation>,
    names: Vec<String>,
    try_call: bool,
    type_descriptions: TypeDescriptions,
  },
  FunctionCallOptions {
    node_id: i32,
    src_location: SourceLocation,
    expression: Box<ASTNode>,
    options: Vec<ASTNode>,
  },
  Identifier {
    node_id: i32,
    src_location: SourceLocation,
    name: String,
    overloaded_declarations: Vec<i32>,
    referenced_declaration: i32,
  },
  IdentifierPath {
    node_id: i32,
    src_location: SourceLocation,
    name: String,
    name_locations: Vec<SourceLocation>,
    referenced_declaration: i32,
  },
  IndexAccess {
    node_id: i32,
    src_location: SourceLocation,
    base_expression: Box<ASTNode>,
    // index_expression is None in this case:
    // `RawTx1559 memory rawTx = abi.decode(parsedDeployData, (RawTx1559));`
    index_expression: Option<Box<ASTNode>>,
  },
  IndexRangeAccess {
    node_id: i32,
    src_location: SourceLocation,
    nodes: Vec<ASTNode>,
    body: Option<Box<ASTNode>>,
  },
  Literal {
    node_id: i32,
    src_location: SourceLocation,
    hex_value: String,
    kind: LiteralKind,
    type_descriptions: TypeDescriptions,
    value: Option<String>,
  },
  MemberAccess {
    node_id: i32,
    src_location: SourceLocation,
    expression: Box<ASTNode>,
    member_location: SourceLocation,
    member_name: String,
    referenced_declaration: Option<i32>,
    type_descriptions: TypeDescriptions,
  },
  NewExpression {
    node_id: i32,
    src_location: SourceLocation,
    type_name: Box<ASTNode>,
  },
  TupleExpression {
    node_id: i32,
    src_location: SourceLocation,
    components: Vec<ASTNode>,
  },
  UnaryOperation {
    node_id: i32,
    src_location: SourceLocation,
    prefix: bool,
    operator: UnaryOperator,
    sub_expression: Box<ASTNode>,
  },
  EnumValue {
    node_id: i32,
    src_location: SourceLocation,
    name: String,
    name_location: SourceLocation,
  },

  // Statement nodes
  Block {
    node_id: i32,
    src_location: SourceLocation,
    statements: Vec<ASTNode>,
  },
  SemanticBlock {
    node_id: i32,
    src_location: SourceLocation,
    documentation: Option<String>,
    statements: Vec<ASTNode>,
  },
  Break {
    node_id: i32,
    src_location: SourceLocation,
  },
  Continue {
    node_id: i32,
    src_location: SourceLocation,
  },
  DoWhileStatement {
    node_id: i32,
    src_location: SourceLocation,
    condition: Box<ASTNode>,
    body: Option<Box<ASTNode>>,
  },
  EmitStatement {
    node_id: i32,
    src_location: SourceLocation,
    event_call: Box<ASTNode>,
  },
  ExpressionStatement {
    node_id: i32,
    src_location: SourceLocation,
    expression: Box<ASTNode>,
  },
  ForStatement {
    node_id: i32,
    src_location: SourceLocation,
    condition: Box<ASTNode>,
    body: Box<ASTNode>,
  },
  /// Synthetic node wrapping ForStatement's loop clauses (init, condition, loop expression).
  /// Generated during parsing, not from the Solidity compiler AST.
  LoopExpression {
    node_id: i32,
    src_location: SourceLocation,
    initialization_expression: Option<Box<ASTNode>>,
    condition: Option<Box<ASTNode>>,
    loop_expression: Option<Box<ASTNode>>,
    is_simple_counter_loop: bool,
  },
  IfStatement {
    node_id: i32,
    src_location: SourceLocation,
    condition: Box<ASTNode>,
    true_body: Box<ASTNode>,
    false_body: Option<Box<ASTNode>>,
  },
  InlineAssembly {
    node_id: i32,
    src_location: SourceLocation,
  },
  PlaceholderStatement {
    node_id: i32,
    src_location: SourceLocation,
  },
  Return {
    node_id: i32,
    src_location: SourceLocation,
    expression: Option<Box<ASTNode>>,
    function_return_parameters: i32,
  },
  RevertStatement {
    node_id: i32,
    src_location: SourceLocation,
    error_call: Box<ASTNode>,
  },
  TryStatement {
    node_id: i32,
    src_location: SourceLocation,
    clauses: Vec<ASTNode>,
    external_call: Box<ASTNode>,
  },
  UncheckedBlock {
    node_id: i32,
    src_location: SourceLocation,
    statements: Vec<ASTNode>,
  },
  VariableDeclarationStatement {
    node_id: i32,
    src_location: SourceLocation,
    declarations: Vec<ASTNode>,
    initial_value: Option<Box<ASTNode>>,
  },
  WhileStatement {
    node_id: i32,
    src_location: SourceLocation,
    condition: Box<ASTNode>,
    body: Option<Box<ASTNode>>,
  },

  // Signature nodes (generated during parsing)
  ContractSignature {
    node_id: i32,
    src_location: SourceLocation,
    documentation: Option<Box<ASTNode>>,
    name: String,
    name_location: SourceLocation,
    /// The node_id of the parent definition (ContractDefinition) that owns
    /// this signature. Set during parsing when the signature is constructed
    /// as a child of the definition node. Unlike `referenced_declaration` on
    /// Identifier/IdentifierPath (which represents a usage→declaration
    /// reference), this represents an ownership relationship: the signature
    /// is an intrinsic part of the definition, not a reference to it.
    declaration_id: i32,
    contract_kind: ContractKind,
    abstract_: bool,
    base_contracts: Vec<ASTNode>,
    directives: Vec<ASTNode>,
  },
  FunctionSignature {
    node_id: i32,
    src_location: SourceLocation,
    documentation: Option<Box<ASTNode>>,
    kind: FunctionKind,
    modifiers: Box<ASTNode>,
    name: String,
    name_location: SourceLocation,
    /// The node_id of the parent definition (FunctionDefinition) that owns
    /// this signature. Set during parsing when the signature is constructed
    /// as a child of the definition node. Unlike `referenced_declaration` on
    /// Identifier/IdentifierPath (which represents a usage→declaration
    /// reference), this represents an ownership relationship: the signature
    /// is an intrinsic part of the definition, not a reference to it.
    declaration_id: i32,
    parameters: Box<ASTNode>,
    return_parameters: Box<ASTNode>,
    scope: i32,
    state_mutability: FunctionStateMutability,
    virtual_: bool,
    visibility: FunctionVisibility,
    /// For interface functions, points to the implementation's function/variable node ID
    implementation_declaration: Option<i32>,
  },
  ModifierSignature {
    node_id: i32,
    src_location: SourceLocation,
    documentation: Option<Box<ASTNode>>,
    name: String,
    name_location: SourceLocation,
    /// The node_id of the parent definition (ModifierDefinition) that owns
    /// this signature. Set during parsing when the signature is constructed
    /// as a child of the definition node. Unlike `referenced_declaration` on
    /// Identifier/IdentifierPath (which represents a usage→declaration
    /// reference), this represents an ownership relationship: the signature
    /// is an intrinsic part of the definition, not a reference to it.
    declaration_id: i32,
    parameters: Box<ASTNode>,
    virtual_: bool,
    visibility: FunctionVisibility,
    /// For interface modifiers, points to the implementation's modifier node ID
    implementation_declaration: Option<i32>,
  },

  // Definition nodes
  ContractDefinition {
    node_id: i32,
    src_location: SourceLocation,
    signature: Box<ASTNode>,
    nodes: Vec<ASTNode>,
  },
  FunctionDefinition {
    node_id: i32,
    src_location: SourceLocation,
    signature: Box<ASTNode>,
    implemented: bool,
    body: Option<Box<ASTNode>>,
  },
  EventDefinition {
    node_id: i32,
    src_location: SourceLocation,
    name: String,
    name_location: SourceLocation,
    parameters: Box<ASTNode>,
  },
  ErrorDefinition {
    node_id: i32,
    src_location: SourceLocation,
    name: String,
    name_location: SourceLocation,
    parameters: Box<ASTNode>,
  },
  ModifierDefinition {
    node_id: i32,
    src_location: SourceLocation,
    signature: Box<ASTNode>,
    body: Box<ASTNode>,
  },
  StructDefinition {
    node_id: i32,
    src_location: SourceLocation,
    members: Vec<ASTNode>,
    canonical_name: String,
    name: String,
    name_location: SourceLocation,
    visibility: VariableVisibility,
  },
  EnumDefinition {
    node_id: i32,
    src_location: SourceLocation,
    members: Vec<ASTNode>,
    canonical_name: String,
    name: String,
    name_location: SourceLocation,
  },
  UserDefinedValueTypeDefinition {
    node_id: i32,
    src_location: SourceLocation,
    name: String,
    underlying_type: Box<ASTNode>,
  },
  VariableDeclaration {
    node_id: i32,
    src_location: SourceLocation,
    constant: bool,
    function_selector: Option<String>,
    mutability: VariableMutability,
    name: String,
    name_location: SourceLocation,
    scope: i32,
    state_variable: bool,
    storage_location: StorageLocation,
    type_name: Box<ASTNode>,
    value: Option<Box<ASTNode>>,
    visibility: VariableVisibility,
    parameter_variable: Option<i32>,
    /// For interface parameters, points to the implementation's parameter node ID
    implementation_declaration: Option<i32>,
    /// For public state variables that implement interface functions, the IDs of the
    /// interface functions this variable's getter implements
    base_functions: Vec<i32>,
    /// True if this variable is a struct field
    struct_field: bool,
  },

  // Directive nodes
  PragmaDirective {
    node_id: i32,
    src_location: SourceLocation,
    literals: Vec<String>,
  },
  ImportDirective {
    node_id: i32,
    src_location: SourceLocation,
    absolute_path: String,
    file: String,
    source_unit: i32,
  },
  UsingForDirective {
    node_id: i32,
    src_location: SourceLocation,
    global: bool,
    library_name: Option<Box<ASTNode>>,
    // This field exists but is complex, so I am commenting it out until we
    // want to add support for it later. An example of this is:
    // `using { unwrap, lt as <, gt as > } for Slot global;` with the node:
    //{
    //   "id": 81050,
    //   "nodeType": "UsingForDirective",
    //   "src": "458:51:133",
    //   "nodes": [],
    //   "functionList": [
    //     {
    //       "function": {
    //         "id": 81045,
    //         "name": "unwrap",
    //         "nameLocations": [
    //           "466:6:133"
    //         ],
    //         "nodeType": "IdentifierPath",
    //         "referencedDeclaration": 81004,
    //         "src": "466:6:133"
    //       }
    //     },
    //     {
    //       "definition": {
    //         "id": 81046,
    //         "name": "lt",
    //         "nameLocations": [
    //           "474:2:133"
    //         ],
    //         "nodeType": "IdentifierPath",
    //         "referencedDeclaration": 81044,
    //         "src": "474:2:133"
    //       },
    //       "operator": "<"
    //     },
    //     {
    //       "definition": {
    //         "id": 81047,
    //         "name": "gt",
    //         "nameLocations": [
    //           "483:2:133"
    //         ],
    //         "nodeType": "IdentifierPath",
    //         "referencedDeclaration": 81024,
    //         "src": "483:2:133"
    //       },
    //       "operator": ">"
    //     }
    //   ],
    //   "global": true,
    //   "typeName": {
    //     "id": 81049,
    //     "nodeType": "UserDefinedTypeName",
    //     "pathNode": {
    //       "id": 81048,
    //       "name": "Slot",
    //       "nameLocations": [
    //         "497:4:133"
    //       ],
    //       "nodeType": "IdentifierPath",
    //       "referencedDeclaration": 80990,
    //       "src": "497:4:133"
    //     },
    //     "referencedDeclaration": 80990,
    //     "src": "497:4:133",
    //     "typeDescriptions": {
    //       "typeIdentifier": "t_userDefinedValueType$_Slot_$80990",
    //       "typeString": "Slot"
    //     }
    //   }
    // },
    // function_list: Option<Vec<ASTNode>>,
    type_name: Option<Box<ASTNode>>,
  },

  // Other nodes
  SourceUnit {
    node_id: i32,
    src_location: SourceLocation,
    nodes: Vec<ASTNode>,
  },
  InheritanceSpecifier {
    node_id: i32,
    src_location: SourceLocation,
    base_name: Box<ASTNode>,
  },
  ElementaryTypeName {
    node_id: i32,
    src_location: SourceLocation,
    name: String,
  },
  FunctionTypeName {
    node_id: i32,
    src_location: SourceLocation,
    parameter_types: Box<ASTNode>,
    return_parameter_types: Box<ASTNode>,
    state_mutability: FunctionStateMutability,
    visibility: FunctionVisibility,
  },
  ParameterList {
    node_id: i32,
    src_location: SourceLocation,
    parameters: Vec<ASTNode>,
    is_return_parameters: bool,
  },
  ModifierList {
    node_id: i32,
    src_location: SourceLocation,
    modifiers: Vec<ASTNode>,
  },
  TryCatchClause {
    node_id: i32,
    src_location: SourceLocation,
    error_name: String,
    block: Box<ASTNode>,
    parameters: Option<Box<ASTNode>>,
  },
  ModifierInvocation {
    node_id: i32,
    src_location: SourceLocation,
    modifier_name: Box<ASTNode>,
    arguments: Option<Vec<ASTNode>>,
  },
  UserDefinedTypeName {
    node_id: i32,
    src_location: SourceLocation,
    path_node: Box<ASTNode>,
    referenced_declaration: i32,
  },
  ArrayTypeName {
    node_id: i32,
    src_location: SourceLocation,
    base_type: Box<ASTNode>,
  },
  Mapping {
    node_id: i32,
    src_location: SourceLocation,
    key_name: Option<String>,
    key_name_location: SourceLocation,
    key_type: Box<ASTNode>,
    value_name: Option<String>,
    value_name_location: SourceLocation,
    value_type: Box<ASTNode>,
  },

  StructuredDocumentation {
    node_id: i32,
    src_location: SourceLocation,
    text: String,
  },

  // Placeholder for another type of node. This node is just used for
  // optimizations, so that a node can hold a placeholder for its children
  // instead of all of its real children (which can be expensive)
  Stub {
    node_id: i32,
    src_location: SourceLocation,
    topic: topic::Topic,
    kind: StubKind,
  },

  // Catch-all for unknown node types
  Other {
    node_id: i32,
    src_location: SourceLocation,
    nodes: Vec<ASTNode>,
    body: Option<Box<ASTNode>>,
    node_type: String,
  },
}

impl ASTNode {
  pub fn node_id(&self) -> i32 {
    match self {
      ASTNode::Assignment { node_id, .. } => *node_id,
      ASTNode::BinaryOperation { node_id, .. } => *node_id,
      ASTNode::Conditional { node_id, .. } => *node_id,
      ASTNode::ElementaryTypeNameExpression { node_id, .. } => *node_id,
      ASTNode::FunctionCall { node_id, .. } => *node_id,
      ASTNode::TypeConversion { node_id, .. } => *node_id,
      ASTNode::StructConstructor { node_id, .. } => *node_id,
      ASTNode::FunctionCallOptions { node_id, .. } => *node_id,
      ASTNode::Identifier { node_id, .. } => *node_id,
      ASTNode::IdentifierPath { node_id, .. } => *node_id,
      ASTNode::IndexAccess { node_id, .. } => *node_id,
      ASTNode::IndexRangeAccess { node_id, .. } => *node_id,
      ASTNode::Literal { node_id, .. } => *node_id,
      ASTNode::MemberAccess { node_id, .. } => *node_id,
      ASTNode::NewExpression { node_id, .. } => *node_id,
      ASTNode::TupleExpression { node_id, .. } => *node_id,
      ASTNode::UnaryOperation { node_id, .. } => *node_id,
      ASTNode::EnumValue { node_id, .. } => *node_id,
      ASTNode::Block { node_id, .. } => *node_id,
      ASTNode::SemanticBlock { node_id, .. } => *node_id,
      ASTNode::Break { node_id, .. } => *node_id,
      ASTNode::Continue { node_id, .. } => *node_id,
      ASTNode::DoWhileStatement { node_id, .. } => *node_id,
      ASTNode::EmitStatement { node_id, .. } => *node_id,
      ASTNode::ExpressionStatement { node_id, .. } => *node_id,
      ASTNode::ForStatement { node_id, .. } => *node_id,
      ASTNode::LoopExpression { node_id, .. } => *node_id,
      ASTNode::IfStatement { node_id, .. } => *node_id,
      ASTNode::InlineAssembly { node_id, .. } => *node_id,
      ASTNode::PlaceholderStatement { node_id, .. } => *node_id,
      ASTNode::Return { node_id, .. } => *node_id,
      ASTNode::RevertStatement { node_id, .. } => *node_id,
      ASTNode::TryStatement { node_id, .. } => *node_id,
      ASTNode::UncheckedBlock { node_id, .. } => *node_id,
      ASTNode::VariableDeclarationStatement { node_id, .. } => *node_id,
      ASTNode::VariableDeclaration { node_id, .. } => *node_id,
      ASTNode::WhileStatement { node_id, .. } => *node_id,
      ASTNode::ContractSignature { node_id, .. } => *node_id,
      ASTNode::FunctionSignature { node_id, .. } => *node_id,
      ASTNode::ContractDefinition { node_id, .. } => *node_id,
      ASTNode::FunctionDefinition { node_id, .. } => *node_id,
      ASTNode::ModifierSignature { node_id, .. } => *node_id,
      ASTNode::EventDefinition { node_id, .. } => *node_id,
      ASTNode::ErrorDefinition { node_id, .. } => *node_id,
      ASTNode::ModifierDefinition { node_id, .. } => *node_id,
      ASTNode::StructDefinition { node_id, .. } => *node_id,
      ASTNode::EnumDefinition { node_id, .. } => *node_id,
      ASTNode::UserDefinedValueTypeDefinition { node_id, .. } => *node_id,
      ASTNode::PragmaDirective { node_id, .. } => *node_id,
      ASTNode::ImportDirective { node_id, .. } => *node_id,
      ASTNode::UsingForDirective { node_id, .. } => *node_id,
      ASTNode::SourceUnit { node_id, .. } => *node_id,
      ASTNode::InheritanceSpecifier { node_id, .. } => *node_id,
      ASTNode::ElementaryTypeName { node_id, .. } => *node_id,
      ASTNode::FunctionTypeName { node_id, .. } => *node_id,
      ASTNode::ParameterList { node_id, .. } => *node_id,
      ASTNode::ModifierList { node_id, .. } => *node_id,
      ASTNode::TryCatchClause { node_id, .. } => *node_id,
      ASTNode::ModifierInvocation { node_id, .. } => *node_id,
      ASTNode::UserDefinedTypeName { node_id, .. } => *node_id,
      ASTNode::ArrayTypeName { node_id, .. } => *node_id,
      ASTNode::Mapping { node_id, .. } => *node_id,
      ASTNode::StructuredDocumentation { node_id, .. } => *node_id,
      ASTNode::Stub { node_id, .. } => *node_id,
      ASTNode::Other { node_id, .. } => *node_id,
      ASTNode::Argument { node_id, .. } => *node_id,
    }
  }

  pub fn src_location(&self) -> &SourceLocation {
    match self {
      ASTNode::Assignment { src_location, .. } => src_location,
      ASTNode::BinaryOperation { src_location, .. } => src_location,
      ASTNode::Conditional { src_location, .. } => src_location,
      ASTNode::ElementaryTypeNameExpression { src_location, .. } => {
        src_location
      }
      ASTNode::FunctionCall { src_location, .. } => src_location,
      ASTNode::TypeConversion { src_location, .. } => src_location,
      ASTNode::StructConstructor { src_location, .. } => src_location,
      ASTNode::FunctionCallOptions { src_location, .. } => src_location,
      ASTNode::Identifier { src_location, .. } => src_location,
      ASTNode::IdentifierPath { src_location, .. } => src_location,
      ASTNode::IndexAccess { src_location, .. } => src_location,
      ASTNode::IndexRangeAccess { src_location, .. } => src_location,
      ASTNode::Literal { src_location, .. } => src_location,
      ASTNode::MemberAccess { src_location, .. } => src_location,
      ASTNode::NewExpression { src_location, .. } => src_location,
      ASTNode::TupleExpression { src_location, .. } => src_location,
      ASTNode::UnaryOperation { src_location, .. } => src_location,
      ASTNode::EnumValue { src_location, .. } => src_location,
      ASTNode::Block { src_location, .. } => src_location,
      ASTNode::SemanticBlock { src_location, .. } => src_location,
      ASTNode::Break { src_location, .. } => src_location,
      ASTNode::Continue { src_location, .. } => src_location,
      ASTNode::DoWhileStatement { src_location, .. } => src_location,
      ASTNode::EmitStatement { src_location, .. } => src_location,
      ASTNode::ExpressionStatement { src_location, .. } => src_location,
      ASTNode::ForStatement { src_location, .. } => src_location,
      ASTNode::LoopExpression { src_location, .. } => src_location,
      ASTNode::IfStatement { src_location, .. } => src_location,
      ASTNode::InlineAssembly { src_location, .. } => src_location,
      ASTNode::PlaceholderStatement { src_location, .. } => src_location,
      ASTNode::Return { src_location, .. } => src_location,
      ASTNode::RevertStatement { src_location, .. } => src_location,
      ASTNode::TryStatement { src_location, .. } => src_location,
      ASTNode::UncheckedBlock { src_location, .. } => src_location,
      ASTNode::VariableDeclarationStatement { src_location, .. } => {
        src_location
      }
      ASTNode::VariableDeclaration { src_location, .. } => src_location,
      ASTNode::WhileStatement { src_location, .. } => src_location,
      ASTNode::ContractSignature { src_location, .. } => src_location,
      ASTNode::FunctionSignature { src_location, .. } => src_location,
      ASTNode::ModifierSignature { src_location, .. } => src_location,
      ASTNode::ContractDefinition { src_location, .. } => src_location,
      ASTNode::FunctionDefinition { src_location, .. } => src_location,
      ASTNode::EventDefinition { src_location, .. } => src_location,
      ASTNode::ErrorDefinition { src_location, .. } => src_location,
      ASTNode::ModifierDefinition { src_location, .. } => src_location,
      ASTNode::StructDefinition { src_location, .. } => src_location,
      ASTNode::EnumDefinition { src_location, .. } => src_location,
      ASTNode::UserDefinedValueTypeDefinition { src_location, .. } => {
        src_location
      }
      ASTNode::PragmaDirective { src_location, .. } => src_location,
      ASTNode::ImportDirective { src_location, .. } => src_location,
      ASTNode::UsingForDirective { src_location, .. } => src_location,
      ASTNode::SourceUnit { src_location, .. } => src_location,
      ASTNode::InheritanceSpecifier { src_location, .. } => src_location,
      ASTNode::ElementaryTypeName { src_location, .. } => src_location,
      ASTNode::FunctionTypeName { src_location, .. } => src_location,
      ASTNode::ParameterList { src_location, .. } => src_location,
      ASTNode::ModifierList { src_location, .. } => src_location,
      ASTNode::TryCatchClause { src_location, .. } => src_location,
      ASTNode::ModifierInvocation { src_location, .. } => src_location,
      ASTNode::UserDefinedTypeName { src_location, .. } => src_location,
      ASTNode::ArrayTypeName { src_location, .. } => src_location,
      ASTNode::Mapping { src_location, .. } => src_location,
      ASTNode::StructuredDocumentation { src_location, .. } => src_location,
      ASTNode::Stub { src_location, .. } => src_location,
      ASTNode::Other { src_location, .. } => src_location,
      ASTNode::Argument { src_location, .. } => src_location,
    }
  }

  pub fn nodes(&self) -> Vec<&ASTNode> {
    match self {
      ASTNode::Assignment {
        right_hand_side,
        left_hand_side,
        ..
      } => vec![right_hand_side, left_hand_side],
      ASTNode::BinaryOperation {
        left_expression,
        right_expression,
        ..
      } => vec![left_expression, right_expression],
      ASTNode::Conditional {
        condition,
        true_expression,
        false_expression,
        ..
      } => match false_expression {
        Some(false_expr) => vec![condition, true_expression, false_expr],
        None => vec![condition, true_expression],
      },
      ASTNode::ElementaryTypeNameExpression { type_name, .. } => {
        vec![type_name]
      }
      ASTNode::FunctionCall {
        arguments,
        expression,
        ..
      } => {
        let mut result = vec![&**expression];
        for item in arguments {
          result.push(item);
        }
        result
      }
      ASTNode::TypeConversion {
        argument,
        expression,
        ..
      } => vec![&**expression, &**argument],
      ASTNode::StructConstructor {
        arguments,
        expression,
        ..
      } => {
        let mut result = vec![&**expression];
        for item in arguments {
          result.push(item);
        }
        result
      }
      ASTNode::FunctionCallOptions {
        expression,
        options,
        ..
      } => {
        let mut result = vec![&**expression];
        for item in options {
          result.push(item);
        }
        result
      }
      ASTNode::Identifier { .. } => vec![],
      ASTNode::IdentifierPath { .. } => vec![],
      ASTNode::IndexAccess {
        base_expression,
        index_expression,
        ..
      } => {
        let mut result = vec![&**base_expression];
        if let Some(index_expression) = index_expression {
          result.push(&**index_expression);
        }
        result
      }
      ASTNode::IndexRangeAccess { .. } => {
        panic!("IndexRangeAccess not implemented")
      }
      ASTNode::Literal { .. } => vec![],
      ASTNode::MemberAccess { expression, .. } => vec![expression],
      ASTNode::NewExpression { type_name, .. } => vec![type_name],
      ASTNode::TupleExpression { components, .. } => {
        let mut result = vec![];
        for item in components {
          result.push(item);
        }
        result
      }
      ASTNode::UnaryOperation { sub_expression, .. } => vec![sub_expression],
      ASTNode::EnumValue { .. } => vec![],
      ASTNode::Block { statements, .. } => {
        let mut result = vec![];
        for item in statements {
          result.push(item);
        }
        result
      }
      ASTNode::SemanticBlock { statements, .. } => {
        let mut result = vec![];
        for item in statements {
          result.push(item);
        }
        result
      }
      ASTNode::Break { .. } => vec![],
      ASTNode::Continue { .. } => vec![],
      ASTNode::DoWhileStatement {
        condition, body, ..
      } => match body {
        Some(body) => vec![condition, body],
        None => vec![condition],
      },
      ASTNode::EmitStatement { event_call, .. } => vec![event_call],
      ASTNode::ExpressionStatement { expression, .. } => vec![expression],
      ASTNode::ForStatement {
        condition, body, ..
      } => vec![condition, body],
      ASTNode::LoopExpression {
        initialization_expression,
        condition,
        loop_expression,
        ..
      } => {
        let mut result = vec![];
        if let Some(init) = initialization_expression {
          result.push(&**init);
        }
        if let Some(cond) = condition {
          result.push(&**cond);
        }
        if let Some(loop_expr) = loop_expression {
          result.push(&**loop_expr);
        }
        result
      }
      ASTNode::IfStatement {
        condition,
        true_body,
        false_body,
        ..
      } => match false_body {
        Some(false_body) => vec![condition, true_body, false_body],
        None => vec![condition, true_body],
      },
      ASTNode::InlineAssembly { .. } => vec![],
      ASTNode::PlaceholderStatement { .. } => vec![],
      ASTNode::Return { expression, .. } => match expression {
        Some(expr) => vec![expr],
        None => vec![],
      },
      ASTNode::RevertStatement { error_call, .. } => vec![error_call],
      ASTNode::TryStatement {
        clauses,
        external_call,
        ..
      } => {
        let mut result = vec![&**external_call];
        for clause in clauses {
          result.push(clause);
        }
        result
      }
      ASTNode::UncheckedBlock { statements, .. } => {
        let mut result = vec![];
        for item in statements {
          result.push(item);
        }
        result
      }
      ASTNode::VariableDeclarationStatement {
        declarations,
        initial_value,
        ..
      } => {
        let mut result = vec![];
        for item in declarations {
          result.push(item);
        }
        match initial_value {
          Some(value) => result.push(value),
          None => {}
        }
        result
      }
      ASTNode::VariableDeclaration {
        type_name, value, ..
      } => match value {
        Some(val) => vec![type_name, val],
        None => vec![type_name],
      },
      ASTNode::WhileStatement {
        condition, body, ..
      } => match body {
        Some(body) => vec![body, condition],
        None => vec![condition],
      },
      ASTNode::ContractSignature {
        documentation,
        base_contracts,
        directives,
        ..
      } => {
        let mut result = vec![];
        if let Some(doc) = documentation {
          result.push(&**doc);
        }
        for item in base_contracts {
          result.push(item);
        }
        for item in directives {
          result.push(item);
        }
        result
      }
      ASTNode::ContractDefinition {
        signature, nodes, ..
      } => {
        let mut result = vec![&**signature];
        for item in nodes {
          result.push(item);
        }
        result
      }
      ASTNode::FunctionSignature {
        documentation,
        modifiers,
        parameters,
        return_parameters,
        ..
      } => {
        let mut result = vec![];
        if let Some(doc) = documentation {
          result.push(&**doc);
        }
        result.push(&**modifiers);
        result.push(&**parameters);
        result.push(&**return_parameters);
        result
      }
      ASTNode::FunctionDefinition {
        signature, body, ..
      } => {
        let mut result = vec![&**signature];
        if let Some(body_node) = body {
          result.push(&**body_node);
        }
        result
      }
      ASTNode::EventDefinition { parameters, .. } => vec![parameters],
      ASTNode::ErrorDefinition { parameters, .. } => vec![parameters],
      ASTNode::ModifierSignature {
        documentation,
        parameters,
        ..
      } => {
        let mut result = vec![];
        if let Some(doc) = documentation {
          result.push(&**doc);
        }
        result.push(&**parameters);
        result
      }
      ASTNode::ModifierDefinition {
        signature, body, ..
      } => {
        vec![&**signature, &**body]
      }
      ASTNode::StructDefinition { members, .. } => {
        let mut result = vec![];
        for item in members {
          result.push(item);
        }
        result
      }
      ASTNode::EnumDefinition { members, .. } => {
        let mut result = vec![];
        for item in members {
          result.push(item);
        }
        result
      }
      ASTNode::UserDefinedValueTypeDefinition {
        underlying_type, ..
      } => vec![underlying_type],
      ASTNode::PragmaDirective { .. } => vec![],
      ASTNode::ImportDirective { .. } => vec![],
      ASTNode::UsingForDirective {
        library_name,
        type_name,
        ..
      } => {
        let mut result = vec![];
        if let Some(type_name) = type_name {
          result.push(&**type_name);
        }
        if let Some(lib_name) = library_name {
          result.push(&**lib_name);
        }
        result
      }
      ASTNode::SourceUnit { nodes, .. } => {
        let mut result = vec![];
        for item in nodes {
          result.push(item);
        }
        result
      }
      ASTNode::InheritanceSpecifier { base_name, .. } => vec![base_name],
      ASTNode::ElementaryTypeName { .. } => vec![],
      ASTNode::FunctionTypeName {
        parameter_types,
        return_parameter_types,
        ..
      } => {
        let mut result = vec![];
        result.push(&**parameter_types);
        result.push(&**return_parameter_types);
        result
      }
      ASTNode::ParameterList { parameters, .. } => {
        let mut result = vec![];
        for item in parameters {
          result.push(item);
        }
        result
      }
      ASTNode::ModifierList { modifiers, .. } => modifiers.iter().collect(),
      ASTNode::TryCatchClause {
        block, parameters, ..
      } => match parameters {
        Some(params) => vec![&**block, &**params],
        None => vec![&**block],
      },
      ASTNode::ModifierInvocation {
        modifier_name,
        arguments,
        ..
      } => {
        let mut result = vec![&**modifier_name];
        match arguments {
          Some(args) => {
            for item in args {
              result.push(item);
            }
          }
          None => {}
        }
        result
      }
      ASTNode::UserDefinedTypeName { path_node, .. } => vec![path_node],
      ASTNode::ArrayTypeName { base_type, .. } => vec![base_type],
      ASTNode::Mapping {
        key_type,
        value_type,
        ..
      } => vec![key_type, value_type],
      ASTNode::StructuredDocumentation { .. } => vec![],
      ASTNode::Stub { .. } => vec![],
      ASTNode::Other { nodes, body, .. } => {
        let mut result = vec![];
        for item in nodes {
          result.push(item);
        }
        match body {
          Some(body_node) => result.push(body_node),
          None => {}
        }
        result
      }
      ASTNode::Argument {
        argument,
        parameter: referenced_parameter,
        ..
      } => {
        let mut result = vec![&**argument];
        if let Some(referenced_parameter) = referenced_parameter {
          result.push(referenced_parameter);
        }
        result
      }
    }
  }

  /// Returns mutable references to all direct child nodes.
  pub fn nodes_mut(&mut self) -> Vec<&mut ASTNode> {
    match self {
      ASTNode::Assignment {
        right_hand_side,
        left_hand_side,
        ..
      } => vec![right_hand_side.as_mut(), left_hand_side.as_mut()],
      ASTNode::BinaryOperation {
        left_expression,
        right_expression,
        ..
      } => vec![left_expression.as_mut(), right_expression.as_mut()],
      ASTNode::Conditional {
        condition,
        true_expression,
        false_expression,
        ..
      } => {
        let mut result = vec![condition.as_mut(), true_expression.as_mut()];
        if let Some(false_expr) = false_expression {
          result.push(false_expr.as_mut());
        }
        result
      }
      ASTNode::ElementaryTypeNameExpression { type_name, .. } => {
        vec![type_name.as_mut()]
      }
      ASTNode::FunctionCall {
        arguments,
        expression,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![expression.as_mut()];
        for item in arguments.iter_mut() {
          result.push(item);
        }
        result
      }
      ASTNode::TypeConversion {
        argument,
        expression,
        ..
      } => vec![expression.as_mut(), argument.as_mut()],
      ASTNode::StructConstructor {
        arguments,
        expression,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![expression.as_mut()];
        for item in arguments.iter_mut() {
          result.push(item);
        }
        result
      }
      ASTNode::FunctionCallOptions {
        expression,
        options,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![expression.as_mut()];
        for item in options.iter_mut() {
          result.push(item);
        }
        result
      }
      ASTNode::Identifier { .. } => vec![],
      ASTNode::IdentifierPath { .. } => vec![],
      ASTNode::IndexAccess {
        base_expression,
        index_expression,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![base_expression.as_mut()];
        if let Some(index_expr) = index_expression {
          result.push(index_expr.as_mut());
        }
        result
      }
      ASTNode::IndexRangeAccess { .. } => vec![],
      ASTNode::Literal { .. } => vec![],
      ASTNode::MemberAccess { expression, .. } => vec![expression.as_mut()],
      ASTNode::NewExpression { type_name, .. } => vec![type_name.as_mut()],
      ASTNode::TupleExpression { components, .. } => {
        components.iter_mut().collect()
      }
      ASTNode::UnaryOperation { sub_expression, .. } => {
        vec![sub_expression.as_mut()]
      }
      ASTNode::EnumValue { .. } => vec![],
      ASTNode::Block { statements, .. } => statements.iter_mut().collect(),
      ASTNode::SemanticBlock { statements, .. } => {
        statements.iter_mut().collect()
      }
      ASTNode::Break { .. } => vec![],
      ASTNode::Continue { .. } => vec![],
      ASTNode::DoWhileStatement {
        condition, body, ..
      } => {
        let mut result = vec![condition.as_mut()];
        if let Some(b) = body {
          result.push(b.as_mut());
        }
        result
      }
      ASTNode::EmitStatement { event_call, .. } => vec![event_call.as_mut()],
      ASTNode::ExpressionStatement { expression, .. } => {
        vec![expression.as_mut()]
      }
      ASTNode::ForStatement {
        condition, body, ..
      } => vec![condition.as_mut(), body.as_mut()],
      ASTNode::LoopExpression {
        initialization_expression,
        condition,
        loop_expression,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![];
        if let Some(init) = initialization_expression {
          result.push(init.as_mut());
        }
        if let Some(cond) = condition {
          result.push(cond.as_mut());
        }
        if let Some(loop_expr) = loop_expression {
          result.push(loop_expr.as_mut());
        }
        result
      }
      ASTNode::IfStatement {
        condition,
        true_body,
        false_body,
        ..
      } => {
        let mut result = vec![condition.as_mut(), true_body.as_mut()];
        if let Some(false_b) = false_body {
          result.push(false_b.as_mut());
        }
        result
      }
      ASTNode::InlineAssembly { .. } => vec![],
      ASTNode::PlaceholderStatement { .. } => vec![],
      ASTNode::Return { expression, .. } => expression
        .as_mut()
        .map(|e| vec![e.as_mut()])
        .unwrap_or_default(),
      ASTNode::RevertStatement { error_call, .. } => vec![error_call.as_mut()],
      ASTNode::TryStatement {
        clauses,
        external_call,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![external_call.as_mut()];
        for clause in clauses.iter_mut() {
          result.push(clause);
        }
        result
      }
      ASTNode::UncheckedBlock { statements, .. } => {
        statements.iter_mut().collect()
      }
      ASTNode::VariableDeclarationStatement {
        declarations,
        initial_value,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = declarations.iter_mut().collect();
        if let Some(value) = initial_value {
          result.push(value.as_mut());
        }
        result
      }
      ASTNode::VariableDeclaration {
        type_name, value, ..
      } => {
        let mut result = vec![type_name.as_mut()];
        if let Some(val) = value {
          result.push(val.as_mut());
        }
        result
      }
      ASTNode::WhileStatement {
        condition, body, ..
      } => {
        let mut result = vec![condition.as_mut()];
        if let Some(b) = body {
          result.push(b.as_mut());
        }
        result
      }
      ASTNode::ContractSignature {
        documentation,
        base_contracts,
        directives,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![];
        if let Some(doc) = documentation {
          result.push(doc.as_mut());
        }
        result.extend(base_contracts.iter_mut());
        result.extend(directives.iter_mut());
        result
      }
      ASTNode::ContractDefinition {
        signature, nodes, ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![signature.as_mut()];
        result.extend(nodes.iter_mut());
        result
      }
      ASTNode::FunctionSignature {
        documentation,
        modifiers,
        parameters,
        return_parameters,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![];
        if let Some(doc) = documentation {
          result.push(doc.as_mut());
        }
        result.push(modifiers.as_mut());
        result.push(parameters.as_mut());
        result.push(return_parameters.as_mut());
        result
      }
      ASTNode::FunctionDefinition {
        signature, body, ..
      } => {
        let mut result = vec![signature.as_mut()];
        if let Some(body_node) = body {
          result.push(body_node.as_mut());
        }
        result
      }
      ASTNode::EventDefinition { parameters, .. } => vec![parameters.as_mut()],
      ASTNode::ErrorDefinition { parameters, .. } => vec![parameters.as_mut()],
      ASTNode::ModifierSignature {
        documentation,
        parameters,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![];
        if let Some(doc) = documentation {
          result.push(doc.as_mut());
        }
        result.push(parameters.as_mut());
        result
      }
      ASTNode::ModifierDefinition {
        signature, body, ..
      } => {
        vec![signature.as_mut(), body.as_mut()]
      }
      ASTNode::StructDefinition { members, .. } => members.iter_mut().collect(),
      ASTNode::EnumDefinition { members, .. } => members.iter_mut().collect(),
      ASTNode::UserDefinedValueTypeDefinition {
        underlying_type, ..
      } => vec![underlying_type.as_mut()],
      ASTNode::PragmaDirective { .. } => vec![],
      ASTNode::ImportDirective { .. } => vec![],
      ASTNode::UsingForDirective {
        library_name,
        type_name,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![];
        if let Some(tn) = type_name {
          result.push(tn.as_mut());
        }
        if let Some(lib) = library_name {
          result.push(lib.as_mut());
        }
        result
      }
      ASTNode::SourceUnit { nodes, .. } => nodes.iter_mut().collect(),
      ASTNode::InheritanceSpecifier { base_name, .. } => {
        vec![base_name.as_mut()]
      }
      ASTNode::ElementaryTypeName { .. } => vec![],
      ASTNode::FunctionTypeName {
        parameter_types,
        return_parameter_types,
        ..
      } => {
        vec![parameter_types.as_mut(), return_parameter_types.as_mut()]
      }
      ASTNode::ParameterList { parameters, .. } => {
        parameters.iter_mut().collect()
      }
      ASTNode::ModifierList { modifiers, .. } => modifiers.iter_mut().collect(),
      ASTNode::TryCatchClause {
        block, parameters, ..
      } => {
        let mut result = vec![block.as_mut()];
        if let Some(params) = parameters {
          result.push(params.as_mut());
        }
        result
      }
      ASTNode::ModifierInvocation {
        modifier_name,
        arguments,
        ..
      } => {
        let mut result: Vec<&mut ASTNode> = vec![modifier_name.as_mut()];
        if let Some(args) = arguments {
          result.extend(args.iter_mut());
        }
        result
      }
      ASTNode::UserDefinedTypeName { path_node, .. } => {
        vec![path_node.as_mut()]
      }
      ASTNode::ArrayTypeName { base_type, .. } => vec![base_type.as_mut()],
      ASTNode::Mapping {
        key_type,
        value_type,
        ..
      } => vec![key_type.as_mut(), value_type.as_mut()],
      ASTNode::StructuredDocumentation { .. } => vec![],
      ASTNode::Stub { .. } => vec![],
      ASTNode::Other { nodes, body, .. } => {
        let mut result: Vec<&mut ASTNode> = nodes.iter_mut().collect();
        if let Some(body_node) = body {
          result.push(body_node.as_mut());
        }
        result
      }
      ASTNode::Argument {
        argument,
        parameter,
        ..
      } => {
        let mut result = vec![argument.as_mut()];
        if let Some(param) = parameter {
          result.push(param.as_mut());
        }
        result
      }
    }
  }

  /// Get children nodes, resolving nodes that are stubs to their real nodes
  /// from the nodes map
  pub fn resolve_nodes(
    &self,
    nodes_map: &BTreeMap<topic::Topic, core::Node>,
  ) -> Vec<ASTNode> {
    let nodes = self.nodes();

    nodes
      .iter()
      .map(|node| match node {
        ASTNode::Stub { topic, .. } => {
          if let Some(core::Node::Solidity(ast_node)) = nodes_map.get(topic) {
            ast_node.clone()
          } else {
            (*node).clone()
          }
        }
        _ => (*node).clone(),
      })
      .collect()
  }

  /// Resolve the current node if it is a node stub
  pub fn resolve<'a>(
    &'a self,
    nodes_map: &'a BTreeMap<topic::Topic, core::Node>,
  ) -> &'a ASTNode {
    match self {
      ASTNode::Stub { topic, .. } => {
        if let Some(core::Node::Solidity(ast_node)) = nodes_map.get(topic) {
          ast_node
        } else {
          self
        }
      }
      _ => self,
    }
  }

  /// Returns true if the node is a "containing block" for reference tracking:
  /// SemanticBlock or FunctionSignature.
  pub fn is_containing_block(&self) -> bool {
    matches!(
      self,
      ASTNode::SemanticBlock { .. } | ASTNode::FunctionSignature { .. }
    )
  }

  /// Return the variant name as a static string.
  pub fn type_name(&self) -> &'static str {
    match self {
      ASTNode::Assignment { .. } => "Assignment",
      ASTNode::BinaryOperation { .. } => "BinaryOperation",
      ASTNode::Conditional { .. } => "Conditional",
      ASTNode::ElementaryTypeNameExpression { .. } => {
        "ElementaryTypeNameExpression"
      }
      ASTNode::FunctionCall { .. } => "FunctionCall",
      ASTNode::Argument { .. } => "Argument",
      ASTNode::TypeConversion { .. } => "TypeConversion",
      ASTNode::StructConstructor { .. } => "StructConstructor",
      ASTNode::FunctionCallOptions { .. } => "FunctionCallOptions",
      ASTNode::Identifier { .. } => "Identifier",
      ASTNode::IdentifierPath { .. } => "IdentifierPath",
      ASTNode::IndexAccess { .. } => "IndexAccess",
      ASTNode::IndexRangeAccess { .. } => "IndexRangeAccess",
      ASTNode::Literal { .. } => "Literal",
      ASTNode::MemberAccess { .. } => "MemberAccess",
      ASTNode::NewExpression { .. } => "NewExpression",
      ASTNode::TupleExpression { .. } => "TupleExpression",
      ASTNode::UnaryOperation { .. } => "UnaryOperation",
      ASTNode::EnumValue { .. } => "EnumValue",
      ASTNode::Block { .. } => "Block",
      ASTNode::SemanticBlock { .. } => "SemanticBlock",
      ASTNode::Break { .. } => "Break",
      ASTNode::Continue { .. } => "Continue",
      ASTNode::DoWhileStatement { .. } => "DoWhileStatement",
      ASTNode::EmitStatement { .. } => "EmitStatement",
      ASTNode::ExpressionStatement { .. } => "ExpressionStatement",
      ASTNode::ForStatement { .. } => "ForStatement",
      ASTNode::LoopExpression { .. } => "LoopExpression",
      ASTNode::IfStatement { .. } => "IfStatement",
      ASTNode::InlineAssembly { .. } => "InlineAssembly",
      ASTNode::PlaceholderStatement { .. } => "PlaceholderStatement",
      ASTNode::Return { .. } => "Return",
      ASTNode::RevertStatement { .. } => "RevertStatement",
      ASTNode::TryStatement { .. } => "TryStatement",
      ASTNode::UncheckedBlock { .. } => "UncheckedBlock",
      ASTNode::VariableDeclarationStatement { .. } => {
        "VariableDeclarationStatement"
      }
      ASTNode::WhileStatement { .. } => "WhileStatement",
      ASTNode::ContractSignature { .. } => "ContractSignature",
      ASTNode::FunctionSignature { .. } => "FunctionSignature",
      ASTNode::ModifierSignature { .. } => "ModifierSignature",
      ASTNode::ContractDefinition { .. } => "ContractDefinition",
      ASTNode::FunctionDefinition { .. } => "FunctionDefinition",
      ASTNode::EventDefinition { .. } => "EventDefinition",
      ASTNode::ErrorDefinition { .. } => "ErrorDefinition",
      ASTNode::ModifierDefinition { .. } => "ModifierDefinition",
      ASTNode::StructDefinition { .. } => "StructDefinition",
      ASTNode::EnumDefinition { .. } => "EnumDefinition",
      ASTNode::UserDefinedValueTypeDefinition { .. } => {
        "UserDefinedValueTypeDefinition"
      }
      ASTNode::VariableDeclaration { .. } => "VariableDeclaration",
      ASTNode::PragmaDirective { .. } => "PragmaDirective",
      ASTNode::ImportDirective { .. } => "ImportDirective",
      ASTNode::UsingForDirective { .. } => "UsingForDirective",
      ASTNode::SourceUnit { .. } => "SourceUnit",
      ASTNode::InheritanceSpecifier { .. } => "InheritanceSpecifier",
      ASTNode::ElementaryTypeName { .. } => "ElementaryTypeName",
      ASTNode::FunctionTypeName { .. } => "FunctionTypeName",
      ASTNode::ParameterList { .. } => "ParameterList",
      ASTNode::ModifierList { .. } => "ModifierList",
      ASTNode::TryCatchClause { .. } => "TryCatchClause",
      ASTNode::ModifierInvocation { .. } => "ModifierInvocation",
      ASTNode::UserDefinedTypeName { .. } => "UserDefinedTypeName",
      ASTNode::ArrayTypeName { .. } => "ArrayTypeName",
      ASTNode::Mapping { .. } => "Mapping",
      ASTNode::StructuredDocumentation { .. } => "StructuredDocumentation",
      ASTNode::Stub { .. } => "Stub",
      ASTNode::Other { .. } => "Other",
    }
  }
}

pub fn classify_node_stub_kind(node: &ASTNode) -> StubKind {
  match node {
    ASTNode::Identifier {
      referenced_declaration,
      ..
    }
    | ASTNode::IdentifierPath {
      referenced_declaration,
      ..
    } => StubKind::Identifier {
      referenced_topic: topic::new_node_topic(referenced_declaration),
    },
    ASTNode::MemberAccess { expression, .. } => StubKind::MemberAccess {
      base_kind: Box::new(classify_node_stub_kind(expression)),
    },
    ASTNode::TypeConversion { argument, .. } => {
      let argument_kind = Box::new(classify_node_stub_kind(argument));
      // The argument's placeholder topic is the referenced declaration topic
      // if the argument is identifier-like, otherwise the argument node's own
      // topic.
      let argument_topic = argument_kind
        .placeholder_topic()
        .cloned()
        .unwrap_or_else(|| topic::new_node_topic(&argument.node_id()));
      StubKind::TypeConversion {
        argument_kind,
        argument_topic,
      }
    }
    ASTNode::FunctionCall { expression, .. }
    | ASTNode::StructConstructor { expression, .. }
    | ASTNode::FunctionCallOptions { expression, .. } => {
      StubKind::CompoundExpression {
        expression_kind: Box::new(classify_node_stub_kind(expression)),
      }
    }
    ASTNode::Literal { .. } => StubKind::Literal,
    ASTNode::VariableDeclarationStatement { .. }
    | ASTNode::VariableDeclaration { .. }
    | ASTNode::ContractDefinition { .. }
    | ASTNode::FunctionDefinition { .. }
    | ASTNode::ModifierDefinition { .. }
    | ASTNode::StructDefinition { .. }
    | ASTNode::EnumDefinition { .. }
    | ASTNode::EventDefinition { .. }
    | ASTNode::ErrorDefinition { .. }
    | ASTNode::UserDefinedValueTypeDefinition { .. } => StubKind::Declaration,
    _ => StubKind::Other,
  }
}

fn node_to_stub(node: &ASTNode) -> ASTNode {
  ASTNode::Stub {
    node_id: node.node_id(),
    src_location: node.src_location().clone(),
    topic: topic::new_node_topic(&node.node_id()),
    kind: classify_node_stub_kind(node),
  }
}

pub fn children_to_stubs(node: ASTNode) -> ASTNode {
  match node {
    ASTNode::Assignment {
      node_id,
      src_location,
      operator,
      right_hand_side,
      left_hand_side,
    } => ASTNode::Assignment {
      node_id: node_id,
      src_location: src_location,
      operator: operator,
      right_hand_side: Box::new(node_to_stub(&right_hand_side)),
      left_hand_side: Box::new(node_to_stub(&left_hand_side)),
    },
    ASTNode::BinaryOperation {
      node_id,
      src_location,
      left_expression,
      operator,
      right_expression,
      type_descriptions,
    } => ASTNode::BinaryOperation {
      node_id: node_id,
      src_location: src_location,
      left_expression: Box::new(node_to_stub(&left_expression)),
      operator: operator,
      right_expression: Box::new(node_to_stub(&right_expression)),
      type_descriptions: type_descriptions,
    },
    ASTNode::Conditional {
      node_id,
      src_location,
      condition,
      true_expression,
      false_expression,
    } => ASTNode::Conditional {
      node_id: node_id,
      src_location: src_location,
      condition: Box::new(node_to_stub(&condition)),
      true_expression: Box::new(node_to_stub(&true_expression)),
      false_expression: match false_expression {
        Some(expr) => Some(Box::new(node_to_stub(&expr))),
        None => None,
      },
    },
    ASTNode::ElementaryTypeNameExpression {
      node_id,
      src_location,
      type_descriptions,
      type_name,
    } => ASTNode::ElementaryTypeNameExpression {
      node_id: node_id,
      src_location: src_location,
      type_descriptions: type_descriptions,
      type_name: Box::new(node_to_stub(&type_name)),
    },
    ASTNode::FunctionCall {
      node_id,
      src_location,
      arguments,
      expression,
      name_locations,
      names,
      try_call,
      type_descriptions,
      referenced_return_declarations,
    } => ASTNode::FunctionCall {
      node_id: node_id,
      src_location: src_location,
      arguments: arguments.iter().map(|n| node_to_stub(n)).collect(),
      expression: Box::new(node_to_stub(&expression)),
      name_locations: name_locations,
      names: names,
      try_call: try_call,
      type_descriptions: type_descriptions,
      referenced_return_declarations: referenced_return_declarations,
    },
    ASTNode::TypeConversion {
      node_id,
      src_location,
      argument,
      expression,
      name_locations,
      names,
      try_call,
      type_descriptions,
    } => ASTNode::TypeConversion {
      node_id: node_id,
      src_location: src_location,
      argument: Box::new(node_to_stub(&argument)),
      expression: Box::new(node_to_stub(&expression)),
      name_locations: name_locations,
      names: names,
      try_call: try_call,
      type_descriptions: type_descriptions,
    },
    ASTNode::StructConstructor {
      node_id,
      src_location,
      arguments,
      expression,
      name_locations,
      names,
      try_call,
      type_descriptions,
    } => ASTNode::StructConstructor {
      node_id: node_id,
      src_location: src_location,
      arguments: arguments.iter().map(|n| node_to_stub(n)).collect(),
      expression: Box::new(node_to_stub(&expression)),
      name_locations: name_locations,
      names: names,
      try_call: try_call,
      type_descriptions: type_descriptions,
    },
    ASTNode::FunctionCallOptions {
      node_id,
      src_location,
      expression,
      options,
    } => ASTNode::FunctionCallOptions {
      node_id: node_id,
      src_location: src_location,
      expression: Box::new(node_to_stub(&expression)),
      options: options.iter().map(|n| node_to_stub(n)).collect(),
    },
    ASTNode::Identifier {
      node_id,
      src_location,
      name,
      overloaded_declarations,
      referenced_declaration,
    } => ASTNode::Identifier {
      node_id: node_id,
      src_location: src_location,
      name: name,
      overloaded_declarations: overloaded_declarations,
      referenced_declaration: referenced_declaration,
    },
    ASTNode::IdentifierPath {
      node_id,
      src_location,
      name,
      name_locations,
      referenced_declaration,
    } => ASTNode::IdentifierPath {
      node_id: node_id,
      src_location: src_location,
      name: name,
      name_locations: name_locations,
      referenced_declaration: referenced_declaration,
    },
    ASTNode::IndexAccess {
      node_id,
      src_location,
      base_expression,
      index_expression,
    } => ASTNode::IndexAccess {
      node_id: node_id,
      src_location: src_location,
      base_expression: Box::new(node_to_stub(&base_expression)),
      index_expression: match index_expression {
        Some(expr) => Some(Box::new(node_to_stub(&expr))),
        None => None,
      },
    },
    ASTNode::IndexRangeAccess {
      node_id,
      src_location,
      nodes,
      body,
    } => ASTNode::IndexRangeAccess {
      node_id: node_id,
      src_location: src_location,
      nodes: nodes.iter().map(|n| node_to_stub(n)).collect(),
      body: match body {
        Some(b) => Some(Box::new(node_to_stub(&b))),
        None => None,
      },
    },
    ASTNode::Literal {
      node_id,
      src_location,
      hex_value,
      kind,
      type_descriptions,
      value,
    } => ASTNode::Literal {
      node_id: node_id,
      src_location: src_location,
      hex_value: hex_value,
      kind: kind,
      type_descriptions: type_descriptions,
      value: value,
    },
    ASTNode::MemberAccess {
      node_id,
      src_location,
      expression,
      member_location,
      member_name,
      referenced_declaration,
      type_descriptions,
    } => ASTNode::MemberAccess {
      node_id: node_id,
      src_location: src_location,
      expression: Box::new(node_to_stub(&expression)),
      member_location: member_location,
      member_name: member_name,
      referenced_declaration: referenced_declaration,
      type_descriptions: type_descriptions,
    },
    ASTNode::NewExpression {
      node_id,
      src_location,
      type_name,
    } => ASTNode::NewExpression {
      node_id: node_id,
      src_location: src_location,
      type_name: Box::new(node_to_stub(&type_name)),
    },
    ASTNode::TupleExpression {
      node_id,
      src_location,
      components,
    } => ASTNode::TupleExpression {
      node_id: node_id,
      src_location: src_location,
      components: components.iter().map(|n| node_to_stub(n)).collect(),
    },
    ASTNode::UnaryOperation {
      node_id,
      src_location,
      prefix,
      operator,
      sub_expression,
    } => ASTNode::UnaryOperation {
      node_id: node_id,
      src_location: src_location,
      prefix: prefix,
      operator: operator,
      sub_expression: Box::new(node_to_stub(&sub_expression)),
    },
    ASTNode::EnumValue {
      node_id,
      src_location,
      name,
      name_location,
    } => ASTNode::EnumValue {
      node_id: node_id,
      src_location: src_location,
      name: name,
      name_location: name_location,
    },
    ASTNode::Block {
      node_id,
      src_location,
      statements,
    } => ASTNode::Block {
      node_id: node_id,
      src_location: src_location,
      statements: statements.iter().map(|n| node_to_stub(n)).collect(),
    },
    ASTNode::SemanticBlock {
      node_id,
      src_location,
      documentation,
      statements,
    } => ASTNode::SemanticBlock {
      node_id: node_id,
      src_location: src_location,
      documentation: documentation,
      statements: statements.iter().map(|n| node_to_stub(n)).collect(),
    },
    ASTNode::Break {
      node_id,
      src_location,
    } => ASTNode::Break {
      node_id: node_id,
      src_location: src_location,
    },
    ASTNode::Continue {
      node_id,
      src_location,
    } => ASTNode::Continue {
      node_id: node_id,
      src_location: src_location,
    },
    ASTNode::DoWhileStatement {
      node_id,
      src_location,
      condition,
      body,
    } => ASTNode::DoWhileStatement {
      node_id: node_id,
      src_location: src_location,
      condition: Box::new(node_to_stub(&condition)),
      body: match body {
        Some(b) => Some(Box::new(node_to_stub(&b))),
        None => None,
      },
    },
    ASTNode::EmitStatement {
      node_id,
      src_location,
      event_call,
    } => ASTNode::EmitStatement {
      node_id: node_id,
      src_location: src_location,
      event_call: Box::new(node_to_stub(&event_call)),
    },
    ASTNode::ExpressionStatement {
      node_id,
      src_location,
      expression,
    } => ASTNode::ExpressionStatement {
      node_id: node_id,
      src_location: src_location,
      expression: Box::new(node_to_stub(&expression)),
    },
    ASTNode::ForStatement {
      node_id,
      src_location,
      condition,
      body,
    } => ASTNode::ForStatement {
      node_id: node_id,
      src_location: src_location,
      condition: Box::new(node_to_stub(&condition)),
      body: Box::new(node_to_stub(&body)),
    },
    ASTNode::LoopExpression {
      node_id,
      src_location,
      initialization_expression,
      condition,
      loop_expression,
      is_simple_counter_loop,
    } => ASTNode::LoopExpression {
      node_id: node_id,
      src_location: src_location,
      initialization_expression: match initialization_expression {
        Some(ie) => Some(Box::new(node_to_stub(&ie))),
        None => None,
      },
      condition: match condition {
        Some(c) => Some(Box::new(node_to_stub(&c))),
        None => None,
      },
      loop_expression: match loop_expression {
        Some(le) => Some(Box::new(node_to_stub(&le))),
        None => None,
      },
      is_simple_counter_loop: is_simple_counter_loop,
    },
    ASTNode::IfStatement {
      node_id,
      src_location,
      condition,
      true_body,
      false_body,
    } => ASTNode::IfStatement {
      node_id: node_id,
      src_location: src_location,
      condition: Box::new(node_to_stub(&condition)),
      true_body: Box::new(node_to_stub(&true_body)),
      false_body: match false_body {
        Some(fb) => Some(Box::new(node_to_stub(&fb))),
        None => None,
      },
    },
    ASTNode::InlineAssembly {
      node_id,
      src_location,
    } => ASTNode::InlineAssembly {
      node_id: node_id,
      src_location: src_location,
    },
    ASTNode::PlaceholderStatement {
      node_id,
      src_location,
    } => ASTNode::PlaceholderStatement {
      node_id: node_id,
      src_location: src_location,
    },
    ASTNode::Return {
      node_id,
      src_location,
      expression,
      function_return_parameters,
    } => ASTNode::Return {
      node_id: node_id,
      src_location: src_location,
      expression: match expression {
        Some(e) => Some(Box::new(node_to_stub(&e))),
        None => None,
      },
      function_return_parameters: function_return_parameters,
    },
    ASTNode::RevertStatement {
      node_id,
      src_location,
      error_call,
    } => ASTNode::RevertStatement {
      node_id: node_id,
      src_location: src_location,
      error_call: Box::new(node_to_stub(&error_call)),
    },
    ASTNode::TryStatement {
      node_id,
      src_location,
      clauses,
      external_call,
    } => ASTNode::TryStatement {
      node_id: node_id,
      src_location: src_location,
      clauses: clauses.iter().map(|n| node_to_stub(n)).collect(),
      external_call: Box::new(node_to_stub(&external_call)),
    },
    ASTNode::UncheckedBlock {
      node_id,
      src_location,
      statements,
    } => ASTNode::UncheckedBlock {
      node_id: node_id,
      src_location: src_location,
      statements: statements.iter().map(|n| node_to_stub(n)).collect(),
    },
    ASTNode::VariableDeclarationStatement {
      node_id,
      src_location,
      declarations,
      initial_value,
    } => ASTNode::VariableDeclarationStatement {
      node_id: node_id,
      src_location: src_location,
      declarations: declarations.iter().map(|n| node_to_stub(n)).collect(),
      initial_value: match initial_value {
        Some(iv) => Some(Box::new(node_to_stub(&iv))),
        None => None,
      },
    },
    ASTNode::VariableDeclaration {
      node_id,
      src_location,
      constant,
      function_selector,
      mutability,
      name,
      name_location,
      scope,
      state_variable,
      storage_location,
      type_name,
      value,
      visibility,
      parameter_variable,
      implementation_declaration,
      base_functions,
      struct_field,
    } => ASTNode::VariableDeclaration {
      node_id: node_id,
      src_location: src_location,
      constant: constant,
      function_selector: function_selector,
      mutability: mutability,
      name: name,
      name_location: name_location,
      scope: scope,
      state_variable: state_variable,
      storage_location: storage_location,
      type_name: Box::new(node_to_stub(&type_name)),
      value: match value {
        Some(v) => Some(Box::new(node_to_stub(&v))),
        None => None,
      },
      visibility: visibility,
      parameter_variable: parameter_variable,
      implementation_declaration: implementation_declaration,
      base_functions: base_functions,
      struct_field: struct_field,
    },
    ASTNode::WhileStatement {
      node_id,
      src_location,
      condition,
      body,
    } => ASTNode::WhileStatement {
      node_id: node_id,
      src_location: src_location,
      condition: Box::new(node_to_stub(&condition)),
      body: match body {
        Some(b) => Some(Box::new(node_to_stub(&b))),
        None => None,
      },
    },
    ASTNode::ContractSignature {
      node_id,
      src_location,
      documentation,
      name,
      name_location,
      declaration_id,
      contract_kind,
      abstract_,
      base_contracts,
      directives,
    } => ASTNode::ContractSignature {
      node_id: node_id,
      src_location: src_location,
      documentation: match documentation {
        Some(d) => Some(Box::new(node_to_stub(&d))),
        None => None,
      },
      name: name,
      name_location: name_location,
      declaration_id: declaration_id,
      contract_kind: contract_kind,
      abstract_: abstract_,
      base_contracts: base_contracts.iter().map(|n| node_to_stub(n)).collect(),
      directives: directives.iter().map(|n| node_to_stub(n)).collect(),
    },
    ASTNode::ContractDefinition {
      node_id,
      src_location,
      signature,
      nodes,
    } => ASTNode::ContractDefinition {
      node_id: node_id,
      src_location: src_location,
      signature: Box::new(node_to_stub(&signature)),
      nodes: nodes.iter().map(|n| node_to_stub(n)).collect(),
    },
    ASTNode::FunctionSignature {
      node_id,
      src_location,
      documentation,
      kind,
      modifiers,
      name,
      name_location,
      declaration_id,
      parameters,
      return_parameters,
      scope,
      state_mutability,
      virtual_,
      visibility,
      implementation_declaration,
    } => ASTNode::FunctionSignature {
      node_id: node_id,
      src_location: src_location,
      documentation: match documentation {
        Some(d) => Some(Box::new(node_to_stub(&d))),
        None => None,
      },
      kind: kind,
      modifiers: Box::new(node_to_stub(&modifiers)),
      name: name,
      name_location: name_location,
      declaration_id: declaration_id,
      parameters: Box::new(node_to_stub(&parameters)),
      return_parameters: Box::new(node_to_stub(&return_parameters)),
      scope: scope,
      state_mutability: state_mutability,
      virtual_: virtual_,
      visibility: visibility,
      implementation_declaration,
    },
    ASTNode::FunctionDefinition {
      node_id,
      src_location,
      signature,
      implemented,
      body,
    } => ASTNode::FunctionDefinition {
      node_id: node_id,
      src_location: src_location,
      signature: Box::new(node_to_stub(&signature)),
      implemented: implemented,
      body: match body {
        Some(b) => Some(Box::new(node_to_stub(&b))),
        None => None,
      },
    },
    ASTNode::EventDefinition {
      node_id,
      src_location,
      name,
      name_location,
      parameters,
    } => ASTNode::EventDefinition {
      node_id: node_id,
      src_location: src_location,
      name: name,
      name_location: name_location,
      parameters: Box::new(node_to_stub(&parameters)),
    },
    ASTNode::ErrorDefinition {
      node_id,
      src_location,
      name,
      name_location,
      parameters,
    } => ASTNode::ErrorDefinition {
      node_id: node_id,
      src_location: src_location,
      name: name,
      name_location: name_location,
      parameters: Box::new(node_to_stub(&parameters)),
    },
    ASTNode::ModifierSignature {
      node_id,
      src_location,
      documentation,
      name,
      name_location,
      declaration_id,
      parameters,
      virtual_,
      visibility,
      implementation_declaration,
    } => ASTNode::ModifierSignature {
      node_id: node_id,
      src_location: src_location,
      documentation: match documentation {
        Some(d) => Some(Box::new(node_to_stub(&d))),
        None => None,
      },
      name: name,
      name_location: name_location,
      declaration_id: declaration_id,
      parameters: Box::new(node_to_stub(&parameters)),
      virtual_: virtual_,
      visibility: visibility,
      implementation_declaration,
    },
    ASTNode::ModifierDefinition {
      node_id,
      src_location,
      signature,
      body,
    } => ASTNode::ModifierDefinition {
      node_id: node_id,
      src_location: src_location,
      signature: Box::new(node_to_stub(&signature)),
      body: Box::new(node_to_stub(&body)),
    },
    ASTNode::StructDefinition {
      node_id,
      src_location,
      members,
      canonical_name,
      name,
      name_location,
      visibility,
    } => ASTNode::StructDefinition {
      node_id: node_id,
      src_location: src_location,
      members: members.iter().map(|n| node_to_stub(n)).collect(),
      canonical_name: canonical_name,
      name: name,
      name_location: name_location,
      visibility: visibility,
    },
    ASTNode::EnumDefinition {
      node_id,
      src_location,
      members,
      canonical_name,
      name,
      name_location,
    } => ASTNode::EnumDefinition {
      node_id: node_id,
      src_location: src_location,
      members: members.iter().map(|n| node_to_stub(n)).collect(),
      canonical_name: canonical_name,
      name: name,
      name_location: name_location,
    },
    ASTNode::UserDefinedValueTypeDefinition {
      node_id,
      src_location,
      name,
      underlying_type,
    } => ASTNode::UserDefinedValueTypeDefinition {
      node_id: node_id,
      src_location: src_location,
      name: name,
      underlying_type: Box::new(node_to_stub(&underlying_type)),
    },
    ASTNode::PragmaDirective {
      node_id,
      src_location,
      literals,
    } => ASTNode::PragmaDirective {
      node_id: node_id,
      src_location: src_location,
      literals: literals,
    },
    ASTNode::ImportDirective {
      node_id,
      src_location,
      absolute_path,
      file,
      source_unit,
    } => ASTNode::ImportDirective {
      node_id: node_id,
      src_location: src_location,
      absolute_path: absolute_path,
      file: file,
      source_unit: source_unit,
    },
    ASTNode::UsingForDirective {
      node_id,
      src_location,
      global,
      library_name,
      type_name,
    } => ASTNode::UsingForDirective {
      node_id: node_id,
      src_location: src_location,
      global: global,
      library_name: match library_name {
        Some(ln) => Some(Box::new(node_to_stub(&ln))),
        None => None,
      },
      type_name: match type_name {
        Some(tn) => Some(Box::new(node_to_stub(&tn))),
        None => None,
      },
    },
    ASTNode::SourceUnit {
      node_id,
      src_location,
      nodes,
    } => ASTNode::SourceUnit {
      node_id: node_id,
      src_location: src_location,
      nodes: nodes.iter().map(|n| node_to_stub(n)).collect(),
    },
    ASTNode::InheritanceSpecifier {
      node_id,
      src_location,
      base_name,
    } => ASTNode::InheritanceSpecifier {
      node_id: node_id,
      src_location: src_location,
      base_name: Box::new(node_to_stub(&base_name)),
    },
    ASTNode::ElementaryTypeName {
      node_id,
      src_location,
      name,
    } => ASTNode::ElementaryTypeName {
      node_id: node_id,
      src_location: src_location,
      name: name,
    },
    ASTNode::FunctionTypeName {
      node_id,
      src_location,
      parameter_types,
      return_parameter_types,
      state_mutability,
      visibility,
    } => ASTNode::FunctionTypeName {
      node_id: node_id,
      src_location: src_location,
      parameter_types: Box::new(node_to_stub(&parameter_types)),
      return_parameter_types: Box::new(node_to_stub(&return_parameter_types)),
      state_mutability: state_mutability,
      visibility: visibility,
    },
    ASTNode::ParameterList {
      node_id,
      src_location,
      parameters,
      is_return_parameters,
    } => ASTNode::ParameterList {
      node_id: node_id,
      src_location: src_location,
      parameters: parameters.iter().map(|n| node_to_stub(n)).collect(),
      is_return_parameters: is_return_parameters,
    },
    ASTNode::ModifierList {
      node_id,
      src_location,
      modifiers,
    } => ASTNode::ModifierList {
      node_id: node_id,
      src_location: src_location,
      modifiers: modifiers.iter().map(|n| node_to_stub(n)).collect(),
    },
    ASTNode::TryCatchClause {
      node_id,
      src_location,
      error_name,
      block,
      parameters,
    } => ASTNode::TryCatchClause {
      node_id: node_id,
      src_location: src_location,
      error_name: error_name,
      block: Box::new(node_to_stub(&block)),
      parameters: match parameters {
        Some(p) => Some(Box::new(node_to_stub(&p))),
        None => None,
      },
    },
    ASTNode::ModifierInvocation {
      node_id,
      src_location,
      modifier_name,
      arguments,
    } => ASTNode::ModifierInvocation {
      node_id: node_id,
      src_location: src_location,
      modifier_name: Box::new(node_to_stub(&modifier_name)),
      arguments: match arguments {
        Some(args) => Some(args.iter().map(|n| node_to_stub(n)).collect()),
        None => None,
      },
    },
    ASTNode::UserDefinedTypeName {
      node_id,
      src_location,
      path_node,
      referenced_declaration,
    } => ASTNode::UserDefinedTypeName {
      node_id: node_id,
      src_location: src_location,
      path_node: Box::new(node_to_stub(&path_node)),
      referenced_declaration: referenced_declaration,
    },
    ASTNode::ArrayTypeName {
      node_id,
      src_location,
      base_type,
    } => ASTNode::ArrayTypeName {
      node_id: node_id,
      src_location: src_location,
      base_type: Box::new(node_to_stub(&base_type)),
    },
    ASTNode::Mapping {
      node_id,
      src_location,
      key_name,
      key_name_location,
      key_type,
      value_name,
      value_name_location,
      value_type,
    } => ASTNode::Mapping {
      node_id: node_id,
      src_location: src_location,
      key_name: key_name,
      key_name_location: key_name_location,
      key_type: Box::new(node_to_stub(&key_type)),
      value_name: value_name,
      value_name_location: value_name_location,
      value_type: Box::new(node_to_stub(&value_type)),
    },
    ASTNode::StructuredDocumentation {
      node_id,
      src_location,
      text,
    } => ASTNode::StructuredDocumentation {
      node_id: node_id,
      src_location: src_location,
      text: text,
    },
    ASTNode::Stub {
      node_id,
      src_location,
      topic,
      kind,
    } => ASTNode::Stub {
      node_id: node_id,
      src_location: src_location,
      topic: topic,
      kind: kind,
    },
    ASTNode::Other {
      node_id,
      src_location,
      nodes,
      body,
      node_type,
    } => ASTNode::Other {
      node_id: node_id,
      src_location: src_location,
      nodes: nodes.iter().map(|n| node_to_stub(n)).collect(),
      body: match body {
        Some(b) => Some(Box::new(node_to_stub(&b))),
        None => None,
      },
      node_type: node_type,
    },
    ASTNode::Argument {
      node_id,
      src_location,
      parameter: referenced_parameter,
      argument,
    } => ASTNode::Argument {
      node_id: node_id,
      src_location: src_location,
      parameter: referenced_parameter,
      argument: Box::new(node_to_stub(&argument)),
    },
  }
}

// Helper functions for JSON parsing
fn get_required_i32(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<i32, String> {
  val
    .get(field_name)
    .and_then(|v| v.as_i64())
    .map(|v| v as i32)
    .ok_or_else(|| {
      let available_fields: Vec<&str> = val
        .as_object()
        .map(|obj| obj.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();
      format!(
        "Missing or invalid {} field. Available fields: {:?}",
        field_name, available_fields
      )
    })
}

fn get_optional_i32_vec(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<Vec<i32>, String> {
  match val.get(field_name) {
    Some(v) => {
      if v.is_null() {
        Ok(Vec::new())
      } else {
        v.as_array()
          .ok_or_else(|| format!("Field '{}' is not an array", field_name))
          .map(|arr| {
            arr
              .iter()
              .filter_map(|item| item.as_i64().map(|n| n as i32))
              .collect()
          })
      }
    }
    None => Ok(Vec::new()),
  }
}

// Helper functions with node type context for better error messages
fn get_required_i32_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<i32, String> {
  get_required_i32(val, field_name)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_required_string_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<String, String> {
  get_required_string(val, field_name)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_optional_string_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<Option<String>, String> {
  match val.get(field_name) {
    Some(v) => {
      if v.is_null() {
        Ok(None)
      } else {
        v.as_str()
          .map(|s| {
            if s.is_empty() {
              None
            } else {
              Some(s.to_string())
            }
          })
          .ok_or_else(|| {
            format!(
              "Error parsing {} node: Invalid {} field type",
              node_type, field_name
            )
          })
      }
    }
    None => Ok(None),
  }
}

fn get_required_bool_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<bool, String> {
  get_required_bool(val, field_name)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_required_source_location_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<SourceLocation, String> {
  get_required_source_location(val, field_name)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_required_enum_with_context<T: FromStr>(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<T, String>
where
  T::Err: std::fmt::Debug,
{
  get_required_enum(val, field_name)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_required_node_vec_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
  context: &ParserContext,
) -> Result<Vec<ASTNode>, String> {
  get_required_node_vec(val, field_name, context)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_optional_node_vec_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
  context: &ParserContext,
) -> Result<Option<Vec<ASTNode>>, String> {
  get_optional_node_vec(val, field_name, context)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_required_node_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
  context: &ParserContext,
) -> Result<Box<ASTNode>, String> {
  get_required_node(val, field_name, context)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_optional_node_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
  context: &ParserContext,
) -> Result<Option<Box<ASTNode>>, String> {
  match val.get(field_name) {
    Some(v) => {
      if v.is_null() {
        Ok(None)
      } else {
        node_from_json(v, context)
          .map(|node| Some(Box::new(node)))
          .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
      }
    }
    None => Ok(None),
  }
}

fn get_required_parameter_variable_declaration_vec_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
  context: &ParserContext,
) -> Result<Vec<ASTNode>, String> {
  let nodes = get_required_node_vec(val, field_name, context)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))?;

  // Set parameter_variable to true for all VariableDeclaration nodes
  let updated_nodes = nodes
    .into_iter()
    .map(|node| match node {
      ASTNode::VariableDeclaration {
        node_id,
        src_location,
        constant,
        function_selector,
        mutability,
        name,
        name_location,
        scope,
        state_variable,
        storage_location,
        type_name,
        value,
        visibility,
        implementation_declaration,
        base_functions,
        ..
      } => ASTNode::VariableDeclaration {
        node_id,
        src_location,
        constant,
        function_selector,
        mutability,
        name,
        name_location,
        scope,
        state_variable,
        storage_location,
        type_name,
        value,
        visibility,
        parameter_variable: context.signature_parent_node.get(),
        implementation_declaration,
        base_functions,
        struct_field: false,
      },
      _ => node,
    })
    .collect();

  Ok(updated_nodes)
}

fn get_required_struct_field_variable_declaration_vec_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
  context: &ParserContext,
) -> Result<Vec<ASTNode>, String> {
  let nodes = get_required_node_vec(val, field_name, context)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))?;

  // Set struct_field to true for all VariableDeclaration nodes
  let updated_nodes = nodes
    .into_iter()
    .map(|node| match node {
      ASTNode::VariableDeclaration {
        node_id,
        src_location,
        constant,
        function_selector,
        mutability,
        name,
        name_location,
        scope,
        state_variable,
        storage_location,
        type_name,
        value,
        visibility,
        parameter_variable,
        implementation_declaration,
        base_functions,
        ..
      } => ASTNode::VariableDeclaration {
        node_id,
        src_location,
        constant,
        function_selector,
        mutability,
        name,
        name_location,
        scope,
        state_variable,
        storage_location,
        type_name,
        value,
        visibility,
        parameter_variable,
        implementation_declaration,
        base_functions,
        struct_field: true,
      },
      _ => node,
    })
    .collect();

  Ok(updated_nodes)
}

fn get_required_string_vec_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<Vec<String>, String> {
  get_required_string_vec(val, field_name)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_required_i32_vec_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<Vec<i32>, String> {
  get_required_i32_vec(val, field_name)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_required_source_location_vec_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<Vec<SourceLocation>, String> {
  get_required_source_location_vec(val, field_name)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_required_type_descriptions_with_context(
  val: &serde_json::Value,
  field_name: &str,
  node_type: &str,
) -> Result<TypeDescriptions, String> {
  get_required_type_descriptions(val, field_name)
    .map_err(|e| format!("Error parsing {} node: {}", node_type, e))
}

fn get_required_string(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<String, String> {
  val
    .get(field_name)
    .and_then(|v| v.as_str())
    .map(|s| s.to_string())
    .ok_or_else(|| {
      let available_fields: Vec<&str> = val
        .as_object()
        .map(|obj| obj.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();
      format!(
        "Missing or invalid {} field. Available fields: {:?}",
        field_name, available_fields
      )
    })
}

fn get_required_bool(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<bool, String> {
  val
    .get(field_name)
    .and_then(|v| v.as_bool())
    .ok_or_else(|| {
      let available_fields: Vec<&str> = val
        .as_object()
        .map(|obj| obj.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();
      format!(
        "Missing or invalid {} field. Available fields: {:?}",
        field_name, available_fields
      )
    })
}

fn get_required_source_location(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<SourceLocation, String> {
  val
    .get(field_name)
    .and_then(|v| v.as_str())
    .ok_or_else(|| format!("Missing {} field: {:?}", field_name, val))
    .and_then(|v| SourceLocation::from_str(v))
}

fn get_required_enum<T: FromStr>(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<T, String>
where
  T::Err: std::fmt::Debug,
{
  val
    .get(field_name)
    .and_then(|v| v.as_str())
    .ok_or_else(|| {
      let available_fields: Vec<&str> = val
        .as_object()
        .map(|obj| obj.keys().map(|k| k.as_str()).collect())
        .unwrap_or_default();
      format!(
        "Missing {} field. Available fields: {:?}",
        field_name, available_fields
      )
    })
    .and_then(|s| {
      s.parse()
        .map_err(|e| format!("Failed to parse {} '{}': {:?}", field_name, s, e))
    })
}

fn get_required_node_vec(
  val: &serde_json::Value,
  field_name: &str,
  context: &ParserContext,
) -> Result<Vec<ASTNode>, String> {
  val
    .get(field_name)
    .and_then(|v| v.as_array())
    .ok_or_else(|| {
      format!("Missing or invalid {} field: {:?}", field_name, val)
    })
    .and_then(|arr| {
      let filtered: Vec<_> =
        arr.iter().filter(|item| !item.is_null()).collect();

      filtered
        .into_iter()
        .map(|item| node_from_json(item, context))
        .collect::<Result<Vec<ASTNode>, String>>()
    })
}

fn get_optional_node_vec(
  val: &serde_json::Value,
  field_name: &str,
  context: &ParserContext,
) -> Result<Option<Vec<ASTNode>>, String> {
  match val.get(field_name) {
    Some(v) => {
      if v.is_null() {
        Ok(None)
      } else {
        v.as_array()
          .ok_or_else(|| format!("Field '{}' is not an array", field_name))
          .and_then(|arr| {
            let filtered: Vec<_> =
              arr.iter().filter(|item| !item.is_null()).collect();

            filtered
              .into_iter()
              .map(|item| node_from_json(item, context))
              .collect::<Result<Vec<ASTNode>, String>>()
              .map(Some)
          })
      }
    }
    None => Ok(None),
  }
}

fn get_required_node(
  val: &serde_json::Value,
  field_name: &str,
  context: &ParserContext,
) -> Result<Box<ASTNode>, String> {
  let available_fields: Vec<String> = val
    .as_object()
    .unwrap_or(&serde_json::Map::new())
    .keys()
    .cloned()
    .collect();

  val
    .get(field_name)
    .ok_or_else(|| {
      format!(
        "Missing {} field. Available fields: {:?}",
        field_name, available_fields
      )
    })
    .and_then(|v| node_from_json(v, context))
    .map(Box::new)
}

fn get_optional_node(
  val: &serde_json::Value,
  field_name: &str,
  context: &ParserContext,
) -> Result<Option<Box<ASTNode>>, String> {
  match val.get(field_name) {
    Some(v) => {
      if v.is_null() {
        Ok(None)
      } else {
        node_from_json(v, context).map(|node| Some(Box::new(node)))
      }
    }
    None => Ok(None),
  }
}

fn get_required_string_vec(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<Vec<String>, String> {
  val
    .get(field_name)
    .and_then(|v| v.as_array())
    .ok_or_else(|| {
      format!("Missing or invalid {} field: {:?}", field_name, val)
    })
    .and_then(|arr| {
      arr
        .iter()
        .map(|item| {
          item.as_str().map(|s| s.to_string()).ok_or_else(|| {
            format!("Invalid string in {} array: {:?}", field_name, item)
          })
        })
        .collect::<Result<Vec<String>, String>>()
    })
}

fn get_required_i32_vec(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<Vec<i32>, String> {
  val
    .get(field_name)
    .and_then(|v| v.as_array())
    .ok_or_else(|| {
      format!("Missing or invalid {} field: {:?}", field_name, val)
    })
    .and_then(|arr| {
      arr
        .iter()
        .map(|item| {
          item.as_i64().map(|i| i as i32).ok_or_else(|| {
            format!("Invalid integer in {} array: {:?}", field_name, item)
          })
        })
        .collect::<Result<Vec<i32>, String>>()
    })
}

fn get_required_source_location_vec(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<Vec<SourceLocation>, String> {
  val
    .get(field_name)
    .and_then(|v| v.as_array())
    .ok_or_else(|| {
      format!("Missing or invalid {} field: {:?}", field_name, val)
    })
    .and_then(|arr| {
      arr
        .iter()
        .map(|item| {
          item
            .as_str()
            .ok_or_else(|| {
              format!(
                "Invalid source location in {} array: {:?}",
                field_name, item
              )
            })
            .and_then(|s| SourceLocation::from_str(s))
        })
        .collect::<Result<Vec<SourceLocation>, String>>()
    })
}

fn get_required_type_descriptions(
  val: &serde_json::Value,
  field_name: &str,
) -> Result<TypeDescriptions, String> {
  val
    .get(field_name)
    .ok_or_else(|| format!("Missing {} field: {:?}", field_name, val))
    .and_then(|v| TypeDescriptions::from_json(v))
}

fn find_semantic_breaks(source: &str, statements: &[ASTNode]) -> Vec<usize> {
  let mut breaks = vec![];

  if statements.len() <= 1 {
    return breaks;
  }

  for i in 0..statements.len() - 1 {
    let current_end = statements[i].src_location().start.unwrap_or(0)
      + statements[i].src_location().length.unwrap_or(0);
    let next_start = statements[i + 1].src_location().start.unwrap_or(0);

    if current_end < next_start {
      let between_text = &source[current_end..next_start];
      let newline_count = between_text.matches('\n').count();

      // Two or more newlines indicate a semantic break
      if newline_count >= 2 {
        breaks.push(i + 1);
      }
    }
  }

  breaks
}

fn extract_block_documentation(
  source: &str,
  group_start: usize,
  first_statement_start: usize,
) -> Option<String> {
  if group_start >= first_statement_start {
    return None;
  }

  let text_before = &source[group_start..first_statement_start];
  let lines: Vec<&str> = text_before.lines().collect();
  let mut doc_lines = vec![];

  for line in lines {
    let trimmed = line.trim();
    if trimmed.starts_with("//") {
      doc_lines.push(trimmed.trim_start_matches("//").trim());
    } else if trimmed.starts_with("/*") && trimmed.ends_with("*/") {
      let content = trimmed
        .trim_start_matches("/*")
        .trim_end_matches("*/")
        .trim();
      doc_lines.push(content);
    } else if !trimmed.is_empty() && !doc_lines.is_empty() {
      // Non-comment, non-empty line breaks the documentation block
      break;
    }
  }

  if doc_lines.is_empty() {
    None
  } else {
    Some(doc_lines.join("\n"))
  }
}

static mut NEXT_GENERATED_NODE_ID: i32 = -100; // Run negative to avoid conflicts

pub fn generate_node_id() -> i32 {
  unsafe {
    let id = NEXT_GENERATED_NODE_ID;
    NEXT_GENERATED_NODE_ID -= 1;
    id
  }
}

/// Wraps a statement in a Block containing a SemanticBlock.
/// If the statement is already a Block (which will already contain
/// SemanticBlock children from the Block parsing logic), returns it unchanged.
fn wrap_statement_in_block(statement: Box<ASTNode>) -> Box<ASTNode> {
  match &*statement {
    // Already a Block — its statements were grouped into SemanticBlocks
    // during Block parsing.
    ASTNode::Block { .. } => statement,
    // Otherwise, wrap the single statement in a SemanticBlock inside a Block
    _ => {
      let src_location = statement.src_location().clone();
      let semantic_block = ASTNode::SemanticBlock {
        node_id: generate_node_id(),
        src_location: src_location.clone(),
        documentation: None,
        statements: vec![*statement],
      };
      Box::new(ASTNode::Block {
        node_id: generate_node_id(),
        src_location,
        statements: vec![semantic_block],
      })
    }
  }
}

fn group_statements_into_semantic_blocks(
  statements: Vec<ASTNode>,
  source: &str,
  block_src_location: &SourceLocation,
) -> Result<Vec<ASTNode>, String> {
  if statements.is_empty() {
    return Ok(vec![]);
  }

  let breaks = find_semantic_breaks(source, &statements);
  let mut semantic_blocks = vec![];
  let mut current_group_start = 0;

  let mut create_semantic_block =
    |group_statements: Vec<ASTNode>, group_start_index: usize| {
      if group_statements.is_empty() {
        return;
      }

      let first_stmt_start =
        group_statements[0].src_location().start.unwrap_or(0);
      let last_stmt = &group_statements[group_statements.len() - 1];
      let last_stmt_end = last_stmt.src_location().start.unwrap_or(0)
        + last_stmt.src_location().length.unwrap_or(0);

      let group_start_pos = if group_start_index == 0 {
        block_src_location.start.unwrap_or(0)
      } else {
        statements[group_start_index - 1]
          .src_location()
          .start
          .unwrap_or(0)
          + statements[group_start_index - 1]
            .src_location()
            .length
            .unwrap_or(0)
      };

      let documentation =
        extract_block_documentation(source, group_start_pos, first_stmt_start);

      let semantic_block = ASTNode::SemanticBlock {
        node_id: generate_node_id(),
        src_location: SourceLocation {
          start: Some(first_stmt_start),
          length: Some(last_stmt_end - first_stmt_start),
          index: None,
        },
        documentation,
        statements: group_statements,
      };

      semantic_blocks.push(semantic_block);
    };

  for &break_index in &breaks {
    let group_statements =
      statements[current_group_start..break_index].to_vec();
    create_semantic_block(group_statements, current_group_start);
    current_group_start = break_index;
  }

  // Handle the last group
  let group_statements = statements[current_group_start..].to_vec();
  create_semantic_block(group_statements, current_group_start);

  Ok(semantic_blocks)
}

/// Extracts the referenced declaration ID from a function call expression.
/// Returns None if the reference cannot be determined (e.g., dynamic calls).
pub fn get_referenced_function_id(expression: &ASTNode) -> Option<i32> {
  match expression {
    // Direct function call: foo()
    ASTNode::Identifier {
      referenced_declaration,
      ..
    } => Some(*referenced_declaration),
    // Method call: obj.foo() or Contract.foo()
    ASTNode::MemberAccess {
      referenced_declaration,
      ..
    } => *referenced_declaration,
    // Chained call options: foo{value: 1}()
    ASTNode::FunctionCallOptions { expression, .. } => {
      get_referenced_function_id(expression)
    }
    _ => None,
  }
}

/// Gets the parameters from a function definition node.
pub fn get_function_parameters(func_def: &ASTNode) -> Option<&Vec<ASTNode>> {
  match func_def {
    ASTNode::FunctionDefinition { signature, .. } => match &**signature {
      ASTNode::FunctionSignature { parameters, .. } => match &**parameters {
        ASTNode::ParameterList { parameters, .. } => Some(parameters),
        _ => None,
      },
      _ => None,
    },
    _ => None,
  }
}

/// Gets the return parameters from a function definition node.
pub fn get_function_return_parameters(
  func_def: &ASTNode,
) -> Option<&Vec<ASTNode>> {
  match func_def {
    ASTNode::FunctionDefinition { signature, .. } => match &**signature {
      ASTNode::FunctionSignature {
        return_parameters, ..
      } => match &**return_parameters {
        ASTNode::ParameterList { parameters, .. } => Some(parameters),
        _ => None,
      },
      _ => None,
    },
    _ => None,
  }
}

/// Gets the members from a struct definition node.
pub fn get_struct_members(struct_def: &ASTNode) -> Option<&Vec<ASTNode>> {
  match struct_def {
    ASTNode::StructDefinition { members, .. } => Some(members),
    _ => None,
  }
}

/// Gets the parameters from an event definition node.
fn get_event_parameters(event_def: &ASTNode) -> Option<&Vec<ASTNode>> {
  match event_def {
    ASTNode::EventDefinition { parameters, .. } => match &**parameters {
      ASTNode::ParameterList { parameters, .. } => Some(parameters),
      _ => None,
    },
    _ => None,
  }
}

/// Gets the parameters from an error definition node.
fn get_error_parameters(error_def: &ASTNode) -> Option<&Vec<ASTNode>> {
  match error_def {
    ASTNode::ErrorDefinition { parameters, .. } => match &**parameters {
      ASTNode::ParameterList { parameters, .. } => Some(parameters),
      _ => None,
    },
    _ => None,
  }
}

/// Gets the parameters/members from any definition node (function, struct, event, or error).
pub fn get_definition_parameters(def: &ASTNode) -> Option<&Vec<ASTNode>> {
  get_function_parameters(def)
    .or_else(|| get_struct_members(def))
    .or_else(|| get_event_parameters(def))
    .or_else(|| get_error_parameters(def))
}

fn node_from_json(
  val: &serde_json::Value,
  context: &ParserContext,
) -> Result<ASTNode, String> {
  // Handle null values - they should not reach here for required fields
  // but this provides a clear error if they do
  if val.is_null() {
    return Err(
      "Cannot parse null node value - this indicates a required field is null"
        .to_string(),
    );
  }

  let node_type_str = val
    .get("nodeType")
    .and_then(|v| v.as_str())
    .ok_or_else(|| format!("Missing nodeType field: {:?}", val))?;

  let node_id = get_required_i32(val, "id")
    .map_err(|e| format!("Error parsing {} node: {}", node_type_str, e))?;
  let src_location = val
    .get("src")
    .and_then(|v| v.as_str())
    .ok_or_else(|| {
      format!("Missing src field in {} node: {:?}", node_type_str, val)
    })
    .and_then(|v| SourceLocation::from_str(v))
    .map_err(|e| format!("Error parsing {} node: {}", node_type_str, e))?;

  match node_type_str {
    "Assignment" => {
      let operator =
        get_required_enum_with_context(val, "operator", node_type_str)?;
      let right_hand_side = get_required_node_with_context(
        val,
        "rightHandSide",
        node_type_str,
        context,
      )?;
      let left_hand_side = get_required_node_with_context(
        val,
        "leftHandSide",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::Assignment {
        node_id,
        src_location,
        operator,
        right_hand_side,
        left_hand_side,
      })
    }
    "BinaryOperation" => {
      let left_expression = get_required_node_with_context(
        val,
        "leftExpression",
        node_type_str,
        context,
      )?;
      let operator =
        get_required_enum_with_context(val, "operator", node_type_str)?;
      let right_expression = get_required_node_with_context(
        val,
        "rightExpression",
        node_type_str,
        context,
      )?;
      let type_descriptions = get_required_type_descriptions_with_context(
        val,
        "typeDescriptions",
        "BinaryOperation",
      )?;

      Ok(ASTNode::BinaryOperation {
        node_id,
        src_location,
        left_expression,
        operator,
        right_expression,
        type_descriptions,
      })
    }
    "Conditional" => {
      let condition = get_required_node_with_context(
        val,
        "condition",
        node_type_str,
        context,
      )?;
      let true_expression = get_required_node_with_context(
        val,
        "trueExpression",
        node_type_str,
        context,
      )?;
      let false_expression = get_optional_node_with_context(
        val,
        "falseExpression",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::Conditional {
        node_id,
        src_location,
        condition,
        true_expression,
        false_expression,
      })
    }
    "ElementaryTypeNameExpression" => {
      let type_descriptions = get_required_type_descriptions_with_context(
        val,
        "typeDescriptions",
        node_type_str,
      )?;
      let type_name = get_required_node_with_context(
        val,
        "typeName",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::ElementaryTypeNameExpression {
        node_id,
        src_location,
        type_descriptions,
        type_name,
      })
    }
    "FunctionCall" => {
      let raw_arguments = get_required_node_vec_with_context(
        val,
        "arguments",
        node_type_str,
        context,
      )?;
      let expression = get_required_node_with_context(
        val,
        "expression",
        node_type_str,
        context,
      )?;
      let kind_str =
        get_required_string_with_context(val, "kind", node_type_str)?;
      let name_locations = get_required_source_location_vec_with_context(
        val,
        "nameLocations",
        node_type_str,
      )?;
      let names =
        get_required_string_vec_with_context(val, "names", node_type_str)?;
      let try_call =
        get_required_bool_with_context(val, "tryCall", node_type_str)?;
      let type_descriptions = get_required_type_descriptions_with_context(
        val,
        "typeDescriptions",
        node_type_str,
      )?;

      // Create the appropriate node type based on the kind
      match kind_str.as_str() {
        "functionCall" => {
          // Arguments are stored as raw expressions; they will be wrapped
          // with Argument nodes during the transform phase after tree-shaking.
          // referenced_return_declarations is populated during transform phase.
          Ok(ASTNode::FunctionCall {
            node_id,
            src_location,
            arguments: raw_arguments,
            expression,
            name_locations,
            names,
            try_call,
            type_descriptions,
            referenced_return_declarations: Vec::new(),
          })
        }
        "typeConversion" => {
          // Type conversions always have exactly one argument
          if raw_arguments.len() != 1 {
            return Err(format!(
              "TypeConversion expected exactly 1 argument, got {}",
              raw_arguments.len()
            ));
          }
          let argument = raw_arguments.into_iter().next().unwrap();
          Ok(ASTNode::TypeConversion {
            node_id,
            src_location,
            argument: Box::new(argument),
            expression,
            name_locations,
            names,
            try_call,
            type_descriptions,
          })
        }
        "structConstructorCall" => {
          // Arguments are stored as raw expressions; they will be wrapped
          // with Argument nodes during the transform phase after tree-shaking
          Ok(ASTNode::StructConstructor {
            node_id,
            src_location,
            arguments: raw_arguments,
            expression,
            name_locations,
            names,
            try_call,
            type_descriptions,
          })
        }
        _ => Err(format!("Unknown function call kind: {}", kind_str)),
      }
    }
    "FunctionCallOptions" => {
      let expression = get_required_node_with_context(
        val,
        "expression",
        node_type_str,
        context,
      )?;
      let options = get_required_node_vec_with_context(
        val,
        "options",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::FunctionCallOptions {
        node_id,
        src_location,
        expression,
        options,
      })
    }
    "Identifier" => {
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let overloaded_declarations = get_required_i32_vec_with_context(
        val,
        "overloadedDeclarations",
        node_type_str,
      )?;
      let referenced_declaration = get_required_i32_with_context(
        val,
        "referencedDeclaration",
        node_type_str,
      )?;

      Ok(ASTNode::Identifier {
        node_id,
        src_location,
        name,
        overloaded_declarations,
        referenced_declaration,
      })
    }
    "IdentifierPath" => {
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_locations = get_required_source_location_vec_with_context(
        val,
        "nameLocations",
        node_type_str,
      )?;
      let referenced_declaration = get_required_i32_with_context(
        val,
        "referencedDeclaration",
        node_type_str,
      )?;

      Ok(ASTNode::IdentifierPath {
        node_id,
        src_location,
        name,
        name_locations,
        referenced_declaration,
      })
    }
    "IndexAccess" => {
      let base_expression = get_required_node_with_context(
        val,
        "baseExpression",
        node_type_str,
        context,
      )?;
      let index_expression = get_optional_node_with_context(
        val,
        "indexExpression",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::IndexAccess {
        node_id,
        src_location,
        base_expression,
        index_expression,
      })
    }
    "IndexRangeAccess" => {
      let nodes = get_required_node_vec_with_context(
        val,
        "nodes",
        node_type_str,
        context,
      )?;
      let body =
        get_optional_node_with_context(val, "body", node_type_str, context)?;

      Ok(ASTNode::IndexRangeAccess {
        node_id,
        src_location,
        nodes,
        body,
      })
    }
    "Literal" => {
      let hex_value =
        get_required_string_with_context(val, "hexValue", node_type_str)?;
      let kind = get_required_enum_with_context(val, "kind", node_type_str)?;
      let type_descriptions = get_required_type_descriptions_with_context(
        val,
        "typeDescriptions",
        node_type_str,
      )?;
      let value =
        get_optional_string_with_context(val, "value", node_type_str)?;

      Ok(ASTNode::Literal {
        node_id,
        src_location,
        hex_value,
        kind,
        type_descriptions,
        value,
      })
    }
    "MemberAccess" => {
      let expression = get_required_node_with_context(
        val,
        "expression",
        node_type_str,
        context,
      )?;
      let member_location = get_required_source_location_with_context(
        val,
        "memberLocation",
        node_type_str,
      )?;
      let member_name =
        get_required_string_with_context(val, "memberName", node_type_str)?;
      let referenced_declaration = val
        .get("referencedDeclaration")
        .and_then(|v| v.as_i64())
        .map(|v| v as i32);
      let type_descriptions = get_required_type_descriptions_with_context(
        val,
        "typeDescriptions",
        node_type_str,
      )?;

      Ok(ASTNode::MemberAccess {
        node_id,
        src_location,
        expression,
        member_location,
        member_name,
        referenced_declaration,
        type_descriptions,
      })
    }
    "NewExpression" => {
      let type_name = get_required_node_with_context(
        val,
        "typeName",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::NewExpression {
        node_id,
        src_location,
        type_name,
      })
    }
    "TupleExpression" => {
      let components = get_required_node_vec_with_context(
        val,
        "components",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::TupleExpression {
        node_id,
        src_location,
        components,
      })
    }
    "UnaryOperation" => {
      let prefix =
        get_required_bool_with_context(val, "prefix", node_type_str)?;
      let operator =
        get_required_enum_with_context(val, "operator", node_type_str)?;
      let sub_expression = get_required_node_with_context(
        val,
        "subExpression",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::UnaryOperation {
        node_id,
        src_location,
        prefix,
        operator,
        sub_expression,
      })
    }
    "EnumValue" => {
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_location = get_required_source_location_with_context(
        val,
        "nameLocation",
        node_type_str,
      )?;

      Ok(ASTNode::EnumValue {
        node_id,
        src_location,
        name,
        name_location,
      })
    }
    "Block" => {
      let statements = get_required_node_vec_with_context(
        val,
        "statements",
        node_type_str,
        context,
      )?;

      // Transform statements into semantic blocks
      let semantic_blocks = if !statements.is_empty() {
        group_statements_into_semantic_blocks(
          statements,
          &context.source_content,
          &src_location,
        )?
      } else {
        vec![]
      };

      Ok(ASTNode::Block {
        node_id,
        src_location,
        statements: semantic_blocks,
      })
    }
    "Break" => Ok(ASTNode::Break {
      node_id,
      src_location,
    }),
    "Continue" => Ok(ASTNode::Continue {
      node_id,
      src_location,
    }),
    "DoWhileStatement" => {
      let condition = get_required_node_with_context(
        val,
        "condition",
        node_type_str,
        context,
      )?;
      let body =
        get_optional_node_with_context(val, "body", node_type_str, context)?;

      // Wrap braceless do-while bodies in a Block, like IfStatement
      let body = body.map(wrap_statement_in_block);

      Ok(ASTNode::DoWhileStatement {
        node_id,
        src_location,
        condition,
        body,
      })
    }
    "EmitStatement" => {
      let event_call = get_required_node_with_context(
        val,
        "eventCall",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::EmitStatement {
        node_id,
        src_location,
        event_call,
      })
    }
    "ExpressionStatement" => {
      let expression = get_required_node_with_context(
        val,
        "expression",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::ExpressionStatement {
        node_id,
        src_location,
        expression,
      })
    }
    "ForStatement" => {
      let body =
        get_required_node_with_context(val, "body", node_type_str, context)?;
      let condition = get_optional_node_with_context(
        val,
        "condition",
        node_type_str,
        context,
      )?;
      let initialization_expression = get_optional_node_with_context(
        val,
        "initializationExpression",
        node_type_str,
        context,
      )?;
      let is_simple_counter_loop = get_required_bool_with_context(
        val,
        "isSimpleCounterLoop",
        node_type_str,
      )?;
      let loop_expression = get_optional_node_with_context(
        val,
        "loopExpression",
        node_type_str,
        context,
      )?;

      // Wrap braceless for-loop bodies in a Block, like IfStatement
      let body = wrap_statement_in_block(body);

      // Create a synthetic LoopExpression node wrapping the loop clauses
      let loop_expr_node = ASTNode::LoopExpression {
        node_id: generate_node_id(),
        src_location: src_location.clone(),
        initialization_expression,
        condition,
        loop_expression,
        is_simple_counter_loop,
      };

      Ok(ASTNode::ForStatement {
        node_id,
        src_location,
        condition: Box::new(loop_expr_node),
        body,
      })
    }
    "IfStatement" => {
      let condition = get_required_node_with_context(
        val,
        "condition",
        node_type_str,
        context,
      )?;
      let true_body = get_required_node_with_context(
        val,
        "trueBody",
        node_type_str,
        context,
      )?;
      let false_body = get_optional_node_with_context(
        val,
        "falseBody",
        node_type_str,
        context,
      )?;

      // Wrap all single statements if statements in a Block because
      // Solidity allows for single statements without blocks in if
      // statements, but we never want this because it is bad practice
      let true_body = wrap_statement_in_block(true_body);
      let false_body = false_body.map(wrap_statement_in_block);

      Ok(ASTNode::IfStatement {
        node_id,
        src_location,
        condition,
        true_body,
        false_body,
      })
    }
    "InlineAssembly" => Ok(ASTNode::InlineAssembly {
      node_id,
      src_location,
    }),
    "PlaceholderStatement" => Ok(ASTNode::PlaceholderStatement {
      node_id,
      src_location,
    }),
    "Return" => {
      let expression = get_optional_node_with_context(
        val,
        "expression",
        node_type_str,
        context,
      )?;
      let function_return_parameters = get_required_i32_with_context(
        val,
        "functionReturnParameters",
        node_type_str,
      )?;

      Ok(ASTNode::Return {
        node_id,
        src_location,
        expression,
        function_return_parameters,
      })
    }
    "RevertStatement" => {
      let error_call = get_required_node_with_context(
        val,
        "errorCall",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::RevertStatement {
        node_id,
        src_location,
        error_call,
      })
    }
    "TryStatement" => {
      let clauses = get_required_node_vec_with_context(
        val,
        "clauses",
        node_type_str,
        context,
      )?;
      let external_call = get_required_node_with_context(
        val,
        "externalCall",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::TryStatement {
        node_id,
        src_location,
        clauses,
        external_call,
      })
    }
    "UncheckedBlock" => {
      let statements = get_required_node_vec_with_context(
        val,
        "statements",
        node_type_str,
        context,
      )?;

      // Transform statements into semantic blocks (same as Block)
      let semantic_blocks = if !statements.is_empty() {
        group_statements_into_semantic_blocks(
          statements,
          &context.source_content,
          &src_location,
        )?
      } else {
        vec![]
      };

      Ok(ASTNode::UncheckedBlock {
        node_id,
        src_location,
        statements: semantic_blocks,
      })
    }
    "VariableDeclarationStatement" => {
      let mut declarations = get_required_node_vec_with_context(
        val,
        "declarations",
        node_type_str,
        context,
      )?;
      let initial_value = get_optional_node_with_context(
        val,
        "initialValue",
        node_type_str,
        context,
      )?;

      // If there's exactly one declaration and an initial value, move the
      // initial value into the VariableDeclaration's value field. This allows
      // the VariableDeclaration to be formatted and analyzed with its initial
      // value without needing to look up the parent VariableDeclarationStatement.
      let initial_value = if declarations.len() == 1 && initial_value.is_some()
      {
        if let Some(ASTNode::VariableDeclaration { value, .. }) =
          declarations.first_mut()
        {
          *value = initial_value;
        }
        None
      } else {
        initial_value
      };

      Ok(ASTNode::VariableDeclarationStatement {
        node_id,
        src_location,
        declarations,
        initial_value,
      })
    }
    "VariableDeclaration" => {
      let constant =
        get_required_bool_with_context(val, "constant", node_type_str)?;
      let function_selector = get_optional_string_with_context(
        val,
        "functionSelector",
        node_type_str,
      )?;
      let mutability =
        get_required_enum_with_context(val, "mutability", node_type_str)?;
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_location = get_required_source_location_with_context(
        val,
        "nameLocation",
        node_type_str,
      )?;
      let scope = get_required_i32_with_context(val, "scope", node_type_str)?;
      let state_variable =
        get_required_bool_with_context(val, "stateVariable", node_type_str)?;
      let storage_location =
        get_required_enum_with_context(val, "storageLocation", node_type_str)?;
      let type_name = get_required_node_with_context(
        val,
        "typeName",
        node_type_str,
        context,
      )?;
      let value =
        get_optional_node_with_context(val, "value", node_type_str, context)?;
      let visibility =
        get_required_enum_with_context(val, "visibility", node_type_str)?;
      // baseFunctions contains IDs of interface functions this state variable implements
      let base_functions = get_optional_i32_vec(val, "baseFunctions")?;

      Ok(ASTNode::VariableDeclaration {
        node_id,
        src_location,
        constant,
        function_selector,
        mutability,
        name,
        name_location,
        scope,
        state_variable,
        storage_location,
        type_name,
        value,
        visibility,
        // This is always set to false initially, but when this is parsed as a
        // child to a ParameterList node, this value will be set to true before
        // setting it into the ParameterList node variant
        parameter_variable: None,
        // Interface-to-implementation mapping is now applied during transform phase
        implementation_declaration: None,
        base_functions,
        // This is set to true when parsed via get_required_struct_field_variable_declaration_vec_with_context
        struct_field: false,
      })
    }
    "WhileStatement" => {
      let condition = get_required_node_with_context(
        val,
        "condition",
        node_type_str,
        context,
      )?;
      let body =
        get_optional_node_with_context(val, "body", node_type_str, context)?;

      // Wrap braceless while-loop bodies in a Block, like IfStatement
      let body = body.map(wrap_statement_in_block);

      Ok(ASTNode::WhileStatement {
        node_id,
        src_location,
        condition,
        body,
      })
    }
    "ContractDefinition" => {
      let all_nodes = get_required_node_vec_with_context(
        val,
        "nodes",
        node_type_str,
        context,
      )?;
      let abstract_ =
        get_required_bool_with_context(val, "abstract", node_type_str)?;
      let base_contracts = get_required_node_vec_with_context(
        val,
        "baseContracts",
        node_type_str,
        context,
      )?;
      let documentation = get_optional_node_with_context(
        val,
        "documentation",
        node_type_str,
        context,
      )?;
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_location = get_required_source_location_with_context(
        val,
        "nameLocation",
        node_type_str,
      )?;
      let contract_kind =
        get_required_enum_with_context(val, "contractKind", node_type_str)?;

      // Separate UsingForDirective nodes from other nodes
      let (directives, nodes): (Vec<ASTNode>, Vec<ASTNode>) = all_nodes
        .into_iter()
        .partition(|node| matches!(node, ASTNode::UsingForDirective { .. }));

      // Create the ContractSignature node with a generated ID
      let signature_node_id = generate_node_id();
      let signature = ASTNode::ContractSignature {
        node_id: signature_node_id,
        src_location: src_location.clone(),
        documentation,
        name,
        name_location,
        declaration_id: node_id,
        contract_kind,
        abstract_,
        base_contracts,
        directives,
      };

      Ok(ASTNode::ContractDefinition {
        node_id,
        src_location,
        signature: Box::new(signature),
        nodes,
      })
    }
    "FunctionDefinition" => {
      let body =
        get_optional_node_with_context(val, "body", node_type_str, context)?;
      let documentation = get_optional_node_with_context(
        val,
        "documentation",
        node_type_str,
        context,
      )?;
      let implemented =
        get_required_bool_with_context(val, "implemented", node_type_str)?;
      let kind = get_required_enum_with_context(val, "kind", node_type_str)?;
      let modifiers = get_required_node_vec_with_context(
        val,
        "modifiers",
        node_type_str,
        context,
      )?;
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_location = get_required_source_location_with_context(
        val,
        "nameLocation",
        node_type_str,
      )?;
      let scope = get_required_i32_with_context(val, "scope", node_type_str)?;
      let state_mutability =
        get_required_enum_with_context(val, "stateMutability", node_type_str)?;
      let virtual_ =
        get_required_bool_with_context(val, "virtual", node_type_str)?;
      let visibility =
        get_required_enum_with_context(val, "visibility", node_type_str)?;

      // Create the FunctionSignature node with a generated ID
      let signature_node_id = generate_node_id();

      // Set the signature parent node in context so that parameter variables
      // can reference it
      let previous_signature_parent = context.signature_parent_node.get();
      context.signature_parent_node.set(Some(signature_node_id));

      let parameters = get_required_node_with_context(
        val,
        "parameters",
        node_type_str,
        context,
      )?;
      let mut return_parameters = get_required_node_with_context(
        val,
        "returnParameters",
        node_type_str,
        context,
      )?;

      // Mark the return parameter list so downstream consumers can distinguish it
      if let ASTNode::ParameterList {
        is_return_parameters,
        ..
      } = &mut *return_parameters
      {
        *is_return_parameters = true;
      }

      // Restore the previous signature parent node
      context.signature_parent_node.set(previous_signature_parent);

      let modifier_list = ASTNode::ModifierList {
        node_id: generate_node_id(),
        src_location: src_location.clone(),
        modifiers,
      };

      let signature = ASTNode::FunctionSignature {
        node_id: signature_node_id,
        src_location: src_location.clone(),
        documentation,
        kind,
        modifiers: Box::new(modifier_list),
        name,
        name_location,
        // Interface-to-implementation mapping is now applied during transform phase
        declaration_id: node_id,
        parameters,
        return_parameters,
        scope,
        state_mutability,
        virtual_,
        visibility,
        // Set during transform phase for interface functions
        implementation_declaration: None,
      };

      Ok(ASTNode::FunctionDefinition {
        node_id,
        src_location,
        signature: Box::new(signature),
        implemented,
        body,
      })
    }
    "EventDefinition" => {
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_location = get_required_source_location_with_context(
        val,
        "nameLocation",
        node_type_str,
      )?;
      let parameters = get_required_node_with_context(
        val,
        "parameters",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::EventDefinition {
        node_id,
        src_location,
        name,
        name_location,
        parameters,
      })
    }
    "ErrorDefinition" => {
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_location = get_required_source_location_with_context(
        val,
        "nameLocation",
        node_type_str,
      )?;
      let parameters = get_required_node_with_context(
        val,
        "parameters",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::ErrorDefinition {
        node_id,
        src_location,
        name,
        name_location,
        parameters,
      })
    }
    "ModifierDefinition" => {
      let body =
        get_required_node_with_context(val, "body", node_type_str, context)?;
      let documentation = get_optional_node_with_context(
        val,
        "documentation",
        node_type_str,
        context,
      )?;
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_location = get_required_source_location_with_context(
        val,
        "nameLocation",
        node_type_str,
      )?;
      let parameters = get_required_node_with_context(
        val,
        "parameters",
        node_type_str,
        context,
      )?;
      let virtual_ =
        get_required_bool_with_context(val, "virtual", node_type_str)?;
      let visibility =
        get_required_enum_with_context(val, "visibility", node_type_str)?;

      // Create the ModifierSignature node with a generated ID
      let signature_node_id = generate_node_id();
      let signature = ASTNode::ModifierSignature {
        node_id: signature_node_id,
        src_location: src_location.clone(),
        documentation,
        name,
        name_location,
        declaration_id: node_id,
        parameters,
        virtual_,
        visibility,
        // Set during transform phase for interface modifiers
        implementation_declaration: None,
      };

      Ok(ASTNode::ModifierDefinition {
        node_id,
        src_location,
        signature: Box::new(signature),
        body,
      })
    }
    "StructDefinition" => {
      let members =
        get_required_struct_field_variable_declaration_vec_with_context(
          val,
          "members",
          node_type_str,
          context,
        )?;
      let canonical_name =
        get_required_string_with_context(val, "canonicalName", node_type_str)?;
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_location = get_required_source_location_with_context(
        val,
        "nameLocation",
        node_type_str,
      )?;
      let visibility =
        get_required_enum_with_context(val, "visibility", node_type_str)?;

      Ok(ASTNode::StructDefinition {
        node_id,
        src_location,
        members,
        canonical_name,
        name,
        name_location,
        visibility,
      })
    }
    "EnumDefinition" => {
      let members = get_required_node_vec_with_context(
        val,
        "members",
        node_type_str,
        context,
      )?;
      let canonical_name =
        get_required_string_with_context(val, "canonicalName", node_type_str)?;
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let name_location = get_required_source_location_with_context(
        val,
        "nameLocation",
        node_type_str,
      )?;

      Ok(ASTNode::EnumDefinition {
        node_id,
        src_location,
        members,
        canonical_name,
        name,
        name_location,
      })
    }
    "UserDefinedValueTypeDefinition" => {
      let name = get_required_string_with_context(val, "name", node_type_str)?;
      let underlying_type = get_required_node_with_context(
        val,
        "underlyingType",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::UserDefinedValueTypeDefinition {
        node_id,
        src_location,
        name,
        underlying_type,
      })
    }
    "PragmaDirective" => {
      let literals =
        get_required_string_vec_with_context(val, "literals", node_type_str)?;

      Ok(ASTNode::PragmaDirective {
        node_id,
        src_location,
        literals,
      })
    }
    "ImportDirective" => {
      let absolute_path =
        get_required_string_with_context(val, "absolutePath", node_type_str)?;
      let file = get_required_string_with_context(val, "file", node_type_str)?;
      let source_unit =
        get_required_i32_with_context(val, "sourceUnit", node_type_str)?;

      Ok(ASTNode::ImportDirective {
        node_id,
        src_location,
        absolute_path,
        file,
        source_unit,
      })
    }
    "UsingForDirective" => {
      let global =
        get_required_bool_with_context(val, "global", node_type_str)?;
      let library_name = get_optional_node_with_context(
        val,
        "libraryName",
        node_type_str,
        context,
      )?;
      let type_name = get_optional_node_with_context(
        val,
        "typeName",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::UsingForDirective {
        node_id,
        src_location,
        global,
        library_name,
        type_name,
      })
    }
    "SourceUnit" => {
      let nodes = get_required_node_vec_with_context(
        val,
        "nodes",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::SourceUnit {
        node_id,
        src_location,
        nodes,
      })
    }
    "InheritanceSpecifier" => {
      let base_name = get_required_node_with_context(
        val,
        "baseName",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::InheritanceSpecifier {
        node_id,
        src_location,
        base_name,
      })
    }
    "ElementaryTypeName" => {
      let name = get_required_string_with_context(val, "name", node_type_str)?;

      Ok(ASTNode::ElementaryTypeName {
        node_id,
        src_location,
        name,
      })
    }
    "FunctionTypeName" => {
      let parameter_types = get_required_node_with_context(
        val,
        "parameterTypes",
        node_type_str,
        context,
      )?;
      let return_parameter_types = get_required_node_with_context(
        val,
        "returnParameterTypes",
        node_type_str,
        context,
      )?;
      let state_mutability =
        get_required_enum_with_context(val, "stateMutability", node_type_str)?;
      let visibility =
        get_required_enum_with_context(val, "visibility", node_type_str)?;

      Ok(ASTNode::FunctionTypeName {
        node_id,
        src_location,
        parameter_types,
        return_parameter_types,
        state_mutability,
        visibility,
      })
    }
    "ParameterList" => {
      let parameters =
        get_required_parameter_variable_declaration_vec_with_context(
          val,
          "parameters",
          node_type_str,
          context,
        )?;

      Ok(ASTNode::ParameterList {
        node_id,
        src_location,
        parameters,
        is_return_parameters: false,
      })
    }
    "TryCatchClause" => {
      let error_name =
        get_required_string_with_context(val, "errorName", node_type_str)?;
      let block =
        get_required_node_with_context(val, "block", node_type_str, context)?;
      let parameters = get_optional_node_with_context(
        val,
        "parameters",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::TryCatchClause {
        node_id,
        src_location,
        error_name,
        block,
        parameters,
      })
    }
    "ModifierInvocation" => {
      let modifier_name = get_required_node_with_context(
        val,
        "modifierName",
        node_type_str,
        context,
      )?;
      let arguments = get_optional_node_vec_with_context(
        val,
        "arguments",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::ModifierInvocation {
        node_id,
        src_location,
        modifier_name,
        arguments,
      })
    }
    "UserDefinedTypeName" => {
      let path_node = get_required_node_with_context(
        val,
        "pathNode",
        node_type_str,
        context,
      )?;
      let referenced_declaration = get_required_i32_with_context(
        val,
        "referencedDeclaration",
        node_type_str,
      )?;

      Ok(ASTNode::UserDefinedTypeName {
        node_id,
        src_location,
        path_node,
        referenced_declaration,
      })
    }
    "ArrayTypeName" => {
      let base_type = get_required_node_with_context(
        val,
        "baseType",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::ArrayTypeName {
        node_id,
        src_location,
        base_type,
      })
    }
    "Mapping" => {
      let key_name =
        get_optional_string_with_context(val, "keyName", node_type_str)?;
      let key_name_location = get_required_source_location_with_context(
        val,
        "keyNameLocation",
        node_type_str,
      )?;
      let key_type =
        get_required_node_with_context(val, "keyType", node_type_str, context)?;
      let value_name =
        get_optional_string_with_context(val, "valueName", node_type_str)?;
      let value_name_location = get_required_source_location_with_context(
        val,
        "valueNameLocation",
        node_type_str,
      )?;
      let value_type = get_required_node_with_context(
        val,
        "valueType",
        node_type_str,
        context,
      )?;

      Ok(ASTNode::Mapping {
        node_id,
        src_location,
        key_name,
        key_name_location,
        key_type,
        value_name,
        value_name_location,
        value_type,
      })
    }
    "StructuredDocumentation" => {
      let text = get_required_string_with_context(val, "text", node_type_str)?;

      Ok(ASTNode::StructuredDocumentation {
        node_id,
        src_location,
        text,
      })
    }
    // Other node type
    _ => {
      let nodes = get_required_node_vec(val, "nodes", context)
        .map_err(|e| format!("Error parsing {} node: {}", node_type_str, e))
        .unwrap_or_else(|_| Vec::new());
      let body = get_optional_node(val, "body", context)
        .map_err(|e| format!("Error parsing {} node: {}", node_type_str, e))
        .unwrap_or(None);

      Ok(ASTNode::Other {
        node_id,
        src_location,
        nodes,
        body,
        node_type: node_type_str.to_string(),
      })
    }
  }
}

// ============================================================================
// NatSpec Parsing
// ============================================================================

/// A parsed NatSpec tag from StructuredDocumentation text.
#[derive(Debug, Clone)]
pub enum NatSpecTag {
  /// @notice — informational, targets the declaration
  Notice,
  /// @dev — technical, targets the declaration
  Dev,
  /// @param <name> — targets a specific parameter VariableDeclaration
  Param(String),
  /// @return — targets a return parameter (text may contain optional name prefix)
  Return,
  /// Untagged text — technical, targets the declaration
  Untagged,
  /// Known but deferred tag (@title, @author, @inheritdoc, etc.) — ignored
  Ignored,
}

/// A section of NatSpec documentation corresponding to one tag.
pub struct NatSpecSection {
  pub tag: NatSpecTag,
  pub text: String,
}

/// Parse StructuredDocumentation text into NatSpec sections.
/// Lines following a tag line that don't start with @ are treated as
/// continuations of that tag. Untagged text before any tag becomes Untagged.
pub fn parse_natspec(text: &str) -> Vec<NatSpecSection> {
  let mut sections: Vec<NatSpecSection> = Vec::new();
  let mut current_tag: Option<NatSpecTag> = None;
  let mut current_text = String::new();

  for line in text.lines() {
    let trimmed = line.trim();
    if trimmed.is_empty() {
      continue;
    }

    if let Some((tag, rest)) = try_parse_natspec_tag(trimmed) {
      if matches!(tag, NatSpecTag::Ignored) {
        // Flush current section but don't start a new one for deferred tags
        flush_natspec_section(&mut current_tag, &mut current_text, &mut sections);
      } else {
        flush_natspec_section(&mut current_tag, &mut current_text, &mut sections);
        current_tag = Some(tag);
        current_text = rest.to_string();
      }
    } else if current_tag.is_some() {
      // Continuation of current tag
      if !current_text.is_empty() {
        current_text.push(' ');
      }
      current_text.push_str(trimmed);
    } else {
      // Untagged line before any tag
      flush_natspec_section(
        &mut current_tag,
        &mut current_text,
        &mut sections,
      );
      current_tag = Some(NatSpecTag::Untagged);
      current_text = trimmed.to_string();
    }
  }

  flush_natspec_section(&mut current_tag, &mut current_text, &mut sections);
  sections
}

fn flush_natspec_section(
  current_tag: &mut Option<NatSpecTag>,
  current_text: &mut String,
  sections: &mut Vec<NatSpecSection>,
) {
  if let Some(tag) = current_tag.take() {
    let text = current_text.trim().to_string();
    if !text.is_empty() {
      sections.push(NatSpecSection { tag, text });
    }
  }
  current_text.clear();
}

/// Try to parse a line as a NatSpec tag.
/// Returns Some((tag, remaining_text)) if the line starts with a known tag.
fn try_parse_natspec_tag(line: &str) -> Option<(NatSpecTag, &str)> {
  if !line.starts_with('@') {
    return None;
  }
  let after_at = &line[1..];

  // @notice [<text>]
  if let Some(rest) = after_at.strip_prefix("notice") {
    if rest.is_empty() || rest.starts_with(' ') {
      return Some((
        NatSpecTag::Notice,
        rest.strip_prefix(' ').unwrap_or(rest),
      ));
    }
  }

  // @dev [<text>]
  if let Some(rest) = after_at.strip_prefix("dev") {
    if rest.is_empty() || rest.starts_with(' ') {
      return Some((
        NatSpecTag::Dev,
        rest.strip_prefix(' ').unwrap_or(rest),
      ));
    }
  }

  // @param <name> [<description>]
  if let Some(rest) = after_at.strip_prefix("param ") {
    if rest.is_empty() {
      return None;
    }
    let (name, desc) = rest.split_once(' ').unwrap_or((rest, ""));
    return Some((NatSpecTag::Param(name.to_string()), desc));
  }

  // @return [<text>]
  if let Some(rest) = after_at.strip_prefix("return") {
    // Avoid matching @returns or other longer tags
    if rest.is_empty() || rest.starts_with(' ') {
      return Some((
        NatSpecTag::Return,
        rest.strip_prefix(' ').unwrap_or(rest),
      ));
    }
  }

  // Known deferred tags — flush current section but don't collect text
  if let Some(rest) = after_at.strip_prefix("title") {
    if rest.is_empty() || rest.starts_with(' ') {
      return Some((NatSpecTag::Ignored, ""));
    }
  }
  if let Some(rest) = after_at.strip_prefix("author") {
    if rest.is_empty() || rest.starts_with(' ') {
      return Some((NatSpecTag::Ignored, ""));
    }
  }
  if let Some(rest) = after_at.strip_prefix("inheritdoc") {
    if rest.is_empty() || rest.starts_with(' ') {
      return Some((NatSpecTag::Ignored, ""));
    }
  }

  // Unknown tag — ignore (line will be treated as untagged or continuation)
  None
}

#[cfg(test)]
mod natspec_tests {
  use super::*;

  fn tag_kind(tag: &NatSpecTag) -> &'static str {
    match tag {
      NatSpecTag::Notice => "notice",
      NatSpecTag::Dev => "dev",
      NatSpecTag::Param(_) => "param",
      NatSpecTag::Return => "return",
      NatSpecTag::Untagged => "untagged",
      NatSpecTag::Ignored => "ignored",
    }
  }

  #[test]
  fn test_parse_natspec_notice() {
    let sections = parse_natspec("@notice Rescues tokens");
    assert_eq!(sections.len(), 1);
    assert_eq!(tag_kind(&sections[0].tag), "notice");
    assert_eq!(sections[0].text, "Rescues tokens");
  }

  #[test]
  fn test_parse_natspec_dev() {
    let sections = parse_natspec("@dev Only callable by admin");
    assert_eq!(sections.len(), 1);
    assert_eq!(tag_kind(&sections[0].tag), "dev");
    assert_eq!(sections[0].text, "Only callable by admin");
  }

  #[test]
  fn test_parse_natspec_param_with_description() {
    let sections = parse_natspec("@param token Address of token");
    assert_eq!(sections.len(), 1);
    assert!(matches!(&sections[0].tag, NatSpecTag::Param(n) if n == "token"));
    assert_eq!(sections[0].text, "Address of token");
  }

  #[test]
  fn test_parse_natspec_param_name_no_description_filtered() {
    // @param token (no description) → name parsed but empty text filtered out
    let sections = parse_natspec("@param token");
    assert!(sections.is_empty());
  }

  #[test]
  fn test_parse_natspec_return_with_text() {
    let sections = parse_natspec("@return amount Amount rescued");
    assert_eq!(sections.len(), 1);
    assert_eq!(tag_kind(&sections[0].tag), "return");
    assert_eq!(sections[0].text, "amount Amount rescued");
  }

  #[test]
  fn test_parse_natspec_return_no_text() {
    let sections = parse_natspec("@return");
    // Empty text is trimmed and filtered out by flush_natspec_section
    assert!(sections.is_empty());
  }

  #[test]
  fn test_parse_natspec_untagged_before_tags() {
    let sections = parse_natspec("This is untagged\n@notice A notice");
    assert_eq!(sections.len(), 2);
    assert_eq!(tag_kind(&sections[0].tag), "untagged");
    assert_eq!(sections[0].text, "This is untagged");
    assert_eq!(tag_kind(&sections[1].tag), "notice");
    assert_eq!(sections[1].text, "A notice");
  }

  #[test]
  fn test_parse_natspec_continuation_lines() {
    let sections = parse_natspec(
      "@notice Rescues tokens\nthat were mistakenly sent\n@dev Only admin",
    );
    assert_eq!(sections.len(), 2);
    assert_eq!(sections[0].text, "Rescues tokens that were mistakenly sent");
    assert_eq!(sections[1].text, "Only admin");
  }

  #[test]
  fn test_parse_natspec_empty_lines_skipped() {
    let sections = parse_natspec("@notice Hello\n\n\nWorld");
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].text, "Hello World");
  }

  #[test]
  fn test_parse_natspec_deferred_tags_flush_preceding() {
    // @title should flush the @notice section without absorbing its text
    let sections = parse_natspec(
      "@notice Rescues tokens\n@title TokenRescuer\n@author Alice\n@dev Only admin",
    );
    assert_eq!(sections.len(), 2);
    assert_eq!(tag_kind(&sections[0].tag), "notice");
    assert_eq!(sections[0].text, "Rescues tokens");
    assert_eq!(tag_kind(&sections[1].tag), "dev");
    assert_eq!(sections[1].text, "Only admin");
  }

  #[test]
  fn test_parse_natspec_inheritdoc_ignored() {
    let sections = parse_natspec("@inheritdoc IERC20");
    assert!(sections.is_empty());
  }

  #[test]
  fn test_parse_natspec_multiple_params() {
    let sections = parse_natspec(
      "@param token Address of token\n@param amount Amount to rescue",
    );
    assert_eq!(sections.len(), 2);
    assert!(matches!(&sections[0].tag, NatSpecTag::Param(n) if n == "token"));
    assert!(matches!(&sections[1].tag, NatSpecTag::Param(n) if n == "amount"));
  }

  #[test]
  fn test_parse_natspec_full_function_doc() {
    let doc = "\
@notice Rescues tokens that were mistakenly sent to the contract
@param token Address of token to rescue
@dev Only callable by NUDGE_ADMIN_ROLE
@return amount Amount of tokens rescued";
    let sections = parse_natspec(doc);
    assert_eq!(sections.len(), 4);
    assert_eq!(tag_kind(&sections[0].tag), "notice");
    assert_eq!(tag_kind(&sections[1].tag), "param");
    assert_eq!(tag_kind(&sections[2].tag), "dev");
    assert_eq!(tag_kind(&sections[3].tag), "return");
  }

  #[test]
  fn test_parse_natspec_empty_text_skipped() {
    let sections = parse_natspec("@notice");
    // @notice with no text → trimmed to empty → not emitted
    assert!(sections.is_empty());
  }

  #[test]
  fn test_parse_natspec_unknown_tag_not_matched() {
    // Unknown @custom tag returns None from try_parse, treated as untagged
    let sections = parse_natspec("@custom some custom tag");
    assert_eq!(sections.len(), 1);
    assert_eq!(tag_kind(&sections[0].tag), "untagged");
    assert_eq!(sections[0].text, "@custom some custom tag");
  }

  #[test]
  fn test_try_parse_tag_word_boundary_notice() {
    // @noticeable should NOT match @notice
    let sections = parse_natspec("@noticeable thing");
    assert_eq!(sections.len(), 1);
    assert_eq!(tag_kind(&sections[0].tag), "untagged");
  }

  #[test]
  fn test_try_parse_tag_word_boundary_dev() {
    // @device should NOT match @dev
    let sections = parse_natspec("@device thing");
    assert_eq!(sections.len(), 1);
    assert_eq!(tag_kind(&sections[0].tag), "untagged");
  }

  #[test]
  fn test_try_parse_tag_word_boundary_return() {
    // @returnable should NOT match @return
    let sections = parse_natspec("@returnable thing");
    assert_eq!(sections.len(), 1);
    assert_eq!(tag_kind(&sections[0].tag), "untagged");
  }
}
