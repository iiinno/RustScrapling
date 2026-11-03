//! Stage 7 MCP server. The tool contract is an AST extraction of
//! D4Vinci/Scrapling at commit 95cf9019ab5a5d6f3e9654b7ef1c6a26e29dc5c0.
//! HTTP and CDP execution reuse the integrated Rust libraries. Capabilities
//! not exposed by those libraries report tool-visible typed errors.

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

mod backend;
pub mod http_transport;
pub use backend::RuntimeBackend;

/// Tool definitions: the pinned upstream server's live `tools/list` catalog
/// (title, description, input/output JSON Schema and annotations), stored
/// verbatim in `tools.json` and checked by `oracle/runtime_schema_oracle.py`.
pub fn tools() -> &'static [Tool] {
    static CATALOG: OnceLock<Vec<Tool>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let definitions: Vec<Value> =
            serde_json::from_str(include_str!("tools.json")).expect("fixed MCP tool contract JSON");
        assert_eq!(definitions.len(), 16, "pinned MCP contract changed");
        definitions
            .into_iter()
            .map(|entry| {
                let object = |key: &str| -> Option<Map<String, Value>> {
                    entry.get(key).and_then(Value::as_object).cloned()
                };
                let name = entry["name"].as_str().expect("tool name").to_string();
                let title = entry["title"].as_str().expect("title").to_string();
                let description = entry["description"]
                    .as_str()
                    .expect("description")
                    .to_string();
                let mut tool = Tool::new(
                    name,
                    description,
                    Arc::new(object("inputSchema").expect("input schema")),
                )
                .with_title(title);
                if let Some(output) = object("outputSchema") {
                    tool = tool.with_raw_output_schema(Arc::new(output));
                }
                if let Some(annotations) = entry.get("annotations") {
                    let annotations: ToolAnnotations =
                        serde_json::from_value(annotations.clone()).expect("tool annotations");
                    tool = tool.with_annotations(annotations);
                }
                tool
            })
            .collect()
    })
}

/// Validate in-process before any backend receives an argument. Supports
/// the JSON Schema subset emitted by upstream's Pydantic models, including
/// local `$ref`/`$defs`, `const`, discriminated `oneOf` and exclusive bounds.
pub fn validate_args(tool: &Tool, arguments: &Map<String, Value>) -> Result<(), String> {
    let root = Value::Object((*tool.input_schema).clone());
    let fields = root["properties"].as_object().expect("schema properties");
    let empty = Vec::new();
    let required = root["required"].as_array().unwrap_or(&empty);
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
        if !matches_schema(value, schema, &root) {
            return Err(format!("invalid value for argument '{name}'"));
        }
    }
    Ok(())
}

/// Validate a structured tool result against the tool's output schema.
pub fn validate_output(tool: &Tool, value: &Value) -> bool {
    match &tool.output_schema {
        Some(schema) => {
            let root = Value::Object((**schema).clone());
            matches_schema(value, &root, &root)
        }
        None => true,
    }
}

fn resolve<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(reference) => reference
            .strip_prefix("#/")
            .map(|path| format!("/{path}"))
            .and_then(|pointer| root.pointer(&pointer))
            .map_or(schema, |target| resolve(target, root)),
        None => schema,
    }
}

