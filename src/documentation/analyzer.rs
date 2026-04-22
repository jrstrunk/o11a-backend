use crate::collaborator::models;
use crate::collaborator::parser as comment_parser;
use crate::core;
use crate::core::topic;
use crate::core::{
  CommentType,
  AST, AuditData, DataContext, Node, Scope, TitledTopicKind, TopicMetadata,
  UnnamedTopicKind, insert_into_context,
};
use crate::documentation::parser::{self, DocumentationAST, DocumentationNode};
use crate::solidity::parser::ASTNode;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicI32, Ordering};

/// Global counter for synthetic developer documentation comment IDs.
/// Uses negative IDs starting from -10 to avoid collision with
/// real DB comments (positive auto-increment). The topic prefix system
/// (C vs N) prevents collision with generated AST node IDs.
static NEXT_DEV_DOC_COMMENT_ID: AtomicI32 = AtomicI32::new(-10);

fn next_dev_doc_comment_id() -> i32 {
  NEXT_DEV_DOC_COMMENT_ID.fetch_sub(1, Ordering::SeqCst)
}

/// Analyzes documentation files and integrates them with the solidity DataContext
/// This MUST be called after solidity analysis completes, as it needs the solidity
/// declarations to resolve inline code references
pub fn analyze(
  project_root: &Path,
  audit_id: &str,
  data_context: &mut DataContext,
  document_files: &[core::DocumentFileEntry],
) -> Result<(), String> {
  // Get the audit data
  let audit_data = data_context
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit '{}' not found", audit_id))?;

  // Build name index for fast topic lookup during code token parsing
  audit_data.name_index = core::TopicNameIndex::build(&audit_data);

  // Build a set of technical document paths for root kind lookup
  let technical_paths: std::collections::HashSet<&core::ProjectPath> =
    document_files
      .iter()
      .filter(|e| e.is_technical)
      .map(|e| &e.project_path)
      .collect();

  // Extract just the project paths for the parser
  let paths: Vec<core::ProjectPath> = document_files
    .iter()
    .map(|e| e.project_path.clone())
    .collect();

  // Parse document files in the order specified by documents.txt
  let ast_map = parser::process_files(project_root, &paths, &audit_data)?;

  // Collect mentions during processing: referenced_topic -> [scope]
  // The scope tells us the container (file), component (section), and member (paragraph)
  let mut mentions_by_topic: BTreeMap<topic::Topic, Vec<Scope>> =
    BTreeMap::new();

  // Process each markdown file and add nodes/declarations to the audit data
  for (project_path, asts) in &ast_map {
    let is_technical = technical_paths.contains(project_path);
    for ast in asts {
      // Add to in_scope_files
      audit_data.in_scope_files.insert(project_path.clone());

      // Add to asts map with stubbed nodes
      let stubbed_ast = DocumentationAST {
        nodes: ast
          .nodes
          .iter()
          .map(|n| parser::children_to_stubs(n.clone()))
          .collect(),
        project_path: project_path.clone(),
        source_content: ast.source_content.clone(),
      };
      audit_data
        .asts
        .insert(project_path.clone(), AST::Documentation(stubbed_ast));

      process_documentation_ast(
        ast,
        project_path,
        is_technical,
        audit_data,
        &mut mentions_by_topic,
      )?;
    }
  }

  // Populate doc_references on each referenced NamedTopic with the most
  // specific doc topic (member if present, otherwise component) that
  // references it. Mirrors the cross-stage enrichment pattern used by
  // populate_ancestry in the solidity analyzer. Only NamedTopics participate
  // in name_index lookup, so referenced_topic can only be a NamedTopic here.
  for (referenced_topic, scopes) in mentions_by_topic {
    let Some(core::TopicMetadata::NamedTopic { doc_references, .. }) =
      audit_data.topic_metadata.get_mut(&referenced_topic)
    else {
      continue;
    };
    for scope in scopes {
      let mentioning_topic = match &scope {
        Scope::Member { member, .. }
        | Scope::ContainingBlock { member, .. } => member.clone(),
        Scope::Component { component, .. } => component.clone(),
        Scope::Global | Scope::Container { .. } => continue,
      };
      if !doc_references.contains(&mentioning_topic) {
        doc_references.push(mentioning_topic);
      }
    }
  }

  // Inject developer documentation from source code as synthetic in-memory
  // CommentTopics. These are derived from inline comments on SemanticBlocks
  // and will eventually include NatSpec docstrings on function/contract
  // declarations. They are rebuilt from source on every load — never persisted
  // to the comment database.
  inject_developer_documentation(audit_data);

  Ok(())
}

