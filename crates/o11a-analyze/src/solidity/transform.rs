//! AST transformation module.
//!
//! This module handles transformations on the parsed AST before tree-shaking.
//! Transformations include:
//! - Wrapping function call arguments with Argument nodes
//! - Remapping interface member references to their implementations in the AST
//!
//! The transform phase operates on the complete AST and mutates it in place.
//! By remapping `referenced_declaration` fields directly in Identifier,
//! IdentifierPath, and MemberAccess nodes, tree shaking naturally follows
//! implementation references without needing a separate reference transfer phase.

use crate::solidity::parser::generate_node_id;
use o11a_core::domain::{CallKind, ContractKind, ProjectPath};
use o11a_core::solidity::ast::{
  ASTNode, FunctionStateMutability, SolidityAST, VariableVisibility,
  contract_members, get_definition_parameters, get_function_return_parameters,
  get_referenced_function_id,
};
use std::collections::{BTreeMap, HashSet};

// ============================================================================
// Definition Collection Types
// ============================================================================

/// Information about a callable definition (function, event, error, modifier).
#[derive(Debug, Clone)]
pub struct CallableDefinition {
  /// The parameters of this callable (VariableDeclaration nodes)
  pub parameters: Vec<ASTNode>,
  /// The return parameters of this callable (VariableDeclaration nodes)
  /// Only applicable to functions; empty for events, errors, and modifiers.
  pub return_parameters: Vec<ASTNode>,
  /// The state mutability of the function, from the compiler's AST.
  /// `None` for modifiers, events, errors (not applicable).
  pub state_mutability: Option<FunctionStateMutability>,
  /// Whether this callable is a modifier. Modifiers are unconditionally
  /// `NonPure` since they execute in the caller's context.
  pub is_modifier: bool,
}

/// Information about a struct definition.
#[derive(Debug, Clone)]
pub struct StructDefinition {
  /// The members of this struct (VariableDeclaration nodes)
  pub members: Vec<ASTNode>,
}

/// Contract info for interface-to-implementation mapping.
#[derive(Debug, Clone)]
pub struct ContractInfo {
  pub node_id: i32,
  pub contract_kind: ContractKind,
  /// The file this contract is defined in
  pub container_file: ProjectPath,
  /// Referenced declaration IDs from baseContracts
  pub base_contract_ids: Vec<i32>,
  /// Functions in this contract with their signatures
  pub functions: Vec<FunctionSignatureInfo>,
  /// Public state variables (for matching interface getter functions)
  pub public_state_variables: Vec<StateVariableInfo>,
}

/// Function signature info for matching interface functions to implementations.
#[derive(Debug, Clone)]
pub struct FunctionSignatureInfo {
  pub node_id: i32,
  pub name: String,
  /// Type strings for each parameter (e.g., ["uint256", "address"])
  pub parameter_types: Vec<String>,
  /// Node IDs for each parameter
  pub parameter_ids: Vec<i32>,
  /// Node IDs for each return parameter
  pub return_parameter_ids: Vec<i32>,
}

/// Public state variable info for getter matching.
#[derive(Debug, Clone)]
pub struct StateVariableInfo {
  pub node_id: i32,
  pub name: String,
  /// IDs of interface functions this state variable's getter implements
  pub base_functions: Vec<i32>,
}

/// Context for the transform phase containing all collected definitions.
pub struct TransformContext {
  /// Map from node ID to callable definition (function, event, error, modifier)
  pub callables: BTreeMap<i32, CallableDefinition>,
  /// Map from node ID to struct definition
  pub structs: BTreeMap<i32, StructDefinition>,
  /// Map from interface member ID to implementation member ID
  pub interface_to_implementation: BTreeMap<i32, i32>,
}

// ============================================================================
// Transform Entry Point
// ============================================================================

