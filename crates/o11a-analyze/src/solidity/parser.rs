use o11a_core::core;
use o11a_core::core::topic;
use o11a_core::core::ProjectPath;
use o11a_core::solidity::ast::{
  ASTNode, NatSpecSection, NatSpecTag, SolidityAST, SourceLocation,
  TypeDescriptions, classify_node_stub_kind,
};
use serde_json;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;

struct ParserContext {
  pub source_content: String,
  pub ast_map: BTreeMap<core::ProjectPath, Vec<SolidityAST>>,
  /// The node ID of the parent signature node, used to set `parameter_variable`
  /// on VariableDeclaration nodes within parameter lists.
  pub signature_parent_node: Cell<Option<i32>>,
}

pub fn process(
  project_root: &Path,
) -> Result<BTreeMap<core::ProjectPath, Vec<SolidityAST>>, String> {
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
  traverse_and_parse_asts(&out_dir, project_root, &mut context)?;

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
      if let Some(dir_name) = path.file_name()
        && dir_name == "build-info"
      {
        continue;
      }
      // Recursively traverse subdirectories
      traverse_and_parse_asts(&path, project_root, context)?;
    } else if path.is_file()
      && let Some(extension) = path.extension()
      && extension == "json"
    {
      println!("Processing JSON file: {:?}", path);
      let ast =
        ast_from_json_file(&path.to_string_lossy(), project_root, context)
          .map_err(|e| {
            format!("Failed to parse JSON file {:?}: {}", path, e)
          })?;

      context
        .ast_map
        .entry(ast.project_path.clone())
        .or_default()
        .push(ast);
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
  let source_content = read_source_file(&project_path, project_root)?;
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
    .map(|node_val| node_from_json(node_val, context))
    .collect();

  let nodes = nodes?;

  Ok(SolidityAST {
    node_id,
    nodes,
    project_path,
  })
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

pub fn type_descriptions_from_json(
  value: &serde_json::Value,
) -> Result<TypeDescriptions, String> {
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
      node_id,
      src_location,
      operator,
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
      node_id,
      src_location,
      left_expression: Box::new(node_to_stub(&left_expression)),
      operator,
      right_expression: Box::new(node_to_stub(&right_expression)),
      type_descriptions,
    },
    ASTNode::Conditional {
      node_id,
      src_location,
      condition,
      true_expression,
      false_expression,
    } => ASTNode::Conditional {
      node_id,
      src_location,
      condition: Box::new(node_to_stub(&condition)),
      true_expression: Box::new(node_to_stub(&true_expression)),
      false_expression: false_expression
        .map(|expr| Box::new(node_to_stub(&expr))),
    },
    ASTNode::ElementaryTypeNameExpression {
      node_id,
      src_location,
      type_descriptions,
      type_name,
    } => ASTNode::ElementaryTypeNameExpression {
      node_id,
      src_location,
      type_descriptions,
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
      node_id,
      src_location,
      arguments: arguments.iter().map(node_to_stub).collect(),
      expression: Box::new(node_to_stub(&expression)),
      name_locations,
      names,
      try_call,
      type_descriptions,
      referenced_return_declarations,
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
      node_id,
      src_location,
      argument: Box::new(node_to_stub(&argument)),
      expression: Box::new(node_to_stub(&expression)),
      name_locations,
      names,
      try_call,
      type_descriptions,
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
      node_id,
      src_location,
      arguments: arguments.iter().map(node_to_stub).collect(),
      expression: Box::new(node_to_stub(&expression)),
      name_locations,
      names,
      try_call,
      type_descriptions,
    },
    ASTNode::FunctionCallOptions {
      node_id,
      src_location,
      expression,
      options,
    } => ASTNode::FunctionCallOptions {
      node_id,
      src_location,
      expression: Box::new(node_to_stub(&expression)),
      options: options.iter().map(node_to_stub).collect(),
    },
    ASTNode::Identifier {
      node_id,
      src_location,
      name,
      overloaded_declarations,
      referenced_declaration,
    } => ASTNode::Identifier {
      node_id,
      src_location,
      name,
      overloaded_declarations,
      referenced_declaration,
    },
    ASTNode::IdentifierPath {
      node_id,
      src_location,
      name,
      name_locations,
      referenced_declaration,
    } => ASTNode::IdentifierPath {
      node_id,
      src_location,
      name,
      name_locations,
      referenced_declaration,
    },
    ASTNode::IndexAccess {
      node_id,
      src_location,
      base_expression,
      index_expression,
    } => ASTNode::IndexAccess {
      node_id,
      src_location,
      base_expression: Box::new(node_to_stub(&base_expression)),
      index_expression: index_expression
        .map(|expr| Box::new(node_to_stub(&expr))),
    },
    ASTNode::IndexRangeAccess {
      node_id,
      src_location,
      nodes,
      body,
    } => ASTNode::IndexRangeAccess {
      node_id,
      src_location,
      nodes: nodes.iter().map(node_to_stub).collect(),
      body: body.map(|b| Box::new(node_to_stub(&b))),
    },
    ASTNode::Literal {
      node_id,
      src_location,
      hex_value,
      kind,
      type_descriptions,
      value,
    } => ASTNode::Literal {
      node_id,
      src_location,
      hex_value,
      kind,
      type_descriptions,
      value,
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
      node_id,
      src_location,
      expression: Box::new(node_to_stub(&expression)),
      member_location,
      member_name,
      referenced_declaration,
      type_descriptions,
    },
    ASTNode::NewExpression {
      node_id,
      src_location,
      type_name,
    } => ASTNode::NewExpression {
      node_id,
      src_location,
      type_name: Box::new(node_to_stub(&type_name)),
    },
    ASTNode::TupleExpression {
      node_id,
      src_location,
      components,
    } => ASTNode::TupleExpression {
      node_id,
      src_location,
      components: components.iter().map(node_to_stub).collect(),
    },
    ASTNode::UnaryOperation {
      node_id,
      src_location,
      prefix,
      operator,
      sub_expression,
    } => ASTNode::UnaryOperation {
      node_id,
      src_location,
      prefix,
      operator,
      sub_expression: Box::new(node_to_stub(&sub_expression)),
    },
    ASTNode::EnumValue {
      node_id,
      src_location,
      name,
      name_location,
    } => ASTNode::EnumValue {
      node_id,
      src_location,
      name,
      name_location,
    },
    ASTNode::Block {
      node_id,
      src_location,
      statements,
    } => ASTNode::Block {
      node_id,
      src_location,
      statements: statements.iter().map(node_to_stub).collect(),
    },
    ASTNode::SemanticBlock {
      node_id,
      src_location,
      documentation,
      statements,
    } => ASTNode::SemanticBlock {
      node_id,
      src_location,
      documentation,
      statements: statements.iter().map(node_to_stub).collect(),
    },
    ASTNode::ContractMemberGroup {
      node_id,
      src_location,
      documentation,
      members,
    } => ASTNode::ContractMemberGroup {
      node_id,
      src_location,
      documentation,
      members: members.iter().map(node_to_stub).collect(),
    },
    ASTNode::Break {
      node_id,
      src_location,
    } => ASTNode::Break {
      node_id,
      src_location,
    },
    ASTNode::Continue {
      node_id,
      src_location,
    } => ASTNode::Continue {
      node_id,
      src_location,
    },
    ASTNode::DoWhileStatement {
      node_id,
      src_location,
      condition,
      body,
    } => ASTNode::DoWhileStatement {
      node_id,
      src_location,
      condition: Box::new(node_to_stub(&condition)),
      body: body.map(|b| Box::new(node_to_stub(&b))),
    },
    ASTNode::EmitStatement {
      node_id,
      src_location,
      event_call,
    } => ASTNode::EmitStatement {
      node_id,
      src_location,
      event_call: Box::new(node_to_stub(&event_call)),
    },
    ASTNode::ExpressionStatement {
      node_id,
      src_location,
      expression,
    } => ASTNode::ExpressionStatement {
      node_id,
      src_location,
      expression: Box::new(node_to_stub(&expression)),
    },
    ASTNode::ForStatement {
      node_id,
      src_location,
      condition,
      body,
    } => ASTNode::ForStatement {
      node_id,
      src_location,
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
      node_id,
      src_location,
      initialization_expression: initialization_expression
        .map(|ie| Box::new(node_to_stub(&ie))),
      condition: condition.map(|c| Box::new(node_to_stub(&c))),
      loop_expression: loop_expression.map(|le| Box::new(node_to_stub(&le))),
      is_simple_counter_loop,
    },
    ASTNode::IfStatement {
      node_id,
      src_location,
      condition,
      true_body,
      false_body,
    } => ASTNode::IfStatement {
      node_id,
      src_location,
      condition: Box::new(node_to_stub(&condition)),
      true_body: Box::new(node_to_stub(&true_body)),
      false_body: false_body.map(|fb| Box::new(node_to_stub(&fb))),
    },
    ASTNode::InlineAssembly {
      node_id,
      src_location,
    } => ASTNode::InlineAssembly {
      node_id,
      src_location,
    },
    ASTNode::PlaceholderStatement {
      node_id,
      src_location,
    } => ASTNode::PlaceholderStatement {
      node_id,
      src_location,
    },
    ASTNode::Return {
      node_id,
      src_location,
      expression,
      function_return_parameters,
    } => ASTNode::Return {
      node_id,
      src_location,
      expression: expression.map(|e| Box::new(node_to_stub(&e))),
      function_return_parameters,
    },
    ASTNode::RevertStatement {
      node_id,
      src_location,
      error_call,
    } => ASTNode::RevertStatement {
      node_id,
      src_location,
      error_call: Box::new(node_to_stub(&error_call)),
    },
    ASTNode::TryStatement {
      node_id,
      src_location,
      clauses,
      external_call,
    } => ASTNode::TryStatement {
      node_id,
      src_location,
      clauses: clauses.iter().map(node_to_stub).collect(),
      external_call: Box::new(node_to_stub(&external_call)),
    },
    ASTNode::UncheckedBlock {
      node_id,
      src_location,
      statements,
    } => ASTNode::UncheckedBlock {
      node_id,
      src_location,
      statements: statements.iter().map(node_to_stub).collect(),
    },
    ASTNode::VariableDeclarationStatement {
      node_id,
      src_location,
      declarations,
      initial_value,
    } => ASTNode::VariableDeclarationStatement {
      node_id,
      src_location,
      declarations: declarations.iter().map(node_to_stub).collect(),
      initial_value: initial_value.map(|iv| Box::new(node_to_stub(&iv))),
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
      type_name: Box::new(node_to_stub(&type_name)),
      value: value.map(|v| Box::new(node_to_stub(&v))),
      visibility,
      parameter_variable,
      implementation_declaration,
      base_functions,
      struct_field,
    },
    ASTNode::WhileStatement {
      node_id,
      src_location,
      condition,
      body,
    } => ASTNode::WhileStatement {
      node_id,
      src_location,
      condition: Box::new(node_to_stub(&condition)),
      body: body.map(|b| Box::new(node_to_stub(&b))),
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
      node_id,
      src_location,
      documentation: documentation.map(|d| Box::new(node_to_stub(&d))),
      name,
      name_location,
      declaration_id,
      contract_kind,
      abstract_,
      base_contracts: base_contracts.iter().map(node_to_stub).collect(),
      directives: directives.iter().map(node_to_stub).collect(),
    },
    ASTNode::ContractDefinition {
      node_id,
      src_location,
      signature,
      nodes,
    } => ASTNode::ContractDefinition {
      node_id,
      src_location,
      signature: Box::new(node_to_stub(&signature)),
      nodes: nodes.iter().map(node_to_stub).collect(),
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
      node_id,
      src_location,
      documentation: documentation.map(|d| Box::new(node_to_stub(&d))),
      kind,
      modifiers: Box::new(node_to_stub(&modifiers)),
      name,
      name_location,
      declaration_id,
      parameters: Box::new(node_to_stub(&parameters)),
      return_parameters: Box::new(node_to_stub(&return_parameters)),
      scope,
      state_mutability,
      virtual_,
      visibility,
      implementation_declaration,
    },
    ASTNode::FunctionDefinition {
      node_id,
      src_location,
      signature,
      implemented,
      body,
    } => ASTNode::FunctionDefinition {
      node_id,
      src_location,
      signature: Box::new(node_to_stub(&signature)),
      implemented,
      body: body.map(|b| Box::new(node_to_stub(&b))),
    },
    ASTNode::EventDefinition {
      node_id,
      src_location,
      name,
      name_location,
      parameters,
    } => ASTNode::EventDefinition {
      node_id,
      src_location,
      name,
      name_location,
      parameters: Box::new(node_to_stub(&parameters)),
    },
    ASTNode::ErrorDefinition {
      node_id,
      src_location,
      name,
      name_location,
      parameters,
    } => ASTNode::ErrorDefinition {
      node_id,
      src_location,
      name,
      name_location,
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
      node_id,
      src_location,
      documentation: documentation.map(|d| Box::new(node_to_stub(&d))),
      name,
      name_location,
      declaration_id,
      parameters: Box::new(node_to_stub(&parameters)),
      virtual_,
      visibility,
      implementation_declaration,
    },
    ASTNode::ModifierDefinition {
      node_id,
      src_location,
      signature,
      body,
    } => ASTNode::ModifierDefinition {
      node_id,
      src_location,
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
      node_id,
      src_location,
      members: members.iter().map(node_to_stub).collect(),
      canonical_name,
      name,
      name_location,
      visibility,
    },
    ASTNode::EnumDefinition {
      node_id,
      src_location,
      members,
      canonical_name,
      name,
      name_location,
    } => ASTNode::EnumDefinition {
      node_id,
      src_location,
      members: members.iter().map(node_to_stub).collect(),
      canonical_name,
      name,
      name_location,
    },
    ASTNode::UserDefinedValueTypeDefinition {
      node_id,
      src_location,
      name,
      underlying_type,
    } => ASTNode::UserDefinedValueTypeDefinition {
      node_id,
      src_location,
      name,
      underlying_type: Box::new(node_to_stub(&underlying_type)),
    },
    ASTNode::PragmaDirective {
      node_id,
      src_location,
      literals,
    } => ASTNode::PragmaDirective {
      node_id,
      src_location,
      literals,
    },
    ASTNode::ImportDirective {
      node_id,
      src_location,
      absolute_path,
      file,
      source_unit,
    } => ASTNode::ImportDirective {
      node_id,
      src_location,
      absolute_path,
      file,
      source_unit,
    },
    ASTNode::UsingForDirective {
      node_id,
      src_location,
      global,
      library_name,
      type_name,
    } => ASTNode::UsingForDirective {
      node_id,
      src_location,
      global,
      library_name: library_name.map(|ln| Box::new(node_to_stub(&ln))),
      type_name: type_name.map(|tn| Box::new(node_to_stub(&tn))),
    },
    ASTNode::SourceUnit {
      node_id,
      src_location,
      nodes,
    } => ASTNode::SourceUnit {
      node_id,
      src_location,
      nodes: nodes.iter().map(node_to_stub).collect(),
    },
    ASTNode::InheritanceSpecifier {
      node_id,
      src_location,
      base_name,
    } => ASTNode::InheritanceSpecifier {
      node_id,
      src_location,
      base_name: Box::new(node_to_stub(&base_name)),
    },
    ASTNode::ElementaryTypeName {
      node_id,
      src_location,
      name,
    } => ASTNode::ElementaryTypeName {
      node_id,
      src_location,
      name,
    },
    ASTNode::FunctionTypeName {
      node_id,
      src_location,
      parameter_types,
      return_parameter_types,
      state_mutability,
      visibility,
    } => ASTNode::FunctionTypeName {
      node_id,
      src_location,
      parameter_types: Box::new(node_to_stub(&parameter_types)),
      return_parameter_types: Box::new(node_to_stub(&return_parameter_types)),
      state_mutability,
      visibility,
    },
    ASTNode::ParameterList {
      node_id,
      src_location,
      parameters,
      is_return_parameters,
    } => ASTNode::ParameterList {
      node_id,
      src_location,
      parameters: parameters.iter().map(node_to_stub).collect(),
      is_return_parameters,
    },
    ASTNode::ModifierList {
      node_id,
      src_location,
      modifiers,
    } => ASTNode::ModifierList {
      node_id,
      src_location,
      modifiers: modifiers.iter().map(node_to_stub).collect(),
    },
    ASTNode::TryCatchClause {
      node_id,
      src_location,
      error_name,
      block,
      parameters,
    } => ASTNode::TryCatchClause {
      node_id,
      src_location,
      error_name,
      block: Box::new(node_to_stub(&block)),
      parameters: parameters.map(|p| Box::new(node_to_stub(&p))),
    },
    ASTNode::ModifierInvocation {
      node_id,
      src_location,
      modifier_name,
      arguments,
    } => ASTNode::ModifierInvocation {
      node_id,
      src_location,
      modifier_name: Box::new(node_to_stub(&modifier_name)),
      arguments: arguments.map(|args| args.iter().map(node_to_stub).collect()),
    },
    ASTNode::UserDefinedTypeName {
      node_id,
      src_location,
      path_node,
      referenced_declaration,
    } => ASTNode::UserDefinedTypeName {
      node_id,
      src_location,
      path_node: Box::new(node_to_stub(&path_node)),
      referenced_declaration,
    },
    ASTNode::ArrayTypeName {
      node_id,
      src_location,
      base_type,
    } => ASTNode::ArrayTypeName {
      node_id,
      src_location,
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
      node_id,
      src_location,
      key_name,
      key_name_location,
      key_type: Box::new(node_to_stub(&key_type)),
      value_name,
      value_name_location,
      value_type: Box::new(node_to_stub(&value_type)),
    },
    ASTNode::StructuredDocumentation {
      node_id,
      src_location,
      text,
    } => ASTNode::StructuredDocumentation {
      node_id,
      src_location,
      text,
    },
    ASTNode::Stub {
      node_id,
      src_location,
      topic,
      kind,
    } => ASTNode::Stub {
      node_id,
      src_location,
      topic,
      kind,
    },
    ASTNode::Other {
      node_id,
      src_location,
      nodes,
      body,
      node_type,
    } => ASTNode::Other {
      node_id,
      src_location,
      nodes: nodes.iter().map(node_to_stub).collect(),
      body: body.map(|b| Box::new(node_to_stub(&b))),
      node_type,
    },
    ASTNode::Argument {
      node_id,
      src_location,
      parameter: referenced_parameter,
      argument,
    } => ASTNode::Argument {
      node_id,
      src_location,
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
    .and_then(SourceLocation::from_str)
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
            .and_then(SourceLocation::from_str)
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
    .and_then(type_descriptions_from_json)
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

/// Groups a list of nodes into slices separated by blank lines, returning
/// each group along with its leading inline documentation comment (if any).
fn group_nodes_by_semantic_breaks(
  statements: Vec<ASTNode>,
  source: &str,
  block_src_location: &SourceLocation,
) -> Vec<(Vec<ASTNode>, Option<String>, SourceLocation)> {
  if statements.is_empty() {
    return vec![];
  }

  let breaks = find_semantic_breaks(source, &statements);
  let mut groups = vec![];

  let build_group =
    |group_statements: Vec<ASTNode>,
     group_start_index: usize,
     all_statements: &[ASTNode]|
     -> Option<(Vec<ASTNode>, Option<String>, SourceLocation)> {
      if group_statements.is_empty() {
        return None;
      }

      let first_stmt_start =
        group_statements[0].src_location().start.unwrap_or(0);
      let last_stmt = &group_statements[group_statements.len() - 1];
      let last_stmt_end = last_stmt.src_location().start.unwrap_or(0)
        + last_stmt.src_location().length.unwrap_or(0);

      let group_start_pos = if group_start_index == 0 {
        block_src_location.start.unwrap_or(0)
      } else {
        all_statements[group_start_index - 1]
          .src_location()
          .start
          .unwrap_or(0)
          + all_statements[group_start_index - 1]
            .src_location()
            .length
            .unwrap_or(0)
      };

      let documentation =
        extract_block_documentation(source, group_start_pos, first_stmt_start);

      let src_location = SourceLocation {
        start: Some(first_stmt_start),
        length: Some(last_stmt_end - first_stmt_start),
        index: None,
      };

      Some((group_statements, documentation, src_location))
    };

  let mut current_group_start = 0;
  for &break_index in &breaks {
    let group_statements =
      statements[current_group_start..break_index].to_vec();
    if let Some(group) =
      build_group(group_statements, current_group_start, &statements)
    {
      groups.push(group);
    }
    current_group_start = break_index;
  }

  let group_statements = statements[current_group_start..].to_vec();
  if let Some(group) =
    build_group(group_statements, current_group_start, &statements)
  {
    groups.push(group);
  }

  groups
}

fn group_statements_into_semantic_blocks(
  statements: Vec<ASTNode>,
  source: &str,
  block_src_location: &SourceLocation,
) -> Result<Vec<ASTNode>, String> {
  let groups =
    group_nodes_by_semantic_breaks(statements, source, block_src_location);
  Ok(
    groups
      .into_iter()
      .map(
        |(statements, documentation, src_location)| ASTNode::SemanticBlock {
          node_id: generate_node_id(),
          src_location,
          documentation,
          statements,
        },
      )
      .collect(),
  )
}

fn group_members_into_contract_member_groups(
  members: Vec<ASTNode>,
  source: &str,
  block_src_location: &SourceLocation,
) -> Result<Vec<ASTNode>, String> {
  let groups =
    group_nodes_by_semantic_breaks(members, source, block_src_location);
  Ok(
    groups
      .into_iter()
      .map(|(members, documentation, src_location)| {
        ASTNode::ContractMemberGroup {
          node_id: generate_node_id(),
          src_location,
          documentation,
          members,
        }
      })
      .collect(),
  )
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
    .and_then(SourceLocation::from_str)
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
      let (directives, mut nodes): (Vec<ASTNode>, Vec<ASTNode>) = all_nodes
        .into_iter()
        .partition(|node| matches!(node, ASTNode::UsingForDirective { .. }));

      // Group contract members separated by blank lines into
      // ContractMemberGroup wrappers so that inline comments are captured
      // on the group and single-child groups become transitive to their
      // child. Unlike SemanticBlock, this wrapper is scope-transparent —
      // its members remain peers of the ContractDefinition for scope
      // purposes.
      if !nodes.is_empty() {
        nodes = group_members_into_contract_member_groups(
          nodes,
          &context.source_content,
          &src_location,
        )?;
      }

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
        flush_natspec_section(
          &mut current_tag,
          &mut current_text,
          &mut sections,
        );
      } else {
        flush_natspec_section(
          &mut current_tag,
          &mut current_text,
          &mut sections,
        );
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
      flush_natspec_section(&mut current_tag, &mut current_text, &mut sections);
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
  if let Some(rest) = after_at.strip_prefix("notice")
    && (rest.is_empty() || rest.starts_with(' '))
  {
    return Some((NatSpecTag::Notice, rest.strip_prefix(' ').unwrap_or(rest)));
  }

  // @dev [<text>]
  if let Some(rest) = after_at.strip_prefix("dev")
    && (rest.is_empty() || rest.starts_with(' '))
  {
    return Some((NatSpecTag::Dev, rest.strip_prefix(' ').unwrap_or(rest)));
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
  if let Some(rest) = after_at.strip_prefix("title")
    && (rest.is_empty() || rest.starts_with(' '))
  {
    return Some((NatSpecTag::Ignored, ""));
  }
  if let Some(rest) = after_at.strip_prefix("author")
    && (rest.is_empty() || rest.starts_with(' '))
  {
    return Some((NatSpecTag::Ignored, ""));
  }
  if let Some(rest) = after_at.strip_prefix("inheritdoc")
    && (rest.is_empty() || rest.starts_with(' '))
  {
    return Some((NatSpecTag::Ignored, ""));
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
