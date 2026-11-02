//! Stage 7 MCP server foundation. The tool contract is an AST extraction of
//! D4Vinci/Scrapling at commit 95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0.
//! HTTP and CDP execution are deliberately left for the integrated fetch and
//! browser crates; tools never pretend an unavailable backend succeeded.

use std::sync::{Arc, OnceLock};

use rmcp::{
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool, ToolAnnotations,
    },
    service::{RequestContext, RoleServer},
    ErrorData as McpError, ServerHandler,
};
use serde_json::{json, Map, Value};

/// Tool definitions including the pinned descriptions and all input fields.
pub fn tools() -> &'static [Tool] {
    static CATALOG: OnceLock<Vec<Tool>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let definitions: Vec<Value> =
            serde_json::from_str(include_str!("tools.json")).expect("fixed MCP tool contract JSON");
        assert_eq!(definitions.len(), 16, "pinned MCP contract changed");
        definitions
            .into_iter()
            .map(|entry| {
                let name = entry["name"].as_str().expect("tool name").to_string();
                let title = entry["title"].as_str().expect("title").to_string();
                let description = entry["description"]
                    .as_str()
                    .expect("description")
                    .to_string();
                let schema: Map<String, Value> = entry["inputSchema"]
                    .as_object()
                    .expect("input schema")
                    .clone();
                let category = match name.as_str() {
                    "browser_open" | "open_request_session" | "close_session" => "session",
                    "browser_save_state" | "browser_load_state" | "browser_actions"
                    | "browser_evaluate" => "input",
                    "list_sessions" | "browser_network_requests" | "browser_network_request" => {
                        "list"
                    }
                    _ => "fetch",
                };
                let annotations = match category {
                    "session" => {
                        ToolAnnotations::from_raw(None, Some(false), Some(false), None, Some(true))
                    }
                    "input" => ToolAnnotations::from_raw(
                        None,
                        Some(false),
                        Some(true),
                        Some(false),
                        Some(true),
                    ),
                    "list" => ToolAnnotations::from_raw(None, Some(true), None, None, Some(false)),
                    _ => ToolAnnotations::from_raw(None, Some(true), None, None, Some(true)),
                };
                Tool::new(name, description, Arc::new(schema))
                    .with_title(title)
                    .with_annotations(annotations)
            })
            .collect()
    })
}

/// Validate in-process before any backend receives an argument. This is kept
/// small to avoid pulling the fetch/browser dependency trees into Stage 7.
pub fn validate_args(tool: &Tool, arguments: &Map<String, Value>) -> Result<(), String> {
    let fields = tool.input_schema["properties"]
        .as_object()
        .expect("schema properties");
    let required = tool.input_schema["required"]
        .as_array()
        .expect("required array");
    for name in required {
        let name = name.as_str().expect("required name");
        if !arguments.contains_key(name) {
            return Err(format!("missing required argument '{name}'"));
        }
    }
    for (name, value) in arguments {
        let Some(schema) = fields.get(name) else {
            return Err(format!("unknown argument '{name}'"));
        };
        if !matches_schema(value, schema) {
            return Err(format!("invalid value for argument '{name}'"));
        }
    }
    Ok(())
}

fn matches_schema(value: &Value, schema: &Value) -> bool {
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        if !branches.iter().any(|branch| matches_schema(value, branch)) {
            return false;
        }
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        if branches
            .iter()
            .filter(|branch| matches_schema(value, branch))
            .count()
            != 1
        {
            return false;
        }
    }
    if let Some(kind) = schema.get("type").and_then(Value::as_str) {
        let valid = match kind {
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "null" => value.is_null(),
            _ => false,
        };
        if !valid {
            return false;
        }
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.contains(value) {
            return false;
        }
    }
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if value.as_f64().is_some_and(|n| n < min) {
            return false;
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if value.as_f64().is_some_and(|n| n > max) {
            return false;
        }
    }
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
        if value
            .as_str()
            .is_some_and(|s| (s.chars().count() as u64) < min)
        {
            return false;
        }
    }
    if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
        if value.as_array().is_some_and(|a| (a.len() as u64) < min) {
            return false;
        }
    }
    if let Some(max) = schema.get("maxItems").and_then(Value::as_u64) {
        if value.as_array().is_some_and(|a| (a.len() as u64) > max) {
            return false;
        }
    }
    if let Some(item_schema) = schema.get("items") {
        if value
            .as_array()
            .is_some_and(|a| !a.iter().all(|v| matches_schema(v, item_schema)))
        {
            return false;
        }
    }
    if let Some(obj) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            if required
                .iter()
                .any(|name| name.as_str().is_some_and(|s| !obj.contains_key(s)))
            {
                return false;
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (name, field_value) in obj {
                if let Some(field_schema) = properties.get(name) {
                    if !matches_schema(field_value, field_schema) {
                        return false;
                    }
                } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                    return false;
                }
            }
        } else if let Some(value_schema) = schema.get("additionalProperties") {
            if value_schema.is_object() && !obj.values().all(|v| matches_schema(v, value_schema)) {
                return false;
            }
        }
    }
    true
}