/// Transforms the AST map by:
/// 1. Collecting all definitions (functions, structs, events, errors)
/// 2. Building interface-to-implementation mappings
/// 3. Wrapping function call and struct constructor arguments with Argument nodes
/// 4. Remapping interface references to implementation references in the AST
///
/// This function runs BEFORE tree shaking and mutates the AST in place.
/// Interface member references (in Identifier, IdentifierPath, and MemberAccess nodes)
/// are remapped to their implementation members, so tree shaking naturally follows
/// implementation references without needing a separate reference transfer phase.
pub fn transform_ast(
  ast_map: &mut BTreeMap<ProjectPath, Vec<SolidityAST>>,
  in_scope_files: &HashSet<ProjectPath>,
) -> Result<(), String> {
  // Phase 1: Collect all definitions and contract info
  let (callables, structs, contracts) = collect_definitions(ast_map);

  // Phase 2: Build interface-to-implementation mapping (only for in-scope implementations)
  let interface_to_implementation =
    build_interface_member_map(&contracts, in_scope_files);

  // Create transform context
  let context = TransformContext {
    callables,
    structs,
    interface_to_implementation,
  };

  // Phase 3: Transform the AST (wrap arguments, apply interface mappings)
  for (_path, asts) in ast_map.iter_mut() {
    for ast in asts.iter_mut() {
      for node in ast.nodes.iter_mut() {
        transform_node(node, &context);
      }
    }
  }

  Ok(())
}

// ============================================================================
// Definition Collection
// ============================================================================

/// Collects all callable definitions (functions, events, errors, modifiers),
/// struct definitions, and contract info from the AST.
fn collect_definitions(
  ast_map: &BTreeMap<ProjectPath, Vec<SolidityAST>>,
) -> (
  BTreeMap<i32, CallableDefinition>,
  BTreeMap<i32, StructDefinition>,
  Vec<ContractInfo>,
) {
  let mut callables = BTreeMap::new();
  let mut structs = BTreeMap::new();
  let mut contracts = Vec::new();

  for (path, asts) in ast_map {
    for ast in asts {
      for node in &ast.nodes {
        collect_definitions_from_node(
          node,
          path,
          &mut callables,
          &mut structs,
          &mut contracts,
        );
      }
    }
  }

  (callables, structs, contracts)
}

/// Recursively collects definitions from a node and its children.
fn collect_definitions_from_node(
  node: &ASTNode,
  container_file: &ProjectPath,
  callables: &mut BTreeMap<i32, CallableDefinition>,
  structs: &mut BTreeMap<i32, StructDefinition>,
  contracts: &mut Vec<ContractInfo>,
) {
  match node {
    ASTNode::ContractDefinition {
      node_id, signature, ..
    } => {
      // Extract contract info for interface mapping
      if let Some(contract_info) =
        extract_contract_info(*node_id, container_file, signature, node)
      {
        contracts.push(contract_info);
      }

      // Recurse into contract members (flatten through semantic blocks)
      for child in contract_members(node) {
        collect_definitions_from_node(
          child,
          container_file,
          callables,
          structs,
          contracts,
        );
      }
    }

    ASTNode::FunctionDefinition {
      node_id, signature, ..
    } => {
      let state_mutability = match signature.as_ref() {
        ASTNode::FunctionSignature {
          state_mutability, ..
        } => Some(*state_mutability),
        _ => None,
      };
      if let Some(params) = get_definition_parameters(node) {
        let return_params = get_function_return_parameters(node)
          .cloned()
          .unwrap_or_default();
        callables.insert(
          *node_id,
          CallableDefinition {
            parameters: params.clone(),
            return_parameters: return_params,
            state_mutability,
            is_modifier: false,
          },
        );
      }
    }

    ASTNode::ModifierDefinition {
      node_id, signature, ..
    } => {
      if let ASTNode::ModifierSignature { parameters, .. } = signature.as_ref()
        && let ASTNode::ParameterList {
          parameters: params, ..
        } = parameters.as_ref()
      {
        callables.insert(
          *node_id,
          CallableDefinition {
            parameters: params.clone(),
            return_parameters: Vec::new(),
            state_mutability: None,
            is_modifier: true,
          },
        );
      }
    }

    ASTNode::EventDefinition {
      node_id,
      parameters,
      ..
    } => {
      if let ASTNode::ParameterList {
        parameters: params, ..
      } = parameters.as_ref()
      {
        callables.insert(
          *node_id,
          CallableDefinition {
            parameters: params.clone(),
            return_parameters: Vec::new(),
            state_mutability: None,
            is_modifier: false,
          },
        );
      }
    }

    ASTNode::ErrorDefinition {
      node_id,
      parameters,
      ..
    } => {
      if let ASTNode::ParameterList {
        parameters: params, ..
      } = parameters.as_ref()
      {
        callables.insert(
          *node_id,
          CallableDefinition {
            parameters: params.clone(),
            return_parameters: Vec::new(),
            state_mutability: None,
            is_modifier: false,
          },
        );
      }
    }

    ASTNode::StructDefinition {
      node_id, members, ..
    } => {
      structs.insert(
        *node_id,
        StructDefinition {
          members: members.clone(),
        },
      );
    }

    // Recurse into source units
    ASTNode::SourceUnit { nodes, .. } => {
      for child in nodes {
        collect_definitions_from_node(
          child,
          container_file,
          callables,
          structs,
          contracts,
        );
      }
    }

    _ => {}
  }
}