/// Walk all in-memory nodes to find SemanticBlocks with `documentation`
/// and create synthetic CommentTopics for each. This runs after the name_index
/// is built so that code references in the developer's prose can be resolved.
fn inject_developer_documentation(audit_data: &mut AuditData) {
  // Collect (target_node_topic, documentation_text) pairs first to avoid
  // borrowing audit_data mutably while iterating.
  let mut dev_docs: Vec<(topic::Topic, String)> = Vec::new();

  for (node_topic, node) in &audit_data.nodes {
    let Node::Solidity(ast_node) = node else {
      continue;
    };
    let ASTNode::SemanticBlock {
      documentation: Some(doc),
      ..
    } = ast_node
    else {
      continue;
    };
    if doc.trim().is_empty() {
      continue;
    }
    dev_docs.push((node_topic.clone(), doc.clone()));
  }

  // Now create synthetic CommentTopics for each documentation entry.
  for (target_topic, doc_text) in dev_docs {
    let comment_id = next_dev_doc_comment_id();
    let comment_topic = topic::new_comment_topic(comment_id);

    // Parse the documentation text through the comment parser to resolve
    // code references (mentions) in the developer's prose.
    let (mentions, comment_nodes) =
      comment_parser::parse_comment(&doc_text, audit_data);

    // Store the parsed AST (rendered on demand by render_source_text)
    audit_data
      .nodes
      .insert(comment_topic.clone(), Node::Comment(comment_nodes));

    // Deduplicate mentions
    let mut mentioned_topics = mentions.clone();
    mentioned_topics.sort_unstable();
    mentioned_topics.dedup();

    // Get the scope from the target topic's metadata
    let scope = audit_data
      .topic_metadata
      .get(&target_topic)
      .map(|m| m.scope().clone())
      .unwrap_or(Scope::Global);

    // Insert CommentTopic metadata
    audit_data.topic_metadata.insert(
      comment_topic.clone(),
      TopicMetadata::CommentTopic {
        topic: comment_topic.clone(),
        target_topic: target_topic.clone(),
        comment_type: CommentType::DevTechnical,
        author_id: models::AUTHOR_DEV_TECHNICAL,
        created_at: String::new(), // Synthetic — no real timestamp
        scope,
        mentioned_topics: mentioned_topics.clone(),
      },
    );

    // Update comment_index: target → [comment topics]
    let comments = audit_data
      .comment_index
      .entry(target_topic.clone())
      .or_default();
    comments.push(comment_topic.clone());

    // Update mentions_index: mentioned topic → [comment topics]
    for mention in &mentioned_topics {
      let entries = audit_data
        .mentions_index
        .entry(mention.clone())
        .or_default();
      entries.push(comment_topic.clone());
    }

    // HTML is rendered and cached lazily on first access via
    // topic_view::get_source_text.
  }
}

fn process_documentation_ast(
  ast: &DocumentationAST,
  project_path: &core::ProjectPath,
  is_technical: bool,
  audit_data: &mut AuditData,
  mentions_by_topic: &mut BTreeMap<topic::Topic, Vec<Scope>>,
) -> Result<(), String> {
  let scope = Scope::Container {
    container: project_path.clone(),
  };

  // Process all nodes in the AST
  for node in &ast.nodes {
    process_documentation_node(
      node,
      &scope,
      is_technical,
      audit_data,
      mentions_by_topic,
    )?;
  }

  Ok(())
}

