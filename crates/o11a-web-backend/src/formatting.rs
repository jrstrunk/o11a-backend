use o11a_core::domain::{NamedTopicKind, VariableMutability, topic};

/// Escapes HTML special characters
pub fn html_escape(s: &str) -> String {
  s.replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace('"', "&quot;")
    .replace('\'', "&#39;")
}

/// Converts a NamedTopicKind to a CSS class for syntax highlighting
pub fn named_topic_kind_to_class(kind: &NamedTopicKind) -> &'static str {
  match kind {
    NamedTopicKind::Contract(_) => "contract",
    NamedTopicKind::Struct => "struct",
    NamedTopicKind::Enum => "enum",
    NamedTopicKind::EnumMember => "enum-value",
    NamedTopicKind::Error => "error",
    NamedTopicKind::Event => "event",
    NamedTopicKind::StateVariable(VariableMutability::Constant) => "constant",
    NamedTopicKind::StateVariable(VariableMutability::Mutable) => {
      "mutable-state-variable"
    }
    NamedTopicKind::StateVariable(VariableMutability::Immutable) => {
      "immutable-state-variable"
    }
    NamedTopicKind::LocalVariable => "local-variable",
    NamedTopicKind::Function(_) => "function",
    NamedTopicKind::Modifier => "modifier",
    NamedTopicKind::Builtin => "global",
  }
}

// ============================================================================
// Basic token formatting (no topic/id association)
// ============================================================================

/// Formats a simple token with a class
pub fn format_token(token: &str, class: &str) -> String {
  format!("<span class=\"{}\">{}</span>", class, token)
}

/// Formats a block with a class
pub fn format_block(content: &str, class: &str) -> String {
  format!("<div class=\"{}\">{}</div>", class, content)
}

pub fn format_keyword(keyword: &str) -> String {
  format_token(keyword, "keyword")
}

pub fn format_operator(op: &str) -> String {
  format_token(&html_escape(op), "operator")
}

pub fn format_type(type_name: &str) -> String {
  format_token(type_name, "type")
}

pub fn format_member(name: &str) -> String {
  format_token(name, "member")
}

pub fn format_global(name: &str) -> String {
  format_token(name, "global")
}

pub fn format_comment(text: &str) -> String {
  format_token(text, "comment")
}

pub fn format_number(val: &str) -> String {
  format_token(val, "number")
}

pub fn format_string(val: &str) -> String {
  format_token(val, "string")
}

// ============================================================================
// Topic-associated formatting (with id and data-topic attributes)
// ============================================================================

/// Formats a topic token with data-topic attribute for focusable elements
pub fn format_topic_token(
  id_topic: &topic::Topic,
  token: &str,
  class: &str,
  data_topic: &topic::Topic,
) -> String {
  format!(
    "<span class=\"topic-token {}\" data-node-topic=\"{}\" data-topic=\"{}\" tabindex=\"0\">{}</span>",
    class,
    id_topic.id(),
    data_topic.id(),
    token
  )
}

/// Formats a topic block (div) with data-topic attribute for focusable elements
pub fn format_topic_block(
  id_topic: &topic::Topic,
  content: &str,
  class: &str,
  data_topic: &topic::Topic,
) -> String {
  format!(
    "<div class=\"topic-token {}\" data-node-topic=\"{}\" data-topic=\"{}\" tabindex=\"0\">{}</div>",
    class,
    id_topic.id(),
    data_topic.id(),
    content
  )
}

pub fn format_topic_keyword(
  id_topic: &topic::Topic,
  keyword: &str,
  data_topic: &topic::Topic,
) -> String {
  format_topic_token(id_topic, keyword, "keyword", data_topic)
}

pub fn format_topic_operator(
  id_topic: &topic::Topic,
  op: &str,
  data_topic: &topic::Topic,
) -> String {
  format_topic_token(id_topic, &html_escape(op), "operator", data_topic)
}

/// Formats an identifier using its NamedTopicKind to determine CSS class
pub fn format_named_identifier(
  id_topic: &topic::Topic,
  name: &str,
  ref_topic: &topic::Topic,
  kind: &NamedTopicKind,
) -> String {
  let css_class = named_topic_kind_to_class(kind);
  format_topic_token(id_topic, name, css_class, ref_topic)
}

/// Formats a function name as a topic token
pub fn format_topic_function_name(
  id_topic: &topic::Topic,
  name: &str,
  data_topic: &topic::Topic,
) -> String {
  format_topic_token(id_topic, name, "function", data_topic)
}

/// Formats an enum value as a topic token
pub fn format_topic_enum_value(
  id_topic: &topic::Topic,
  name: &str,
  data_topic: &topic::Topic,
) -> String {
  format_topic_token(id_topic, name, "enum-value", data_topic)
}

// ============================================================================
// Node/element formatting with IDs
// ============================================================================

/// Formats a node wrapper span with id and class
pub fn format_node(
  node_str: &str,
  topic: &topic::Topic,
  class: &str,
) -> String {
  format!(
    "<span class=\"{}\" data-node-topic=\"{}\">{}</span>",
    class,
    topic.id(),
    node_str
  )
}