/// Extracts contract info from a ContractDefinition for interface mapping.
fn extract_contract_info(
  node_id: i32,
  container_file: &ProjectPath,
  signature: &ASTNode,
  contract_node: &ASTNode,
) -> Option<ContractInfo> {
  let (contract_kind, base_contracts) = match signature {
    ASTNode::ContractSignature {
      contract_kind,
      base_contracts,
      ..
    } => (contract_kind, base_contracts),
    _ => return None,
  };

  // Extract base contract IDs from InheritanceSpecifier nodes
  let base_contract_ids: Vec<i32> = base_contracts
    .iter()
    .filter_map(|spec| {
      if let ASTNode::InheritanceSpecifier { base_name, .. } = spec
        && let ASTNode::IdentifierPath {
          referenced_declaration,
          ..
        } = base_name.as_ref()
      {
        return Some(*referenced_declaration);
      }
      None
    })
    .collect();

  // Extract functions and public state variables
  let mut functions = Vec::new();
  let mut public_state_variables = Vec::new();

  for child in contract_members(contract_node) {
    match child {
      ASTNode::FunctionDefinition {
        node_id: func_id,
        signature,
        ..
      } => {
        if let Some(func_info) =
          extract_function_signature_info(*func_id, signature)
        {
          functions.push(func_info);
        }
      }
      ASTNode::VariableDeclaration {
        node_id: var_id,
        name,
        visibility,
        state_variable,
        base_functions,
        ..
      } => {
        if *state_variable && *visibility == VariableVisibility::Public {
          public_state_variables.push(StateVariableInfo {
            node_id: *var_id,
            name: name.clone(),
            base_functions: base_functions.clone(),
          });
        }
      }
      _ => {}
    }
  }

  Some(ContractInfo {
    node_id,
    contract_kind: *contract_kind,
    container_file: container_file.clone(),
    base_contract_ids,
    functions,
    public_state_variables,
  })
}

/// Extracts function signature info for interface matching.
fn extract_function_signature_info(
  node_id: i32,
  signature: &ASTNode,
) -> Option<FunctionSignatureInfo> {
  let (name, parameters, return_parameters) = match signature {
    ASTNode::FunctionSignature {
      name,
      parameters,
      return_parameters,
      ..
    } => (name, parameters, return_parameters),
    _ => return None,
  };

  let (parameter_types, parameter_ids) = extract_parameter_info(parameters);
  let (_, return_parameter_ids) = extract_parameter_info(return_parameters);

  Some(FunctionSignatureInfo {
    node_id,
    name: name.clone(),
    parameter_types,
    parameter_ids,
    return_parameter_ids,
  })
}