fn process_documentation_node(
  node: &DocumentationNode,
  scope: &Scope,
  is_technical: bool,
  audit_data: &mut AuditData,
  mentions_by_topic: &mut BTreeMap<topic::Topic, Vec<Scope>>,
) -> Result<(), String> {
  let topic = topic::new_documentation_topic(node.node_id());

  match node {
    DocumentationNode::Root { children, .. } => {
      // Add the Root node first so build_self_context can look up its source location
      audit_data.nodes.insert(
        topic.clone(),
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic.clone(),
        TopicMetadata::DocumentationTopic {
          topic: topic.clone(),
          is_technical,
          scope: scope.clone(),
        },
      );
      audit_data.topic_context.insert(topic.clone(), context);

      // Process children with the same scope
      for child in children {
        process_documentation_node(
          child,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    // Heading: contains text content and optionally a Section child
    // The Heading itself is scoped at the current level, but its Section child
    // creates a nested scope for the section's content.
    DocumentationNode::Heading {
      children, section, ..
    } => {
      // Add the heading node first so build_self_context can look up its source location
      audit_data.nodes.insert(
        topic.clone(),
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic.clone(),
        TopicMetadata::UnnamedTopic {
          topic: topic.clone(),
          kind: UnnamedTopicKind::DocumentationHeading,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic.clone(), context);

      // Process heading text children (inline formatting nodes)
      for child in children {
        process_documentation_node(
          child,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }

      // Process the section child if present - it will create its own nested scope
      if let Some(sec) = section {
        process_documentation_node(
          sec,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    // Section: groups content under a heading. The section is a child of the Heading,
    // and creates a nested scope for its content.
    DocumentationNode::Section {
      title, children, ..
    } => {
      // Add the section node first so build_self_context can look up its source location
      audit_data.nodes.insert(
        topic.clone(),
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      // Build a self-referencing context so the topic panel shows this section's content
      let context = build_self_context(&topic, scope, &audit_data.nodes);

      // Add topic metadata for the section
      audit_data.topic_metadata.insert(
        topic.clone(),
        TopicMetadata::TitledTopic {
          topic: topic.clone(),
          scope: scope.clone(),
          kind: TitledTopicKind::DocumentationSection,
          title: title.clone(),
        },
      );
      audit_data.topic_context.insert(topic.clone(), context);

      // Create nested scope by adding this section to the scope hierarchy.
      // Sections are added regardless of heading level:
      // - First section becomes Component (Container -> Component)
      // - Second nested section becomes Member (Component -> Member)
      // - Third nested section becomes SemanticBlock (Member -> SemanticBlock)
      // - Further nesting stays at SemanticBlock level
      let section_scope = core::add_to_scope(scope, topic.clone());

      // Process section children with the nested scope
      for child in children {
        process_documentation_node(
          child,
          &section_scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    DocumentationNode::Paragraph { children, .. } => {
      audit_data.nodes.insert(
        topic.clone(),
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic.clone(),
        TopicMetadata::UnnamedTopic {
          topic: topic.clone(),
          kind: UnnamedTopicKind::DocumentationParagraph,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic.clone(), context);

      // Paragraphs don't add to scope - only sections/headers define scope hierarchy.
      // Process children with the same scope.
      for child in children {
        process_documentation_node(
          child,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    DocumentationNode::Sentence { children, .. } => {
      audit_data.nodes.insert(
        topic.clone(),
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic.clone(),
        TopicMetadata::UnnamedTopic {
          topic: topic.clone(),
          kind: UnnamedTopicKind::DocumentationSentence,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic.clone(), context);

      // Sentences don't create a new scope level - they stay within the
      // paragraph's semantic block scope. Process children with same scope.
      for child in children {
        process_documentation_node(
          child,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    DocumentationNode::CodeBlock { children, .. } => {
      audit_data.nodes.insert(
        topic.clone(),
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic.clone(),
        TopicMetadata::UnnamedTopic {
          topic: topic.clone(),
          kind: UnnamedTopicKind::DocumentationCodeBlock,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic.clone(), context);

      // Process children (code tokens) with the same scope
      for child in children {
        process_documentation_node(
          child,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    DocumentationNode::List { children, .. } => {
      audit_data.nodes.insert(
        topic.clone(),
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic.clone(),
        TopicMetadata::UnnamedTopic {
          topic: topic.clone(),
          kind: UnnamedTopicKind::DocumentationList,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic.clone(), context);

      // Process children with the same scope
      for child in children {
        process_documentation_node(
          child,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    DocumentationNode::BlockQuote { children, .. } => {
      audit_data.nodes.insert(
        topic.clone(),
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic.clone(),
        TopicMetadata::UnnamedTopic {
          topic: topic.clone(),
          kind: UnnamedTopicKind::DocumentationBlockQuote,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic.clone(), context);

      // Process children with the same scope
      for child in children {
        process_documentation_node(
          child,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    DocumentationNode::InlineCode { children, .. } => {
      audit_data.nodes.insert(
        topic.clone(),
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic.clone(),
        TopicMetadata::UnnamedTopic {
          topic: topic.clone(),
          kind: UnnamedTopicKind::DocumentationInlineCode,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic.clone(), context);

      // Process children (code tokens) with the same scope
      for child in children {
        process_documentation_node(
          child,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    // CodeIdentifier with a referenced_topic creates a mention
    DocumentationNode::CodeIdentifier {
      referenced_topic: Some(ref_topic),
      ..
    } => {
      audit_data
        .nodes
        .insert(topic, Node::Documentation(node.clone()));

      // Record the mention using the current scope
      // The scope tells us the containing document element (paragraph, section, or file)
      if !matches!(scope, Scope::Global) {
        mentions_by_topic
          .entry(ref_topic.clone())
          .or_default()
          .push(scope.clone());
      }
    }

    // For all other node types, just add them to the nodes map (no topic_metadata)
    DocumentationNode::Text { .. }
    | DocumentationNode::ThematicBreak { .. }
    | DocumentationNode::Break { .. }
    | DocumentationNode::CodeKeyword { .. }
    | DocumentationNode::CodeOperator { .. }
    | DocumentationNode::CodeIdentifier { .. }
    | DocumentationNode::CodeText { .. }
    | DocumentationNode::Image { .. }
    | DocumentationNode::Html { .. }
    | DocumentationNode::FootnoteReference { .. }
    | DocumentationNode::ImageReference { .. }
    | DocumentationNode::Definition { .. }
    | DocumentationNode::Frontmatter { .. }
    | DocumentationNode::Math { .. }
    | DocumentationNode::InlineMath { .. } => {
      audit_data
        .nodes
        .insert(topic, Node::Documentation(node.clone()));
    }

    // For nodes with children that don't create topic_metadata, add the node and recurse
    DocumentationNode::ListItem { children, .. }
    | DocumentationNode::Emphasis { children, .. }
    | DocumentationNode::Strong { children, .. }
    | DocumentationNode::Link { children, .. }
    | DocumentationNode::Delete { children, .. }
    | DocumentationNode::Table { children, .. }
    | DocumentationNode::TableRow { children, .. }
    | DocumentationNode::TableCell { children, .. }
    | DocumentationNode::FootnoteDefinition { children, .. }
    | DocumentationNode::LinkReference { children, .. } => {
      // Add the node with children converted to stubs
      audit_data.nodes.insert(
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      // Process children with the same scope
      for child in children {
        process_documentation_node(
          child,
          scope,
          is_technical,
          audit_data,
          mentions_by_topic,
        )?;
      }
    }

    DocumentationNode::Stub { .. } => {
      // Stubs are already processed, skip
    }
  }

  Ok(())
}

/// Builds a self-referencing SourceContext for a documentation topic.
/// Places the topic as a reference within its scope hierarchy so the
/// topic panel shows the topic's own rendered content.
fn build_self_context(
  topic: &topic::Topic,
  scope: &Scope,
  nodes: &BTreeMap<topic::Topic, Node>,
) -> Vec<core::SourceContext> {
  let mut groups: Vec<core::SourceContext> = Vec::new();
  let sort_key = get_source_location_start(topic, nodes);

  match scope {
    Scope::Global | Scope::Container { .. } => {
      // Topic is at the top level (e.g., Root or H1 section) — use itself as
      // both the scope and the reference so the panel renders its content.
      insert_into_context(
        &mut groups,
        topic.clone(),
        sort_key,
        true,
        None,
        &[],
        core::Reference::project_reference(topic.clone(), sort_key),
      );
    }
    Scope::Component { component, .. } => {
      // Topic is under a component (e.g., H2 section under H1)
      let component_sort_key = get_source_location_start(component, nodes);
      insert_into_context(
        &mut groups,
        component.clone(),
        component_sort_key,
        true,
        None,
        &[],
        core::Reference::project_reference(topic.clone(), sort_key),
      );
    }
    Scope::Member {
      component, member, ..
    } => {
      // Topic is under a member (e.g., H3 section under H2 under H1)
      let component_sort_key = get_source_location_start(component, nodes);
      let member_sort_key = get_source_location_start(member, nodes);
      insert_into_context(
        &mut groups,
        component.clone(),
        component_sort_key,
        true,
        Some((member.clone(), member_sort_key)),
        &[],
        core::Reference::project_reference(topic.clone(), sort_key),
      );
    }
    Scope::ContainingBlock {
      component,
      member,
      containing_blocks,
      ..
    } => {
      if let Some(layer) = containing_blocks.last() {
        let component_sort_key = get_source_location_start(component, nodes);
        let member_sort_key = get_source_location_start(member, nodes);
        let cb_sort_key = get_source_location_start(&layer.block, nodes);
        insert_into_context(
          &mut groups,
          component.clone(),
          component_sort_key,
          true,
          Some((member.clone(), member_sort_key)),
          &[],
          core::Reference::project_reference(layer.block.clone(), cb_sort_key),
        );
      }
    }
  }

  groups
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
