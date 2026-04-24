use crate::core;
use crate::core::topic;
use crate::core::{
  ContractKind, FunctionKind, ProjectPath, VariableMutability,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::str::FromStr;

#[derive(Debug, Clone, Serialize, Deserialize)]
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
#[derive(
  Clone, Debug, PartialEq, Eq, Hash, Default, Serialize, Deserialize,
)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FunctionStateMutability {
  Pure,
  View,
  NonPayable,
  Payable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FunctionVisibility {
  Public,
  Private,
  Internal,
  External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VariableVisibility {
  Public,
  Private,
  Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StorageLocation {
  Default,
  Storage,
  Memory,
  Calldata,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Copy, Serialize, Deserialize)]
pub enum LiteralKind {
  Number,
  Bool,
  String,
  HexString,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TypeDescriptions {
  pub type_identifier: String,
  pub type_string: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// Classification of a stub's underlying node type, used by the formatter
/// to decide whether to emit inline documentation placeholders without
/// needing to resolve the stub via a nodes_map lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
  /// A group of contract-body members (state variables, functions, etc.)
  /// separated from neighboring groups by a blank line. Exists only to
  /// attach a leading inline comment (captured in `documentation`) to a
  /// group of related members. Unlike `SemanticBlock`, this node does NOT
  /// introduce a scope layer — contract members inside it are semantically
  /// peers of the ContractDefinition.
  ContractMemberGroup {
    node_id: i32,
    src_location: SourceLocation,
    documentation: Option<String>,
    members: Vec<ASTNode>,
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
      ASTNode::ContractMemberGroup { node_id, .. } => *node_id,
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
      ASTNode::ContractMemberGroup { src_location, .. } => src_location,
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
      ASTNode::ContractMemberGroup { members, .. } => {
        let mut result = vec![];
        for item in members {
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
        if let Some(value) = initial_value {
          result.push(value)
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
        vec![&**parameter_types, &**return_parameter_types]
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
        if let Some(args) = arguments {
          for item in args {
            result.push(item);
          }
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
        if let Some(body_node) = body {
          result.push(body_node)
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
      ASTNode::ContractMemberGroup { members, .. } => {
        members.iter_mut().collect()
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
      ASTNode::ContractMemberGroup { .. } => "ContractMemberGroup",
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

/// Flattens contract members through ContractMemberGroup wrappers.
/// After grouping, ContractDefinition.nodes may contain ContractMemberGroup
/// nodes; this returns the leaf declaration nodes within them.
pub fn contract_members(contract: &ASTNode) -> Vec<&ASTNode> {
  let nodes = match contract {
    ASTNode::ContractDefinition { nodes, .. } => nodes,
    _ => return vec![],
  };
  let mut result = Vec::new();
  for node in nodes {
    match node {
      ASTNode::ContractMemberGroup { members, .. } => {
        result.extend(members.iter());
      }
      _ => result.push(node),
    }
  }
  result
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

/// Classifies an AST node into a `StubKind` describing how it should be
/// rendered when stubbed out (i.e., displayed without recursing into its
/// children). Used by the formatter and by the parser when collapsing
/// nested expressions into stubs.
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
