use crate::formatting;
use o11a_core::core;
use o11a_core::core::topic;
use o11a_core::documentation::parser::DocumentationNode;
use std::collections::BTreeMap;

/// Converts a documentation node to formatted HTML string.
/// When `semantics` is provided, inline code references with resolved
/// identifiers are annotated with their functional semantic meaning.
pub fn node_to_html(
  node: &DocumentationNode,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
) -> String {
  do_node_to_html(node, 0, nodes_map, None)
}

/// Like `node_to_html` but with functional semantics injected after inline
/// code references (e.g., `pID` becomes `pID` (participation identifier)).
/// `semantics` maps a declaration topic to the semantic text strings that
/// describe it.
pub fn node_to_html_with_semantics(
  node: &DocumentationNode,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
  semantics: &BTreeMap<topic::Topic, Vec<String>>,
) -> String {
  do_node_to_html(node, 0, nodes_map, Some(semantics))
}

#[allow(clippy::only_used_in_recursion)]
fn do_node_to_html(
  node: &DocumentationNode,
  indent_level: usize,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
  semantics: Option<&BTreeMap<topic::Topic, Vec<String>>>,
) -> String {
  match node.resolve(nodes_map) {
    DocumentationNode::Root { children, .. } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("\n");

      formatting::format_block(&content, "documentation-root")
    }

    // Section: renders just the content (no header, since header is parent)
    DocumentationNode::Section { children, .. } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("\n");

      formatting::format_block(&content, "section")
    }

    // Heading: renders the heading text and its section child (if any)
    DocumentationNode::Heading {
      level,
      children,
      node_id,
      section,
      ..
    } => {
      let heading_content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      let topic_id = topic::new_documentation_topic(*node_id);
      let heading_html =
        formatting::format_topic_heading(*level, &topic_id, &heading_content);

      // If there's a section child, render it after the heading
      match section {
        Some(sec) => {
          let section_html =
            do_node_to_html(sec, indent_level, nodes_map, semantics);
          format!("{}\n{}", heading_html, section_html)
        }
        None => heading_html,
      }
    }

    DocumentationNode::Paragraph {
      children, node_id, ..
    } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      let topic_id = topic::new_documentation_topic(*node_id);
      formatting::format_topic_block(
        &topic_id,
        &content,
        "paragraph",
        &topic_id,
      )
    }

    DocumentationNode::Sentence {
      children, node_id, ..
    } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      let topic_id = topic::new_documentation_topic(*node_id);
      formatting::format_topic_token(&topic_id, &content, "sentence", &topic_id)
    }

    DocumentationNode::Text { value, .. } => formatting::html_escape(value),

    DocumentationNode::InlineCode { children, .. } => {
      let inner: String = children
        .iter()
        .map(|c| do_node_to_html(c, indent_level, nodes_map, semantics))
        .collect();
      let code_html = formatting::format_inline_code(&inner);

      // Inject functional semantic annotation after the code span
      if let Some(sem_map) = semantics {
        for child in children {
          if let DocumentationNode::CodeIdentifier {
            referenced_topic: Some(t),
            ..
          } = child.resolve(nodes_map)
            && let Some(sems) = sem_map.get(t)
          {
            let joined: String = sems
              .iter()
              .map(|s| formatting::html_escape(s))
              .collect::<Vec<_>>()
              .join("; ");
            if !joined.is_empty() {
              return format!(
                "{} <span class=\"semantic\" style=\"opacity: 0.7; font-style: italic;\">({})</span>",
                code_html, joined
              );
            }
          }
        }
      }

      code_html
    }

    DocumentationNode::CodeBlock {
      lang,
      children,
      node_id,
      ..
    } => {
      let inner: String = children
        .iter()
        .map(|c| do_node_to_html(c, indent_level, nodes_map, semantics))
        .collect();

      let topic_id = topic::new_documentation_topic(*node_id);
      formatting::format_topic_code_block(&topic_id, lang.as_deref(), &inner)
    }

    DocumentationNode::CodeKeyword { value, .. } => {
      formatting::format_keyword(&formatting::html_escape(value))
    }

    DocumentationNode::CodeOperator { value, .. } => {
      // format_operator already escapes HTML, so don't double-escape
      formatting::format_operator(value)
    }

    DocumentationNode::CodeIdentifier {
      node_id,
      value,
      referenced_topic,
      kind,
      referenced_name,
      ..
    } => {
      let display_value = referenced_name.as_deref().unwrap_or(value);
      let class = kind.as_ref().map(formatting::named_topic_kind_to_class);

      match referenced_topic {
        // Code references always keep topic attributes
        Some(ref_topic) => {
          let node_topic = topic::new_documentation_topic(*node_id);
          formatting::format_topic_token(
            &node_topic,
            &formatting::html_escape(display_value),
            class.unwrap_or("identifier"),
            ref_topic,
          )
        }
        // No matching identifier found — render as plain text
        None => match class {
          Some(class) => formatting::format_token(
            &formatting::html_escape(display_value),
            class,
          ),
          None => formatting::html_escape(display_value),
        },
      }
    }

    DocumentationNode::CodeText { value, .. } => formatting::html_escape(value),

    DocumentationNode::List {
      node_id,
      ordered,
      children,
      ..
    } => {
      let list_items = children
        .iter()
        .map(|child| {
          do_node_to_html(child, indent_level + 1, nodes_map, semantics)
        })
        .collect::<Vec<_>>()
        .join("\n");

      let topic = topic::new_documentation_topic(*node_id);
      formatting::format_topic_list(&topic, *ordered, &list_items)
    }

    DocumentationNode::ListItem { children, .. } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      formatting::format_list_item(&content)
    }

    DocumentationNode::Emphasis { children, .. } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      formatting::format_emphasis(&content)
    }

    DocumentationNode::Strong { children, .. } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      formatting::format_strong(&content)
    }

    DocumentationNode::Link {
      url,
      title,
      children,
      ..
    } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      formatting::format_link(url, title.as_deref(), &content)
    }

    DocumentationNode::BlockQuote {
      children, node_id, ..
    } => {
      let content = children
        .iter()
        .map(|child| {
          do_node_to_html(child, indent_level + 1, nodes_map, semantics)
        })
        .collect::<Vec<_>>()
        .join("\n");

      let topic_id = topic::new_documentation_topic(*node_id);
      formatting::format_topic_block(
        &topic_id,
        &content,
        "blockquote",
        &topic_id,
      )
    }

    DocumentationNode::ThematicBreak { .. } => {
      formatting::format_thematic_break()
    }

    DocumentationNode::Break { .. } => "<br />".to_string(),

    DocumentationNode::Delete { children, .. } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      format!("<del>{}</del>", content)
    }

    DocumentationNode::Image {
      alt, url, title, ..
    } => {
      let label = if !alt.is_empty() {
        alt.as_str()
      } else if let Some(t) = title.as_deref() {
        t
      } else {
        url.as_str()
      };
      format_image_link(
        &formatting::html_escape(url),
        &formatting::html_escape(label),
      )
    }

    DocumentationNode::Table {
      children, node_id, ..
    } => {
      let rows = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("\n");

      let topic_id = topic::new_documentation_topic(*node_id);
      formatting::format_topic_block(&topic_id, &rows, "table", &topic_id)
    }

    DocumentationNode::TableRow { children, .. } => {
      let cells = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      format!("<tr>{}</tr>", cells)
    }

    DocumentationNode::TableCell { children, .. } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      format!("<td>{}</td>", content)
    }

    DocumentationNode::Html { value, .. } => {
      // Convert any HTML containing <img> tags to links instead of embedding images
      if value.contains("<img") {
        let src = extract_html_attr(value, "src").unwrap_or_default();
        let alt = extract_html_attr(value, "alt")
          .or_else(|| extract_html_attr(value, "title"))
          .unwrap_or(src.clone());
        let label = if alt.is_empty() { "Img" } else { &alt };
        format_image_link(
          &formatting::html_escape(&src),
          &formatting::html_escape(label),
        )
      } else {
        value.clone()
      }
    }

    DocumentationNode::FootnoteDefinition {
      identifier,
      children,
      ..
    } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      format!(
        "<div class=\"footnote\" id=\"fn-{}\"><sup>{}</sup> {}</div>",
        formatting::html_escape(identifier),
        formatting::html_escape(identifier),
        content
      )
    }

    DocumentationNode::FootnoteReference { identifier, .. } => {
      format!(
        "<sup><a href=\"#fn-{}\">{}</a></sup>",
        formatting::html_escape(identifier),
        formatting::html_escape(identifier)
      )
    }

    DocumentationNode::LinkReference {
      identifier,
      label,
      children,
      ..
    } => {
      let content = children
        .iter()
        .map(|child| do_node_to_html(child, indent_level, nodes_map, semantics))
        .collect::<Vec<_>>()
        .join("");

      let display = if content.is_empty() {
        label.as_deref().unwrap_or(identifier).to_string()
      } else {
        content
      };
      format!("[{}]", display)
    }

    DocumentationNode::ImageReference {
      alt, identifier, ..
    } => {
      let display = if alt.is_empty() {
        identifier.as_str()
      } else {
        alt.as_str()
      };
      format!("[{}]", formatting::html_escape(display))
    }

    DocumentationNode::Definition { .. } => {
      // Link definitions are invisible in rendered output
      String::new()
    }

    DocumentationNode::Frontmatter { value, .. } => {
      formatting::format_code_block(
        Some("yaml"),
        &formatting::html_escape(value),
      )
    }

    DocumentationNode::Math { value, .. } => formatting::format_code_block(
      Some("math"),
      &formatting::html_escape(value),
    ),

    DocumentationNode::InlineMath { value, .. } => {
      formatting::format_inline_code(&formatting::html_escape(value))
    }

    DocumentationNode::Stub { topic, .. } => formatting::format_stub(topic),
  }
}

