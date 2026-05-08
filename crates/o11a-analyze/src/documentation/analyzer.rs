use crate::documentation::parser;
use crate::documentation::resolution_pass;
use o11a_core::documentation::ast::{DocumentationAST, DocumentationNode};
use o11a_core::domain;
use o11a_core::domain::topic;
use o11a_core::domain::{
  AST, AuditData, DataContext, Node, Scope, TitledTopicKind, TopicMetadata,
  UnnamedTopicKind, insert_into_context,
};
use std::collections::BTreeMap;
use std::path::Path;

/// Analyzes documentation files and integrates them with the solidity DataContext
/// This MUST be called after solidity analysis completes, as it needs the solidity
/// declarations to resolve inline code references
pub fn analyze(
  project_root: &Path,
  audit_id: &str,
  data_context: &mut DataContext,
  document_files: &[domain::DocumentFileEntry],
) -> Result<(), String> {
  // Get the audit data
  let audit_data = data_context
    .get_audit_mut(audit_id)
    .ok_or_else(|| format!("Audit '{}' not found", audit_id))?;

  // Build a set of technical document paths for root kind lookup
  let technical_paths: std::collections::HashSet<&domain::ProjectPath> =
    document_files
      .iter()
      .filter(|e| e.is_technical)
      .map(|e| &e.project_path)
      .collect();

  // Extract just the project paths for the parser
  let paths: Vec<domain::ProjectPath> = document_files
    .iter()
    .map(|e| e.project_path.clone())
    .collect();

  // Parse document files in the order specified by documents.txt
  let mut ast_map = parser::process_files(project_root, &paths, audit_data)?;

  // Phase B: graph-driven resolution of ambiguous CodeIdentifier
  // references in the parsed doc tree. Mutates `referenced_topic` in
  // place; downstream passes (mention collection here, semantic-link
  // analysis later) read the improved values transparently.
  //
  // The pass runs *before* mention collection and AST stubbing so the
  // stubbed copy in `audit_data.asts` and `mentions_by_topic` both
  // reflect Phase B's resolutions. When `audit_data.resolution_graph`
  // is `None` (e.g. tests that skip Phase 4), the pass is a no-op.
  for asts in ast_map.values_mut() {
    for ast in asts.iter_mut() {
      for node in &mut ast.nodes {
        let traces = resolution_pass::resolve_doc_tree(node, audit_data);
        for (key, trace) in traces {
          audit_data.resolution_traces.insert(key, trace);
        }
      }
    }
  }

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
    let Some(domain::TopicMetadata::NamedTopic { doc_references, .. }) =
      audit_data.topic_metadata.get_mut(&referenced_topic)
    else {
      continue;
    };
    for scope in scopes {
      let mentioning_topic = match &scope {
        Scope::Member { member, .. }
        | Scope::ContainingBlock { member, .. } => *member,
        Scope::Component { component, .. } => *component,
        Scope::Global | Scope::Container { .. } => continue,
      };
      if !doc_references.contains(&mentioning_topic) {
        doc_references.push(mentioning_topic);
      }
    }
  }

  Ok(())
}

fn process_documentation_ast(
  ast: &DocumentationAST,
  project_path: &domain::ProjectPath,
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
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic,
        TopicMetadata::DocumentationTopic {
          topic,
          is_technical,
          scope: scope.clone(),
        },
      );
      audit_data.topic_context.insert(topic, context);

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
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic,
        TopicMetadata::UnnamedTopic {
          topic,
          kind: UnnamedTopicKind::DocumentationHeading,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic, context);

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
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      // Build a self-referencing context so the topic panel shows this section's content
      let context = build_self_context(&topic, scope, &audit_data.nodes);

      // Add topic metadata for the section
      audit_data.topic_metadata.insert(
        topic,
        TopicMetadata::TitledTopic {
          topic,
          scope: scope.clone(),
          kind: TitledTopicKind::DocumentationSection,
          title: title.clone(),
        },
      );
      audit_data.topic_context.insert(topic, context);

      // Create nested scope by adding this section to the scope hierarchy.
      // Sections are added regardless of heading level:
      // - First section becomes Component (Container -> Component)
      // - Second nested section becomes Member (Component -> Member)
      // - Third nested section becomes SemanticBlock (Member -> SemanticBlock)
      // - Further nesting stays at SemanticBlock level
      let section_scope = domain::add_to_scope(scope, topic);

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
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic,
        TopicMetadata::UnnamedTopic {
          topic,
          kind: UnnamedTopicKind::DocumentationParagraph,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic, context);

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
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic,
        TopicMetadata::UnnamedTopic {
          topic,
          kind: UnnamedTopicKind::DocumentationSentence,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic, context);

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
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic,
        TopicMetadata::UnnamedTopic {
          topic,
          kind: UnnamedTopicKind::DocumentationCodeBlock,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic, context);

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
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic,
        TopicMetadata::UnnamedTopic {
          topic,
          kind: UnnamedTopicKind::DocumentationList,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic, context);

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
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic,
        TopicMetadata::UnnamedTopic {
          topic,
          kind: UnnamedTopicKind::DocumentationBlockQuote,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic, context);

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
        topic,
        Node::Documentation(parser::children_to_stubs(node.clone())),
      );

      let context = build_self_context(&topic, scope, &audit_data.nodes);

      audit_data.topic_metadata.insert(
        topic,
        TopicMetadata::UnnamedTopic {
          topic,
          kind: UnnamedTopicKind::DocumentationInlineCode,
          scope: scope.clone(),
          transitive_topic: None,
        },
      );
      audit_data.topic_context.insert(topic, context);

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
          .entry(*ref_topic)
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
) -> Vec<domain::SourceContext> {
  let mut groups: Vec<domain::SourceContext> = Vec::new();
  let sort_key = get_source_location_start(topic, nodes);

  match scope {
    Scope::Global | Scope::Container { .. } => {
      // Topic is at the top level (e.g., Root or H1 section) — use itself as
      // both the scope and the reference so the panel renders its content.
      insert_into_context(
        &mut groups,
        *topic,
        sort_key,
        true,
        None,
        &[],
        domain::Reference::project_reference(*topic, sort_key),
      );
    }
    Scope::Component { component, .. } => {
      // Topic is under a component (e.g., H2 section under H1)
      let component_sort_key = get_source_location_start(component, nodes);
      insert_into_context(
        &mut groups,
        *component,
        component_sort_key,
        true,
        None,
        &[],
        domain::Reference::project_reference(*topic, sort_key),
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
        *component,
        component_sort_key,
        true,
        Some((*member, member_sort_key)),
        &[],
        domain::Reference::project_reference(*topic, sort_key),
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
          *component,
          component_sort_key,
          true,
          Some((*member, member_sort_key)),
          &[],
          domain::Reference::project_reference(layer.block, cb_sort_key),
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