/// Formats a brace with indent level class
pub fn format_brace(brace: &str, indent_level: usize) -> String {
  format!(
    "<span class=\"brace indent-level-{}\">{}</span>",
    indent_level, brace
  )
}

/// Creates an indentation wrapper with padding
pub fn indent(content: &str, indent_level: usize) -> String {
  format!("\n{}", inline_indent(content, indent_level))
}

/// Creates an indent with no newline
pub fn inline_indent(content: &str, indent_level: usize) -> String {
  format!(
    "<span class=\"indent indent-level-{}\">{}</span>",
    indent_level, content
  )
}

// ============================================================================
// Documentation element formatting
// ============================================================================

/// Formats a heading element with the specified level
pub fn format_heading(level: u8, content: &str) -> String {
  format!("<h{}>{}</h{}>", level, content, level)
}

/// Formats a heading element with topic association
pub fn format_topic_heading(
  level: u8,
  topic: &topic::Topic,
  content: &str,
) -> String {
  let inner = format_topic_token(topic, content, "heading", topic);
  format!("<h{}>{}</h{}>", level, inner, level)
}

/// Formats inline code
pub fn format_inline_code(content: &str) -> String {
  format!("<code class=\"inline-code\">{}</code>", content)
}

/// Formats a code block with optional language class
pub fn format_code_block(lang: Option<&str>, content: &str) -> String {
  let lang_class = lang.map(|l| format!(" language-{}", l)).unwrap_or_default();
  format!(
    "<pre class=\"code-block{}\"><code>{}</code></pre>",
    lang_class, content
  )
}

/// Formats a code block with topic association
pub fn format_topic_code_block(
  topic: &topic::Topic,
  lang: Option<&str>,
  content: &str,
) -> String {
  let lang_class = lang.map(|l| format!(" language-{}", l)).unwrap_or_default();
  format!(
    "<pre class=\"topic-token code-block{}\" data-node-topic=\"{}\" data-topic=\"{}\" tabindex=\"0\"><code>{}</code></pre>",
    lang_class,
    topic.id(),
    topic.id(),
    content
  )
}

/// Formats an ordered or unordered list
pub fn format_list(ordered: bool, items: &str) -> String {
  if ordered {
    format!("<ol class=\"list ordered\">\n{}\n</ol>", items)
  } else {
    format!("<ul class=\"list unordered\">\n{}\n</ul>", items)
  }
}

/// Formats an ordered or unordered list with topic association
pub fn format_topic_list(
  topic: &topic::Topic,
  ordered: bool,
  items: &str,
) -> String {
  if ordered {
    format!(
      "<ol class=\"topic-token list ordered\" data-topic=\"{}\" tabindex=\"0\">\n{}\n</ol>",
      topic.id(),
      items
    )
  } else {
    format!(
      "<ul class=\"topic-token list unordered\" data-topic=\"{}\" tabindex=\"0\">\n{}\n</ul>",
      topic.id(),
      items
    )
  }
}

/// Formats a list item
pub fn format_list_item(content: &str) -> String {
  format!("<li class=\"list-item\">{}</li>", content)
}

/// Formats emphasized text
pub fn format_emphasis(content: &str) -> String {
  format!("<em class=\"emphasis\">{}</em>", content)
}

/// Formats strong/bold text
pub fn format_strong(content: &str) -> String {
  format!("<strong class=\"strong\">{}</strong>", content)
}

/// Formats a hyperlink
pub fn format_link(url: &str, title: Option<&str>, content: &str) -> String {
  let title_attr = title
    .map(|t| format!(" title=\"{}\"", html_escape(t)))
    .unwrap_or_default();
  format!(
    "<a href=\"{}\" class=\"link\"{}>{}</a>",
    html_escape(url),
    title_attr,
    content
  )
}

/// Formats a thematic break (horizontal rule)
pub fn format_thematic_break() -> String {
  "<hr class=\"thematic-break\">".to_string()
}

/// Formats a stub placeholder
pub fn format_stub(topic: &topic::Topic) -> String {
  format!(
    "<span class=\"stub\" data-topic=\"{}\">Stub</span>",
    topic.id()
  )
}

// ============================================================================
// Inline documentation placeholders
// ============================================================================

/// Formats an invisible placeholder for inline identifier documentation.
/// The front end can query for elements with the `identifier-inline-comment`
/// class and inject documentation into them.
pub fn format_identifier_placeholder(topic: &topic::Topic) -> String {
  format!(
    "<div class=\"identifier-inline-comment\" data-placeholder-topic=\"{}\"></div>",
    topic.id()
  )
}

/// Formats an invisible placeholder for inline statement documentation.
/// The front end can query for elements with the `statement-inline-comment`
/// class and inject documentation into them.
pub fn format_statement_placeholder(topic: &topic::Topic) -> String {
  format!(
    "<div class=\"statement-inline-comment\" data-placeholder-topic=\"{}\"></div>",
    topic.id()
  )
}

/// Formats an invisible placeholder for inline containing block documentation.
/// The front end can query for elements with the `containing-block-inline-comment`
/// class and inject documentation into them.
pub fn format_containing_block_placeholder(topic: &topic::Topic) -> String {
  format!(
    "<div class=\"containing-block-inline-comment\" data-placeholder-topic=\"{}\"></div>",
    topic.id()
  )
}