fn typed_error(kind: &str, message: &str, hint: &str) -> CallToolResult {
    let details = json!({"error": {"kind":kind, "message":message, "hint":hint}});
    CallToolResult::error(vec![ContentBlock::text(details.to_string())])
}

#[derive(Clone, Default)]
pub struct RustScraplingMcp;

impl ServerHandler for RustScraplingMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "RustScrapling stage-7 API foundation. HTTP and browser operations return explicit backend_unavailable errors until integrated.",
            )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools().to_vec()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().iter().find(|tool| tool.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let Some(tool) = self.get_tool(&request.name) else {
            return Err(McpError::invalid_params(
                format!("unknown MCP tool '{}'", request.name),
                None,
            ));
        };
        let arguments = request.arguments.unwrap_or_default();
        if let Err(reason) = validate_args(&tool, &arguments) {
            return Ok(typed_error(
                "invalid_arguments",
                &reason,
                "Inspect tools/list inputSchema for required fields, types, and limits",
            )
            .into());
        }
        if request.name == "list_sessions" {
            // No backends exist in this stage, and therefore no sessions.
            return Ok(CallToolResult::success(vec![ContentBlock::text("[]")]).into());
        }
        Ok(typed_error(
            "backend_unavailable",
            &format!(
                "MCP tool '{}' is registered but its HTTP/browser backend is not integrated",
                request.name
            ),
            "Integrate stage 3/4/5 fetch, browser and session backends before invoking this tool",
        )
        .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_catalog_has_all_tools_and_documented_parameters() {
        let all = tools();
        assert_eq!(all.len(), 16);
        for tool in all {
            assert!(
                !tool.description.as_deref().unwrap_or("").is_empty(),
                "{}",
                tool.name
            );
            assert!(!tool.description.as_deref().unwrap_or("").contains(":param"));
            for (param, schema) in tool.input_schema["properties"].as_object().unwrap() {
                assert!(
                    !schema["description"].as_str().unwrap_or("").is_empty(),
                    "{}.{}",
                    tool.name,
                    param
                );
            }
        }
        let make_request = all.iter().find(|t| t.name == "make_request").unwrap();
        assert_eq!(make_request.input_schema["required"], json!(["url"]));
        assert_eq!(
            make_request.input_schema["properties"]["method"]["default"],
            "GET"
        );
        assert_eq!(
            make_request.input_schema["properties"]["method"]["enum"],
            json!(["GET", "POST", "PUT", "DELETE"])
        );
        assert_eq!(
            make_request.annotations.as_ref().unwrap().read_only_hint,
            Some(true)
        );
        let browser = all.iter().find(|t| t.name == "browser_fetch").unwrap();
        assert_eq!(
            browser.input_schema["required"],
            json!(["url", "session_id"])
        );
        let actions = all.iter().find(|t| t.name == "browser_actions").unwrap();
        assert_eq!(
            actions.input_schema["properties"]["actions"]["items"]["oneOf"]
                .as_array()
                .unwrap()
                .len(),
            12
        );
        assert_eq!(
            actions.annotations.as_ref().unwrap().destructive_hint,
            Some(true)
        );
        let screenshot = all.iter().find(|t| t.name == "browser_screenshot").unwrap();
        assert_eq!(
            screenshot.input_schema["properties"]["quality"]["anyOf"][0]["maximum"],
            100
        );
        let network = all
            .iter()
            .find(|t| t.name == "browser_network_request")
            .unwrap();
        assert_eq!(
            network.input_schema["properties"]["part"]["enum"],
            json!([
                "summary",
                "request_headers",
                "request_body",
                "response_headers",
                "response_body"
            ])
        );
    }

    #[test]
    fn validates_required_enums_types_and_bounds() {
        let tool = tools()
            .iter()
            .find(|t| t.name == "browser_network_requests")
            .unwrap();
        assert!(validate_args(
            tool,
            &serde_json::from_value(json!({"session_id":"s", "limit": 200})).unwrap()
        )
        .is_ok());
        assert!(validate_args(
            tool,
            &serde_json::from_value(json!({"session_id":"s", "limit": 201})).unwrap()
        )
        .is_err());
        assert!(validate_args(
            tool,
            &serde_json::from_value(json!({"session_id":"s", "after_id": -1})).unwrap()
        )
        .is_err());
        assert!(validate_args(tool, &Map::new()).is_err());
        assert!(validate_args(
            tool,
            &serde_json::from_value(json!({"session_id":"s", "unexpected":true})).unwrap()
        )
        .is_err());
        let actions = tools()
            .iter()
            .find(|t| t.name == "browser_actions")
            .unwrap();
        assert!(validate_args(
            actions,
            &serde_json::from_value(json!({
                "session_id": "s", "actions": [{"type":"wait_time", "milliseconds":250}]
            }))
            .unwrap()
        )
        .is_ok());
        assert!(validate_args(
            actions,
            &serde_json::from_value(json!({
                "session_id": "s", "actions": [{"type":"wait_time", "milliseconds":-1}]
            }))
            .unwrap()
        )
        .is_err());
        assert!(validate_args(
            actions,
            &serde_json::from_value(json!({
                "session_id": "s", "actions": [{"type":"unknown"}]
            }))
            .unwrap()
        )
        .is_err());
    }
}
