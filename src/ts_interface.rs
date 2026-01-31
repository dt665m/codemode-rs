use std::collections::HashMap;
use std::sync::RwLock;

use serde_json::Value;
use tracing::debug;

use crate::schema::JsonSchema;
use crate::tool::Tool;

#[derive(Default)]
struct ToolInterfaceCache {
    entries: RwLock<HashMap<String, String>>,
}

impl ToolInterfaceCache {
    fn get(&self, tool_name: &str) -> Option<String> {
        self.entries
            .read()
            .expect("tool interface cache lock")
            .get(tool_name)
            .cloned()
    }

    fn insert(&self, tool_name: &str, interface: String) {
        self.entries
            .write()
            .expect("tool interface cache lock")
            .insert(tool_name.to_string(), interface);
    }
}

pub struct ToolInterfaceGenerator {
    cache: ToolInterfaceCache,
}

impl Default for ToolInterfaceGenerator {
    fn default() -> Self {
        Self {
            cache: ToolInterfaceCache::default(),
        }
    }
}

impl ToolInterfaceGenerator {
    pub fn tool_to_typescript_interface(&self, tool: &Tool) -> String {
        debug!(tool = tool.name.as_str(), "tool interface generate");
        if let Some(interface) = self.cache.get(&tool.name) {
            return interface;
        }

        let (interface_content, access_pattern) = if tool.name.contains('.') {
            let mut parts = tool.name.split('.');
            let manual_name = parts.next().unwrap_or("manual");
            let tool_parts: Vec<&str> = parts.collect();
            let sanitized_manual = sanitize_identifier(manual_name);
            let tool_name = tool_parts
                .into_iter()
                .map(sanitize_identifier)
                .collect::<Vec<String>>()
                .join("_");
            let access_pattern = format!("{sanitized_manual}.{tool_name}");

            let input_content = json_schema_to_object_content(&tool.inputs);
            let output_content = json_schema_to_object_content(&tool.outputs);
            let output_interface = if tool.is_async {
                format!(
                    "  type {tool_name}Output = Promise<{tool_name}OutputBase>;\n\n  interface {tool_name}OutputBase {{\n{output_content}\n  }}"
                )
            } else {
                format!("  interface {tool_name}Output {{\n{output_content}\n  }}")
            };

            let interface_content = format!(
                "\
namespace {sanitized_manual} {{
  interface {tool_name}Input {{
{input_content}
  }}

{output_interface}
}}"
            );

            (interface_content, access_pattern)
        } else {
            let sanitized_tool = sanitize_identifier(&tool.name);
            let access_pattern = sanitized_tool.clone();
            let input_type =
                json_schema_to_typescript(&tool.inputs, &format!("{sanitized_tool}Input"));
            let output_type_name = if tool.is_async {
                format!("{sanitized_tool}OutputBase")
            } else {
                format!("{sanitized_tool}Output")
            };
            let output_type = json_schema_to_typescript(&tool.outputs, &output_type_name);
            let output_type = if tool.is_async {
                format!("{output_type}\n\ntype {sanitized_tool}Output = Promise<{sanitized_tool}OutputBase>;")
            } else {
                output_type
            };
            (format!("{input_type}\n\n{output_type}"), access_pattern)
        };

        let access_comment = if tool.is_async {
            format!("await {access_pattern}")
        } else {
            access_pattern.clone()
        };

        let interface_string = format!(
            "\
{interface_content}

/**
 * {description}
 * Tags: {tags}
 * Access as: {access_comment}(args)
 */",
            description = escape_comment(&tool.description),
            tags = escape_comment(&tool.tags.join(", ")),
            access_comment = access_comment
        );

        self.cache.insert(&tool.name, interface_string.clone());
        interface_string
    }

    pub fn tool_access_path(&self, tool: &Tool) -> String {
        if tool.name.contains('.') {
            let mut parts = tool.name.split('.');
            let manual_name = parts.next().unwrap_or("manual");
            let tool_parts: Vec<&str> = parts.collect();
            let sanitized_manual = sanitize_identifier(manual_name);
            let tool_name = tool_parts
                .into_iter()
                .map(sanitize_identifier)
                .collect::<Vec<String>>()
                .join("_");
            format!("{sanitized_manual}.{tool_name}")
        } else {
            sanitize_identifier(&tool.name)
        }
    }
}

fn sanitize_identifier(name: &str) -> String {
    let mut sanitized = String::with_capacity(name.len());
    for (idx, ch) in name.chars().enumerate() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            if idx == 0 && ch.is_ascii_digit() {
                sanitized.push('_');
            }
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }

    if sanitized.is_empty() {
        "_".to_string()
    } else {
        sanitized
    }
}

fn escape_comment(text: &str) -> String {
    text.replace("*/", "*\\/").replace('\n', " ")
}