/// Converts a documentation node to plain text (raw markdown without semantics).
pub fn node_to_plain_text(
  node: &DocumentationNode,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
) -> String {
  do_node_to_plain_text(node, nodes_map)
}

fn do_node_to_plain_text(
  node: &DocumentationNode,
  nodes_map: &BTreeMap<topic::Topic, core::Node>,
) -> String {
  let node = node.resolve(nodes_map);
  match node {
    DocumentationNode::Root { children, .. } => children
      .iter()
      .map(|c| do_node_to_plain_text(c, nodes_map))
      .collect::<Vec<_>>()
      .join("\n"),

    DocumentationNode::Section { children, .. } => children
      .iter()
      .map(|c| do_node_to_plain_text(c, nodes_map))
      .collect::<Vec<_>>()
      .join("\n"),

    DocumentationNode::Heading {
      level,
      children,
      section,
      ..
    } => {
      let prefix = "#".repeat(*level as usize);
      let text: String = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect();
      let heading = format!("{} {}", prefix, text);
      match section {
        Some(sec) => {
          let section_text = do_node_to_plain_text(sec, nodes_map);
          format!("{}\n{}", heading, section_text)
        }
        None => heading,
      }
    }

    DocumentationNode::Paragraph { children, .. } => {
      let content: String = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect();
      format!("{}\n", content)
    }

    DocumentationNode::Sentence { children, .. } => children
      .iter()
      .map(|c| do_node_to_plain_text(c, nodes_map))
      .collect(),

    DocumentationNode::Text { value, .. } => value.clone(),

    DocumentationNode::InlineCode { value, .. } => format!("`{}`", value),

    DocumentationNode::CodeBlock { lang, value, .. } => {
      let fence_lang = lang.as_deref().unwrap_or("");
      format!("```{}\n{}\n```", fence_lang, value)
    }

    DocumentationNode::CodeKeyword { value, .. }
    | DocumentationNode::CodeOperator { value, .. }
    | DocumentationNode::CodeIdentifier { value, .. }
    | DocumentationNode::CodeText { value, .. } => value.clone(),

    DocumentationNode::List {
      ordered, children, ..
    } => {
      let items: Vec<String> = children
        .iter()
        .enumerate()
        .map(|(i, c)| {
          let inner = do_node_to_plain_text(c, nodes_map);
          if *ordered {
            format!("{}. {}", i + 1, inner)
          } else {
            format!("- {}", inner)
          }
        })
        .collect();
      items.join("\n")
    }

    DocumentationNode::ListItem { children, .. } => children
      .iter()
      .map(|c| do_node_to_plain_text(c, nodes_map))
      .collect(),

    DocumentationNode::Emphasis { children, .. } => {
      let content: String = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect();
      format!("*{}*", content)
    }

    DocumentationNode::Strong { children, .. } => {
      let content: String = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect();
      format!("**{}**", content)
    }

    DocumentationNode::Link { url, children, .. } => {
      let content: String = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect();
      format!("[{}]({})", content, url)
    }

    DocumentationNode::BlockQuote { children, .. } => {
      let content = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect::<Vec<_>>()
        .join("\n");
      content
        .lines()
        .map(|line| format!("> {}", line))
        .collect::<Vec<_>>()
        .join("\n")
    }

    DocumentationNode::ThematicBreak { .. } => "---".to_string(),

    DocumentationNode::Break { .. } => "\n".to_string(),

    DocumentationNode::Delete { children, .. } => {
      let content: String = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect();
      format!("~~{}~~", content)
    }

    DocumentationNode::Image {
      alt, url, title, ..
    } => match title {
      Some(t) => format!("![{}]({} \"{}\")", alt, url, t),
      None => format!("![{}]({})", alt, url),
    },

    DocumentationNode::Table { children, .. } => children
      .iter()
      .map(|c| do_node_to_plain_text(c, nodes_map))
      .collect::<Vec<_>>()
      .join("\n"),

    DocumentationNode::TableRow { children, .. } => {
      let cells: Vec<String> = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect();
      format!("| {} |", cells.join(" | "))
    }

    DocumentationNode::TableCell { children, .. } => children
      .iter()
      .map(|c| do_node_to_plain_text(c, nodes_map))
      .collect(),

    DocumentationNode::Html { value, .. } => value.clone(),

    DocumentationNode::FootnoteDefinition {
      identifier,
      children,
      ..
    } => {
      let content: String = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect();
      format!("[^{}]: {}", identifier, content)
    }

    DocumentationNode::FootnoteReference { identifier, .. } => {
      format!("[^{}]", identifier)
    }

    DocumentationNode::LinkReference {
      identifier,
      label,
      children,
      ..
    } => {
      let content: String = children
        .iter()
        .map(|c| do_node_to_plain_text(c, nodes_map))
        .collect();
      if content.is_empty() {
        format!("[{}]", label.as_deref().unwrap_or(identifier))
      } else {
        format!("[{}][{}]", content, identifier)
      }
    }

    DocumentationNode::ImageReference {
      alt, identifier, ..
    } => {
      format!("![{}][{}]", alt, identifier)
    }

    DocumentationNode::Definition {
      identifier,
      url,
      title,
      ..
    } => match title {
      Some(t) => format!("[{}]: {} \"{}\"", identifier, url, t),
      None => format!("[{}]: {}", identifier, url),
    },

    DocumentationNode::Frontmatter { value, .. } => {
      format!("---\n{}\n---", value)
    }

    DocumentationNode::Math { value, .. } => {
      format!("$$\n{}\n$$", value)
    }

    DocumentationNode::InlineMath { value, .. } => {
      format!("${}$", value)
    }

    DocumentationNode::Stub { .. } => String::new(),
  }
}

/// Format an image as a styled link with the 🖼 prefix.
fn format_image_link(url: &str, label: &str) -> String {
  format!(
    "\u{1f5bc} <a href=\"{}\" style=\"color: var(--color-function)\">{}</a>",
    url, label,
  )
}

/// Extract an HTML attribute value from a raw HTML string.
/// e.g. `extract_html_attr("<img src=\"foo.png\">", "src")` => `Some("foo.png")`
fn extract_html_attr(html: &str, attr: &str) -> Option<String> {
  let pattern = format!("{}=\"", attr);
  let start = html.find(&pattern)? + pattern.len();
  let rest = &html[start..];
  let end = rest.find('"')?;
  Some(rest[..end].to_string())
}
