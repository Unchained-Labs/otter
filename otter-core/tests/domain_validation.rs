use otter_core::domain::{CreateProjectRequest, EnqueuePromptRequest, UpdateQueuePositionRequest};
use validator::Validate;

#[test]
fn rejects_empty_prompt() {
    let payload = EnqueuePromptRequest {
        workspace_id: uuid::Uuid::new_v4(),
        prompt: String::new(),
        priority: None,
        schedule_at: None,
    };
    assert!(payload.validate().is_err());
}

#[test]
fn accepts_valid_project_name() {
    let payload = CreateProjectRequest {
        name: "backend-platform".to_string(),
        description: None,
    };
    assert!(payload.validate().is_ok());
}

#[test]
fn rejects_invalid_queue_priority() {
    let payload = UpdateQueuePositionRequest { priority: 0 };
    assert!(payload.validate().is_err());
}