fn json_schema_to_object_content(schema: &JsonSchema) -> String {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return "    [key: string]: any;".to_string();
    }

    let properties = schema.get("properties").and_then(Value::as_object);
    let required = schema.get("required").and_then(Value::as_array);
    let required_set: Vec<String> = required
        .map(|arr| {
            arr.iter()
                .filter_map(|val| val.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let mut lines = Vec::new();
    if let Some(props) = properties {
        for (prop_name, prop_schema) in props.iter() {
            let is_required = required_set.iter().any(|req| req == prop_name);
            let optional_marker = if is_required { "" } else { "?" };
            let description = prop_schema
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("");
            let ts_type = json_schema_to_typescript_type(prop_schema);

            if !description.is_empty() {
                lines.push(format!("    /** {} */", escape_comment(description)));
            }
            lines.push(format!("    {prop_name}{optional_marker}: {ts_type};"));
        }
    }

    if lines.is_empty() {
        "    [key: string]: any;".to_string()
    } else {
        lines.join("\n")
    }
}

fn json_schema_to_typescript(schema: &JsonSchema, type_name: &str) -> String {
    let schema_type = schema.get("type");
    match schema_type.and_then(Value::as_str) {
        Some("object") => object_schema_to_typescript(schema, type_name),
        Some("array") => array_schema_to_typescript(schema, type_name),
        Some("string") => primitive_schema_to_typescript(schema, type_name, "string"),
        Some("number") | Some("integer") => {
            primitive_schema_to_typescript(schema, type_name, "number")
        }
        Some("boolean") => primitive_schema_to_typescript(schema, type_name, "boolean"),
        Some("null") => format!("type {type_name} = null;"),
        _ => {
            if let Some(Value::Array(types)) = schema_type {
                let union = types
                    .iter()
                    .filter_map(|v| v.as_str())
                    .map(map_json_type_to_ts)
                    .collect::<Vec<&str>>()
                    .join(" | ");
                return format!("type {type_name} = {union};");
            }
            format!("type {type_name} = any;")
        }
    }
}

fn object_schema_to_typescript(schema: &JsonSchema, type_name: &str) -> String {
    let properties = schema.get("properties").and_then(Value::as_object);
    if properties.is_none() {
        return format!("interface {type_name} {{\n  [key: string]: any;\n}}");
    }

    let required = schema.get("required").and_then(Value::as_array);
    let required_set: Vec<String> = required
        .map(|arr| {
            arr.iter()
                .filter_map(|val| val.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let props = properties
        .unwrap()
        .iter()
        .map(|(key, prop_schema)| {
            let is_required = required_set.iter().any(|req| req == key);
            let optional = if is_required { "" } else { "?" };
            let prop_type = json_schema_to_typescript_type(prop_schema);
            let description = prop_schema
                .get("description")
                .and_then(Value::as_str)
                .map(|desc| format!("  /** {} */\n", escape_comment(desc)))
                .unwrap_or_default();
            format!("{description}  {key}{optional}: {prop_type};")
        })
        .collect::<Vec<String>>()
        .join("\n");

    format!("interface {type_name} {{\n{props}\n}}")
}

fn array_schema_to_typescript(schema: &JsonSchema, type_name: &str) -> String {
    let items = schema.get("items");
    if items.is_none() {
        return format!("type {type_name} = any[];");
    }

    let item_type = match items {
        Some(Value::Array(arr)) => arr
            .iter()
            .map(json_schema_to_typescript_type)
            .collect::<Vec<String>>()
            .join(" | "),
        Some(item) => json_schema_to_typescript_type(item),
        None => "any".to_string(),
    };

    format!("type {type_name} = ({item_type})[];")
}

fn primitive_schema_to_typescript(schema: &JsonSchema, type_name: &str, base_type: &str) -> String {
    if let Some(Value::Array(values)) = schema.get("enum") {
        let union = values
            .iter()
            .map(|val| match val {
                Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string()),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "null".to_string(),
                _ => "".to_string(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<String>>()
            .join(" | ");
        return format!("type {type_name} = {union};");
    }

    format!("type {type_name} = {base_type};")
}

fn json_schema_to_typescript_type(schema: &JsonSchema) -> String {
    if let Some(Value::Array(values)) = schema.get("enum") {
        let union = values
            .iter()
            .map(|val| match val {
                Value::String(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string()),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                Value::Null => "null".to_string(),
                _ => "".to_string(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<String>>()
            .join(" | ");
        return union;
    }

    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let properties = schema.get("properties").and_then(Value::as_object);
            if properties.is_none() {
                return "{ [key: string]: any }".to_string();
            }

            let required = schema.get("required").and_then(Value::as_array);
            let required_set: Vec<String> = required
                .map(|arr| {
                    arr.iter()
                        .filter_map(|val| val.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let props = properties
                .unwrap()
                .iter()
                .map(|(key, prop_schema)| {
                    let is_required = required_set.iter().any(|req| req == key);
                    let optional = if is_required { "" } else { "?" };
                    let prop_type = json_schema_to_typescript_type(prop_schema);
                    format!("{key}{optional}: {prop_type}")
                })
                .collect::<Vec<String>>()
                .join("; ");
            format!("{{ {props} }}")
        }
        Some("array") => {
            let items = schema.get("items");
            let item_type = match items {
                Some(Value::Array(arr)) => arr
                    .iter()
                    .map(json_schema_to_typescript_type)
                    .collect::<Vec<String>>()
                    .join(" | "),
                Some(item) => json_schema_to_typescript_type(item),
                None => "any".to_string(),
            };
            format!("({item_type})[]")
        }
        Some("string") => "string".to_string(),
        Some("number") | Some("integer") => "number".to_string(),
        Some("boolean") => "boolean".to_string(),
        Some("null") => "null".to_string(),
        Some(other) => map_json_type_to_ts(other).to_string(),
        None => "any".to_string(),
    }
}

fn map_json_type_to_ts(schema_type: &str) -> &str {
    match schema_type {
        "string" => "string",
        "number" | "integer" => "number",
        "boolean" => "boolean",
        "null" => "null",
        "object" => "object",
        "array" => "any[]",
        _ => "any",
    }
}