/// Extracts parameter type strings and IDs from a ParameterList.
fn extract_parameter_info(param_list: &ASTNode) -> (Vec<String>, Vec<i32>) {
  let parameters = match param_list {
    ASTNode::ParameterList { parameters, .. } => parameters,
    _ => return (Vec::new(), Vec::new()),
  };

  let types: Vec<String> = parameters
    .iter()
    .filter_map(|param| {
      if let ASTNode::VariableDeclaration { type_name, .. } = param {
        extract_type_string(type_name)
      } else {
        None
      }
    })
    .collect();

  let ids: Vec<i32> = parameters.iter().map(|p| p.node_id()).collect();

  (types, ids)
}

/// Extracts a type string from a type name node.
fn extract_type_string(type_name: &ASTNode) -> Option<String> {
  match type_name {
    ASTNode::ElementaryTypeName { name, .. } => Some(name.clone()),
    ASTNode::UserDefinedTypeName { path_node, .. } => {
      if let ASTNode::IdentifierPath { name, .. } = path_node.as_ref() {
        Some(name.clone())
      } else {
        None
      }
    }
    ASTNode::ArrayTypeName { base_type, .. } => {
      extract_type_string(base_type).map(|t| format!("{}[]", t))
    }
    ASTNode::Mapping {
      key_type,
      value_type,
      ..
    } => {
      let key = extract_type_string(key_type)?;
      let value = extract_type_string(value_type)?;
      Some(format!("mapping({} => {})", key, value))
    }
    _ => None,
  }
}

// ============================================================================
// Interface-to-Implementation Mapping
// ============================================================================

/// Builds a mapping from interface member node IDs to implementation member node IDs.
/// Only interfaces with exactly one in-scope implementation are mapped.
fn build_interface_member_map(
  contracts: &[ContractInfo],
  in_scope_files: &HashSet<ProjectPath>,
) -> BTreeMap<i32, i32> {
  // Build a map of interface ID -> list of in-scope implementation IDs
  let mut interface_implementations: BTreeMap<i32, Vec<i32>> = BTreeMap::new();

  for contract in contracts {
    // Skip interfaces - they can't be implementations
    if contract.contract_kind == ContractKind::Interface {
      continue;
    }

    // Only consider implementations that are in scope
    if !in_scope_files.contains(&contract.container_file) {
      continue;
    }

    // For each base contract (interface or parent), record this as an implementation
    for &base_id in &contract.base_contract_ids {
      interface_implementations
        .entry(base_id)
        .or_default()
        .push(contract.node_id);
    }
  }

  // Now build member mappings for interfaces with exactly one implementation
  let mut member_map: BTreeMap<i32, i32> = BTreeMap::new();

  // Create a map of contract node_id -> contract info for quick lookup
  let contracts_by_id: BTreeMap<i32, &ContractInfo> =
    contracts.iter().map(|c| (c.node_id, c)).collect();

  for (interface_id, impl_ids) in &interface_implementations {
    // Only process if exactly one implementation
    if impl_ids.len() != 1 {
      continue;
    }

    let impl_id = impl_ids[0];

    // Get the interface and implementation info
    let interface = match contracts_by_id.get(interface_id) {
      Some(c) if c.contract_kind == ContractKind::Interface => c,
      _ => continue, // Not an interface or not found
    };
    let implementation = match contracts_by_id.get(&impl_id) {
      Some(c) => c,
      None => continue,
    };

    // Map interface functions to implementation functions by signature
    for iface_func in &interface.functions {
      // First try to match by function signature
      if let Some(impl_func) =
        find_matching_function(iface_func, &implementation.functions)
      {
        member_map.insert(iface_func.node_id, impl_func.node_id);

        // Also map interface parameter IDs to implementation parameter IDs
        for (iface_param_id, impl_param_id) in iface_func
          .parameter_ids
          .iter()
          .zip(impl_func.parameter_ids.iter())
        {
          member_map.insert(*iface_param_id, *impl_param_id);
        }

        // Also map interface return parameter IDs to implementation return parameter IDs
        for (iface_ret_id, impl_ret_id) in iface_func
          .return_parameter_ids
          .iter()
          .zip(impl_func.return_parameter_ids.iter())
        {
          member_map.insert(*iface_ret_id, *impl_ret_id);
        }
      }
      // If no function match, try to match to a public state variable
      // First check if any state variable's baseFunctions contains this interface function
      else if let Some(state_var) = implementation
        .public_state_variables
        .iter()
        .find(|sv| sv.base_functions.contains(&iface_func.node_id))
      {
        member_map.insert(iface_func.node_id, state_var.node_id);
      }
      // Fall back to name matching for state variables with no parameters
      else if iface_func.parameter_types.is_empty()
        && let Some(state_var) = implementation
          .public_state_variables
          .iter()
          .find(|sv| sv.name == iface_func.name)
      {
        member_map.insert(iface_func.node_id, state_var.node_id);
      }
    }
  }

  member_map
}