fn matches_schema(value: &Value, schema: &Value, root: &Value) -> bool {
    if schema.get("$ref").is_some() {
        let target = resolve(schema, root);
        if std::ptr::eq(target, schema) || !matches_schema(value, target, root) {
            return false;
        }
    }
    if let Some(expected) = schema.get("const") {
        if value != expected {
            return false;
        }
    }
    if let Some(branches) = schema.get("anyOf").and_then(Value::as_array) {
        if !branches
            .iter()
            .any(|branch| matches_schema(value, branch, root))
        {
            return false;
        }
    }
    if let Some(branches) = schema.get("oneOf").and_then(Value::as_array) {
        if branches
            .iter()
            .filter(|branch| matches_schema(value, branch, root))
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
    if let Some(number) = value.as_f64() {
        let bound = |key: &str| schema.get(key).and_then(Value::as_f64);
        if bound("minimum").is_some_and(|min| number < min)
            || bound("maximum").is_some_and(|max| number > max)
            || bound("exclusiveMinimum").is_some_and(|min| number <= min)
            || bound("exclusiveMaximum").is_some_and(|max| number >= max)
        {
            return false;
        }
    }
    if let Some(text) = value.as_str() {
        let length = text.chars().count() as u64;
        if schema
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|min| length < min)
            || schema
                .get("maxLength")
                .and_then(Value::as_u64)
                .is_some_and(|max| length > max)
        {
            return false;
        }
    }
    if let Some(items) = value.as_array() {
        let count = items.len() as u64;
        if schema
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|min| count < min)
            || schema
                .get("maxItems")
                .and_then(Value::as_u64)
                .is_some_and(|max| count > max)
        {
            return false;
        }
        if schema.get("uniqueItems") == Some(&Value::Bool(true))
            && items
                .iter()
                .enumerate()
                .any(|(i, item)| items[..i].contains(item))
        {
            return false;
        }
        if let Some(item_schema) = schema.get("items") {
            if !items
                .iter()
                .all(|item| matches_schema(item, item_schema, root))
            {
                return false;
            }
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
        let properties = schema.get("properties").and_then(Value::as_object);
        let additional = schema.get("additionalProperties");
        for (name, field_value) in obj {
            match properties.and_then(|properties| properties.get(name)) {
                Some(field_schema) => {
                    if !matches_schema(field_value, field_schema, root) {
                        return false;
                    }
                }
                None => match additional {
                    Some(Value::Bool(false)) => return false,
                    Some(extra)
                        if extra.is_object() && !matches_schema(field_value, extra, root) =>
                    {
                        return false
                    }
                    _ => {}
                },
            }
        }
    }
    true
}

fn typed_error(kind: &str, message: &str, hint: &str) -> CallToolResult {
    let details = json!({"error": {"kind":kind, "message":message, "hint":hint}});
    CallToolResult::error(vec![ContentBlock::text(details.to_string())])
}

#[derive(Clone)]
pub struct RustScraplingMcp {
    backend: std::sync::Arc<RuntimeBackend>,
}

impl Default for RustScraplingMcp {
    fn default() -> Self {
        Self {
            backend: std::sync::Arc::new(RuntimeBackend::new()),
        }
    }
}

impl RustScraplingMcp {
    /// Shared runtime for all requests received by a single MCP server process.
    pub fn with_backend(backend: std::sync::Arc<RuntimeBackend>) -> Self {
        Self { backend }
    }

    pub async fn execute_tool(
        &self,
        name: &str,
        arguments: Map<String, Value>,
    ) -> Result<CallToolResult, McpError> {
        let Some(tool) = tools().iter().find(|tool| tool.name == name) else {
            return Err(McpError::invalid_params(
                format!("unknown MCP tool '{name}'"),
                None,
            ));
        };
        if let Err(reason) = validate_args(tool, &arguments) {
            return Ok(typed_error(
                "invalid_arguments",
                &reason,
                "Check tools/list inputSchema",
            ));
        }
        Ok(match self.backend.call(name, arguments).await {
            Ok(result) => {
                // Never hand clients structured data that contradicts the
                // advertised outputSchema; report it as an internal error.
                match &result.structured_content {
                    Some(value) if !validate_output(tool, value) => typed_error(
                        "internal",
                        "structured result does not match the tool outputSchema",
                        "Report this as a RustScrapling bug",
                    ),
                    _ => result,
                }
            }
            Err(error) => typed_error(&error.kind, &error.message, &error.hint),
        })
    }
}

/// Called by `rustscrapling mcp` and the standalone debug binary.
pub async fn serve_stdio(server: RustScraplingMcp) -> Result<(), Box<dyn std::error::Error>> {
    use rmcp::ServiceExt;
    let service = server.serve(rmcp::transport::stdio()).await?;
    service.waiting().await?;
    Ok(())
}

impl ServerHandler for RustScraplingMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions(
                "RustScrapling MCP: HTTP, browser fetching and sessions are available; unsupported browser actions return typed errors. Check isError before relying on results.",
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
        self.execute_tool(&request.name, request.arguments.unwrap_or_default())
            .await
            .map(Into::into)
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
