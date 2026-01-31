use codemode_rs::{Tool, ToolInterfaceGenerator};
use serde_json::json;

#[test]
fn generates_namespaced_interfaces_with_jsdoc() {
    let tool = Tool {
        name: "github.get_pull_request".to_string(),
        description: "Fetch a pull request".to_string(),
        tags: vec!["github".to_string(), "pulls".to_string()],
        inputs: json!({
            "type": "object",
            "properties": {
                "owner": { "type": "string", "description": "Repository owner" },
                "repo": { "type": "string" },
                "pull_number": { "type": "integer" },
                "state": { "type": "string", "enum": ["open", "closed"] }
            },
            "required": ["owner", "repo", "pull_number"]
        }),
        outputs: json!({
            "type": "object",
            "properties": {
                "title": { "type": "string" }
            }
        }),
        is_async: true,
    };

    let generator = ToolInterfaceGenerator::default();
    let output = generator.tool_to_typescript_interface(&tool);

    assert!(output.contains("namespace github"));
    assert!(output.contains("interface get_pull_requestInput"));
    assert!(output.contains("pull_number: number"));
    assert!(output.contains("state?: \"open\" | \"closed\""));
    assert!(output.contains("Promise<get_pull_requestOutputBase>"));
    assert!(output.contains("Access as: await github.get_pull_request(args)"));
}