/// Finds a function in the list that matches the given function's signature.
fn find_matching_function<'a>(
  target: &FunctionSignatureInfo,
  candidates: &'a [FunctionSignatureInfo],
) -> Option<&'a FunctionSignatureInfo> {
  candidates.iter().find(|c| {
    c.name == target.name && c.parameter_types == target.parameter_types
  })
}

// ============================================================================
// AST Transformation
// ============================================================================

/// Transforms a node by wrapping arguments and applying interface mappings.
/// This function mutates the node in place.
fn transform_node(node: &mut ASTNode, context: &TransformContext) {
  match node {
    ASTNode::FunctionCall {
      arguments,
      expression,
      referenced_return_declarations,
      call_purity,
      ..
    } => {
      // Check if this is a library method-style call (e.g., x.mulDiv(a, b))
      // and transform it to include the implicit first argument (e.g., uint256.mulDiv(x, a, b))
      if let ASTNode::MemberAccess {
        expression: base_expr,
        type_descriptions,
        ..
      } = expression.as_mut()
      {
        // Library extension methods have "attached_to" in their type_identifier
        if let Some(attached_type) =
          extract_attached_type(&type_descriptions.type_identifier)
        {
          // Extract the base expression and prepend it as the first argument
          // Replace the base expression with the attached type (e.g., uint256)
          let type_node = ASTNode::ElementaryTypeName {
            node_id: generate_node_id(),
            src_location: base_expr.src_location().clone(),
            name: attached_type,
          };
          let implicit_first_arg =
            std::mem::replace(base_expr.as_mut(), type_node);
          arguments.insert(0, implicit_first_arg);
        }
      }

      // Wrap arguments with Argument nodes
      let new_arguments = wrap_arguments(arguments, expression, context);
      *arguments = new_arguments;

      // Populate referenced_return_declarations with return parameter node IDs
      *referenced_return_declarations =
        get_return_declaration_ids(expression, context);

      // Populate call_purity from the callee's stateMutability.
      // Modifiers are unconditionally NonPure. Functions are Pure when the
      // compiler marks them pure or view. Unresolvable callees retain the
      // conservative NonPure default set during parsing.
      *call_purity = resolve_call_purity(expression, context);

      // Recurse into expression and wrapped arguments
      transform_node(expression.as_mut(), context);
      for arg in arguments.iter_mut() {
        transform_node(arg, context);
      }
    }

    ASTNode::StructConstructor {
      arguments,
      expression,
      ..
    } => {
      // Wrap arguments with Argument nodes (linking to struct members)
      let new_arguments = wrap_struct_arguments(arguments, expression, context);
      *arguments = new_arguments;

      // Recurse into expression and wrapped arguments
      transform_node(expression.as_mut(), context);
      for arg in arguments.iter_mut() {
        transform_node(arg, context);
      }
    }

    // Set implementation_declaration on interface FunctionSignatures
    ASTNode::FunctionSignature {
      declaration_id,
      implementation_declaration,
      parameters,
      return_parameters,
      modifiers,
      documentation,
      ..
    } => {
      // Check if this function has a mapping to an implementation
      // Use declaration_id which points to the FunctionDefinition's node_id
      if let Some(&impl_id) =
        context.interface_to_implementation.get(declaration_id)
      {
        *implementation_declaration = Some(impl_id);
      }

      // Recurse into children
      transform_node(parameters.as_mut(), context);
      transform_node(return_parameters.as_mut(), context);
      transform_node(modifiers.as_mut(), context);
      if let Some(doc) = documentation {
        transform_node(doc.as_mut(), context);
      }
    }

    // Set implementation_declaration on interface ModifierSignatures
    ASTNode::ModifierSignature {
      declaration_id,
      implementation_declaration,
      parameters,
      documentation,
      ..
    } => {
      // Check if this modifier has a mapping to an implementation
      if let Some(&impl_id) =
        context.interface_to_implementation.get(declaration_id)
      {
        *implementation_declaration = Some(impl_id);
      }

      // Recurse into children
      transform_node(parameters.as_mut(), context);
      if let Some(doc) = documentation {
        transform_node(doc.as_mut(), context);
      }
    }

    // Set implementation_declaration on interface parameter VariableDeclarations
    ASTNode::VariableDeclaration {
      node_id,
      implementation_declaration,
      type_name,
      value,
      ..
    } => {
      // Check if this parameter has a mapping to an implementation parameter
      if let Some(&impl_param_id) =
        context.interface_to_implementation.get(node_id)
      {
        *implementation_declaration = Some(impl_param_id);
      }

      // Recurse into children - type_name and value may contain FunctionCalls
      transform_node(type_name.as_mut(), context);
      if let Some(val) = value {
        transform_node(val.as_mut(), context);
      }
    }

    // Remap MemberAccess references from interface members to implementations
    ASTNode::MemberAccess {
      referenced_declaration,
      expression,
      ..
    } => {
      if let Some(ref_decl) = referenced_declaration
        && let Some(&impl_id) =
          context.interface_to_implementation.get(ref_decl)
      {
        *referenced_declaration = Some(impl_id);
      }

      // Recurse into expression
      transform_node(expression.as_mut(), context);
    }

    // Remap Identifier references from interface members to implementations
    ASTNode::Identifier {
      referenced_declaration,
      ..
    }
    | ASTNode::IdentifierPath {
      referenced_declaration,
      ..
    } => {
      if let Some(&impl_id) = context
        .interface_to_implementation
        .get(referenced_declaration)
      {
        *referenced_declaration = impl_id;
      }
      // Identifier has no children to recurse into
    }

    // Recurse into all child nodes
    _ => {
      for child in node.nodes_mut() {
        transform_node(child, context);
      }
    }
  }
}

