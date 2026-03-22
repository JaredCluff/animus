use animus_cortex::tools::{self, ToolRegistry, check_autonomy, AutonomyDecision, Tool, ToolContext};
use animus_cortex::telos::Autonomy;
use animus_cortex::{TurnContent, Turn, Role, StopReason, ReasoningOutput, ToolCall};

#[test]
fn test_tool_registry_definitions_generated() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(tools::read_file::ReadFileTool));
    registry.register(Box::new(tools::write_file::WriteFileTool));

    let defs = registry.definitions();
    assert_eq!(defs.len(), 2);

    let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
    assert!(names.contains(&"read_file"));
    assert!(names.contains(&"write_file"));
}

#[test]
fn test_tool_registry_filters_by_autonomy() {
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(tools::read_file::ReadFileTool));   // Inform
    registry.register(Box::new(tools::write_file::WriteFileTool)); // Act

    // At Inform level, only read_file should be available
    let inform_defs = registry.definitions_for_autonomy(Autonomy::Inform);
    assert_eq!(inform_defs.len(), 1);
    assert_eq!(inform_defs[0].name, "read_file");

    // At Act level, both should be available
    let act_defs = registry.definitions_for_autonomy(Autonomy::Act);
    assert_eq!(act_defs.len(), 2);
}

#[test]
fn test_autonomy_gating_logic() {
    // Act grants access to Suggest-level tools
    assert_eq!(check_autonomy(Autonomy::Act, Autonomy::Suggest), AutonomyDecision::Execute);
    // Inform does not grant access to Act-level tools
    assert_eq!(check_autonomy(Autonomy::Inform, Autonomy::Act), AutonomyDecision::Denied);
    // Full grants everything
    assert_eq!(check_autonomy(Autonomy::Full, Autonomy::Act), AutonomyDecision::Execute);
    // Same level grants access
    assert_eq!(check_autonomy(Autonomy::Suggest, Autonomy::Suggest), AutonomyDecision::Execute);
}

#[tokio::test]
async fn test_read_file_tool_reads_existing_file() {
    let tool = tools::read_file::ReadFileTool;
    let ctx = ToolContext { data_dir: std::path::PathBuf::from("/tmp") };
    let result = tool.execute(serde_json::json!({
        "path": "/etc/hostname"
    }), &ctx).await;

    // This file may or may not exist depending on OS, but the tool should not panic
    match result {
        Ok(_r) => { /* either success or "Error reading file" — both valid */ }
        Err(e) => panic!("Tool should not return Err: {e}"),
    }
}

#[tokio::test]
async fn test_read_file_tool_handles_missing_file() {
    let tool = tools::read_file::ReadFileTool;
    let ctx = ToolContext { data_dir: std::path::PathBuf::from("/tmp") };
    let result = tool.execute(serde_json::json!({
        "path": "/nonexistent/path/file.txt"
    }), &ctx).await.unwrap();

    assert!(result.is_error);
    assert!(result.content.contains("Error"));
}

#[test]
fn test_turn_with_tool_result() {
    let turn = Turn {
        role: Role::User,
        content: vec![TurnContent::ToolResult {
            tool_use_id: "call_123".to_string(),
            content: "file contents".to_string(),
            is_error: false,
        }],
    };
    assert_eq!(turn.role, Role::User);
    assert_eq!(turn.content.len(), 1);
}

#[test]
fn test_reasoning_output_with_tool_calls() {
    let output = ReasoningOutput {
        content: "Let me read that file.".to_string(),
        input_tokens: 100,
        output_tokens: 50,
        tool_calls: vec![ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            input: serde_json::json!({"path": "/tmp/x"}),
        }],
        stop_reason: StopReason::ToolUse,
    };
    assert_eq!(output.stop_reason, StopReason::ToolUse);
    assert_eq!(output.tool_calls.len(), 1);
    assert_eq!(output.tool_calls[0].name, "read_file");
}
