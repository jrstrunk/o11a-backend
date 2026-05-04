use foundry_compilers_artifacts::Visibility;

use crate::solidity::parser;
use crate::solidity::transform;
use o11a_core::collaborator::models;
use o11a_core::collaborator::synthetic::create_synthetic_dev_comment;
use o11a_core::domain::topic;
use o11a_core::domain::{self, AuditData, UnnamedTopicKind};
use o11a_core::domain::{
  AST, CommentType, DataContext, ElementaryType, FunctionModProperties,
  NamedTopicKind, Node, RevertConstraintKind, Scope, SolidityType,
  SourceContext, TopicMetadata, insert_into_context,
};
use o11a_core::solidity::ast::{
  self, ASTNode, FunctionVisibility, NatSpecSection, NatSpecTag, SolidityAST,
  VariableVisibility, contract_members,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

pub fn analyze(
  project_root: &Path,
  audit_id: &str,
  data_context: &mut DataContext,
) -> Result<(), String> {
  // Get or create the audit data
  let audit_data = data_context
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit '{}' not found", audit_id))?;

  // Parse all ASTs
  let mut ast_map = parser::process(project_root).map_err(|e| e.to_string())?;

  // Transform phase: Apply AST transformations before the first pass
  // This wraps function call arguments with Argument nodes and remaps
  // interface member references to their implementation members in the AST.
  transform::transform_ast(&mut ast_map, &audit_data.in_scope_files)?;

  // First pass: build a comprehensive declaration dictionary
  // This processes every declaration in every file, regardless of scope
  let first_pass_source_topics =
    first_pass(&ast_map, &audit_data.in_scope_files)?;

  // Persist contract inheritance from first-pass `base_contracts` before
  // tree_shake consumes (and drops) that data. Used by the resolution-graph
  // builder to emit `implements` edges.
  collect_contract_inheritance(
    &first_pass_source_topics,
    &mut audit_data.inheritance,
  );

  // Ancestry pass: collect variable ancestry and relatives relationships
  let (all_ancestors, all_relatives) = ancestry_pass(&ast_map);

  // Tree shaking: Build in-scope dictionary by following references from
  // publicly visible declarations. Also builds a map of variable mutations.
  // Note: Interface references are already remapped to implementations in the AST
  // by the transform phase, so tree shaking naturally follows implementation references.
  let (in_scope_source_topics, mutations_map) =
    tree_shake(&first_pass_source_topics)?;

  // Filter ancestors/relatives to only in-scope variables and derive descendants
  let (ancestors_map, descendants_map, relatives_map) =
    filter_and_derive_descendants(
      &all_ancestors,
      &all_relatives,
      &in_scope_source_topics,
    );

  // Populate nodes pass: Build the nodes map before second_pass
  // This allows reference nodes to be looked up for sorting by source location
  populate_nodes_pass(&ast_map, &in_scope_source_topics, &mut audit_data.nodes);

  // Second pass: Build final data structures for in-scope declarations
  // Pass mutable references to audit_data's maps directly
  second_pass(
    &ast_map,
    &in_scope_source_topics,
    &audit_data.in_scope_files,
    &mutations_map,
    &ancestors_map,
    &descendants_map,
    &relatives_map,
    &mut audit_data.nodes,
    &mut audit_data.topic_metadata,
    &mut audit_data.topic_context,
    &mut audit_data.expanded_topic_context,
    &mut audit_data.function_properties,
    &mut audit_data.variable_types,
  )?;

  // Insert ASTs with stubbed nodes
  for (path, ast_list) in ast_map {
    for ast in ast_list {
      let stubbed_ast = SolidityAST {
        node_id: ast.node_id,
        nodes: ast
          .nodes
          .iter()
          .map(|n| parser::children_to_stubs(n.clone()))
          .collect(),
        project_path: ast.project_path.clone(),
      };
      audit_data
        .asts
        .insert(path.clone(), AST::Solidity(stubbed_ast));
    }
  }

  // Build name index for fast topic lookup. Required by dev doc injection
  // (which resolves code references in developer prose) and by the
  // documentation analyzer (which resolves inline code tokens).
  audit_data.name_index = domain::TopicNameIndex::build(audit_data);

  // Dev-doc injection (`inject_developer_documentation`) is intentionally
  // *not* called from here. The analysis pipeline orchestrator
  // (`o11a_analyze::analysis::run_analysis`) drives it after the
  // resolution graph has been built so that future graph-driven
  // resolution passes (Phase 7) have a graph available when they parse
  // synthetic dev-doc comments.

  Ok(())
}

// ============================================================================
// First Pass Revert Types
// ============================================================================

/// A revert or require statement found during first pass traversal.
/// Only the statement node ID and kind are recorded; control flow context
/// is derived from scope in the second pass.
#[derive(Debug, Clone)]
pub struct FirstPassRevert {
  pub statement_node: i32,
  pub kind: RevertConstraintKind,
  /// Node ID of the custom error referenced by `revert MyError(...)`.
  /// `None` for `require(cond, "string")` and bare `revert("string")` —
  /// those have no associated error declaration.
  pub error_node: Option<i32>,
}

// ============================================================================
// First Pass Declaration Types
// ============================================================================

/// First pass declaration structure used during initial AST traversal.
/// Contains basic declaration information and references without topic IDs.
/// This is used to build a comprehensive dictionary of all declarations
/// before determining which ones are in-scope and need detailed analysis.
/// The is_publicly_in_scope field indicates if the declaration is publicly
/// visible (contracts, public/external functions, constructors, fallback, receive).
///
/// Two variants exist:
/// - Block: For declarations that contain executable code (functions, modifiers)
///   These track referenced nodes and revert constraints for analysis
/// - Flat: For simple declarations without executable code (contracts, structs, etc.)
///   These only track basic declaration information
#[derive(Debug, Clone)]
pub enum FirstPassDeclaration {
  FunctionMod {
    /// The contract/library/interface that defines this function or modifier.
    /// Used during tree shaking to correctly scope internal references to
    /// their defining contract rather than the calling contract. For example,
    /// when NudgeCampaign calls SafeERC20.safeTransfer, references inside
    /// safeTransfer should be scoped to SafeERC20, not NudgeCampaign.
    parent_contract: Option<i32>,
    declaration_kind: NamedTopicKind,
    visibility: Visibility,
    name: String,
    referenced_nodes: Vec<ReferencedNode>,
    reverts: Vec<FirstPassRevert>,
    function_calls: Vec<i32>,
    variable_mutations: Vec<ReferencedNode>,
    /// Node IDs of events emitted by this function/modifier (from
    /// `EmitStatement` AST nodes). Unsorted; sort/dedup happens in
    /// second_pass.
    events_emitted: Vec<i32>,
  },
  Contract {
    /// The file where this contract is defined. Used to determine if the
    /// contract is in scope when building reference groups.
    container_file: domain::ProjectPath,
    is_publicly_in_scope: bool,
    declaration_kind: NamedTopicKind,
    visibility: Visibility,
    name: String,
    base_contracts: Vec<ReferencedNode>,
    other_contracts: Vec<ReferencedNode>,
    public_members: Vec<i32>,
    referenced_nodes: Vec<ReferencedNode>,
  },
  Flat {
    /// The contract/library/interface that defines this declaration.
    /// See FunctionMod::parent_contract for detailed explanation.
    parent_contract: Option<i32>,
    declaration_kind: NamedTopicKind,
    visibility: Visibility,
    name: String,
  },
}

#[derive(Debug, Clone)]
pub struct ReferencedNode {
  statement_node: i32,
  referenced_node: i32,
}

/// A reference to a declaration with its scope context.
/// Used to track where references occur for grouping and sorting.
#[derive(Debug, Clone)]
pub struct ScopedReference {
  /// The node ID of the reference (statement/expression that references the declaration)
  pub reference_node: i32,
  /// The node ID of the containing component (contract/interface/library)
  pub containing_component: i32,
  /// The containing function/modifier, if the reference is within a member.
  /// Some = reference is inside a function/modifier (member scope)
  /// None = reference is at contract level (contract scope)
  pub containing_member: Option<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceProcessingMethod {
  Normal,
  ProcessAllContractMembers,
}

// ============================================================================
// Type Extraction
// ============================================================================

/// Extracts a SolidityType from a type AST node.
/// Returns None if the type cannot be determined.
pub fn extract_solidity_type(type_node: &ASTNode) -> Option<SolidityType> {
  match type_node {
    ASTNode::ElementaryTypeName { name, .. } => {
      parse_elementary_type_name(name).map(SolidityType::Elementary)
    }
    ASTNode::UserDefinedTypeName {
      referenced_declaration,
      ..
    } => Some(SolidityType::UserDefined {
      declaration_topic: topic::new_node_topic(referenced_declaration),
    }),
    ASTNode::ArrayTypeName { base_type, .. } => {
      let base = extract_solidity_type(base_type)?;
      // TODO: Extract array length from the AST if it's a fixed-size array
      Some(SolidityType::Array {
        base_type: Box::new(base),
        length: None,
      })
    }
    ASTNode::Mapping {
      key_type,
      value_type,
      ..
    } => {
      let key = extract_solidity_type(key_type)?;
      let value = extract_solidity_type(value_type)?;
      Some(SolidityType::Mapping {
        key_type: Box::new(key),
        value_type: Box::new(value),
      })
    }
    ASTNode::FunctionTypeName {
      parameter_types,
      return_parameter_types,
      ..
    } => {
      let params = extract_parameter_types(parameter_types);
      let returns = extract_parameter_types(return_parameter_types);
      Some(SolidityType::Function {
        parameter_types: params,
        return_types: returns,
      })
    }
    _ => None,
  }
}

/// Extracts types from a ParameterList node
fn extract_parameter_types(param_list: &ASTNode) -> Vec<SolidityType> {
  if let ASTNode::ParameterList { parameters, .. } = param_list {
    parameters
      .iter()
      .filter_map(|param| {
        if let ASTNode::VariableDeclaration { type_name, .. } = param {
          extract_solidity_type(type_name)
        } else {
          None
        }
      })
      .collect()
  } else {
    Vec::new()
  }
}

/// Parses an elementary type name string into an ElementaryType.
/// Handles: bool, address, address payable, string, bytes, bytesN, intN, uintN
fn parse_elementary_type_name(name: &str) -> Option<ElementaryType> {
  match name {
    "bool" => Some(ElementaryType::Bool),
    "address" => Some(ElementaryType::Address),
    "address payable" => Some(ElementaryType::AddressPayable),
    "string" => Some(ElementaryType::String),
    "bytes" => Some(ElementaryType::Bytes),
    _ => {
      // Try to parse bytesN (bytes1 to bytes32)
      if let Some(suffix) = name.strip_prefix("bytes")
        && let Ok(n) = suffix.parse::<u8>()
        && (1..=32).contains(&n)
      {
        return Some(ElementaryType::FixedBytes(n));
      }
      // Try to parse uintN (uint8 to uint256)
      if let Some(suffix) = name.strip_prefix("uint") {
        if suffix.is_empty() {
          // "uint" defaults to uint256
          return Some(ElementaryType::Uint { bits: 256 });
        }
        if let Ok(bits) = suffix.parse::<u16>()
          && (8..=256).contains(&bits)
          && bits % 8 == 0
        {
          return Some(ElementaryType::Uint { bits });
        }
      }
      // Try to parse intN (int8 to int256)
      if let Some(suffix) = name.strip_prefix("int") {
        if suffix.is_empty() {
          // "int" defaults to int256
          return Some(ElementaryType::Int { bits: 256 });
        }
        if let Ok(bits) = suffix.parse::<u16>()
          && (8..=256).contains(&bits)
          && bits % 8 == 0
        {
          return Some(ElementaryType::Int { bits });
        }
      }
      None
    }
  }
}

pub enum InScopeDeclaration {
  // Functions and Modifiers
  FunctionMod {
    declaration_kind: NamedTopicKind,
    visibility: Visibility,
    name: String,
    references: Vec<ScopedReference>,
    reverts: Vec<FirstPassRevert>,
    function_calls: Vec<i32>,
    variable_mutations: Vec<ReferencedNode>,
    events_emitted: Vec<i32>,
  },
  Contract {
    container_file: domain::ProjectPath,
    declaration_kind: NamedTopicKind,
    visibility: Visibility,
    name: String,
    references: Vec<ScopedReference>,
    base_contracts: Vec<ReferencedNode>,
    other_contracts: Vec<ReferencedNode>,
    public_members: Vec<i32>,
  },
  // All other declarations
  Flat {
    declaration_kind: NamedTopicKind,
    visibility: Visibility,
    name: String,
    references: Vec<ScopedReference>,
  },
}

impl InScopeDeclaration {
  pub fn add_reference_if_not_present(&mut self, reference: ScopedReference) {
    match self {
      InScopeDeclaration::FunctionMod { references, .. }
      | InScopeDeclaration::Flat { references, .. }
      | InScopeDeclaration::Contract { references, .. } => {
        if !references
          .iter()
          .any(|r| r.reference_node == reference.reference_node)
        {
          references.push(reference);
        }
      }
    }
  }

  pub fn declaration_kind(&self) -> &NamedTopicKind {
    match self {
      InScopeDeclaration::FunctionMod {
        declaration_kind, ..
      }
      | InScopeDeclaration::Flat {
        declaration_kind, ..
      }
      | InScopeDeclaration::Contract {
        declaration_kind, ..
      } => declaration_kind,
    }
  }

  pub fn name(&self) -> &String {
    match self {
      InScopeDeclaration::FunctionMod { name, .. }
      | InScopeDeclaration::Contract { name, .. }
      | InScopeDeclaration::Flat { name, .. } => name,
    }
  }

  /// Get the references for any declaration variant
  pub fn references(&self) -> &[ScopedReference] {
    match self {
      InScopeDeclaration::FunctionMod { references, .. }
      | InScopeDeclaration::Contract { references, .. }
      | InScopeDeclaration::Flat { references, .. } => references,
    }
  }

  pub fn visibility(&self) -> &Visibility {
    match self {
      InScopeDeclaration::FunctionMod { visibility, .. }
      | InScopeDeclaration::Contract { visibility, .. }
      | InScopeDeclaration::Flat { visibility, .. } => visibility,
    }
  }
}

fn first_pass(
  ast_map: &std::collections::BTreeMap<domain::ProjectPath, Vec<SolidityAST>>,
  in_scope_files: &HashSet<domain::ProjectPath>,
) -> Result<BTreeMap<i32, FirstPassDeclaration>, String> {
  let mut first_pass_declarations = BTreeMap::new();

  for (path, asts) in ast_map {
    let is_file_in_scope = in_scope_files.contains(path);

    for ast in asts {
      process_first_pass_ast_nodes(
        &ast.nodes.iter().collect(),
        path,
        is_file_in_scope,
        None, // No parent contract at file level
        &mut first_pass_declarations,
      )?;
    }
  }

  Ok(first_pass_declarations)
}

/// Ancestry pass: Collects variable ancestry relationships from the AST.
/// This traverses the AST to find assignments, initializations, function arguments,
/// and return statements to determine which variables flow into which other variables.
fn ancestry_pass(
  ast_map: &std::collections::BTreeMap<domain::ProjectPath, Vec<SolidityAST>>,
) -> (AncestorsMap, RelativesMap) {
  let mut ancestors_map = AncestorsMap::new();
  let mut relatives_map = RelativesMap::new();

  for asts in ast_map.values() {
    for ast in asts {
      for node in &ast.nodes {
        collect_ancestry_from_node(
          node,
          None,
          &mut ancestors_map,
          &mut relatives_map,
        );
      }
    }
  }

  (ancestors_map, relatives_map)
}

fn process_first_pass_ast_nodes(
  nodes: &Vec<&ASTNode>,
  file_path: &domain::ProjectPath,
  is_file_in_scope: bool,
  parent_contract: Option<i32>,
  first_pass_declarations: &mut BTreeMap<i32, FirstPassDeclaration>,
) -> Result<(), String> {
  for node in nodes {
    match node {
      ASTNode::ContractDefinition {
        node_id, signature, ..
      } => {
        // Extract fields from the ContractSignature
        let (
          signature_node_id,
          name,
          contract_kind,
          base_contracts,
          directives,
        ) = match signature.as_ref() {
          ASTNode::ContractSignature {
            node_id,
            name,
            contract_kind,
            base_contracts,
            directives,
            ..
          } => (node_id, name, contract_kind, base_contracts, directives),
          _ => {
            panic!("Expected ContractSignature in ContractDefinition.signature")
          }
        };

        let declaration_kind = NamedTopicKind::Contract(*contract_kind);

        // When getting these base contract node ids, set the signature node as
        // the containing statement node
        let base_contract_ids: Vec<ReferencedNode> = base_contracts
          .iter()
          .map(|base_contract| {
            // Each base_contract should be an InheritanceSpecifier
            match base_contract {
              ASTNode::InheritanceSpecifier { base_name, .. } => {
                // base_name should be an IdentifierPath with a referenced_declaration
                match base_name.as_ref() {
                  ASTNode::IdentifierPath { referenced_declaration, .. } => ReferencedNode {
                    statement_node: *signature_node_id,
                    referenced_node: *referenced_declaration,
                  },
                  _ => panic!(
                    "Expected IdentifierPath in InheritanceSpecifier base_name, got: {:?}",
                    base_name
                  ),
                }
              }
              _ => panic!(
                "Expected InheritanceSpecifier in base_contracts, got: {:?}",
                base_contract
              ),
            }
          })
          .collect();

        // When getting these directive node ids, set the signature node as
        // the containing statement node
        let using_for_contracts: Vec<ReferencedNode> = directives
          .iter()
          .filter_map(|node| match node {
            ASTNode::UsingForDirective {
              library_name,
              type_name,
              ..
            } => {
              // Helper function to extract referenced_declaration from either IdentifierPath or UserDefinedTypeName
              let extract_reference = |node: &ASTNode| match node {
                ASTNode::IdentifierPath {
                  referenced_declaration,
                  ..
                } => Some(ReferencedNode {
                  statement_node: *signature_node_id,
                  referenced_node: *referenced_declaration,
                }),
                ASTNode::UserDefinedTypeName { path_node, .. } => {
                  match path_node.as_ref() {
                    ASTNode::IdentifierPath {
                      referenced_declaration,
                      ..
                    } => Some(ReferencedNode {
                      statement_node: *signature_node_id,
                      referenced_node: *referenced_declaration,
                    }),
                    _ => None,
                  }
                }
                _ => None,
              };

              // Extract referenced_declaration from library_name
              let library_ref = library_name
                .as_ref()
                .and_then(|lib_node| extract_reference(lib_node.as_ref()));

              // Extract referenced_declaration from type_name
              let type_ref = type_name
                .as_ref()
                .and_then(|type_node| extract_reference(type_node.as_ref()));

              // Return both references if they exist
              match (library_ref, type_ref) {
                (Some(lib), Some(typ)) => Some(vec![lib, typ]),
                (Some(lib), None) => Some(vec![lib]),
                (None, Some(typ)) => Some(vec![typ]),
                (None, None) => None,
              }
            }
            _ => None,
          })
          .flatten()
          .collect();

        let public_member_ids: Vec<i32> = contract_members(node)
          .iter()
          .filter_map(|member| match member {
            // Public functions
            ASTNode::FunctionDefinition {
              node_id, signature, ..
            } => {
              if let ASTNode::FunctionSignature { visibility, .. } =
                signature.as_ref()
              {
                if matches!(
                  visibility,
                  FunctionVisibility::Public | FunctionVisibility::External
                ) {
                  Some(*node_id)
                } else {
                  None
                }
              } else {
                None
              }
            }
            // Public modifiers
            ASTNode::ModifierDefinition {
              node_id, signature, ..
            } => {
              let visibility = match signature.as_ref() {
                ASTNode::ModifierSignature { visibility, .. } => visibility,
                _ => panic!("Expected ModifierSignature"),
              };
              if matches!(
                visibility,
                FunctionVisibility::Public | FunctionVisibility::External
              ) {
                Some(*node_id)
              } else {
                None
              }
            }
            // Events (all events are public)
            ASTNode::EventDefinition { node_id, .. } => Some(*node_id),
            // Errors (all errors are public)
            ASTNode::ErrorDefinition { node_id, .. } => Some(*node_id),
            ASTNode::StructDefinition {
              node_id,
              visibility,
              ..
            } if *visibility == VariableVisibility::Public => Some(*node_id),
            ASTNode::EnumDefinition { node_id, .. } => Some(*node_id),
            // Public state variables
            ASTNode::VariableDeclaration {
              node_id,
              visibility,
              state_variable,
              ..
            } if *state_variable
              && matches!(visibility, VariableVisibility::Public) =>
            {
              Some(*node_id)
            }
            _ => None,
          })
          .collect();

        // Collect type references from state variable declarations
        let mut variable_type_references: Vec<ReferencedNode> = Vec::new();
        for member in contract_members(node) {
          match member {
            ASTNode::VariableDeclaration {
              node_id: var_node_id,
              state_variable,
              type_name,
              ..
            } if *state_variable => {
              collect_type_references(
                type_name,
                *var_node_id,
                &mut variable_type_references,
              );
            }
            ASTNode::StructDefinition {
              node_id, members, ..
            } => {
              for node in members {
                collect_type_references(
                  node,
                  *node_id,
                  &mut variable_type_references,
                );
              }
            }
            _ => (),
          }
        }

        first_pass_declarations.insert(
          *node_id,
          FirstPassDeclaration::Contract {
            container_file: file_path.clone(),
            is_publicly_in_scope: is_file_in_scope, // All contracts are publicly visible
            name: name.clone(),
            declaration_kind,
            visibility: Visibility::Public,
            base_contracts: base_contract_ids,
            other_contracts: using_for_contracts,
            public_members: public_member_ids,
            referenced_nodes: variable_type_references,
          },
        );

        let child_nodes: Vec<&ASTNode> = contract_members(node);
        process_first_pass_ast_nodes(
          &child_nodes,
          file_path,
          is_file_in_scope,
          Some(*node_id), // Contract members belong to this contract
          first_pass_declarations,
        )?;
      }

      ASTNode::FunctionDefinition {
        node_id, signature, ..
      } => {
        // Extract name, kind, and visibility from the signature
        let (name, kind, visibility) = match signature.as_ref() {
          ASTNode::FunctionSignature {
            name,
            kind,
            visibility,
            ..
          } => (name, kind, visibility),
          _ => {
            panic!("Expected FunctionSignature in FunctionDefinition.signature")
          }
        };

        let mut referenced_nodes = Vec::new();
        let mut reverts = Vec::new();
        let mut function_calls = Vec::new();
        let mut variable_mutations = Vec::new();
        let mut events_emitted = Vec::new();

        // Process entire function node to find references and reverts
        collect_references_and_statements(
          node,
          None, // No containing block context initially
          &mut referenced_nodes,
          &mut reverts,
          &mut function_calls,
          &mut variable_mutations,
          &mut events_emitted,
        );

        first_pass_declarations.insert(
          *node_id,
          FirstPassDeclaration::FunctionMod {
            parent_contract,
            declaration_kind: NamedTopicKind::Function(*kind),
            visibility: function_visibility_to_visibility(visibility),
            name: name.clone(),
            referenced_nodes,
            reverts,
            function_calls,
            variable_mutations,
            events_emitted,
          },
        );

        // Process function body nodes for local variables
        let child_nodes = node.nodes();
        process_first_pass_ast_nodes(
          &child_nodes,
          file_path,
          is_file_in_scope,
          parent_contract,
          first_pass_declarations,
        )?;
      }

      ASTNode::ModifierDefinition {
        node_id, signature, ..
      } => {
        let name = match signature.as_ref() {
          ASTNode::ModifierSignature { name, .. } => name,
          _ => panic!("Expected ModifierSignature"),
        };

        let mut referenced_nodes = Vec::new();
        let mut reverts = Vec::new();
        let mut function_calls = Vec::new();
        let mut variable_mutations = Vec::new();
        let mut events_emitted = Vec::new();

        // Process entire modifier node to find references and reverts
        collect_references_and_statements(
          node,
          None, // No containing block context initially
          &mut referenced_nodes,
          &mut reverts,
          &mut function_calls,
          &mut variable_mutations,
          &mut events_emitted,
        );

        // Modifiers are always internal visibility
        first_pass_declarations.insert(
          *node_id,
          FirstPassDeclaration::FunctionMod {
            parent_contract,
            declaration_kind: NamedTopicKind::Modifier,
            visibility: Visibility::Internal,
            name: name.clone(),
            referenced_nodes,
            reverts,
            function_calls,
            variable_mutations,
            events_emitted,
          },
        );

        // Process modifier body nodes
        let child_nodes = node.nodes();
        process_first_pass_ast_nodes(
          &child_nodes,
          file_path,
          is_file_in_scope,
          parent_contract,
          first_pass_declarations,
        )?;
      }

      ASTNode::VariableDeclaration {
        node_id,
        state_variable,
        mutability,
        name,
        visibility,
        ..
      } => {
        let declaration_kind = if *state_variable {
          NamedTopicKind::StateVariable(*mutability)
        } else {
          NamedTopicKind::LocalVariable
        };

        first_pass_declarations.insert(
          *node_id,
          FirstPassDeclaration::Flat {
            parent_contract,
            declaration_kind,
            visibility: variable_visibility_to_visibility(visibility),
            name: name.clone(),
          },
        );
      }

      ASTNode::EventDefinition { node_id, name, .. } => {
        // Events don't have visibility in Solidity, but are effectively public
        first_pass_declarations.insert(
          *node_id,
          FirstPassDeclaration::Flat {
            parent_contract,
            declaration_kind: NamedTopicKind::Event,
            visibility: Visibility::Public,
            name: name.clone(),
          },
        );

        // Process event parameters
        let child_nodes = node.nodes();
        process_first_pass_ast_nodes(
          &child_nodes,
          file_path,
          is_file_in_scope,
          parent_contract,
          first_pass_declarations,
        )?;
      }

      ASTNode::ErrorDefinition { node_id, name, .. } => {
        // Errors don't have visibility in Solidity, but are effectively public
        first_pass_declarations.insert(
          *node_id,
          FirstPassDeclaration::Flat {
            parent_contract,
            declaration_kind: NamedTopicKind::Error,
            visibility: Visibility::Public,
            name: name.clone(),
          },
        );

        // Process error parameters
        let child_nodes = node.nodes();
        process_first_pass_ast_nodes(
          &child_nodes,
          file_path,
          is_file_in_scope,
          parent_contract,
          first_pass_declarations,
        )?;
      }

      ASTNode::StructDefinition {
        node_id,
        name,
        visibility,
        ..
      } => {
        // Structs don't have visibility in Solidity, but are effectively public
        first_pass_declarations.insert(
          *node_id,
          FirstPassDeclaration::Flat {
            parent_contract,
            declaration_kind: NamedTopicKind::Struct,
            visibility: variable_visibility_to_visibility(visibility),
            name: name.clone(),
          },
        );

        // Process struct members
        let member_nodes = node.nodes();
        process_first_pass_ast_nodes(
          &member_nodes,
          file_path,
          is_file_in_scope,
          parent_contract,
          first_pass_declarations,
        )?;
      }

      ASTNode::EnumDefinition { node_id, name, .. } => {
        // Enums don't have visibility in Solidity, but are effectively public
        first_pass_declarations.insert(
          *node_id,
          FirstPassDeclaration::Flat {
            parent_contract,
            declaration_kind: NamedTopicKind::Enum,
            visibility: Visibility::Public,
            name: name.clone(),
          },
        );

        // Process enum members
        let member_nodes = node.nodes();
        process_first_pass_ast_nodes(
          &member_nodes,
          file_path,
          is_file_in_scope,
          parent_contract,
          first_pass_declarations,
        )?;
      }

      ASTNode::EnumValue { node_id, name, .. } => {
        // Enum values don't have visibility in Solidity, but are effectively public
        first_pass_declarations.insert(
          *node_id,
          FirstPassDeclaration::Flat {
            parent_contract,
            declaration_kind: NamedTopicKind::EnumMember,
            visibility: Visibility::Public,
            name: name.clone(),
          },
        );
      }

      _ => {
        // For other node types, recursively process their child nodes
        let child_nodes = node.nodes();
        process_first_pass_ast_nodes(
          &child_nodes,
          file_path,
          is_file_in_scope,
          parent_contract,
          first_pass_declarations,
        )?;
      }
    }
  }

  Ok(())
}

// ============================================================================
// Populate Nodes Pass
// ============================================================================

/// Populates the nodes map by traversing all AST nodes that are in scope.
/// This pass must run before second_pass so that reference nodes can be looked up
/// for sorting by source location.
fn populate_nodes_pass(
  ast_map: &BTreeMap<domain::ProjectPath, Vec<SolidityAST>>,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
  nodes: &mut BTreeMap<topic::Topic, Node>,
) {
  for asts in ast_map.values() {
    for ast in asts {
      for node in &ast.nodes {
        populate_nodes_recursive(node, false, in_scope_source_topics, nodes);
      }
    }
  }
}

/// Recursively populates nodes for a subtree.
/// If parent_in_scope is true, all nodes in the subtree are added.
fn populate_nodes_recursive(
  node: &ASTNode,
  parent_in_scope: bool,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
  nodes: &mut BTreeMap<topic::Topic, Node>,
) {
  let node_id = node.node_id();
  let topic = topic::new_node_topic(&node_id);

  // Check if this node is in scope
  let is_declaration_in_scope = in_scope_source_topics.contains_key(&node_id);
  let is_in_scope = parent_in_scope || is_declaration_in_scope;

  if is_in_scope {
    // Add the node with its children converted to stubs
    let stubbed_node = Node::Solidity(parser::children_to_stubs(node.clone()));
    nodes.insert(topic, stubbed_node);
  }

  // Recurse into children
  for child in node.nodes() {
    populate_nodes_recursive(child, is_in_scope, in_scope_source_topics, nodes);
  }
}

/// Builds SourceContext structs from ScopedReferences.
/// Groups references by their scope, sorts references within groups by source location,
/// and sorts groups by component name then member source location.
///
/// Converts a Scope and node_id into a ScopedReference representing the declaration itself.
/// Returns None for Global scope since it doesn't have a containing component.
fn scope_to_self_reference(
  scope: &Scope,
  node_id: i32,
) -> Option<ScopedReference> {
  match scope {
    Scope::Global => None,
    Scope::Container { .. } => {
      // Declaration is a contract/interface/library itself - it references itself
      // with itself as the containing component
      Some(ScopedReference {
        reference_node: node_id,
        containing_component: node_id,
        containing_member: None,
      })
    }
    Scope::Component { component, .. } => {
      // Declaration is at contract level (e.g., state variable, function declaration)
      Some(ScopedReference {
        reference_node: node_id,
        containing_component: component.numeric_id(),
        containing_member: None,
      })
    }
    Scope::Member {
      component, member, ..
    } => {
      // Declaration is at member level (e.g., local variable, parameter)
      Some(ScopedReference {
        reference_node: node_id,
        containing_component: component.numeric_id(),
        containing_member: Some(member.numeric_id()),
      })
    }
    Scope::ContainingBlock {
      component,
      member,
      containing_blocks,
      ..
    } => {
      // Declaration is within a containing block — use the innermost containing
      // block as the referenceeee node so the group points to the block rather
      // than the individual declaration.
      let innermost_block = &containing_blocks.last()?.block;
      Some(ScopedReference {
        reference_node: innermost_block.numeric_id(),
        containing_component: component.numeric_id(),
        containing_member: Some(member.numeric_id()),
      })
    }
  }
}

/// Extracts the annotation chain from a scope's containing blocks.
/// Returns the sequence of BlockAnnotation values (filtering out None entries).
fn extract_annotation_chain(scope: &Scope) -> Vec<domain::BlockAnnotation> {
  match scope {
    Scope::ContainingBlock {
      containing_blocks, ..
    } => containing_blocks
      .iter()
      .filter_map(|layer| layer.annotation.clone())
      .collect(),
    _ => vec![],
  }
}

/// Builds reference groups from scoped references.
/// Groups references by containing_component (contract) and containing_member (function),
/// with control flow sub-grouping derived from the scope_map.
/// The self_reference, if provided, is included in the appropriate group.
/// Groups are post-sorted: subject's contract first, then by contract name.
fn build_source_context(
  scoped_refs: &[ScopedReference],
  self_reference: Option<ScopedReference>,
  scope_map: &BTreeMap<i32, Scope>,
  nodes: &BTreeMap<topic::Topic, Node>,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
  in_scope_files: &HashSet<domain::ProjectPath>,
) -> Vec<SourceContext> {
  let mut groups: Vec<SourceContext> = Vec::new();

  // Collect all reference topics to detect scope-based subsumption.
  // If block B is nested inside block A and both are reference_nodes,
  // block B is redundant because expanding A already shows B's content.
  let all_ref_topics: HashSet<topic::Topic> = self_reference
    .iter()
    .map(|r| topic::new_node_topic(&r.reference_node))
    .chain(
      scoped_refs
        .iter()
        .map(|r| topic::new_node_topic(&r.reference_node)),
    )
    .collect();

  // A reference_node is subsumed if any ancestor in its scope chain
  // is also a reference (meaning a higher scope already covers it).
  let is_subsumed = |ref_node: i32| -> bool {
    if let Some(scope) = scope_map.get(&ref_node) {
      return scope
        .ancestor_topics()
        .iter()
        .any(|t| all_ref_topics.contains(t));
    }
    false
  };

  let mut insert_scoped_ref = |scoped_ref: &ScopedReference| {
    if is_subsumed(scoped_ref.reference_node) {
      return;
    }

    let ref_topic = topic::new_node_topic(&scoped_ref.reference_node);
    let ref_sort_key = get_source_location_start(&ref_topic, nodes);
    let contract_topic =
      topic::new_node_topic(&scoped_ref.containing_component);
    let contract_sort_key = get_source_location_start(&contract_topic, nodes);
    let is_in_scope = is_contract_in_scope(
      scoped_ref.containing_component,
      in_scope_source_topics,
      in_scope_files,
    );

    let subscope = scoped_ref.containing_member.map(|member_id| {
      let member_topic = topic::new_node_topic(&member_id);
      let member_sort_key = get_source_location_start(&member_topic, nodes);
      (member_topic, member_sort_key)
    });

    let annotation_chain = scope_map
      .get(&scoped_ref.reference_node)
      .map(extract_annotation_chain)
      .unwrap_or_default();

    insert_into_context(
      &mut groups,
      contract_topic,
      contract_sort_key,
      is_in_scope,
      subscope,
      &annotation_chain,
      domain::Reference::project_reference(ref_topic, ref_sort_key),
    );
  };

  if let Some(ref self_ref) = self_reference {
    insert_scoped_ref(self_ref);
  }
  for scoped_ref in scoped_refs {
    insert_scoped_ref(scoped_ref);
  }

  // Post-sort: subject's contract first, then by contract name
  let subject_contract_id = self_reference.map(|r| r.containing_component);
  groups.sort_by(|a, b| {
    let id_a = a.scope().numeric_id();
    let id_b = b.scope().numeric_id();

    let is_subject_a = subject_contract_id == Some(id_a);
    let is_subject_b = subject_contract_id == Some(id_b);

    if is_subject_a != is_subject_b {
      return is_subject_a.cmp(&is_subject_b).reverse();
    }

    let name_a = get_scope_name(a.scope(), in_scope_source_topics);
    let name_b = get_scope_name(b.scope(), in_scope_source_topics);
    name_a.cmp(&name_b)
  });

  groups
}

/// Builds reference groups for expanded_context, sorted by ancestor/descendant counts.
/// Groups are post-sorted: in-scope first, then by ancestry counts.
#[allow(clippy::too_many_arguments)]
fn build_expanded_source_context(
  scoped_refs: &[ScopedReference],
  ancestors: &HashSet<i32>,
  descendants: &HashSet<i32>,
  declaration_scopes: &BTreeMap<i32, Scope>,
  scope_map: &BTreeMap<i32, Scope>,
  nodes: &BTreeMap<topic::Topic, Node>,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
  in_scope_files: &HashSet<domain::ProjectPath>,
) -> Vec<SourceContext> {
  // Track ancestor/descendant counts per contract
  let mut contract_ancestry_counts: BTreeMap<i32, (usize, usize)> =
    BTreeMap::new();

  let get_component_id = |scope: &Scope| -> Option<i32> {
    match scope {
      Scope::Global | Scope::Container { .. } => None,
      Scope::Component { component, .. }
      | Scope::Member { component, .. }
      | Scope::ContainingBlock { component, .. } => {
        Some(component.numeric_id())
      }
    }
  };

  for &var_id in ancestors.iter().chain(descendants.iter()) {
    if let Some(scope) = declaration_scopes.get(&var_id)
      && let Some(contract_id) = get_component_id(scope)
    {
      let (ancestor_count, descendant_count) =
        contract_ancestry_counts.entry(contract_id).or_default();
      if ancestors.contains(&var_id) {
        *ancestor_count += 1;
      }
      if descendants.contains(&var_id) {
        *descendant_count += 1;
      }
    }
  }

  let mut groups: Vec<SourceContext> = Vec::new();

  for scoped_ref in scoped_refs {
    let ref_topic = topic::new_node_topic(&scoped_ref.reference_node);
    let ref_sort_key = get_source_location_start(&ref_topic, nodes);
    let contract_topic =
      topic::new_node_topic(&scoped_ref.containing_component);
    let contract_sort_key = get_source_location_start(&contract_topic, nodes);
    let in_scope = is_contract_in_scope(
      scoped_ref.containing_component,
      in_scope_source_topics,
      in_scope_files,
    );

    let subscope = scoped_ref.containing_member.map(|member_id| {
      let member_topic = topic::new_node_topic(&member_id);
      let member_sort_key = get_source_location_start(&member_topic, nodes);
      (member_topic, member_sort_key)
    });

    let annotation_chain = scope_map
      .get(&scoped_ref.reference_node)
      .map(extract_annotation_chain)
      .unwrap_or_default();

    insert_into_context(
      &mut groups,
      contract_topic,
      contract_sort_key,
      in_scope,
      subscope,
      &annotation_chain,
      domain::Reference::project_reference(ref_topic, ref_sort_key),
    );
  }

  // Post-sort: in-scope first, then by ancestry counts
  groups.sort_by(|a, b| {
    let in_scope_a = a.is_in_scope();
    let in_scope_b = b.is_in_scope();

    if in_scope_a != in_scope_b {
      return in_scope_b.cmp(&in_scope_a);
    }

    let id_a = a.scope().numeric_id();
    let id_b = b.scope().numeric_id();

    let (ancestors_a, _) = contract_ancestry_counts
      .get(&id_a)
      .copied()
      .unwrap_or((0, 0));
    let (ancestors_b, _) = contract_ancestry_counts
      .get(&id_b)
      .copied()
      .unwrap_or((0, 0));
    let (_, descendants_a) = contract_ancestry_counts
      .get(&id_a)
      .copied()
      .unwrap_or((0, 0));
    let (_, descendants_b) = contract_ancestry_counts
      .get(&id_b)
      .copied()
      .unwrap_or((0, 0));

    let has_ancestors_a = ancestors_a > 0;
    let has_ancestors_b = ancestors_b > 0;

    if has_ancestors_a != has_ancestors_b {
      return has_ancestors_b.cmp(&has_ancestors_a);
    }

    if has_ancestors_a {
      ancestors_a.cmp(&ancestors_b)
    } else {
      descendants_b.cmp(&descendants_a)
    }
  });

  groups
}

/// Checks whether a contract (by node ID) is defined in one of the audit's in-scope files.
fn is_contract_in_scope(
  contract_id: i32,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
  in_scope_files: &HashSet<domain::ProjectPath>,
) -> bool {
  in_scope_source_topics
    .get(&contract_id)
    .and_then(|decl| match decl {
      InScopeDeclaration::Contract { container_file, .. } => {
        Some(in_scope_files.contains(container_file))
      }
      _ => None,
    })
    .unwrap_or(false)
}

/// Gets the source location start for a topic from the nodes map.
fn get_source_location_start(
  topic: &topic::Topic,
  nodes: &BTreeMap<topic::Topic, Node>,
) -> Option<usize> {
  nodes
    .get(topic)
    .and_then(|node| node.source_location_start())
}

/// Gets the scope name from a topic for sorting.
fn get_scope_name(
  scope_topic: &topic::Topic,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
) -> String {
  let node_id = scope_topic.numeric_id();
  if let Some(decl) = in_scope_source_topics.get(&node_id) {
    return decl.name().clone();
  }
  scope_topic.id()
}

/// Second pass: Parse each AST and build the final data structures for in-scope nodes
/// This processes each AST one at a time, checking declarations for inclusion in the
/// in-scope dictionary. When found, adds the node and all child nodes to the accumulating
/// data structures.
#[allow(clippy::too_many_arguments)]
fn second_pass(
  ast_map: &BTreeMap<domain::ProjectPath, Vec<SolidityAST>>,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
  in_scope_files: &HashSet<domain::ProjectPath>,
  mutations_map: &BTreeMap<i32, Vec<i32>>,
  ancestors_map: &AncestorsMap,
  descendants_map: &DescendantsMap,
  relatives_map: &RelativesMap,
  nodes: &mut BTreeMap<topic::Topic, Node>,
  topic_metadata: &mut BTreeMap<topic::Topic, TopicMetadata>,
  topic_context: &mut BTreeMap<topic::Topic, Vec<SourceContext>>,
  expanded_topic_context: &mut BTreeMap<topic::Topic, Vec<SourceContext>>,
  function_properties: &mut BTreeMap<topic::Topic, FunctionModProperties>,
  variable_types: &mut BTreeMap<topic::Topic, SolidityType>,
) -> Result<(), String> {
  // Process each AST file
  for (file_path, asts) in ast_map {
    for ast in asts {
      process_second_pass_nodes(
        &ast.nodes.iter().collect(),
        false, // Parent is not in scope automatically - check each node
        in_scope_source_topics,
        in_scope_files,
        mutations_map,
        ancestors_map,
        descendants_map,
        relatives_map,
        nodes,
        &domain::Scope::Container {
          container: file_path.clone(),
        },
        topic_metadata,
        function_properties,
        variable_types,
      )?;
    }
  }

  // Populate references after all topic_metadata entries exist
  // (requires scopes to extract control flow chains for reference grouping)
  populate_context(
    topic_metadata,
    topic_context,
    in_scope_source_topics,
    nodes,
    in_scope_files,
  );

  // Populate expanded_context after all topic_metadata entries exist
  // (requires scopes from all ancestry-related topics)
  populate_expanded_context(
    topic_metadata,
    expanded_topic_context,
    ancestors_map,
    descendants_map,
    relatives_map,
    nodes,
    in_scope_source_topics,
    in_scope_files,
  );

  Ok(())
}

/// Recursively process nodes during the second pass
/// If parent_in_scope is true, all nodes are assumed to be in scope
#[allow(clippy::too_many_arguments, clippy::only_used_in_recursion)]
fn process_second_pass_nodes(
  ast_nodes: &Vec<&ASTNode>,
  parent_in_scope: bool,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
  in_scope_files: &HashSet<domain::ProjectPath>,
  mutations_map: &BTreeMap<i32, Vec<i32>>,
  ancestors_map: &AncestorsMap,
  descendants_map: &DescendantsMap,
  relatives_map: &RelativesMap,
  nodes: &mut BTreeMap<topic::Topic, Node>,
  scope: &Scope,
  topic_metadata: &mut BTreeMap<topic::Topic, TopicMetadata>,
  function_properties: &mut BTreeMap<topic::Topic, FunctionModProperties>,
  variable_types: &mut BTreeMap<topic::Topic, SolidityType>,
) -> Result<(), String> {
  for node in ast_nodes {
    let node_id = node.node_id();
    let topic = topic::new_node_topic(&node_id);

    // Check if this node should be processed (either parent is in scope or it's in the in_scope_declarations)
    let in_scope_topic_declaration = in_scope_source_topics.get(&node_id);
    let is_in_scope = parent_in_scope || in_scope_topic_declaration.is_some();

    if is_in_scope {
      // Add the node with its children converted to stubs
      let stubbed_node =
        Node::Solidity(parser::children_to_stubs((*node).clone()));

      nodes.insert(topic, stubbed_node);
    }

    // Process declarations only if they exist in in_scope_declarations
    if let Some(in_scope_topic_declaration) = in_scope_topic_declaration {
      // Build ancestor, descendant, and relative topics for this declaration
      let ancestor_topics: Vec<topic::Topic> = ancestors_map
        .get(&node_id)
        .map(|ids| ids.iter().map(|&id| topic::new_node_topic(&id)).collect())
        .unwrap_or_default();

      let descendant_topics: Vec<topic::Topic> = descendants_map
        .get(&node_id)
        .map(|ids| ids.iter().map(|&id| topic::new_node_topic(&id)).collect())
        .unwrap_or_default();

      let relative_topics: Vec<topic::Topic> = relatives_map
        .get(&node_id)
        .map(|ids| ids.iter().map(|&id| topic::new_node_topic(&id)).collect())
        .unwrap_or_default();

      // Check if this declaration has mutations
      let is_mutable_variable = *in_scope_topic_declaration.declaration_kind()
        == NamedTopicKind::StateVariable(domain::VariableMutability::Mutable)
        || *in_scope_topic_declaration.declaration_kind()
          == NamedTopicKind::LocalVariable;

      let (is_mutable, mutation_topics) = if let Some(mutation_node_ids) =
        mutations_map.get(&node_id)
        && is_mutable_variable
      {
        (
          true,
          mutation_node_ids
            .iter()
            .map(|&id| topic::new_node_topic(&id))
            .collect(),
        )
      } else {
        (false, vec![])
      };

      // Extract transitive_topic from implementation_declaration on AST nodes.
      // Interface functions/modifiers/parameters with exactly one in-scope
      // implementation are transitive to their implementation counterpart.
      let transitive_topic = match node {
        ASTNode::FunctionDefinition { signature, .. }
        | ASTNode::ModifierDefinition { signature, .. } => {
          match signature.as_ref() {
            ASTNode::FunctionSignature {
              implementation_declaration: Some(impl_id),
              ..
            }
            | ASTNode::ModifierSignature {
              implementation_declaration: Some(impl_id),
              ..
            } => Some(topic::new_node_topic(impl_id)),
            _ => None,
          }
        }
        ASTNode::VariableDeclaration {
          implementation_declaration: Some(impl_id),
          ..
        } => Some(topic::new_node_topic(impl_id)),
        _ => None,
      };

      let topic_metadata_entry = TopicMetadata::NamedTopic {
        topic: topic,
        kind: in_scope_topic_declaration.declaration_kind().clone(),
        visibility: visibility_to_named_topic_visibility(
          in_scope_topic_declaration.visibility(),
        ),
        name: in_scope_topic_declaration.name().clone(),
        scope: scope.clone(),
        is_mutable,
        mutations: mutation_topics,
        ancestors: ancestor_topics,
        descendants: descendant_topics,
        relatives: relative_topics,
        transitive_topic: transitive_topic,
        doc_references: Vec::new(),
      };

      topic_metadata.insert(topic, topic_metadata_entry);

      match in_scope_topic_declaration {
        InScopeDeclaration::FunctionMod {
          reverts: first_pass_reverts,
          function_calls,
          variable_mutations,
          events_emitted: first_pass_events,
          ..
        } if transitive_topic.is_none() => {
          // Skip function properties for transitive declarations (e.g., interface
          // functions with one implementation) — they have empty bodies and their
          // implementation counterpart provides the real properties.

          // Convert first-pass reverts to RevertInfo (topic + kind + error_topic)
          let reverts: Vec<domain::RevertInfo> = first_pass_reverts
            .iter()
            .map(|fp| domain::RevertInfo {
              topic: topic::new_node_topic(&fp.statement_node),
              kind: fp.kind,
              error_topic: fp.error_node.map(|n| topic::new_node_topic(&n)),
            })
            .collect();

          let call_topics: Vec<topic::Topic> = function_calls
            .iter()
            .map(|&id| topic::new_node_topic(&id))
            .collect();
          let mutation_topics: Vec<topic::Topic> = variable_mutations
            .iter()
            .map(|ref_node| topic::new_node_topic(&ref_node.referenced_node))
            .collect();
          let mut event_topics: Vec<topic::Topic> = first_pass_events
            .iter()
            .map(|&id| topic::new_node_topic(&id))
            .collect();
          event_topics.sort();
          event_topics.dedup();

          match node {
            ASTNode::FunctionDefinition { .. } => {
              function_properties.insert(
                topic,
                FunctionModProperties::FunctionProperties {
                  reverts,
                  calls: call_topics,
                  mutations: mutation_topics,
                  events_emitted: event_topics,
                },
              );
            }
            ASTNode::ModifierDefinition { .. } => {
              function_properties.insert(
                topic,
                FunctionModProperties::ModifierProperties {
                  reverts,
                  calls: call_topics,
                  mutations: mutation_topics,
                  events_emitted: event_topics,
                },
              );
            }

            _ => (),
          }
        }

        _ => (),
      }

      // Extract variable types for VariableDeclaration nodes
      if let ASTNode::VariableDeclaration { type_name, .. } = node
        && let Some(solidity_type) = extract_solidity_type(type_name)
      {
        variable_types.insert(topic, solidity_type);
      }
    } else {
      // Control flow nodes get their own TopicMetadata variant
      let control_flow_metadata = match node {
        ASTNode::IfStatement { condition, .. } => Some((
          domain::ControlFlowStatementKind::If,
          topic::new_node_topic(&condition.node_id()),
        )),
        ASTNode::ForStatement { condition, .. } => Some((
          domain::ControlFlowStatementKind::For,
          topic::new_node_topic(&condition.node_id()),
        )),
        ASTNode::WhileStatement { condition, .. } => Some((
          domain::ControlFlowStatementKind::While,
          topic::new_node_topic(&condition.node_id()),
        )),
        ASTNode::DoWhileStatement { condition, .. } => Some((
          domain::ControlFlowStatementKind::DoWhile,
          topic::new_node_topic(&condition.node_id()),
        )),
        _ => None,
      };

      if let Some((kind, condition)) = control_flow_metadata {
        topic_metadata.insert(
          topic,
          TopicMetadata::ControlFlow {
            topic,
            scope: scope.clone(),
            kind,
            condition,
          },
        );
      } else {
        let kind = match node {
          ASTNode::Assignment { .. } => UnnamedTopicKind::VariableMutation,
          ASTNode::BinaryOperation { operator, .. } => match operator {
            ast::BinaryOperator::Add => UnnamedTopicKind::Arithmetic,
            ast::BinaryOperator::Subtract => UnnamedTopicKind::Arithmetic,
            ast::BinaryOperator::Multiply => UnnamedTopicKind::Arithmetic,
            ast::BinaryOperator::Divide => UnnamedTopicKind::Arithmetic,
            ast::BinaryOperator::Modulo => UnnamedTopicKind::Arithmetic,
            ast::BinaryOperator::Power => UnnamedTopicKind::Arithmetic,
            ast::BinaryOperator::Equal => UnnamedTopicKind::Comparison,
            ast::BinaryOperator::NotEqual => UnnamedTopicKind::Comparison,
            ast::BinaryOperator::LessThan => UnnamedTopicKind::Comparison,
            ast::BinaryOperator::LessThanOrEqual => {
              UnnamedTopicKind::Comparison
            }
            ast::BinaryOperator::GreaterThan => UnnamedTopicKind::Comparison,
            ast::BinaryOperator::GreaterThanOrEqual => {
              UnnamedTopicKind::Comparison
            }
            ast::BinaryOperator::And => UnnamedTopicKind::Logical,
            ast::BinaryOperator::Or => UnnamedTopicKind::Logical,
            ast::BinaryOperator::BitwiseAnd => UnnamedTopicKind::Bitwise,
            ast::BinaryOperator::BitwiseOr => UnnamedTopicKind::Bitwise,
            ast::BinaryOperator::BitwiseXor => UnnamedTopicKind::Bitwise,
            ast::BinaryOperator::LeftShift => UnnamedTopicKind::Bitwise,
            ast::BinaryOperator::RightShift => UnnamedTopicKind::Bitwise,
          },
          ASTNode::Conditional { .. } => UnnamedTopicKind::Conditional,
          ASTNode::FunctionCall { .. } => UnnamedTopicKind::FunctionCall,
          ASTNode::TypeConversion { .. } => UnnamedTopicKind::TypeConversion,
          ASTNode::StructConstructor { .. } => {
            UnnamedTopicKind::StructConstruction
          }
          ASTNode::NewExpression { .. } => UnnamedTopicKind::NewExpression,
          ASTNode::Literal { .. } => UnnamedTopicKind::Literal,
          ASTNode::SemanticBlock { .. } => UnnamedTopicKind::SemanticBlock,
          ASTNode::ContractMemberGroup { .. } => {
            UnnamedTopicKind::ContractMemberGroup
          }
          ASTNode::Break { .. } => UnnamedTopicKind::Break,
          ASTNode::Continue { .. } => UnnamedTopicKind::Continue,
          ASTNode::EmitStatement { .. } => UnnamedTopicKind::Emit,
          ASTNode::InlineAssembly { .. } => UnnamedTopicKind::InlineAssembly,
          ASTNode::LoopExpression { .. } => UnnamedTopicKind::LoopExpression,
          ASTNode::PlaceholderStatement { .. } => UnnamedTopicKind::Placeholder,
          ASTNode::Return { .. } => UnnamedTopicKind::Return,
          ASTNode::RevertStatement { .. } => UnnamedTopicKind::Revert,
          ASTNode::TryStatement { .. } => UnnamedTopicKind::Try,
          ASTNode::UncheckedBlock { .. } => UnnamedTopicKind::UncheckedBlock,
          ASTNode::MemberAccess {
            referenced_declaration: None,
            ..
          } => UnnamedTopicKind::Reference,
          ASTNode::Identifier {
            referenced_declaration,
            ..
          }
          | ASTNode::IdentifierPath {
            referenced_declaration,
            ..
          }
          | ASTNode::MemberAccess {
            referenced_declaration: Some(referenced_declaration),
            ..
          } => {
            // Check if the referenced variable has any mutations in scope
            if mutations_map.contains_key(referenced_declaration) {
              UnnamedTopicKind::MutableReference
            } else {
              UnnamedTopicKind::Reference
            }
          }
          ASTNode::FunctionSignature { .. }
          | ASTNode::ModifierSignature { .. }
          | ASTNode::ContractSignature { .. } => UnnamedTopicKind::Signature,
          _ => UnnamedTopicKind::Other,
        };

        let transitive_topic = match node {
          // A semantic block with exactly one child statement is transitive
          // to that statement — they represent the same logical unit.
          ASTNode::SemanticBlock { statements, .. }
            if statements.len() == 1 =>
          {
            Some(topic::new_node_topic(&statements[0].node_id()))
          }
          // A contract member group with exactly one child member is
          // transitive to that member — the group's comment resolves onto
          // the member directly.
          ASTNode::ContractMemberGroup { members, .. }
            if members.len() == 1 =>
          {
            Some(topic::new_node_topic(&members[0].node_id()))
          }
          // Signature nodes are transitive to their parent definition node.
          // FunctionSignature → FunctionDefinition, ModifierSignature →
          // ModifierDefinition, ContractSignature → ContractDefinition.
          // The `declaration_id` field on each signature points to the parent
          // definition's node_id, set during parsing.
          ASTNode::FunctionSignature { declaration_id, .. }
          | ASTNode::ModifierSignature { declaration_id, .. }
          | ASTNode::ContractSignature { declaration_id, .. } => {
            Some(topic::new_node_topic(declaration_id))
          }
          _ => None,
        };

        topic_metadata.insert(
          topic,
          TopicMetadata::UnnamedTopic {
            topic,
            scope: scope.clone(),
            kind,
            transitive_topic,
          },
        );
      }
    }

    // Process children with appropriate context
    // Control flow nodes add their info to the current scope immediately,
    // then recurse into their body children with the updated scope.
    match node {
      ASTNode::IfStatement {
        node_id,
        condition,
        true_body,
        false_body,
        ..
      } => {
        let cf_topic = topic::new_node_topic(node_id);

        // Process condition without control flow context
        process_second_pass_nodes(
          &vec![condition.as_ref()],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;

        // Process true body with True branch control flow added to scope
        let true_cf = domain::BlockAnnotation {
          topic: cf_topic,
          kind: domain::BlockAnnotationKind::If(
            domain::ControlFlowBranch::True,
          ),
        };
        let true_scope = domain::add_annotation_to_scope(scope, true_cf);
        process_second_pass_nodes(
          &vec![true_body.as_ref()],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          &true_scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;

        // Process false body with False branch control flow added to scope
        if let Some(false_body) = false_body {
          let false_cf = domain::BlockAnnotation {
            topic: cf_topic,
            kind: domain::BlockAnnotationKind::If(
              domain::ControlFlowBranch::False,
            ),
          };
          let false_scope = domain::add_annotation_to_scope(scope, false_cf);
          process_second_pass_nodes(
            &vec![false_body.as_ref()],
            is_in_scope,
            in_scope_source_topics,
            in_scope_files,
            mutations_map,
            ancestors_map,
            descendants_map,
            relatives_map,
            nodes,
            &false_scope,
            topic_metadata,
            function_properties,
            variable_types,
          )?;
        }
      }

      ASTNode::ForStatement {
        node_id,
        condition,
        body,
        ..
      } => {
        let cf = domain::BlockAnnotation {
          topic: topic::new_node_topic(node_id),
          kind: domain::BlockAnnotationKind::For,
        };

        // Process condition (LoopExpression) without control flow context
        process_second_pass_nodes(
          &vec![condition.as_ref()],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;

        // Process body with control flow added to scope
        let cf_scope = domain::add_annotation_to_scope(scope, cf);
        process_second_pass_nodes(
          &vec![body.as_ref()],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          &cf_scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;
      }

      ASTNode::WhileStatement {
        node_id,
        condition,
        body,
        ..
      } => {
        let cf = domain::BlockAnnotation {
          topic: topic::new_node_topic(node_id),
          kind: domain::BlockAnnotationKind::While,
        };

        // Process condition without control flow context
        process_second_pass_nodes(
          &vec![condition.as_ref()],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;

        // Process body with control flow added to scope
        if let Some(body) = body {
          let cf_scope = domain::add_annotation_to_scope(scope, cf);
          process_second_pass_nodes(
            &vec![body.as_ref()],
            is_in_scope,
            in_scope_source_topics,
            in_scope_files,
            mutations_map,
            ancestors_map,
            descendants_map,
            relatives_map,
            nodes,
            &cf_scope,
            topic_metadata,
            function_properties,
            variable_types,
          )?;
        }
      }

      ASTNode::DoWhileStatement {
        node_id,
        condition,
        body,
        ..
      } => {
        let cf = domain::BlockAnnotation {
          topic: topic::new_node_topic(node_id),
          kind: domain::BlockAnnotationKind::DoWhile,
        };

        // Process condition without control flow context
        process_second_pass_nodes(
          &vec![condition.as_ref()],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;

        // Process body with control flow added to scope
        if let Some(body) = body {
          let cf_scope = domain::add_annotation_to_scope(scope, cf);
          process_second_pass_nodes(
            &vec![body.as_ref()],
            is_in_scope,
            in_scope_source_topics,
            in_scope_files,
            mutations_map,
            ancestors_map,
            descendants_map,
            relatives_map,
            nodes,
            &cf_scope,
            topic_metadata,
            function_properties,
            variable_types,
          )?;
        }
      }

      ASTNode::UncheckedBlock {
        node_id,
        statements,
        ..
      } => {
        let block_scope =
          domain::add_to_scope(scope, topic::new_node_topic(node_id));
        let ann = domain::BlockAnnotation {
          topic: topic::new_node_topic(node_id),
          kind: domain::BlockAnnotationKind::Unchecked,
        };
        let annotated_scope =
          domain::add_annotation_to_scope(&block_scope, ann);
        let child_nodes: Vec<&ASTNode> = statements.iter().collect();
        process_second_pass_nodes(
          &child_nodes,
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          &annotated_scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;
      }

      ASTNode::InlineAssembly { node_id, .. } => {
        let block_scope =
          domain::add_to_scope(scope, topic::new_node_topic(node_id));
        let ann = domain::BlockAnnotation {
          topic: topic::new_node_topic(node_id),
          kind: domain::BlockAnnotationKind::InlineAssembly,
        };
        let _annotated_scope =
          domain::add_annotation_to_scope(&block_scope, ann);
        // InlineAssembly has no parseable children — scope is created but
        // not walked further.
      }

      ASTNode::FunctionSignature {
        documentation,
        modifiers,
        parameters,
        return_parameters,
        ..
      } => {
        // Documentation is processed with the plain member scope
        if let Some(doc) = documentation {
          process_second_pass_nodes(
            &vec![&**doc],
            is_in_scope,
            in_scope_source_topics,
            in_scope_files,
            mutations_map,
            ancestors_map,
            descendants_map,
            relatives_map,
            nodes,
            scope,
            topic_metadata,
            function_properties,
            variable_types,
          )?;
        }

        // Modifiers list: set signature_container to the ModifierList node
        let mods_scope = domain::set_signature_container(
          scope,
          topic::new_node_topic(&modifiers.node_id()),
        );
        process_second_pass_nodes(
          &vec![&**modifiers],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          &mods_scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;

        // Parameters: set signature_container to the ParameterList node
        let params_scope = domain::set_signature_container(
          scope,
          topic::new_node_topic(&parameters.node_id()),
        );
        process_second_pass_nodes(
          &vec![&**parameters],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          &params_scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;

        // Return parameters: set signature_container to the return ParameterList node
        let ret_scope = domain::set_signature_container(
          scope,
          topic::new_node_topic(&return_parameters.node_id()),
        );
        process_second_pass_nodes(
          &vec![&**return_parameters],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          &ret_scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;
      }

      ASTNode::ModifierSignature {
        documentation,
        parameters,
        ..
      } => {
        // Documentation is processed with the plain member scope
        if let Some(doc) = documentation {
          process_second_pass_nodes(
            &vec![&**doc],
            is_in_scope,
            in_scope_source_topics,
            in_scope_files,
            mutations_map,
            ancestors_map,
            descendants_map,
            relatives_map,
            nodes,
            scope,
            topic_metadata,
            function_properties,
            variable_types,
          )?;
        }

        // Parameters: set signature_container to the ParameterList node
        let params_scope = domain::set_signature_container(
          scope,
          topic::new_node_topic(&parameters.node_id()),
        );
        process_second_pass_nodes(
          &vec![&**parameters],
          is_in_scope,
          in_scope_source_topics,
          in_scope_files,
          mutations_map,
          ancestors_map,
          descendants_map,
          relatives_map,
          nodes,
          &params_scope,
          topic_metadata,
          function_properties,
          variable_types,
        )?;
      }

      // Default: process all children generically
      _ => {
        let child_nodes = node.nodes();
        if !child_nodes.is_empty() {
          let scope = match node {
            ASTNode::SemanticBlock { node_id, .. }
            | ASTNode::ContractDefinition { node_id, .. }
            | ASTNode::FunctionDefinition { node_id, .. }
            | ASTNode::ModifierDefinition { node_id, .. }
            | ASTNode::StructDefinition { node_id, .. }
            | ASTNode::EnumDefinition { node_id, .. }
            | ASTNode::EventDefinition { node_id, .. }
            | ASTNode::ErrorDefinition { node_id, .. } => {
              domain::add_to_scope(scope, topic::new_node_topic(node_id))
            }
            // Transparent nodes (e.g. Block) — pass scope through unchanged
            _ => scope.clone(),
          };

          process_second_pass_nodes(
            &child_nodes,
            is_in_scope,
            in_scope_source_topics,
            in_scope_files,
            mutations_map,
            ancestors_map,
            descendants_map,
            relatives_map,
            nodes,
            &scope,
            topic_metadata,
            function_properties,
            variable_types,
          )?;
        }
      }
    }
  }

  Ok(())
}

fn collect_references_and_statements(
  node: &ASTNode,
  current_containing_block: Option<i32>,
  referenced_nodes: &mut Vec<ReferencedNode>,
  reverts: &mut Vec<FirstPassRevert>,
  function_calls: &mut Vec<i32>,
  variable_mutations: &mut Vec<ReferencedNode>,
  events_emitted: &mut Vec<i32>,
) {
  // Update current_containing_block when entering a block-like node.
  let containing_block = if node.is_containing_block() {
    Some(node.node_id())
  } else {
    current_containing_block
  };

  match node {
    // References
    ASTNode::Identifier {
      referenced_declaration,
      ..
    }
    | ASTNode::IdentifierPath {
      referenced_declaration,
      ..
    } => {
      if let Some(block_id) = containing_block {
        referenced_nodes.push(ReferencedNode {
          statement_node: block_id,
          referenced_node: *referenced_declaration,
        });
      }
    }

    // Member access (e.g., EnumType.Value, contract.member)
    ASTNode::MemberAccess {
      referenced_declaration: Some(referenced_declaration),
      ..
    } => {
      if let Some(block_id) = containing_block {
        referenced_nodes.push(ReferencedNode {
          statement_node: block_id,
          referenced_node: *referenced_declaration,
        });
      }
    }

    // Function calls - check for require()/revert()
    ASTNode::FunctionCall {
      node_id,
      expression,
      referenced_return_declarations,
      ..
    } => {
      // Add references to the function's return parameter declarations
      if let Some(block_id) = containing_block {
        for &return_decl_id in referenced_return_declarations {
          referenced_nodes.push(ReferencedNode {
            statement_node: block_id,
            referenced_node: return_decl_id,
          });
        }
      }

      if let ASTNode::Identifier {
        name,
        referenced_declaration,
        ..
      } = expression.as_ref()
      {
        if name == "require" {
          reverts.push(FirstPassRevert {
            statement_node: *node_id,
            kind: RevertConstraintKind::Require,
            error_node: None,
          });
        } else if name == "revert" {
          // Bare `revert("string")` — no custom error declaration.
          reverts.push(FirstPassRevert {
            statement_node: *node_id,
            kind: RevertConstraintKind::Revert,
            error_node: None,
          });
        } else {
          // For other function calls, extract the function reference
          function_calls.push(*referenced_declaration);
        }
      }
    }

    // RevertStatement: `revert MyError(args)` or `revert C.MyError(args)`.
    // The error declaration is the `referenced_declaration` of the inner
    // FunctionCall's expression.
    //
    // We handle the inner FunctionCall manually instead of falling through
    // to the generic child walk so the error identifier lands only in
    // `reverts.error_node` — never in `function_calls`. Arguments and any
    // return-decl references still flow through the normal recursion.
    ASTNode::RevertStatement {
      node_id,
      error_call,
      ..
    } => {
      let error_node =
        if let ASTNode::FunctionCall { expression, .. } = error_call.as_ref() {
          extract_referenced_declaration(expression)
        } else {
          None
        };
      reverts.push(FirstPassRevert {
        statement_node: *node_id,
        kind: RevertConstraintKind::Revert,
        error_node,
      });
      walk_call_skipping_callee(
        error_call,
        containing_block,
        referenced_nodes,
        reverts,
        function_calls,
        variable_mutations,
        events_emitted,
      );
      return;
    }

    // EmitStatement: `emit SomeEvent(args)`. The event declaration is the
    // `referenced_declaration` of the inner FunctionCall's expression.
    //
    // Same pattern as RevertStatement: walk the inner call manually so the
    // event identifier lands only in `events_emitted` — never in
    // `function_calls`.
    ASTNode::EmitStatement { event_call, .. } => {
      if let ASTNode::FunctionCall { expression, .. } = event_call.as_ref()
        && let Some(event_decl) = extract_referenced_declaration(expression)
      {
        events_emitted.push(event_decl);
      }
      walk_call_skipping_callee(
        event_call,
        containing_block,
        referenced_nodes,
        reverts,
        function_calls,
        variable_mutations,
        events_emitted,
      );
      return;
    }

    // Mutations - Assignments (including compound assignments like +=, -=, etc.)
    ASTNode::Assignment {
      node_id,
      left_hand_side,
      ..
    } => {
      if let Some(referenced_declaration) =
        extract_base_variable_reference(left_hand_side)
      {
        variable_mutations.push(ReferencedNode {
          statement_node: *node_id,
          referenced_node: referenced_declaration,
        });
      }
    }

    // Mutations - Unary operations (++, --, delete)
    ASTNode::UnaryOperation {
      node_id,
      operator,
      sub_expression,
      ..
    } => {
      if matches!(
        operator,
        ast::UnaryOperator::Increment
          | ast::UnaryOperator::Decrement
          | ast::UnaryOperator::Delete
      ) && let Some(referenced_declaration) =
        extract_base_variable_reference(sub_expression)
      {
        variable_mutations.push(ReferencedNode {
          statement_node: *node_id,
          referenced_node: referenced_declaration,
        });
      }
    }

    _ => (),
  }

  // Continue traversing child nodes
  let child_nodes = node.nodes();
  for child in child_nodes {
    collect_references_and_statements(
      child,
      containing_block,
      referenced_nodes,
      reverts,
      function_calls,
      variable_mutations,
      events_emitted,
    );
  }
}

/// Walk an `emit Foo(...)` / `revert Bar(...)`'s inner FunctionCall
/// like the generic visitor would, but bypass the FunctionCall arm
/// itself — its only side effect on this path is pushing the callee
/// into `function_calls`, which is exactly the double-bookkeeping we
/// want to avoid. The expression is still walked recursively so the
/// event/error identifier flows into `referenced_nodes` through the
/// Identifier / IdentifierPath / MemberAccess arms as before.
/// Arguments and `referenced_return_declarations` are also collected
/// so references inside them survive.
fn walk_call_skipping_callee(
  call: &ASTNode,
  containing_block: Option<i32>,
  referenced_nodes: &mut Vec<ReferencedNode>,
  reverts: &mut Vec<FirstPassRevert>,
  function_calls: &mut Vec<i32>,
  variable_mutations: &mut Vec<ReferencedNode>,
  events_emitted: &mut Vec<i32>,
) {
  let ASTNode::FunctionCall {
    expression,
    arguments,
    referenced_return_declarations,
    ..
  } = call
  else {
    return;
  };
  if let Some(block_id) = containing_block {
    for &return_decl_id in referenced_return_declarations {
      referenced_nodes.push(ReferencedNode {
        statement_node: block_id,
        referenced_node: return_decl_id,
      });
    }
  }
  collect_references_and_statements(
    expression,
    containing_block,
    referenced_nodes,
    reverts,
    function_calls,
    variable_mutations,
    events_emitted,
  );
  for arg in arguments {
    collect_references_and_statements(
      arg,
      containing_block,
      referenced_nodes,
      reverts,
      function_calls,
      variable_mutations,
      events_emitted,
    );
  }
}

/// Pulls a declaration node ID out of an expression that names a
/// declaration directly (`Identifier`, `IdentifierPath`) or via member
/// access (`C.MyError`, `lib.SomeEvent`). Returns `None` for expressions
/// whose declaration cannot be resolved without further context.
fn extract_referenced_declaration(expr: &ASTNode) -> Option<i32> {
  match expr {
    ASTNode::Identifier {
      referenced_declaration,
      ..
    }
    | ASTNode::IdentifierPath {
      referenced_declaration,
      ..
    } => Some(*referenced_declaration),
    ASTNode::MemberAccess {
      referenced_declaration: Some(referenced_declaration),
      ..
    } => Some(*referenced_declaration),
    _ => None,
  }
}

/// Extracts the base variable's referenced_declaration from an expression that
/// may be mutated. Handles direct variable references (Identifier, IdentifierPath),
/// member access chains (e.g., someStruct.field), and index access chains
/// (e.g., arr[i], mapping[key]).
///
/// Returns the referenced_declaration of the base variable, or None if it cannot
/// be determined (e.g., for complex expressions or when the reference is missing).
fn extract_base_variable_reference(node: &ASTNode) -> Option<i32> {
  match node {
    // Direct variable reference
    ASTNode::Identifier {
      referenced_declaration,
      ..
    } => Some(*referenced_declaration),

    // Path-based variable reference (e.g., SomeContract.someVar)
    ASTNode::IdentifierPath {
      referenced_declaration,
      ..
    } => Some(*referenced_declaration),

    // Member access (e.g., someStruct.field) - recurse to find the base variable
    ASTNode::MemberAccess { expression, .. } => {
      extract_base_variable_reference(expression)
    }

    // Index access (e.g., arr[i], mapping[key]) - recurse to find the base variable
    ASTNode::IndexAccess {
      base_expression, ..
    } => extract_base_variable_reference(base_expression),

    // For other node types (e.g., function calls returning values), we can't
    // determine a base variable
    _ => None,
  }
}

/// Recursively extracts type references from a type AST node.
fn collect_type_references(
  type_node: &ASTNode,
  statement_node: i32,
  references: &mut Vec<ReferencedNode>,
) {
  match type_node {
    ASTNode::VariableDeclaration { type_name, .. } => {
      collect_type_references(type_name, statement_node, references);
    }
    ASTNode::UserDefinedTypeName {
      referenced_declaration,
      ..
    } => {
      references.push(ReferencedNode {
        statement_node,
        referenced_node: *referenced_declaration,
      });
    }
    ASTNode::Mapping {
      key_type,
      value_type,
      ..
    } => {
      collect_type_references(key_type, statement_node, references);
      collect_type_references(value_type, statement_node, references);
    }
    ASTNode::ArrayTypeName { base_type, .. } => {
      collect_type_references(base_type, statement_node, references);
    }
    _ => (),
  }
}

impl FirstPassDeclaration {
  /// Get the declaration kind for any declaration variant
  pub fn declaration_kind(&self) -> &NamedTopicKind {
    match self {
      FirstPassDeclaration::FunctionMod {
        declaration_kind, ..
      } => declaration_kind,
      FirstPassDeclaration::Contract {
        declaration_kind, ..
      } => declaration_kind,
      FirstPassDeclaration::Flat {
        declaration_kind, ..
      } => declaration_kind,
    }
  }
}

/// Convert ast::FunctionVisibility to foundry_compilers_artifacts::Visibility
fn function_visibility_to_visibility(
  vis: &ast::FunctionVisibility,
) -> Visibility {
  match vis {
    ast::FunctionVisibility::Public => Visibility::Public,
    ast::FunctionVisibility::Private => Visibility::Private,
    ast::FunctionVisibility::Internal => Visibility::Internal,
    ast::FunctionVisibility::External => Visibility::External,
  }
}

/// Convert ast::VariableVisibility to foundry_compilers_artifacts::Visibility
fn variable_visibility_to_visibility(
  vis: &ast::VariableVisibility,
) -> Visibility {
  match vis {
    ast::VariableVisibility::Public => Visibility::Public,
    ast::VariableVisibility::Private => Visibility::Private,
    ast::VariableVisibility::Internal => Visibility::Internal,
  }
}

/// Convert foundry_compilers_artifacts::Visibility to domain::NamedTopicVisibility
fn visibility_to_named_topic_visibility(
  vis: &Visibility,
) -> domain::NamedTopicVisibility {
  match vis {
    Visibility::Public => domain::NamedTopicVisibility::Public,
    Visibility::Private => domain::NamedTopicVisibility::Private,
    Visibility::Internal => domain::NamedTopicVisibility::Internal,
    Visibility::External => domain::NamedTopicVisibility::External,
  }
}

/// Walk the first-pass declarations and write each contract's
/// `base_contracts` into `inheritance`, sorted ascending by topic ID.
/// Contracts with no bases are absent (sparse storage). Run before
/// `tree_shake`, which otherwise drops `base_contracts`.
fn collect_contract_inheritance(
  first_pass_declarations: &BTreeMap<i32, FirstPassDeclaration>,
  inheritance: &mut BTreeMap<topic::Topic, Vec<topic::Topic>>,
) {
  for (&node_id, decl) in first_pass_declarations {
    if let FirstPassDeclaration::Contract { base_contracts, .. } = decl
      && !base_contracts.is_empty()
    {
      let mut bases: Vec<topic::Topic> = base_contracts
        .iter()
        .map(|r| topic::new_node_topic(&r.referenced_node))
        .collect();
      bases.sort();
      bases.dedup();
      inheritance.insert(topic::new_node_topic(&node_id), bases);
    }
  }
}

/// Tree shake the first pass declarations to include only in-scope and used declarations.
/// Returns a tuple of:
/// - A map of node_id to InScopeDeclaration containing all nodes that reference each declaration
/// - A map of variable node_id to Vec of mutation node_ids (assignment/unary operation nodes)
type TreeShakeResult =
  (BTreeMap<i32, InScopeDeclaration>, BTreeMap<i32, Vec<i32>>);

fn tree_shake(
  first_pass_declarations: &BTreeMap<i32, FirstPassDeclaration>,
) -> Result<TreeShakeResult, String> {
  let mut in_scope_declarations = BTreeMap::new();
  let mut mutations_map: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
  let mut visiting = HashSet::new(); // For cycle detection

  // First, collect all publicly in-scope declarations as starting points
  let in_scope_contracts: Vec<i32> = first_pass_declarations
    .iter()
    .filter_map(|(node_id, decl)| match decl {
      FirstPassDeclaration::Contract {
        is_publicly_in_scope: true,
        ..
      } => Some(*node_id),
      _ => None,
    })
    .collect();

  // Process each publicly visible declaration recursively
  for &node_id in &in_scope_contracts {
    process_tree_shake_declarations(
      node_id,
      None, // No referencing node for root declarations
      ReferenceProcessingMethod::ProcessAllContractMembers, // Process all public members of in scope contracts
      Some(node_id), // The contract itself is the current component
      first_pass_declarations,
      &mut in_scope_declarations,
      &mut mutations_map,
      &mut visiting,
    )?;
  }

  Ok((in_scope_declarations, mutations_map))
}

/// Recursively process a declaration and all its references
#[allow(clippy::too_many_arguments)]
fn process_tree_shake_declarations(
  node_id: i32,
  referencing_info: Option<ScopedReference>, // The reference with its scope context
  reference_processing_method: ReferenceProcessingMethod,
  current_component: Option<i32>, // The component (contract/interface) we're processing from
  first_pass_declarations: &BTreeMap<i32, FirstPassDeclaration>,
  in_scope_declarations: &mut BTreeMap<i32, InScopeDeclaration>,
  mutations_map: &mut BTreeMap<i32, Vec<i32>>,
  visiting: &mut HashSet<i32>,
) -> Result<(), String> {
  // Cycle detection
  if visiting.contains(&node_id) {
    // We found a cycle, but this is not necessarily an error in the code analysis
    // Just skip processing this node to avoid infinite recursion
    return Ok(());
  }

  // If already processed, add this reference and return
  if let Some(in_scope_decl) = in_scope_declarations.get_mut(&node_id) {
    if let Some(ref_info) = referencing_info {
      in_scope_decl.add_reference_if_not_present(ref_info)
    }
    return Ok(());
  }

  // Check if declaration exists in our first pass dictionary
  let first_pass_decl = match first_pass_declarations.get(&node_id) {
    Some(decl) => decl,
    None => {
      return Ok(());
    }
  };

  // Mark as currently being visited (for cycle detection)
  visiting.insert(node_id);

  // Create new in-scope declaration with direct reference
  let in_scope_decl = match first_pass_decl {
    FirstPassDeclaration::FunctionMod {
      declaration_kind,
      visibility,
      name,
      reverts,
      function_calls,
      variable_mutations,
      events_emitted,
      ..
    } => {
      // Filter function calls to exclude events, errors, and modifiers
      let filtered_function_calls = function_calls
        .iter()
        .filter(|&&call_id| {
          if let Some(called_decl) = first_pass_declarations.get(&call_id) {
            !matches!(
              called_decl.declaration_kind(),
              NamedTopicKind::Event
                | NamedTopicKind::Error
                | NamedTopicKind::Modifier
            )
          } else {
            // Keep calls to declarations not in our map (external references)
            true
          }
        })
        .cloned()
        .collect();

      // Collect mutations into the mutations_map
      // Maps variable_node_id -> Vec<mutation_node_id>
      for mutation in variable_mutations {
        mutations_map
          .entry(mutation.referenced_node)
          .or_default()
          .push(mutation.statement_node);
      }

      let references: Vec<ScopedReference> =
        referencing_info.into_iter().collect();
      InScopeDeclaration::FunctionMod {
        declaration_kind: declaration_kind.clone(),
        visibility: visibility.clone(),
        name: name.clone(),
        references,
        reverts: reverts.clone(),
        function_calls: filtered_function_calls,
        variable_mutations: variable_mutations.clone(),
        events_emitted: events_emitted.clone(),
      }
    }
    FirstPassDeclaration::Flat {
      declaration_kind,
      visibility,
      name,
      ..
    } => {
      let references: Vec<ScopedReference> =
        referencing_info.into_iter().collect();
      InScopeDeclaration::Flat {
        declaration_kind: declaration_kind.clone(),
        visibility: visibility.clone(),
        name: name.clone(),
        references,
      }
    }
    FirstPassDeclaration::Contract {
      container_file,
      declaration_kind,
      visibility,
      name,
      base_contracts,
      other_contracts,
      public_members,
      ..
    } => {
      let references: Vec<ScopedReference> =
        referencing_info.into_iter().collect();
      InScopeDeclaration::Contract {
        container_file: container_file.clone(),
        declaration_kind: declaration_kind.clone(),
        visibility: visibility.clone(),
        name: name.clone(),
        references,
        base_contracts: base_contracts.clone(),
        other_contracts: other_contracts.clone(),
        public_members: public_members.clone(),
      }
    }
  };

  in_scope_declarations.insert(node_id, in_scope_decl);

  // Process all referenced nodes from this declaration if it is a function,
  // modifier, or contract
  match first_pass_decl {
    FirstPassDeclaration::FunctionMod {
      parent_contract,
      referenced_nodes,
      ..
    } => {
      // References from within a function/modifier are at Member scope level
      // The containing member is the current function/modifier (node_id)
      // The containing component should be the function's actual parent contract,
      // not the contract that called it (current_component). This ensures that
      // library functions have their internal references scoped correctly to the
      // library, not to the calling contract.
      let function_component = parent_contract.or(current_component);
      if let Some(component_id) = function_component {
        for ref_node in referenced_nodes {
          process_tree_shake_declarations(
            ref_node.referenced_node,
            Some(ScopedReference {
              reference_node: ref_node.statement_node,
              containing_component: component_id,
              containing_member: Some(node_id),
            }),
            ReferenceProcessingMethod::Normal,
            function_component,
            first_pass_declarations,
            in_scope_declarations,
            mutations_map,
            visiting,
          )?;
        }
      }
    }
    FirstPassDeclaration::Contract {
      base_contracts,
      other_contracts,
      public_members,
      referenced_nodes,
      ..
    } => {
      // For contracts, the current node_id IS the component
      // Base contracts and other contract references are at contract scope level
      for base_contract_ref in base_contracts {
        process_tree_shake_declarations(
          base_contract_ref.referenced_node,
          Some(ScopedReference {
            reference_node: base_contract_ref.statement_node,
            containing_component: node_id,
            containing_member: None,
          }),
          ReferenceProcessingMethod::ProcessAllContractMembers,
          Some(base_contract_ref.referenced_node), // The base contract becomes the new component context
          first_pass_declarations,
          in_scope_declarations,
          mutations_map,
          visiting,
        )?;
      }

      // Process other contracts (using for, type references) at contract scope
      for other_contract_ref in other_contracts {
        process_tree_shake_declarations(
          other_contract_ref.referenced_node,
          Some(ScopedReference {
            reference_node: other_contract_ref.statement_node,
            containing_component: node_id,
            containing_member: None,
          }),
          ReferenceProcessingMethod::Normal,
          Some(node_id), // Stay in current component context
          first_pass_declarations,
          in_scope_declarations,
          mutations_map,
          visiting,
        )?;
      }

      // Process type references from state variable declarations at contract scope
      for ref_node in referenced_nodes {
        process_tree_shake_declarations(
          ref_node.referenced_node,
          Some(ScopedReference {
            reference_node: ref_node.statement_node,
            containing_component: node_id,
            containing_member: None,
          }),
          ReferenceProcessingMethod::Normal,
          Some(node_id), // Stay in current component context
          first_pass_declarations,
          in_scope_declarations,
          mutations_map,
          visiting,
        )?;
      }

      // Process public members (no referencing info - these are root declarations)
      // They belong to this contract (node_id)
      if reference_processing_method
        == ReferenceProcessingMethod::ProcessAllContractMembers
      {
        for &public_member_id in public_members {
          process_tree_shake_declarations(
            public_member_id,
            None,
            ReferenceProcessingMethod::Normal,
            Some(node_id), // The contract is the component context for its members
            first_pass_declarations,
            in_scope_declarations,
            mutations_map,
            visiting,
          )?;
        }
      }
    }
    FirstPassDeclaration::Flat { .. } => (),
  };

  // Mark as fully processed
  visiting.remove(&node_id);

  Ok(())
}

// ============================================================================
// Ancestry Pass
// ============================================================================

/// Maps variable node_id -> Vec of ancestor variable node_ids (direct ancestors only)
pub type AncestorsMap = BTreeMap<i32, Vec<i32>>;

/// Maps variable node_id -> Vec of descendant variable node_ids
pub type DescendantsMap = BTreeMap<i32, Vec<i32>>;

/// Maps variable node_id -> Vec of relative variable node_ids.
/// Relatives are variables that:
/// 1. Appear together in comparison, arithmetic, or bitwise binary operations
/// 2. Appear as alternatives in conditional (ternary) expressions
/// 3. Are involved in another variable's assignment (RHS of assignments)
pub type RelativesMap = BTreeMap<i32, Vec<i32>>;

/// Context for tracking function return parameters during ancestry collection.
/// Used when processing Return statements to link expression variables to return parameters.
struct AncestryContext {
  /// The return parameter node IDs for the current function being processed
  return_parameter_ids: Vec<i32>,
}

/// Recursively collects ancestry and relatives relationships from a node and its children.
fn collect_ancestry_from_node(
  node: &ASTNode,
  context: Option<&AncestryContext>,
  ancestors_map: &mut AncestorsMap,
  relatives_map: &mut RelativesMap,
) {
  match node {
    // Variable declaration with initializer (state variable or local)
    ASTNode::VariableDeclaration {
      node_id,
      value: Some(initial_value),
      ..
    } => {
      // Collect all variable references from the initial value expression
      // These are relatives (variables involved in assignment), not ancestors
      let relative_ids = collect_variable_refs_from_expression(initial_value);
      add_relatives_unidirectional(relatives_map, *node_id, &relative_ids);

      // Recurse into the initial value
      collect_ancestry_from_node(
        initial_value,
        context,
        ancestors_map,
        relatives_map,
      );
    }

    // Variable declaration statement (local variables with initializer)
    ASTNode::VariableDeclarationStatement {
      declarations,
      initial_value: Some(init_value),
      ..
    } => {
      // Check if this is a multi-return function call
      if let ASTNode::FunctionCall {
        referenced_return_declarations,
        ..
      } = init_value.as_ref()
        && referenced_return_declarations.len() > 1
        && declarations.len() == referenced_return_declarations.len()
      {
        // Multi-return case: pair each declaration with its corresponding return declaration
        // The return declaration is a relative of this variable (involved in assignment)
        for (i, decl) in declarations.iter().enumerate() {
          if let ASTNode::VariableDeclaration { node_id, .. } = decl {
            add_relatives_unidirectional(
              relatives_map,
              *node_id,
              &[referenced_return_declarations[i]],
            );
          }
        }
        // Also recurse into the function call to process its arguments
        collect_ancestry_from_node(
          init_value,
          context,
          ancestors_map,
          relatives_map,
        );
        // Recurse into declarations (they may have nested structures)
        for decl in declarations {
          collect_ancestry_from_node(
            decl,
            context,
            ancestors_map,
            relatives_map,
          );
        }
        return;
      }

      // Single variable or single-return function call case
      // Collect all variable references from the initial value
      // These are relatives (variables involved in assignment), not ancestors
      let relative_ids = collect_variable_refs_from_expression(init_value);

      // Add relatives to each declared variable
      for decl in declarations {
        if let ASTNode::VariableDeclaration { node_id, .. } = decl {
          add_relatives_unidirectional(relatives_map, *node_id, &relative_ids);
        }
      }

      // Recurse into initial value and declarations
      collect_ancestry_from_node(
        init_value,
        context,
        ancestors_map,
        relatives_map,
      );
      for decl in declarations {
        collect_ancestry_from_node(decl, context, ancestors_map, relatives_map);
      }
    }

    // Assignment: RHS variables are relatives of LHS base variable
    // Also handles index access on LHS (e.g., myMap[key] = val)
    ASTNode::Assignment {
      left_hand_side,
      right_hand_side,
      ..
    } => {
      if let Some(target_var_id) =
        extract_base_variable_reference(left_hand_side)
      {
        // Collect relatives from RHS (variables involved in assignment)
        let mut relative_ids =
          collect_variable_refs_from_expression(right_hand_side);

        // Also collect index expressions from LHS (for mappings/arrays)
        collect_index_refs_from_lhs(left_hand_side, &mut relative_ids);

        add_relatives_unidirectional(
          relatives_map,
          target_var_id,
          &relative_ids,
        );
      }

      // Recurse into both sides
      collect_ancestry_from_node(
        left_hand_side,
        context,
        ancestors_map,
        relatives_map,
      );
      collect_ancestry_from_node(
        right_hand_side,
        context,
        ancestors_map,
        relatives_map,
      );
    }

    // Function call with Argument nodes: argument variables are ancestors of parameter variables
    ASTNode::FunctionCall {
      arguments,
      expression,
      ..
    } => {
      for arg in arguments {
        if let ASTNode::Argument {
          parameter: Some(param_identifier),
          argument: arg_expr,
          ..
        } = arg
        {
          // The parameter identifier references the VariableDeclaration
          if let ASTNode::Identifier {
            referenced_declaration: param_var_id,
            ..
          } = param_identifier.as_ref()
          {
            let ancestor_ids = collect_variable_refs_from_expression(arg_expr);
            add_ancestors(ancestors_map, *param_var_id, &ancestor_ids);
          }
        }
        // Recurse into argument
        collect_ancestry_from_node(arg, context, ancestors_map, relatives_map);
      }

      // Recurse into expression
      collect_ancestry_from_node(
        expression,
        context,
        ancestors_map,
        relatives_map,
      );
    }

    // Return statement: expression variables are ancestors of return parameter variables
    ASTNode::Return {
      expression: Some(return_expr),
      ..
    } => {
      // We need the return parameters from context or look them up
      // The function_return_parameters field is the node_id of the ParameterList
      // We need to find the actual parameter VariableDeclarations
      if let Some(ctx) = context {
        let ancestor_ids = collect_variable_refs_from_expression(return_expr);

        // Handle tuple returns: if return_expr is a TupleExpression, pair with return params
        if let ASTNode::TupleExpression { components, .. } =
          return_expr.as_ref()
        {
          if components.len() == ctx.return_parameter_ids.len() {
            // Pair each component with its corresponding return parameter
            for (i, component) in components.iter().enumerate() {
              let comp_ancestors =
                collect_variable_refs_from_expression(component);
              add_ancestors(
                ancestors_map,
                ctx.return_parameter_ids[i],
                &comp_ancestors,
              );
            }
          } else {
            // Fallback: all expression variables are ancestors of all return params
            for &ret_param_id in &ctx.return_parameter_ids {
              add_ancestors(ancestors_map, ret_param_id, &ancestor_ids);
            }
          }
        } else {
          // Single return value - all expression variables are ancestors of all return params
          // (typically there's only one return param in this case)
          for &ret_param_id in &ctx.return_parameter_ids {
            add_ancestors(ancestors_map, ret_param_id, &ancestor_ids);
          }
        }
      }
      // Note: If we don't have context, we can't resolve the return parameters.
      // This is handled by building context when entering functions.

      // Still recurse into the return expression
      collect_ancestry_from_node(
        return_expr,
        context,
        ancestors_map,
        relatives_map,
      );
    }

    // Function definition: build context with return parameters and recurse
    ASTNode::FunctionDefinition {
      signature, body, ..
    } => {
      let return_param_ids = extract_return_parameter_ids(signature);
      let func_context = AncestryContext {
        return_parameter_ids: return_param_ids,
      };

      // Recurse into signature and body with the function context
      collect_ancestry_from_node(
        signature,
        Some(&func_context),
        ancestors_map,
        relatives_map,
      );
      if let Some(body_node) = body {
        collect_ancestry_from_node(
          body_node,
          Some(&func_context),
          ancestors_map,
          relatives_map,
        );
      }
    }

    // Modifier definition: similar to function
    ASTNode::ModifierDefinition {
      signature, body, ..
    } => {
      // Modifiers don't have return parameters, but we still recurse
      collect_ancestry_from_node(
        signature,
        context,
        ancestors_map,
        relatives_map,
      );
      collect_ancestry_from_node(body, context, ancestors_map, relatives_map);
    }

    // Binary operation: collect relatives for non-boolean operators
    ASTNode::BinaryOperation {
      operator,
      left_expression,
      right_expression,
      ..
    } => {
      if operator.is_relative_operator() {
        let left_refs = collect_variable_refs_from_expression(left_expression);
        let right_refs =
          collect_variable_refs_from_expression(right_expression);
        add_relatives_bidirectional(relatives_map, &left_refs, &right_refs);
      }

      // Recurse into both sides
      collect_ancestry_from_node(
        left_expression,
        context,
        ancestors_map,
        relatives_map,
      );
      collect_ancestry_from_node(
        right_expression,
        context,
        ancestors_map,
        relatives_map,
      );
    }

    // Conditional (ternary): true and false branch variables are relatives
    ASTNode::Conditional {
      condition,
      true_expression,
      false_expression,
      ..
    } => {
      let true_refs = collect_variable_refs_from_expression(true_expression);
      if let Some(false_expr) = false_expression {
        let false_refs = collect_variable_refs_from_expression(false_expr);
        add_relatives_bidirectional(relatives_map, &true_refs, &false_refs);
        collect_ancestry_from_node(
          false_expr,
          context,
          ancestors_map,
          relatives_map,
        );
      }

      // Recurse into all parts
      collect_ancestry_from_node(
        condition,
        context,
        ancestors_map,
        relatives_map,
      );
      collect_ancestry_from_node(
        true_expression,
        context,
        ancestors_map,
        relatives_map,
      );
    }

    // For all other nodes, just recurse into children
    _ => {
      for child in node.nodes() {
        collect_ancestry_from_node(
          child,
          context,
          ancestors_map,
          relatives_map,
        );
      }
    }
  }
}

/// Extracts return parameter node IDs from a FunctionSignature
fn extract_return_parameter_ids(signature: &ASTNode) -> Vec<i32> {
  if let ASTNode::FunctionSignature {
    return_parameters, ..
  } = signature
    && let ASTNode::ParameterList { parameters, .. } =
      return_parameters.as_ref()
  {
    return parameters
      .iter()
      .filter_map(|p| {
        if let ASTNode::VariableDeclaration { node_id, .. } = p {
          Some(*node_id)
        } else {
          None
        }
      })
      .collect();
  }
  Vec::new()
}

/// Collects all variable reference node IDs from an expression.
/// Returns the referenced_declaration values (variable node IDs), not the expression node IDs.
fn collect_variable_refs_from_expression(node: &ASTNode) -> Vec<i32> {
  let mut refs = Vec::new();
  collect_variable_refs_recursive(node, &mut refs);
  refs
}

/// Checks if a function call expression refers to a builtin function.
/// Builtins have negative referenced_declaration values (e.g., keccak256 is -8).
/// Also handles member access on builtins (e.g., abi.encode where abi is -1).
fn is_builtin_function_call(expression: &ASTNode) -> bool {
  match expression {
    // Direct builtin call: keccak256(...)
    ASTNode::Identifier {
      referenced_declaration,
      ..
    } => *referenced_declaration < 0,

    // Member access on builtin: abi.encode(...), abi.encodePacked(...)
    ASTNode::MemberAccess { expression, .. } => {
      // Check if the base expression is a builtin
      match expression.as_ref() {
        ASTNode::Identifier {
          referenced_declaration,
          ..
        } => *referenced_declaration < 0,
        _ => false,
      }
    }

    _ => false,
  }
}

/// Recursive helper for collecting variable references.
///
/// This function collects variables that directly contribute to a value, NOT variables
/// that are used as function arguments or as the base of member access expressions.
/// For example, in `x = Create2.computeAddress(salt, hash)`:
/// - The ancestor of `x` is the return value of `computeAddress`, not `salt`, `hash`, or `Create2`
/// - The arguments `salt` and `hash` flow into the function's parameters (handled separately)
fn collect_variable_refs_recursive(node: &ASTNode, refs: &mut Vec<i32>) {
  match node {
    ASTNode::Identifier {
      referenced_declaration,
      ..
    }
    | ASTNode::IdentifierPath {
      referenced_declaration,
      ..
    } => {
      if !refs.contains(referenced_declaration) {
        refs.push(*referenced_declaration);
      }
    }

    // MemberAccess: only capture the referenced member, don't recurse into base expression
    // e.g., in `obj.field`, we want `field`, not `obj`
    ASTNode::MemberAccess {
      referenced_declaration: Some(ref_decl),
      ..
    } => {
      if !refs.contains(ref_decl) {
        refs.push(*ref_decl);
      }
      // Do NOT recurse into expression - the base object is not an ancestor of the member value
    }

    ASTNode::MemberAccess {
      referenced_declaration: None,
      ..
    } => {
      // No direct reference and no recursion needed
    }

    // Function calls: for regular functions, only include the return declaration references
    // and do NOT recurse into arguments - they flow into parameters, not the result.
    // However, for builtin functions (negative referenced_declaration), we cannot trace
    // through the function body, so we treat arguments as direct ancestors of the result.
    ASTNode::FunctionCall {
      referenced_return_declarations,
      arguments,
      expression,
      ..
    } => {
      // Check if this is a builtin function call
      let is_builtin = is_builtin_function_call(expression);

      if is_builtin {
        // For builtins, recurse into arguments since we can't trace through the function body
        for arg in arguments {
          collect_variable_refs_recursive(arg, refs);
        }
      } else {
        // For regular functions, add the return declarations as ancestors
        for &ret_decl_id in referenced_return_declarations {
          if !refs.contains(&ret_decl_id) {
            refs.push(ret_decl_id);
          }
        }
        // Do NOT recurse into expression or arguments for regular functions
      }
    }

    // TypeConversion: recurse into the argument being converted
    ASTNode::TypeConversion { argument, .. } => {
      collect_variable_refs_recursive(argument, refs);
    }

    // For other nodes, recurse into children
    _ => {
      for child in node.nodes() {
        collect_variable_refs_recursive(child, refs);
      }
    }
  }
}

/// Collects variable references from index expressions in the LHS of an assignment.
/// For example, in `myMap[key1][key2] = val`, this collects key1 and key2.
fn collect_index_refs_from_lhs(lhs: &ASTNode, refs: &mut Vec<i32>) {
  match lhs {
    ASTNode::IndexAccess {
      base_expression,
      index_expression,
      ..
    } => {
      // Collect refs from the index expression
      if let Some(index_expr) = index_expression {
        let index_refs = collect_variable_refs_from_expression(index_expr);
        for ref_id in index_refs {
          if !refs.contains(&ref_id) {
            refs.push(ref_id);
          }
        }
      }
      // Recurse into base expression for nested index access
      collect_index_refs_from_lhs(base_expression, refs);
    }
    ASTNode::MemberAccess { expression, .. } => {
      // For member access like `obj.field[key] = val`, recurse into expression
      collect_index_refs_from_lhs(expression, refs);
    }
    _ => {}
  }
}

/// Adds ancestor variable IDs to the ancestors map for a target variable.
fn add_ancestors(
  ancestors_map: &mut AncestorsMap,
  target_id: i32,
  ancestor_ids: &[i32],
) {
  if ancestor_ids.is_empty() {
    return;
  }

  let entry = ancestors_map.entry(target_id).or_default();
  for &ancestor_id in ancestor_ids {
    // Don't add self-references
    if ancestor_id != target_id && !entry.contains(&ancestor_id) {
      entry.push(ancestor_id);
    }
  }
}

/// Adds relative variable IDs to the relatives map for a target variable (unidirectional).
/// Used for assignment contexts where RHS variables become relatives of the LHS variable,
/// but not vice versa.
fn add_relatives_unidirectional(
  relatives_map: &mut RelativesMap,
  target_id: i32,
  relative_ids: &[i32],
) {
  if relative_ids.is_empty() {
    return;
  }

  let entry = relatives_map.entry(target_id).or_default();
  for &relative_id in relative_ids {
    // Don't add self-references
    if relative_id != target_id && !entry.contains(&relative_id) {
      entry.push(relative_id);
    }
  }
}

/// Adds relative relationships bidirectionally between two sets of variable IDs.
/// Each variable in `left_ids` becomes a relative of each variable in `right_ids` and vice versa.
fn add_relatives_bidirectional(
  relatives_map: &mut RelativesMap,
  left_ids: &[i32],
  right_ids: &[i32],
) {
  // Add right IDs as relatives of each left ID
  for &left_id in left_ids {
    let entry = relatives_map.entry(left_id).or_default();
    for &right_id in right_ids {
      if right_id != left_id && !entry.contains(&right_id) {
        entry.push(right_id);
      }
    }
  }

  // Add left IDs as relatives of each right ID
  for &right_id in right_ids {
    let entry = relatives_map.entry(right_id).or_default();
    for &left_id in left_ids {
      if left_id != right_id && !entry.contains(&left_id) {
        entry.push(left_id);
      }
    }
  }
}

/// Recursively collects all ancestors and descendants for a given node.
/// Returns a set of unique node IDs (excluding the starting node itself).
/// Uses a visited set to avoid infinite recursion from circular dependencies.
/// Result of collecting recursive ancestry, with ancestors, descendants, and relatives tracked separately
struct RecursiveAncestry {
  ancestors: HashSet<i32>,
  descendants: HashSet<i32>,
  relatives: HashSet<i32>,
}

fn collect_recursive_ancestry(
  start_node_id: i32,
  ancestors_map: &AncestorsMap,
  descendants_map: &DescendantsMap,
  relatives_map: &RelativesMap,
) -> RecursiveAncestry {
  let mut ancestors = HashSet::new();
  let mut descendants = HashSet::new();
  let mut relatives = HashSet::new();
  let mut visited = HashSet::new();

  // Collect recursive ancestors
  collect_ancestry_in_direction(
    start_node_id,
    ancestors_map,
    &mut ancestors,
    &mut visited,
  );

  // Reset visited for descendants traversal
  visited.clear();

  // Collect recursive descendants
  collect_ancestry_in_direction(
    start_node_id,
    descendants_map,
    &mut descendants,
    &mut visited,
  );

  // Reset visited for relatives traversal
  visited.clear();

  // Collect recursive relatives
  collect_ancestry_in_direction(
    start_node_id,
    relatives_map,
    &mut relatives,
    &mut visited,
  );

  // Remove self if present
  ancestors.remove(&start_node_id);
  descendants.remove(&start_node_id);
  relatives.remove(&start_node_id);

  RecursiveAncestry {
    ancestors,
    descendants,
    relatives,
  }
}

/// Helper function to recursively collect related nodes in one direction
/// (either ancestors or descendants).
fn collect_ancestry_in_direction(
  node_id: i32,
  direction_map: &BTreeMap<i32, Vec<i32>>,
  result: &mut HashSet<i32>,
  visited: &mut HashSet<i32>,
) {
  // Avoid infinite recursion from cycles
  if visited.contains(&node_id) {
    return;
  }
  visited.insert(node_id);

  if let Some(related_ids) = direction_map.get(&node_id) {
    for &related_id in related_ids {
      result.insert(related_id);
      // Recurse to get transitive relationships
      collect_ancestry_in_direction(related_id, direction_map, result, visited);
    }
  }
}

/// Populates the context field for all TopicMetadata entries.
/// For NamedTopics: builds context from declaration references + self-reference.
/// For non-named topics: builds context from just the self-reference in the scope hierarchy.
/// Must be called after all TopicMetadata entries have been created,
/// so that the scope_map is complete and control flow chains can be
/// extracted for every reference_node.
fn populate_context(
  topic_metadata: &mut BTreeMap<topic::Topic, TopicMetadata>,
  topic_context: &mut BTreeMap<topic::Topic, Vec<SourceContext>>,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
  nodes: &BTreeMap<topic::Topic, Node>,
  in_scope_files: &HashSet<domain::ProjectPath>,
) {
  // Build scope_map from completed topic_metadata
  let scope_map: BTreeMap<i32, Scope> = topic_metadata
    .iter()
    .filter_map(|(topic, metadata)| {
      let node_id = topic.numeric_id();
      Some((node_id, metadata.scope().clone()))
    })
    .collect();

  // Build reference groups for each declaration
  let refs_map: BTreeMap<topic::Topic, Vec<SourceContext>> =
    in_scope_source_topics
      .iter()
      .filter_map(|(&node_id, decl)| {
        let topic = topic::new_node_topic(&node_id);
        // Only process declarations that have topic_metadata (i.e. are in scope)
        let scope = scope_map.get(&node_id)?;
        let self_reference = scope_to_self_reference(scope, node_id);
        let reference_groups = build_source_context(
          decl.references(),
          self_reference,
          &scope_map,
          nodes,
          in_scope_source_topics,
          in_scope_files,
        );
        Some((topic, reference_groups))
      })
      .collect();

  // Update each named topic with context
  for (topic, metadata) in topic_metadata.iter() {
    if let Some(refs) = refs_map.get(topic)
      && matches!(metadata, TopicMetadata::NamedTopic { .. })
    {
      topic_context.insert(*topic, refs.clone());
    }
  }

  // Build context for non-named topics (just the self-reference in scope hierarchy)
  for (_topic, metadata) in topic_metadata.iter() {
    match metadata {
      TopicMetadata::NamedTopic { .. } => {} // Already populated above
      TopicMetadata::UnnamedTopic { topic, scope, .. }
      | TopicMetadata::ControlFlow { topic, scope, .. }
      | TopicMetadata::TitledTopic { topic, scope, .. }
      | TopicMetadata::CommentTopic { topic, scope, .. } => {
        let node_id = topic.numeric_id();
        if let Some(self_ref) = scope_to_self_reference(scope, node_id) {
          let context = build_source_context(
            &[],
            Some(self_ref),
            &scope_map,
            nodes,
            in_scope_source_topics,
            in_scope_files,
          );
          topic_context.insert(*topic, context);
        }
      }
      TopicMetadata::FeatureTopic { .. }
      | TopicMetadata::RequirementTopic { .. }
      | TopicMetadata::BehaviorTopic { .. }
      | TopicMetadata::FunctionalSemanticTopic { .. }
      | TopicMetadata::ThreatTopic { .. }
      | TopicMetadata::InvariantTopic { .. }
      | TopicMetadata::DocumentationTopic { .. } => {}
    }
  }
}

/// Populates the expanded_context field for all TopicMetadata entries.
/// This must be called after all TopicMetadata entries have been created,
/// as it needs access to the scopes of all ancestry-related topics.
///
/// This was written by Claude Code and I think it is pretty bloated, but
/// it works.
#[allow(clippy::too_many_arguments)]
fn populate_expanded_context(
  topic_metadata: &BTreeMap<topic::Topic, TopicMetadata>,
  expanded_topic_context: &mut BTreeMap<topic::Topic, Vec<SourceContext>>,
  ancestors_map: &AncestorsMap,
  descendants_map: &DescendantsMap,
  relatives_map: &RelativesMap,
  nodes: &BTreeMap<topic::Topic, Node>,
  in_scope_source_topics: &BTreeMap<i32, InScopeDeclaration>,
  in_scope_files: &HashSet<domain::ProjectPath>,
) {
  // First, collect all scopes from existing topic_metadata
  let scope_map: BTreeMap<i32, Scope> = topic_metadata
    .iter()
    .filter_map(|(topic, metadata)| {
      let node_id = topic.numeric_id();
      Some((node_id, metadata.scope().clone()))
    })
    .collect();

  // Collect expanded references for each topic before mutating
  let expanded_refs_map: BTreeMap<topic::Topic, Vec<SourceContext>> =
    topic_metadata
      .iter()
      .filter_map(|(topic, _metadata)| {
        let node_id = topic.numeric_id();

        // Get recursive ancestry (ancestors, descendants, and relatives tracked separately)
        let ancestry = collect_recursive_ancestry(
          node_id,
          ancestors_map,
          descendants_map,
          relatives_map,
        );

        // Build ScopedReferences from ancestry:
        // 1. The declaration itself (self-reference) for each ancestor/descendant/relative
        // 2. All references to each ancestor/descendant/relative
        // Use a set to track seen reference_node IDs for deduplication
        let mut seen_refs: HashSet<i32> = HashSet::new();
        let mut scoped_refs: Vec<ScopedReference> = Vec::new();

        // Process all ancestors, descendants, and relatives
        let all_related: HashSet<i32> = ancestry
          .ancestors
          .iter()
          .chain(ancestry.descendants.iter())
          .chain(ancestry.relatives.iter())
          .copied()
          .collect();

        // Collect self-references and track which containing blocks contain declarations
        let mut pending_self_refs: Vec<(ScopedReference, Option<i32>)> =
          Vec::new(); // (self_ref, containing_block_id)

        // Map from declaration node_id to its containing block node_id
        let mut declaration_to_containing_block: BTreeMap<i32, i32> =
          BTreeMap::new();

        for &rel_id in &all_related {
          if let Some(scope) = scope_map.get(&rel_id)
            && let Some(self_ref) = scope_to_self_reference(scope, rel_id)
          {
            // Check if this declaration is inside a containing block
            let containing_block_id = match scope {
              Scope::ContainingBlock {
                containing_blocks, ..
              } => containing_blocks
                .last()
                .map(|layer| layer.block.numeric_id()),
              _ => None,
            };

            // Track the mapping from declaration to its containing block
            if let Some(block_id) = containing_block_id {
              declaration_to_containing_block.insert(rel_id, block_id);
            }

            pending_self_refs.push((self_ref, containing_block_id));
          }
        }

        // Collect all references to ancestors/descendants first to know which
        // containing blocks will appear in the final output
        let mut all_reference_nodes: HashSet<i32> = HashSet::new();
        for &rel_id in &all_related {
          if let Some(in_scope_decl) = in_scope_source_topics.get(&rel_id) {
            for reference in in_scope_decl.references() {
              all_reference_nodes.insert(reference.reference_node);
            }
          }
        }

        // Add self-references, filtering out declarations whose containing
        // block will also appear (either as another self-reference or
        // as a reference_node from the references)
        for (self_ref, containing_block_id) in pending_self_refs {
          let should_include = match containing_block_id {
            Some(block_id) => {
              // Skip if the containing block is in our reference nodes
              // (meaning the containing block itself will be displayed)
              !all_reference_nodes.contains(&block_id)
            }
            // No containing block, always include
            None => true,
          };

          if should_include && seen_refs.insert(self_ref.reference_node) {
            scoped_refs.push(self_ref);
          }
        }

        // Add all references to each ancestor/descendant
        for &rel_id in &all_related {
          if let Some(in_scope_decl) = in_scope_source_topics.get(&rel_id) {
            for reference in in_scope_decl.references() {
              // Skip references whose reference_node is a declaration that's
              // inside a containing block that will also appear
              let dominated_by_containing_block =
                declaration_to_containing_block
                  .get(&reference.reference_node)
                  .is_some_and(|block_id| {
                    all_reference_nodes.contains(block_id)
                  });

              if !dominated_by_containing_block
                && seen_refs.insert(reference.reference_node)
              {
                scoped_refs.push(reference.clone());
              }
            }
          }
        }

        // Build reference groups with ancestry-aware sorting
        let expanded_refs = build_expanded_source_context(
          &scoped_refs,
          &ancestry.ancestors,
          &ancestry.descendants,
          &scope_map,
          &scope_map,
          nodes,
          in_scope_source_topics,
          in_scope_files,
        );

        Some((*topic, expanded_refs))
      })
      .collect();

  // Now insert expanded_context entries for NamedTopics
  for (topic, metadata) in topic_metadata.iter() {
    if !matches!(metadata, TopicMetadata::NamedTopic { .. }) {
      continue;
    }
    if let Some(expanded_refs) = expanded_refs_map.get(topic) {
      if expanded_refs.is_empty() {
        expanded_topic_context.remove(topic);
      } else {
        expanded_topic_context.insert(*topic, expanded_refs.clone());
      }
    }
  }
}

/// Filters the ancestors and relatives maps to only include in-scope variables and derives descendants.
/// Returns the filtered ancestors map, derived descendants map, and filtered relatives map.
fn filter_and_derive_descendants(
  ancestors_map: &AncestorsMap,
  relatives_map: &RelativesMap,
  in_scope_declarations: &BTreeMap<i32, InScopeDeclaration>,
) -> (AncestorsMap, DescendantsMap, RelativesMap) {
  let mut filtered_ancestors: AncestorsMap = BTreeMap::new();
  let mut descendants: DescendantsMap = BTreeMap::new();
  let mut filtered_relatives: RelativesMap = BTreeMap::new();

  // Filter ancestors to only include in-scope variables
  for (&var_id, ancestor_ids) in ancestors_map {
    // Only include if the target variable is in scope
    if !in_scope_declarations.contains_key(&var_id) {
      continue;
    }

    // Filter ancestor IDs to only in-scope variables
    let filtered_ancestor_ids: Vec<i32> = ancestor_ids
      .iter()
      .filter(|&&aid| in_scope_declarations.contains_key(&aid))
      .copied()
      .collect();

    if !filtered_ancestor_ids.is_empty() {
      filtered_ancestors.insert(var_id, filtered_ancestor_ids.clone());

      // Build descendants: for each ancestor, this variable is a descendant
      for ancestor_id in filtered_ancestor_ids {
        descendants.entry(ancestor_id).or_default().push(var_id);
      }
    }
  }

  // Filter relatives to only include in-scope variables
  for (&var_id, relative_ids) in relatives_map {
    // Only include if the target variable is in scope
    if !in_scope_declarations.contains_key(&var_id) {
      continue;
    }

    // Filter relative IDs to only in-scope variables
    let filtered_relative_ids: Vec<i32> = relative_ids
      .iter()
      .filter(|&&rid| in_scope_declarations.contains_key(&rid))
      .copied()
      .collect();

    if !filtered_relative_ids.is_empty() {
      filtered_relatives.insert(var_id, filtered_relative_ids);
    }
  }

  (filtered_ancestors, descendants, filtered_relatives)
}

// ============================================================================
// Developer Documentation Injection
// ============================================================================

/// Collected documentation from a signature node (contract, function, modifier).
struct SignatureDoc {
  signature_topic: topic::Topic,
  doc_text: String,
  param_map: HashMap<String, topic::Topic>,
  return_params: Vec<(String, topic::Topic)>,
}

/// A resolved developer doc ready to become a synthetic CommentTopic.
struct ResolvedDoc {
  target_topic: topic::Topic,
  text: String,
  comment_type: CommentType,
  author: models::Author,
}

/// Walk all in-memory nodes to find developer documentation and create
/// synthetic CommentTopics for each. Runs after the name_index is built so
/// that code references in the developer's prose can be resolved.
///
/// Public so the top-level analysis pipeline can sequence it after the
/// resolution-graph build but before the documentation analyzer.
pub fn inject_developer_documentation(audit_data: &mut AuditData) {
  // ── Phase 1: Collect raw documentation ──────────────────────────────────

  let mut semantic_block_docs: Vec<(topic::Topic, String)> = Vec::new();
  let mut signature_docs: Vec<SignatureDoc> = Vec::new();

  for (node_topic, node) in &audit_data.nodes {
    let Node::Solidity(ast_node) = node else {
      continue;
    };

    match ast_node {
      // SemanticBlock inline comments (// and /* */)
      ASTNode::SemanticBlock {
        documentation: Some(doc),
        ..
      } => {
        if !doc.trim().is_empty() {
          semantic_block_docs.push((*node_topic, doc.clone()));
        }
      }

      // ContractMemberGroup inline comments (// and /* */) on top-level
      // contract members (state variables, functions, etc.)
      ASTNode::ContractMemberGroup {
        documentation: Some(doc),
        ..
      } => {
        if !doc.trim().is_empty() {
          semantic_block_docs.push((*node_topic, doc.clone()));
        }
      }

      // Function NatSpec docstrings
      ASTNode::FunctionSignature {
        documentation,
        parameters,
        return_parameters,
        ..
      } => {
        if let Some(sig_doc) =
          extract_signature_doc(node_topic, documentation, &audit_data.nodes)
        {
          let param_map =
            build_param_map(parameters.as_ref(), &audit_data.nodes);
          let return_params =
            build_return_map(return_parameters.as_ref(), &audit_data.nodes);
          signature_docs.push(SignatureDoc {
            signature_topic: sig_doc.0,
            doc_text: sig_doc.1,
            param_map,
            return_params,
          });
        }
      }

      // Modifier NatSpec docstrings
      ASTNode::ModifierSignature {
        documentation,
        parameters,
        ..
      } => {
        if let Some(sig_doc) =
          extract_signature_doc(node_topic, documentation, &audit_data.nodes)
        {
          let param_map =
            build_param_map(parameters.as_ref(), &audit_data.nodes);
          signature_docs.push(SignatureDoc {
            signature_topic: sig_doc.0,
            doc_text: sig_doc.1,
            param_map,
            return_params: Vec::new(),
          });
        }
      }

      // Contract NatSpec docstrings
      ASTNode::ContractSignature { documentation, .. } => {
        if let Some(sig_doc) =
          extract_signature_doc(node_topic, documentation, &audit_data.nodes)
        {
          signature_docs.push(SignatureDoc {
            signature_topic: sig_doc.0,
            doc_text: sig_doc.1,
            param_map: HashMap::new(),
            return_params: Vec::new(),
          });
        }
      }

      _ => {}
    }
  }

  // ── Phase 2: Create synthetic CommentTopics ─────────────────────────────

  // SemanticBlock inline comments → one DevTechnical each
  for (target_topic, doc_text) in semantic_block_docs {
    // Resolve through transitive chain so comments land on the canonical
    // topic. A SemanticBlock with one child statement is transitive to
    // that child, and comments should appear on whichever topic the user
    // actually views.
    let resolved_topic = domain::resolve_transitive_topic(
      &target_topic,
      &audit_data.topic_metadata,
    );
    create_synthetic_dev_comment(
      &resolved_topic,
      &doc_text,
      CommentType::DevTechnical,
      models::Author::DevTechnical,
      audit_data,
    );
  }

  // Signature NatSpec → parsed into tagged sections and resolved
  for sig_doc in signature_docs {
    // Resolve through transitive chain so comments land on the canonical
    // definition topic (e.g., FunctionDefinition) rather than the signature
    // topic (e.g., FunctionSignature). Users view the definition, and the
    // transitive chain only goes signature → definition, not the reverse.
    let resolved_topic = domain::resolve_transitive_topic(
      &sig_doc.signature_topic,
      &audit_data.topic_metadata,
    );
    let sections = parser::parse_natspec(&sig_doc.doc_text);
    let resolved = resolve_natspec(
      &sections,
      &resolved_topic,
      &sig_doc.param_map,
      &sig_doc.return_params,
    );
    for doc in resolved {
      if !doc.text.is_empty() {
        create_synthetic_dev_comment(
          &doc.target_topic,
          &doc.text,
          doc.comment_type,
          doc.author,
          audit_data,
        );
      }
    }
  }
}

// ============================================================================
// Signature Documentation Extraction
// ============================================================================

/// Extract the documentation text from a signature's StructuredDocumentation.
/// Returns Some((signature_topic, doc_text)) if documentation is present and
/// non-empty, None otherwise. Resolves stubs to get the full text.
fn extract_signature_doc(
  signature_topic: &topic::Topic,
  documentation: &Option<Box<ASTNode>>,
  nodes_map: &BTreeMap<topic::Topic, Node>,
) -> Option<(topic::Topic, String)> {
  let doc_node = documentation.as_ref()?;
  let resolved = doc_node.resolve(nodes_map);
  let ASTNode::StructuredDocumentation { text, .. } = resolved else {
    return None;
  };
  if text.trim().is_empty() {
    return None;
  }
  Some((*signature_topic, text.clone()))
}

/// Build a map of parameter name → topic from a ParameterList node.
/// Resolves stubs to get the VariableDeclaration names.
fn build_param_map(
  param_list_node: &ASTNode,
  nodes_map: &BTreeMap<topic::Topic, Node>,
) -> HashMap<String, topic::Topic> {
  let mut map = HashMap::new();
  let resolved = param_list_node.resolve(nodes_map);
  let ASTNode::ParameterList { parameters, .. } = resolved else {
    return map;
  };
  for param in parameters {
    let resolved_param = param.resolve(nodes_map);
    if let ASTNode::VariableDeclaration { node_id, name, .. } = resolved_param
      && !name.is_empty()
    {
      map.insert(name.clone(), topic::new_node_topic(node_id));
    }
  }
  map
}

/// Build a list of (name, topic) for return parameters from a ParameterList
/// node. Return params may be unnamed (empty string).
fn build_return_map(
  return_param_list_node: &ASTNode,
  nodes_map: &BTreeMap<topic::Topic, Node>,
) -> Vec<(String, topic::Topic)> {
  let mut list = Vec::new();
  let resolved = return_param_list_node.resolve(nodes_map);
  let ASTNode::ParameterList { parameters, .. } = resolved else {
    return list;
  };
  for param in parameters {
    let resolved_param = param.resolve(nodes_map);
    if let ASTNode::VariableDeclaration { node_id, name, .. } = resolved_param {
      list.push((name.clone(), topic::new_node_topic(node_id)));
    }
  }
  list
}

// ============================================================================
// NatSpec Resolution
// ============================================================================

/// Resolve parsed NatSpec sections into concrete (target, text, type, author)
/// groups. Sections targeting the same topic with the same type are combined
/// to minimize the total number of synthetic comments.
fn resolve_natspec(
  sections: &[NatSpecSection],
  signature_topic: &topic::Topic,
  param_map: &HashMap<String, topic::Topic>,
  return_params: &[(String, topic::Topic)],
) -> Vec<ResolvedDoc> {
  let mut notice_parts: Vec<String> = Vec::new();
  let mut dev_parts: Vec<String> = Vec::new();
  // param name → combined text parts
  let mut param_docs: HashMap<String, Vec<String>> = HashMap::new();
  // return param topic → combined text parts
  let mut return_docs: HashMap<topic::Topic, Vec<String>> = HashMap::new();

  for section in sections {
    match &section.tag {
      NatSpecTag::Notice => {
        if !section.text.is_empty() {
          notice_parts.push(section.text.clone());
        }
      }
      NatSpecTag::Dev | NatSpecTag::Untagged => {
        if !section.text.is_empty() {
          dev_parts.push(section.text.clone());
        }
      }
      NatSpecTag::Param(name) => {
        if param_map.contains_key(name) {
          param_docs
            .entry(name.clone())
            .or_default()
            .push(section.text.clone());
        } else {
          // Failed resolve — fall back to signature as DevTechnical
          dev_parts.push(
            format!("@param {} {}", name, section.text)
              .trim()
              .to_string(),
          );
        }
      }
      NatSpecTag::Return => {
        if let Some((_name, desc, topic)) =
          resolve_return_target(&section.text, return_params)
        {
          return_docs.entry(topic).or_default().push(desc.to_string());
        } else {
          // Failed resolve — fall back to signature as DevTechnical
          dev_parts
            .push(format!("@return {}", section.text).trim().to_string());
        }
      }
      NatSpecTag::Ignored => {
        // Deferred tag (@title, @author, @inheritdoc) — already excluded
        // from sections by parse_natspec, but handle defensively.
      }
    }
  }

  let mut result = Vec::new();

  // @notice → DevDocumentation on signature
  if !notice_parts.is_empty() {
    result.push(ResolvedDoc {
      target_topic: *signature_topic,
      text: notice_parts.join("\n"),
      comment_type: CommentType::DevDocumentation,
      author: models::Author::DevDocumentation,
    });
  }

  // @dev + untagged + failed resolves → DevTechnical on signature
  if !dev_parts.is_empty() {
    result.push(ResolvedDoc {
      target_topic: *signature_topic,
      text: dev_parts.join("\n"),
      comment_type: CommentType::DevTechnical,
      author: models::Author::DevTechnical,
    });
  }

  // Resolved @param → DevDocumentation on parameter topic
  for (name, texts) in &param_docs {
    if let Some(param_topic) = param_map.get(name) {
      result.push(ResolvedDoc {
        target_topic: *param_topic,
        text: texts.join("\n"),
        comment_type: CommentType::DevDocumentation,
        author: models::Author::DevDocumentation,
      });
    }
  }

  // Resolved @return → DevDocumentation on return param topic
  for (topic, texts) in return_docs {
    result.push(ResolvedDoc {
      target_topic: topic,
      text: texts.join("\n"),
      comment_type: CommentType::DevDocumentation,
      author: models::Author::DevDocumentation,
    });
  }

  result
}

/// Try to resolve @return text against the return parameter list.
/// Returns Some((param_name, description_text, param_topic)) if resolved.
/// Handles named returns (@return amount ...) and single unnamed returns.
fn resolve_return_target<'a>(
  text: &'a str,
  return_params: &[(String, topic::Topic)],
) -> Option<(String, &'a str, topic::Topic)> {
  if return_params.is_empty() {
    return None;
  }

  // Try to match first word against a return param name
  let first_word = text.split_whitespace().next().unwrap_or("");
  for (name, ret_topic) in return_params {
    if !name.is_empty() && name == first_word {
      let rest = text[first_word.len()..].trim_start();
      return Some((name.clone(), rest, *ret_topic));
    }
  }

  // No name match — if single return param, target it with full text
  if return_params.len() == 1 {
    return Some((return_params[0].0.clone(), text, return_params[0].1));
  }

  // Multiple unnamed returns, no name match — can't resolve
  None
}

#[cfg(test)]
mod tests {
  use super::*;
  use o11a_core::solidity::ast::SourceLocation;

  fn dummy_src_location() -> SourceLocation {
    SourceLocation {
      start: None,
      length: None,
      index: None,
    }
  }

  // =========================================================================
  // Type Extraction Tests
  // =========================================================================

  #[test]
  fn test_parse_elementary_type_uint256() {
    let result = parse_elementary_type_name("uint256");
    assert_eq!(result, Some(ElementaryType::Uint { bits: 256 }));
  }

  #[test]
  fn test_parse_elementary_type_uint_default() {
    // "uint" without size defaults to uint256
    let result = parse_elementary_type_name("uint");
    assert_eq!(result, Some(ElementaryType::Uint { bits: 256 }));
  }

  #[test]
  fn test_parse_elementary_type_uint8() {
    let result = parse_elementary_type_name("uint8");
    assert_eq!(result, Some(ElementaryType::Uint { bits: 8 }));
  }

  #[test]
  fn test_parse_elementary_type_int256() {
    let result = parse_elementary_type_name("int256");
    assert_eq!(result, Some(ElementaryType::Int { bits: 256 }));
  }

  #[test]
  fn test_parse_elementary_type_int_default() {
    // "int" without size defaults to int256
    let result = parse_elementary_type_name("int");
    assert_eq!(result, Some(ElementaryType::Int { bits: 256 }));
  }

  #[test]
  fn test_parse_elementary_type_int8() {
    let result = parse_elementary_type_name("int8");
    assert_eq!(result, Some(ElementaryType::Int { bits: 8 }));
  }

  #[test]
  fn test_parse_elementary_type_address() {
    let result = parse_elementary_type_name("address");
    assert_eq!(result, Some(ElementaryType::Address));
  }

  #[test]
  fn test_parse_elementary_type_address_payable() {
    let result = parse_elementary_type_name("address payable");
    assert_eq!(result, Some(ElementaryType::AddressPayable));
  }

  #[test]
  fn test_parse_elementary_type_bool() {
    let result = parse_elementary_type_name("bool");
    assert_eq!(result, Some(ElementaryType::Bool));
  }

  #[test]
  fn test_parse_elementary_type_string() {
    let result = parse_elementary_type_name("string");
    assert_eq!(result, Some(ElementaryType::String));
  }

  #[test]
  fn test_parse_elementary_type_bytes() {
    let result = parse_elementary_type_name("bytes");
    assert_eq!(result, Some(ElementaryType::Bytes));
  }

  #[test]
  fn test_parse_elementary_type_bytes32() {
    let result = parse_elementary_type_name("bytes32");
    assert_eq!(result, Some(ElementaryType::FixedBytes(32)));
  }

  #[test]
  fn test_parse_elementary_type_bytes1() {
    let result = parse_elementary_type_name("bytes1");
    assert_eq!(result, Some(ElementaryType::FixedBytes(1)));
  }

  #[test]
  fn test_extract_solidity_type_elementary() {
    let node = ASTNode::ElementaryTypeName {
      node_id: 1,
      src_location: dummy_src_location(),
      name: "uint256".to_string(),
    };

    let result = extract_solidity_type(&node);
    assert_eq!(
      result,
      Some(SolidityType::Elementary(ElementaryType::Uint { bits: 256 }))
    );
  }

  #[test]
  fn test_extract_solidity_type_array() {
    let base_type = ASTNode::ElementaryTypeName {
      node_id: 2,
      src_location: dummy_src_location(),
      name: "uint256".to_string(),
    };

    let node = ASTNode::ArrayTypeName {
      node_id: 1,
      src_location: dummy_src_location(),
      base_type: Box::new(base_type),
    };

    let result = extract_solidity_type(&node);
    assert_eq!(
      result,
      Some(SolidityType::Array {
        base_type: Box::new(SolidityType::Elementary(ElementaryType::Uint {
          bits: 256
        })),
        length: None,
      })
    );
  }

  #[test]
  fn test_extract_solidity_type_mapping() {
    let key_type = ASTNode::ElementaryTypeName {
      node_id: 2,
      src_location: dummy_src_location(),
      name: "address".to_string(),
    };

    let value_type = ASTNode::ElementaryTypeName {
      node_id: 3,
      src_location: dummy_src_location(),
      name: "uint256".to_string(),
    };

    let node = ASTNode::Mapping {
      node_id: 1,
      src_location: dummy_src_location(),
      key_name: None,
      key_name_location: dummy_src_location(),
      key_type: Box::new(key_type),
      value_name: None,
      value_name_location: dummy_src_location(),
      value_type: Box::new(value_type),
    };

    let result = extract_solidity_type(&node);
    assert_eq!(
      result,
      Some(SolidityType::Mapping {
        key_type: Box::new(SolidityType::Elementary(ElementaryType::Address)),
        value_type: Box::new(SolidityType::Elementary(ElementaryType::Uint {
          bits: 256
        })),
      })
    );
  }

  #[test]
  fn test_extract_solidity_type_user_defined() {
    let path_node = ASTNode::IdentifierPath {
      node_id: 2,
      src_location: dummy_src_location(),
      name: "MyStruct".to_string(),
      name_locations: vec![],
      referenced_declaration: 100,
    };

    let node = ASTNode::UserDefinedTypeName {
      node_id: 1,
      src_location: dummy_src_location(),
      referenced_declaration: 100,
      path_node: Box::new(path_node),
    };

    let result = extract_solidity_type(&node);
    assert_eq!(
      result,
      Some(SolidityType::UserDefined {
        declaration_topic: topic::new_node_topic(&100),
      })
    );
  }

  // =========================================================================
  // First Pass Revert Tests
  // =========================================================================

  #[test]
  fn test_first_pass_revert_require() {
    let revert = FirstPassRevert {
      statement_node: 100,
      kind: RevertConstraintKind::Require,
      error_node: None,
    };

    assert_eq!(revert.statement_node, 100);
    assert_eq!(revert.kind, RevertConstraintKind::Require);
    assert_eq!(revert.error_node, None);
  }

  #[test]
  fn test_first_pass_revert_revert_with_error() {
    let revert = FirstPassRevert {
      statement_node: 200,
      kind: RevertConstraintKind::Revert,
      error_node: Some(42),
    };

    assert_eq!(revert.statement_node, 200);
    assert_eq!(revert.kind, RevertConstraintKind::Revert);
    assert_eq!(revert.error_node, Some(42));
  }

  // =========================================================================
  // Emit / Revert Visitor Tests
  // =========================================================================

  fn make_identifier(node_id: i32, name: &str, ref_decl: i32) -> ASTNode {
    ASTNode::Identifier {
      node_id,
      src_location: dummy_src_location(),
      name: name.to_string(),
      overloaded_declarations: Vec::new(),
      referenced_declaration: ref_decl,
    }
  }

  fn make_identifier_path(node_id: i32, name: &str, ref_decl: i32) -> ASTNode {
    ASTNode::IdentifierPath {
      node_id,
      src_location: dummy_src_location(),
      name: name.to_string(),
      name_locations: Vec::new(),
      referenced_declaration: ref_decl,
    }
  }

  fn make_member_access(
    node_id: i32,
    base: ASTNode,
    member_name: &str,
    ref_decl: Option<i32>,
  ) -> ASTNode {
    ASTNode::MemberAccess {
      node_id,
      src_location: dummy_src_location(),
      expression: Box::new(base),
      member_location: dummy_src_location(),
      member_name: member_name.to_string(),
      referenced_declaration: ref_decl,
      type_descriptions: ast::TypeDescriptions {
        type_identifier: String::new(),
        type_string: String::new(),
      },
    }
  }

  fn make_function_call(node_id: i32, expression: ASTNode) -> ASTNode {
    ASTNode::FunctionCall {
      node_id,
      src_location: dummy_src_location(),
      arguments: Vec::new(),
      expression: Box::new(expression),
      name_locations: Vec::new(),
      names: Vec::new(),
      try_call: false,
      type_descriptions: ast::TypeDescriptions {
        type_identifier: String::new(),
        type_string: String::new(),
      },
      referenced_return_declarations: Vec::new(),
    }
  }

  fn make_function_call_with_args(
    node_id: i32,
    expression: ASTNode,
    arguments: Vec<ASTNode>,
  ) -> ASTNode {
    ASTNode::FunctionCall {
      node_id,
      src_location: dummy_src_location(),
      arguments,
      expression: Box::new(expression),
      name_locations: Vec::new(),
      names: Vec::new(),
      try_call: false,
      type_descriptions: ast::TypeDescriptions {
        type_identifier: String::new(),
        type_string: String::new(),
      },
      referenced_return_declarations: Vec::new(),
    }
  }

  fn make_emit(node_id: i32, expression: ASTNode) -> ASTNode {
    ASTNode::EmitStatement {
      node_id,
      src_location: dummy_src_location(),
      event_call: Box::new(make_function_call(node_id + 1, expression)),
    }
  }

  fn make_emit_with_args(
    node_id: i32,
    expression: ASTNode,
    arguments: Vec<ASTNode>,
  ) -> ASTNode {
    ASTNode::EmitStatement {
      node_id,
      src_location: dummy_src_location(),
      event_call: Box::new(make_function_call_with_args(
        node_id + 1,
        expression,
        arguments,
      )),
    }
  }

  fn make_revert(node_id: i32, expression: ASTNode) -> ASTNode {
    ASTNode::RevertStatement {
      node_id,
      src_location: dummy_src_location(),
      error_call: Box::new(make_function_call(node_id + 1, expression)),
    }
  }

  fn make_revert_with_args(
    node_id: i32,
    expression: ASTNode,
    arguments: Vec<ASTNode>,
  ) -> ASTNode {
    ASTNode::RevertStatement {
      node_id,
      src_location: dummy_src_location(),
      error_call: Box::new(make_function_call_with_args(
        node_id + 1,
        expression,
        arguments,
      )),
    }
  }

  fn make_block(node_id: i32, statements: Vec<ASTNode>) -> ASTNode {
    ASTNode::Block {
      node_id,
      src_location: dummy_src_location(),
      statements,
    }
  }

  fn make_if(
    node_id: i32,
    condition: ASTNode,
    true_body: ASTNode,
    false_body: Option<ASTNode>,
  ) -> ASTNode {
    ASTNode::IfStatement {
      node_id,
      src_location: dummy_src_location(),
      condition: Box::new(condition),
      true_body: Box::new(true_body),
      false_body: false_body.map(Box::new),
    }
  }

  /// Collected output of `collect_references_and_statements` for tests
  /// that need to inspect more than `reverts` + `events_emitted`.
  struct VisitorOutput {
    referenced_nodes: Vec<ReferencedNode>,
    reverts: Vec<FirstPassRevert>,
    function_calls: Vec<i32>,
    variable_mutations: Vec<ReferencedNode>,
    events_emitted: Vec<i32>,
  }

  fn run_visitor_full(
    node: &ASTNode,
    containing_block: Option<i32>,
  ) -> VisitorOutput {
    let mut out = VisitorOutput {
      referenced_nodes: Vec::new(),
      reverts: Vec::new(),
      function_calls: Vec::new(),
      variable_mutations: Vec::new(),
      events_emitted: Vec::new(),
    };
    collect_references_and_statements(
      node,
      containing_block,
      &mut out.referenced_nodes,
      &mut out.reverts,
      &mut out.function_calls,
      &mut out.variable_mutations,
      &mut out.events_emitted,
    );
    out
  }

  fn run_visitor(node: &ASTNode) -> (Vec<FirstPassRevert>, Vec<i32>) {
    let out = run_visitor_full(node, None);
    (out.reverts, out.events_emitted)
  }

  #[test]
  fn collect_events_emitted_records_emit_statements() {
    // emit Transfer(...) where Transfer is declared at node 42.
    let emit = make_emit(1, make_identifier(3, "Transfer", 42));
    let (_reverts, events) = run_visitor(&emit);
    assert_eq!(events, vec![42]);
  }

  #[test]
  fn collect_events_emitted_via_member_access() {
    // emit Lib.SomeEvent(...) — event referenced through MemberAccess.
    // The MemberAccess node carries `referenced_declaration: Some(N)`.
    let base = make_identifier(2, "Lib", 1000);
    let member = make_member_access(3, base, "SomeEvent", Some(77));
    let emit = make_emit(4, member);
    let (_reverts, events) = run_visitor(&emit);
    assert_eq!(events, vec![77]);
  }

  #[test]
  fn collect_events_emitted_via_identifier_path() {
    // IdentifierPath form (qualified name in NatSpec/inheritance contexts).
    let path = make_identifier_path(5, "OtherContract.Event", 88);
    let emit = make_emit(6, path);
    let (_reverts, events) = run_visitor(&emit);
    assert_eq!(events, vec![88]);
  }

  #[test]
  fn collect_events_emitted_via_unresolved_member_access_yields_none() {
    // MemberAccess.referenced_declaration is None when the compiler did
    // not resolve the call target (e.g., dynamic call). The visitor
    // skips the emit rather than recording a bogus node ID.
    let base = make_identifier(7, "x", 0);
    let member = make_member_access(8, base, "Pulse", None);
    let emit = make_emit(9, member);
    let (_reverts, events) = run_visitor(&emit);
    assert!(events.is_empty());
  }

  #[test]
  fn collect_events_emitted_multiple_in_nested_blocks() {
    // function body { emit A(); if (cond) { emit B(); } emit C(); }
    let emit_a = make_emit(10, make_identifier(11, "A", 1));
    let emit_b = make_emit(12, make_identifier(13, "B", 2));
    let emit_c = make_emit(14, make_identifier(15, "C", 3));
    let if_stmt = make_if(
      20,
      make_identifier(21, "cond", 99),
      make_block(22, vec![emit_b]),
      None,
    );
    let body = make_block(30, vec![emit_a, if_stmt, emit_c]);

    let (_reverts, events) = run_visitor(&body);
    // Walker visits in source order; sort/dedup happens in second_pass.
    assert_eq!(events, vec![1, 2, 3]);
  }

  #[test]
  fn collect_events_emitted_does_not_pollute_function_calls() {
    // The event topic lands in `events_emitted` only — never in
    // `function_calls`. Pre-fix, the inner FunctionCall walk would also
    // push the event identifier into `function_calls`, leaving
    // tree_shake to filter it out by `NamedTopicKind::Event`. We now
    // suppress the push at the source.
    let emit = make_emit(1, make_identifier(2, "Pinged", 5));
    let out = run_visitor_full(&emit, None);
    assert_eq!(out.events_emitted, vec![5]);
    assert!(
      out.function_calls.is_empty(),
      "event identifier must not appear in function_calls; got {:?}",
      out.function_calls
    );
  }

  #[test]
  fn collect_events_emitted_via_member_access_does_not_pollute_function_calls()
  {
    // `emit Lib.Transfer(...)` already avoided the pollution pre-fix
    // (the FunctionCall arm only pushes when expression is `Identifier`).
    // Lock the behavior in for the MemberAccess form so a future
    // refactor can't reintroduce it asymmetrically across the call
    // shapes.
    let base = make_identifier(2, "Lib", 1000);
    let member = make_member_access(3, base, "Transfer", Some(77));
    let emit = make_emit(4, member);
    let out = run_visitor_full(&emit, None);
    assert_eq!(out.events_emitted, vec![77]);
    assert!(out.function_calls.is_empty());
  }

  #[test]
  fn collect_revert_does_not_pollute_function_calls() {
    // Symmetric to `collect_events_emitted_does_not_pollute_function_calls`
    // for `revert MyError(...)`. The error topic is bookkept only in
    // `reverts.error_node`.
    let revert = make_revert(1, make_identifier(2, "MyError", 5));
    let out = run_visitor_full(&revert, None);
    assert_eq!(out.reverts.len(), 1);
    assert_eq!(out.reverts[0].error_node, Some(5));
    assert!(
      out.function_calls.is_empty(),
      "error identifier must not appear in function_calls; got {:?}",
      out.function_calls
    );
  }

  #[test]
  fn collect_emit_preserves_argument_references() {
    // The fix walks the inner FunctionCall manually instead of via the
    // generic child walk. Make sure that rewrite still records
    // references to argument identifiers — without them, callers of a
    // state variable that's only read inside an emit's argument list
    // would lose the reference edge.
    //
    // `containing_block = Some(1)` simulates being inside a function
    // body's scope, so the Identifier arm pushes references
    // (it only fires when containing_block is set).
    let emit = make_emit_with_args(
      2,
      make_identifier(7, "Transfer", 42),
      vec![
        make_identifier(4, "from", 100),
        make_identifier(5, "to", 200),
        make_identifier(6, "amount", 300),
      ],
    );
    let out = run_visitor_full(&emit, Some(1));

    assert_eq!(out.events_emitted, vec![42]);
    assert!(out.function_calls.is_empty());

    // Every argument identifier (and the event identifier itself, via
    // the expression walk) flowed into `referenced_nodes`.
    let arg_refs: Vec<i32> = out
      .referenced_nodes
      .iter()
      .filter(|r| r.statement_node == 1)
      .map(|r| r.referenced_node)
      .collect();
    assert!(arg_refs.contains(&100), "missing `from`: {:?}", arg_refs);
    assert!(arg_refs.contains(&200), "missing `to`: {:?}", arg_refs);
    assert!(arg_refs.contains(&300), "missing `amount`: {:?}", arg_refs);
    assert!(
      arg_refs.contains(&42),
      "event identifier should still be in referenced_nodes (only \
       function_calls is suppressed); got {:?}",
      arg_refs
    );
  }

  #[test]
  fn collect_revert_preserves_argument_references() {
    // Same argument-walk preservation contract for `revert MyError(arg)`.
    let revert = make_revert_with_args(
      2,
      make_identifier(5, "MyError", 42),
      vec![make_identifier(4, "code", 100)],
    );
    let out = run_visitor_full(&revert, Some(1));

    assert_eq!(out.reverts.len(), 1);
    assert_eq!(out.reverts[0].error_node, Some(42));
    assert!(out.function_calls.is_empty());

    let arg_refs: Vec<i32> = out
      .referenced_nodes
      .iter()
      .filter(|r| r.statement_node == 1)
      .map(|r| r.referenced_node)
      .collect();
    assert!(arg_refs.contains(&100), "missing `code`: {:?}", arg_refs);
    assert!(
      arg_refs.contains(&42),
      "error identifier should still be in referenced_nodes; got {:?}",
      arg_refs
    );
  }

  #[test]
  fn collect_emit_with_nested_assignment_in_args_records_mutation() {
    // Defensive: the helper that walks emit/revert arguments must
    // recurse fully — not just one level — so a mutation buried inside
    // an argument expression still lands in `variable_mutations`. This
    // would catch a future rewrite that walked arguments shallowly.
    let emit = make_emit_with_args(
      2,
      make_identifier(7, "Transfer", 42),
      vec![ASTNode::Assignment {
        node_id: 10,
        src_location: dummy_src_location(),
        left_hand_side: Box::new(make_identifier(11, "x", 99)),
        operator: ast::AssignmentOperator::Assign,
        right_hand_side: Box::new(make_identifier(12, "y", 100)),
      }],
    );
    let out = run_visitor_full(&emit, Some(1));

    assert_eq!(out.events_emitted, vec![42]);
    assert_eq!(out.variable_mutations.len(), 1);
    assert_eq!(out.variable_mutations[0].referenced_node, 99);
  }

  #[test]
  fn collect_interleaved_emit_revert_and_call_keeps_buckets_separate() {
    // function body {
    //   emit A();          // → events_emitted only
    //   revert B();        // → reverts only
    //   doWork();          // → function_calls only
    //   emit C();          // → events_emitted only
    // }
    // The custom EmitStatement / RevertStatement walk paths must not
    // bleed identifiers across buckets. Locks down the contract that
    // the new helper preserves.
    let body = make_block(
      1,
      vec![
        make_emit(10, make_identifier(11, "A", 100)),
        make_revert(20, make_identifier(21, "B", 200)),
        make_function_call(30, make_identifier(31, "doWork", 300)),
        make_emit(40, make_identifier(41, "C", 400)),
      ],
    );
    let out = run_visitor_full(&body, Some(1));

    assert_eq!(out.events_emitted, vec![100, 400]);
    assert_eq!(out.reverts.len(), 1);
    assert_eq!(out.reverts[0].error_node, Some(200));
    // Only `doWork` lands in function_calls — A, B, C are bucketed
    // elsewhere.
    assert_eq!(out.function_calls, vec![300]);
  }

  #[test]
  fn collect_emit_inside_revert_arguments_still_extracts_event() {
    // Pathological but legal-by-AST: an emit nested inside a revert's
    // argument list. The visitor must recurse into the revert's
    // arguments and still record the event in `events_emitted`.
    // (Solidity rejects this at compile time, but the visitor is a
    // pure AST walker — its contract is "walk completely," not "walk
    // only what compiles.")
    let revert = make_revert_with_args(
      10,
      make_identifier(11, "OuterError", 100),
      vec![make_emit(20, make_identifier(21, "Inner", 200))],
    );
    let out = run_visitor_full(&revert, Some(1));

    assert_eq!(out.events_emitted, vec![200]);
    assert_eq!(out.reverts.len(), 1);
    assert_eq!(out.reverts[0].error_node, Some(100));
    assert!(out.function_calls.is_empty());
  }

  #[test]
  fn collect_revert_with_custom_error_records_error_node() {
    // revert MyError(...) where MyError is declared at node 99.
    let revert = make_revert(10, make_identifier(12, "MyError", 99));
    let (reverts, _events) = run_visitor(&revert);

    assert_eq!(reverts.len(), 1);
    assert_eq!(reverts[0].kind, RevertConstraintKind::Revert);
    assert_eq!(reverts[0].error_node, Some(99));

    // RevertInfo (the second-pass conversion) carries the error topic.
    let revert_info = domain::RevertInfo {
      topic: topic::new_node_topic(&reverts[0].statement_node),
      kind: reverts[0].kind,
      error_topic: reverts[0].error_node.map(|n| topic::new_node_topic(&n)),
    };
    assert_eq!(revert_info.error_topic, Some(topic::new_node_topic(&99)));
  }

  #[test]
  fn collect_revert_via_member_access() {
    // revert C.MyError(...) — error referenced through MemberAccess.
    let base = make_identifier(2, "C", 100);
    let member = make_member_access(3, base, "MyError", Some(55));
    let revert = make_revert(4, member);
    let (reverts, _events) = run_visitor(&revert);
    assert_eq!(reverts.len(), 1);
    assert_eq!(reverts[0].error_node, Some(55));
  }

  #[test]
  fn collect_revert_via_identifier_path() {
    let path = make_identifier_path(5, "lib.MyError", 66);
    let revert = make_revert(6, path);
    let (reverts, _events) = run_visitor(&revert);
    assert_eq!(reverts.len(), 1);
    assert_eq!(reverts[0].error_node, Some(66));
  }

  #[test]
  fn collect_revert_unresolved_member_access_yields_no_error_topic() {
    let base = make_identifier(7, "x", 0);
    let member = make_member_access(8, base, "Boom", None);
    let revert = make_revert(9, member);
    let (reverts, _events) = run_visitor(&revert);
    assert_eq!(reverts.len(), 1);
    assert_eq!(reverts[0].error_node, None);
  }

  #[test]
  fn collect_require_with_string_has_no_error_topic() {
    // require(cond, "msg") — built-in require, no custom error.
    let require = make_function_call(20, make_identifier(21, "require", 0));
    let (reverts, _events) = run_visitor(&require);
    assert_eq!(reverts.len(), 1);
    assert_eq!(reverts[0].kind, RevertConstraintKind::Require);
    assert_eq!(reverts[0].error_node, None);
  }

  #[test]
  fn collect_revert_string_form_has_no_error_topic() {
    // revert("msg") — built-in revert taking a string, no custom error.
    let revert_call = make_function_call(30, make_identifier(31, "revert", 0));
    let (reverts, _events) = run_visitor(&revert_call);
    assert_eq!(reverts.len(), 1);
    assert_eq!(reverts[0].kind, RevertConstraintKind::Revert);
    assert_eq!(reverts[0].error_node, None);
  }

  #[test]
  fn collect_multiple_reverts_in_function_body_preserves_order() {
    let r1 = make_revert(40, make_identifier(41, "ErrA", 100));
    let r2 = make_revert(42, make_identifier(43, "ErrB", 200));
    let body = make_block(50, vec![r1, r2]);
    let (reverts, _events) = run_visitor(&body);
    assert_eq!(reverts.len(), 2);
    assert_eq!(reverts[0].error_node, Some(100));
    assert_eq!(reverts[1].error_node, Some(200));
  }

  // =========================================================================
  // Inheritance Collection Tests
  // =========================================================================

  fn test_contract(name: &str, base_node_ids: &[i32]) -> FirstPassDeclaration {
    FirstPassDeclaration::Contract {
      container_file: domain::ProjectPath {
        file_path: "test.sol".to_string(),
      },
      is_publicly_in_scope: true,
      declaration_kind: NamedTopicKind::Contract(
        domain::ContractKind::Contract,
      ),
      visibility: Visibility::Public,
      name: name.to_string(),
      base_contracts: base_node_ids
        .iter()
        .map(|id| ReferencedNode {
          statement_node: 0,
          referenced_node: *id,
        })
        .collect(),
      other_contracts: Vec::new(),
      public_members: Vec::new(),
      referenced_nodes: Vec::new(),
    }
  }

  #[test]
  fn collect_contract_inheritance_populates_bases_sorted() {
    let mut decls = BTreeMap::new();
    // contract A is C, B  — bases sorted ascending in output
    decls.insert(1, test_contract("A", &[3, 2]));
    decls.insert(2, test_contract("B", &[]));
    decls.insert(3, test_contract("C", &[]));

    let mut inheritance = BTreeMap::new();
    collect_contract_inheritance(&decls, &mut inheritance);

    let topic_a = topic::new_node_topic(&1);
    let topic_b = topic::new_node_topic(&2);
    let topic_c = topic::new_node_topic(&3);
    assert_eq!(inheritance.get(&topic_a), Some(&vec![topic_b, topic_c]));
    // Sparse: contracts with no bases are absent.
    assert_eq!(inheritance.get(&topic_b), None);
    assert_eq!(inheritance.get(&topic_c), None);
  }

  #[test]
  fn collect_contract_inheritance_skips_non_contract_decls() {
    let mut decls = BTreeMap::new();
    decls.insert(
      1,
      FirstPassDeclaration::Flat {
        parent_contract: None,
        declaration_kind: NamedTopicKind::Builtin,
        visibility: Visibility::Public,
        name: "x".to_string(),
      },
    );

    let mut inheritance = BTreeMap::new();
    collect_contract_inheritance(&decls, &mut inheritance);

    assert!(inheritance.is_empty());
  }

  #[test]
  fn collect_contract_inheritance_dedups_duplicate_bases() {
    // Defensive: if first_pass somehow records `is A, A` (e.g., due to
    // explicit + linearized ancestor entries), the output must dedup.
    let mut decls = BTreeMap::new();
    decls.insert(1, test_contract("A", &[2, 2]));
    decls.insert(2, test_contract("B", &[]));

    let mut inheritance = BTreeMap::new();
    collect_contract_inheritance(&decls, &mut inheritance);

    let topic_a = topic::new_node_topic(&1);
    let topic_b = topic::new_node_topic(&2);
    assert_eq!(inheritance.get(&topic_a), Some(&vec![topic_b]));
  }

  #[test]
  fn collect_contract_inheritance_handles_diamond() {
    // Diamond: D is B, C; B is A; C is A.
    let mut decls = BTreeMap::new();
    decls.insert(1, test_contract("A", &[]));
    decls.insert(2, test_contract("B", &[1]));
    decls.insert(3, test_contract("C", &[1]));
    decls.insert(4, test_contract("D", &[2, 3]));

    let mut inheritance = BTreeMap::new();
    collect_contract_inheritance(&decls, &mut inheritance);

    let topic_a = topic::new_node_topic(&1);
    let topic_b = topic::new_node_topic(&2);
    let topic_c = topic::new_node_topic(&3);
    let topic_d = topic::new_node_topic(&4);

    assert_eq!(inheritance.get(&topic_a), None);
    assert_eq!(inheritance.get(&topic_b), Some(&vec![topic_a]));
    assert_eq!(inheritance.get(&topic_c), Some(&vec![topic_a]));
    assert_eq!(inheritance.get(&topic_d), Some(&vec![topic_b, topic_c]));
  }

  #[test]
  fn collect_contract_inheritance_is_deterministic() {
    // Building the same map twice produces byte-identical results.
    let mut decls = BTreeMap::new();
    decls.insert(1, test_contract("Z", &[5, 3, 4]));
    decls.insert(2, test_contract("Y", &[]));

    let mut a = BTreeMap::new();
    let mut b = BTreeMap::new();
    collect_contract_inheritance(&decls, &mut a);
    collect_contract_inheritance(&decls, &mut b);
    assert_eq!(a, b);
  }

  // =========================================================================
  // Phase 0 Integration Test
  // =========================================================================
  //
  // Verifies that the second_pass conversion preserves the new fields end
  // to end: events_emitted is sorted+deduped, and revert error_topics
  // round-trip through the FirstPassRevert → RevertInfo conversion.

  #[test]
  fn second_pass_conversion_sorts_and_dedups_events_emitted() {
    // Walker emits in source order; second_pass sorts ascending and dedups.
    let body = make_block(
      1,
      vec![
        make_emit(2, make_identifier(3, "C", 30)),
        make_emit(4, make_identifier(5, "A", 10)),
        make_emit(6, make_identifier(7, "A", 10)), // duplicate
        make_emit(8, make_identifier(9, "B", 20)),
      ],
    );
    let (_reverts, events) = run_visitor(&body);
    assert_eq!(events, vec![30, 10, 10, 20]);

    // Apply the second_pass-side normalization.
    let mut topics: Vec<topic::Topic> =
      events.iter().map(|&n| topic::new_node_topic(&n)).collect();
    topics.sort();
    topics.dedup();
    assert_eq!(
      topics,
      vec![
        topic::new_node_topic(&10),
        topic::new_node_topic(&20),
        topic::new_node_topic(&30),
      ]
    );
  }

  #[test]
  fn second_pass_conversion_preserves_revert_error_topic() {
    let body = make_block(
      1,
      vec![
        make_revert(2, make_identifier(3, "ErrA", 100)),
        make_function_call(4, make_identifier(5, "require", 0)),
        make_revert(6, make_identifier(7, "ErrB", 200)),
      ],
    );
    let (reverts, _events) = run_visitor(&body);

    // Apply the same conversion second_pass uses.
    let infos: Vec<domain::RevertInfo> = reverts
      .iter()
      .map(|fp| domain::RevertInfo {
        topic: topic::new_node_topic(&fp.statement_node),
        kind: fp.kind,
        error_topic: fp.error_node.map(|n| topic::new_node_topic(&n)),
      })
      .collect();

    assert_eq!(infos.len(), 3);
    assert_eq!(infos[0].kind, RevertConstraintKind::Revert);
    assert_eq!(infos[0].error_topic, Some(topic::new_node_topic(&100)));
    assert_eq!(infos[1].kind, RevertConstraintKind::Require);
    assert_eq!(infos[1].error_topic, None);
    assert_eq!(infos[2].kind, RevertConstraintKind::Revert);
    assert_eq!(infos[2].error_topic, Some(topic::new_node_topic(&200)));
  }

  #[test]
  fn test_elementary_type_is_numeric() {
    assert!(ElementaryType::Uint { bits: 256 }.is_numeric());
    assert!(ElementaryType::Int { bits: 8 }.is_numeric());
    assert!(!ElementaryType::Address.is_numeric());
    assert!(!ElementaryType::Bool.is_numeric());
    assert!(!ElementaryType::String.is_numeric());
  }

  #[test]
  fn test_elementary_type_is_address() {
    assert!(ElementaryType::Address.is_address());
    assert!(ElementaryType::AddressPayable.is_address());
    assert!(!ElementaryType::Uint { bits: 256 }.is_address());
    assert!(!ElementaryType::Bool.is_address());
  }

  // =========================================================================
  // NatSpec Resolution Tests
  // =========================================================================

  fn sig_topic() -> topic::Topic {
    topic::new_node_topic(&100)
  }

  fn param_topic(name: &str, id: i32) -> (String, topic::Topic) {
    (name.to_string(), topic::new_node_topic(&id))
  }

  #[test]
  fn test_resolve_natspec_notice_only() {
    let sections = parser::parse_natspec("@notice Rescues tokens");
    let result = resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &[]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].comment_type, CommentType::DevDocumentation);
    assert_eq!(result[0].target_topic, sig_topic());
    assert_eq!(result[0].text, "Rescues tokens");
    assert_eq!(result[0].author, models::Author::DevDocumentation);
  }

  #[test]
  fn test_resolve_natspec_dev_only() {
    let sections = parser::parse_natspec("@dev Only admin");
    let result = resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &[]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].comment_type, CommentType::DevTechnical);
    assert_eq!(result[0].target_topic, sig_topic());
    assert_eq!(result[0].text, "Only admin");
    assert_eq!(result[0].author, models::Author::DevTechnical);
  }

  #[test]
  fn test_resolve_natspec_untagged_becomes_dev_technical() {
    let sections = parser::parse_natspec("This is untagged");
    let result = resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &[]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].comment_type, CommentType::DevTechnical);
    assert_eq!(result[0].text, "This is untagged");
  }

  #[test]
  fn test_resolve_natspec_param_resolved() {
    let sections = parser::parse_natspec("@param token Address of token");
    let token_topic = topic::new_node_topic(&200);
    let mut param_map = HashMap::new();
    param_map.insert("token".to_string(), token_topic.clone());

    let result = resolve_natspec(&sections, &sig_topic(), &param_map, &[]);
    // Should produce a DevDocumentation on the parameter topic
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].comment_type, CommentType::DevDocumentation);
    assert_eq!(result[0].target_topic, token_topic);
    assert_eq!(result[0].text, "Address of token");
  }

  #[test]
  fn test_resolve_natspec_param_unresolved_falls_back() {
    let sections = parser::parse_natspec("@param unknown some desc");
    let result = resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &[]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].comment_type, CommentType::DevTechnical);
    assert_eq!(result[0].target_topic, sig_topic());
    assert!(result[0].text.contains("@param unknown"));
  }

  #[test]
  fn test_resolve_natspec_return_named_match() {
    let sections = parser::parse_natspec("@return amount Amount rescued");
    let ret_topic = topic::new_node_topic(&300);
    let return_params = vec![param_topic("amount", 300)];

    let result =
      resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &return_params);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].comment_type, CommentType::DevDocumentation);
    assert_eq!(result[0].target_topic, ret_topic);
    assert_eq!(result[0].text, "Amount rescued");
  }

  #[test]
  fn test_resolve_natspec_return_single_unnamed() {
    let sections = parser::parse_natspec("@return the total amount");
    let ret_topic = topic::new_node_topic(&301);
    let return_params = vec![("".to_string(), ret_topic.clone())];

    let result =
      resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &return_params);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].comment_type, CommentType::DevDocumentation);
    assert_eq!(result[0].text, "the total amount");
  }

  #[test]
  fn test_resolve_natspec_return_multiple_unresolved_falls_back() {
    let sections = parser::parse_natspec("@return some value");
    let return_params = vec![
      ("".to_string(), topic::new_node_topic(&302)),
      ("".to_string(), topic::new_node_topic(&303)),
    ];

    let result =
      resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &return_params);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].comment_type, CommentType::DevTechnical);
    assert!(result[0].text.contains("@return"));
  }

  #[test]
  fn test_resolve_natspec_notice_and_dev_combined() {
    let doc = "@notice Does a thing\n@dev Only callable by admin";
    let sections = parser::parse_natspec(doc);
    let result = resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &[]);
    assert_eq!(result.len(), 2);
    // notice first
    assert_eq!(result[0].comment_type, CommentType::DevDocumentation);
    assert_eq!(result[0].text, "Does a thing");
    // dev second
    assert_eq!(result[1].comment_type, CommentType::DevTechnical);
    assert_eq!(result[1].text, "Only callable by admin");
  }

  #[test]
  fn test_resolve_natspec_multiple_notices_combined() {
    let doc = "@notice First part\n@notice Second part";
    let sections = parser::parse_natspec(doc);
    let result = resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &[]);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].text, "First part\nSecond part");
  }

  #[test]
  fn test_resolve_natspec_full_function() {
    let doc = "\
@notice Rescues tokens that were mistakenly sent
@param token Address of token to rescue
@dev Only callable by admin
@return amount Amount of tokens rescued";
    let sections = parser::parse_natspec(doc);
    let token_topic = topic::new_node_topic(&200);
    let amount_topic = topic::new_node_topic(&300);
    let mut param_map = HashMap::new();
    param_map.insert("token".to_string(), token_topic.clone());
    let return_params = vec![param_topic("amount", 300)];

    let result =
      resolve_natspec(&sections, &sig_topic(), &param_map, &return_params);

    // 4 results: notice(sig), dev(sig), param(token), return(amount)
    assert_eq!(result.len(), 4);
    assert_eq!(result[0].comment_type, CommentType::DevDocumentation);
    assert_eq!(result[0].target_topic, sig_topic());
    assert_eq!(result[1].comment_type, CommentType::DevTechnical);
    assert_eq!(result[1].target_topic, sig_topic());
    assert_eq!(result[2].comment_type, CommentType::DevDocumentation);
    assert_eq!(result[2].target_topic, token_topic);
    assert_eq!(result[3].comment_type, CommentType::DevDocumentation);
    assert_eq!(result[3].target_topic, amount_topic);
  }

  #[test]
  fn test_resolve_natspec_deferred_tags_ignored() {
    let doc = "@notice Hello\n@title MyContract\n@author Bob\n@dev World";
    let sections = parser::parse_natspec(doc);
    let result = resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &[]);
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].text, "Hello");
    assert_eq!(result[1].text, "World");
  }

  #[test]
  fn test_resolve_natspec_empty_sections_no_output() {
    let sections = parser::parse_natspec("");
    let result = resolve_natspec(&sections, &sig_topic(), &HashMap::new(), &[]);
    assert!(result.is_empty());
  }

  // =========================================================================
  // Return Target Resolution Tests
  // =========================================================================

  #[test]
  fn test_resolve_return_target_empty_params() {
    assert!(resolve_return_target("some text", &[]).is_none());
  }

  #[test]
  fn test_resolve_return_target_named_match() {
    let params = vec![param_topic("amount", 300)];
    let (name, desc, t) =
      resolve_return_target("amount the rescued", &params).unwrap();
    assert_eq!(name, "amount");
    assert_eq!(desc, "the rescued");
    assert_eq!(t, topic::new_node_topic(&300));
  }

  #[test]
  fn test_resolve_return_target_single_unnamed() {
    let params = vec![("".to_string(), topic::new_node_topic(&301))];
    let (name, desc, t) = resolve_return_target("some value", &params).unwrap();
    assert_eq!(name, "");
    assert_eq!(desc, "some value");
    assert_eq!(t, topic::new_node_topic(&301));
  }

  #[test]
  fn test_resolve_return_target_multiple_no_match() {
    let params = vec![
      ("".to_string(), topic::new_node_topic(&302)),
      ("".to_string(), topic::new_node_topic(&303)),
    ];
    assert!(resolve_return_target("some value", &params).is_none());
  }

  #[test]
  fn test_resolve_return_target_named_no_match_single_falls_back() {
    let params = vec![param_topic("amount", 300)];
    // "total" doesn't match "amount", but single return param auto-targets
    let (_, desc, _) =
      resolve_return_target("total the value", &params).unwrap();
    assert_eq!(desc, "total the value");
  }

  #[test]
  fn test_resolve_return_target_named_no_match_multiple() {
    let params = vec![param_topic("amount", 300), param_topic("total", 301)];
    // "foo" doesn't match either, and multiple params → can't resolve
    assert!(resolve_return_target("foo the value", &params).is_none());
  }

  #[test]
  fn test_resolve_return_target_named_match_name_only() {
    let params = vec![param_topic("amount", 300)];
    let (_, desc, _) = resolve_return_target("amount", &params).unwrap();
    assert_eq!(desc, "");
  }

  // =========================================================================
  // Developer Documentation Injection Tests
  // =========================================================================

  #[test]
  fn test_inject_contract_member_group_single_member_resolves_transitively() {
    // A ContractMemberGroup with exactly one member has `transitive_topic`
    // pointing at the member, so the group's inline comment should resolve
    // through and land in `comment_index[member_topic]`.
    let mut audit_data =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let member_id = 100;
    let group_id = -200;
    let member_topic = topic::new_node_topic(&member_id);
    let group_topic = topic::new_node_topic(&group_id);

    let member_node = ASTNode::Break {
      node_id: member_id,
      src_location: dummy_src_location(),
    };
    let group_node = ASTNode::ContractMemberGroup {
      node_id: group_id,
      src_location: dummy_src_location(),
      documentation: Some("Group docs for single member".to_string()),
      members: vec![member_node],
    };

    audit_data
      .nodes
      .insert(group_topic.clone(), Node::Solidity(group_node));

    audit_data.topic_metadata.insert(
      group_topic.clone(),
      TopicMetadata::UnnamedTopic {
        topic: group_topic.clone(),
        scope: Scope::Global,
        kind: UnnamedTopicKind::ContractMemberGroup,
        transitive_topic: Some(member_topic.clone()),
      },
    );

    inject_developer_documentation(&mut audit_data);

    let member_comments = audit_data
      .comment_index
      .get(&member_topic)
      .cloned()
      .unwrap_or_default();
    assert_eq!(
      member_comments.len(),
      1,
      "expected the comment to land on the transitive member topic"
    );
    assert!(
      audit_data
        .comment_index
        .get(&group_topic)
        .is_none_or(|v| v.is_empty()),
      "comment should not remain on the group topic when it resolves through"
    );

    let comment_topic = &member_comments[0];
    match audit_data.nodes.get(comment_topic) {
      Some(Node::Comment(comment_nodes)) => {
        let text = o11a_core::collaborator::parser::render_comment_plain_text(
          comment_nodes,
        );
        assert!(
          text.contains("Group docs for single member"),
          "comment text missing: {:?}",
          text
        );
      }
      _ => panic!("expected Node::Comment for the synthetic dev comment"),
    }
  }

  #[test]
  fn test_inject_contract_member_group_multi_member_stays_on_group() {
    // A multi-member ContractMemberGroup has no transitive topic, so the
    // inline comment stays on the group topic itself. Individual members
    // do not receive the comment.
    let mut audit_data =
      domain::new_audit_data("test".to_string(), HashSet::new(), None);

    let group_id = -300;
    let member_a_id = 101;
    let member_b_id = 102;
    let group_topic = topic::new_node_topic(&group_id);
    let member_a_topic = topic::new_node_topic(&member_a_id);
    let member_b_topic = topic::new_node_topic(&member_b_id);

    let group_node = ASTNode::ContractMemberGroup {
      node_id: group_id,
      src_location: dummy_src_location(),
      documentation: Some("Shared configuration".to_string()),
      members: vec![
        ASTNode::Break {
          node_id: member_a_id,
          src_location: dummy_src_location(),
        },
        ASTNode::Break {
          node_id: member_b_id,
          src_location: dummy_src_location(),
        },
      ],
    };

    audit_data
      .nodes
      .insert(group_topic.clone(), Node::Solidity(group_node));

    audit_data.topic_metadata.insert(
      group_topic.clone(),
      TopicMetadata::UnnamedTopic {
        topic: group_topic.clone(),
        scope: Scope::Global,
        kind: UnnamedTopicKind::ContractMemberGroup,
        transitive_topic: None,
      },
    );

    inject_developer_documentation(&mut audit_data);

    let group_comments = audit_data
      .comment_index
      .get(&group_topic)
      .cloned()
      .unwrap_or_default();
    assert_eq!(
      group_comments.len(),
      1,
      "expected the comment to land on the group topic"
    );
    assert!(
      !audit_data.comment_index.contains_key(&member_a_topic),
      "member A should not receive a duplicate of the group comment"
    );
    assert!(
      !audit_data.comment_index.contains_key(&member_b_topic),
      "member B should not receive a duplicate of the group comment"
    );
  }
}