/// Wraps function call arguments with Argument nodes.
fn wrap_arguments(
  arguments: &mut Vec<ASTNode>,
  expression: &ASTNode,
  context: &TransformContext,
) -> Vec<ASTNode> {
  // Try to find the referenced definition and its parameters
  let parameters: Option<&Vec<ASTNode>> =
    get_referenced_function_id(expression)
      .and_then(|def_id| {
        // First check if this is an interface member that maps to an implementation
        let effective_id = context
          .interface_to_implementation
          .get(&def_id)
          .copied()
          .unwrap_or(def_id);
        context.callables.get(&effective_id)
      })
      .map(|def| &def.parameters);

  wrap_arguments_impl(std::mem::take(arguments), parameters)
}

/// Gets the node IDs of return parameter declarations for a function call.
fn get_return_declaration_ids(
  expression: &ASTNode,
  context: &TransformContext,
) -> Vec<i32> {
  get_referenced_function_id(expression)
    .and_then(|def_id| {
      // First check if this is an interface member that maps to an implementation
      let effective_id = context
        .interface_to_implementation
        .get(&def_id)
        .copied()
        .unwrap_or(def_id);
      context.callables.get(&effective_id)
    })
    .map(|def| {
      def
        .return_parameters
        .iter()
        .map(|param| param.node_id())
        .collect()
    })
    .unwrap_or_default()
}

/// Wraps struct constructor arguments with Argument nodes.
fn wrap_struct_arguments(
  arguments: &mut Vec<ASTNode>,
  expression: &ASTNode,
  context: &TransformContext,
) -> Vec<ASTNode> {
  // Try to find the referenced struct and its members
  let members: Option<&Vec<ASTNode>> = get_referenced_function_id(expression)
    .and_then(|struct_id| context.structs.get(&struct_id))
    .map(|def| &def.members);

  wrap_arguments_impl(std::mem::take(arguments), members)
}

/// Common implementation for wrapping arguments with Argument nodes.
fn wrap_arguments_impl(
  arguments: Vec<ASTNode>,
  parameters: Option<&Vec<ASTNode>>,
) -> Vec<ASTNode> {
  arguments
    .into_iter()
    .enumerate()
    .map(|(i, arg)| {
      let arg_src_location = arg.src_location().clone();

      // Create an Identifier node that references the parameter's VariableDeclaration
      let referenced_param = parameters
        .and_then(|params| params.get(i))
        .and_then(|param| {
          if let ASTNode::VariableDeclaration {
            node_id,
            name,
            name_location,
            ..
          } = param
          {
            Some(Box::new(ASTNode::Identifier {
              node_id: generate_node_id(),
              src_location: name_location.clone(),
              name: name.clone(),
              overloaded_declarations: vec![],
              referenced_declaration: *node_id,
            }))
          } else {
            None
          }
        });

      ASTNode::Argument {
        node_id: generate_node_id(),
        src_location: arg_src_location,
        parameter: referenced_param,
        argument: Box::new(arg),
      }
    })
    .collect()
}

/// Extracts the attached type from a type_identifier string.
/// Library extension methods have type_identifiers like:
/// "t_function_internal_pure$_..._$attached_to$_t_uint256_$"
/// This function extracts "uint256" from such strings.
fn extract_attached_type(type_identifier: &str) -> Option<String> {
  // Find the "attached_to$" marker
  let marker = "attached_to$_t_";
  let start = type_identifier.find(marker)?;
  let after_marker = &type_identifier[start + marker.len()..];

  // Find the ending "_$" for the type
  let end = after_marker.find("_$")?;
  let type_str = &after_marker[..end];

  // Handle contract types: "contract$_ContractName_$12345" -> "ContractName"
  if let Some(after_contract) = type_str.strip_prefix("contract$_") {
    // Find the next "_$" which separates the name from the ID
    if let Some(name_end) = after_contract.find("_$") {
      return Some(after_contract[..name_end].to_string());
    }
    return Some(after_contract.to_string());
  }

  // For elementary types like "uint256", "address", etc., return as-is
  Some(type_str.to_string())
}

/// Resolves the purity of a function call by looking up the callee's
/// `stateMutability` from the compiler AST. Modifiers are unconditionally
/// `NonPure`. Unresolvable callees default to `NonPure` (conservative).
fn resolve_call_purity(
  expression: &ASTNode,
  context: &TransformContext,
) -> CallKind {
  let effective_def =
    get_referenced_function_id(expression).and_then(|def_id| {
      let effective_id = context
        .interface_to_implementation
        .get(&def_id)
        .copied()
        .unwrap_or(def_id);
      context.callables.get(&effective_id)
    });

  let Some(def) = effective_def else {
    return CallKind::NonPure;
  };

  if def.is_modifier {
    return CallKind::NonPure;
  }

  match def.state_mutability {
    Some(FunctionStateMutability::Pure | FunctionStateMutability::View) => {
      CallKind::Pure
    }
    _ => CallKind::NonPure,
  }
}
