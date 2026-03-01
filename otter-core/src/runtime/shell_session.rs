use uuid::Uuid;

pub fn build_shell_session_key(workspace_id: Uuid, session_id: &str) -> String {
    format!("{workspace_id}:{session_id}")
}

#[cfg(test)]
mod tests {
    use super::build_shell_session_key;
    use uuid::Uuid;

    #[test]
    fn shell_session_key_is_stable() {
        let workspace_id = Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap();
        let key = build_shell_session_key(workspace_id, "terminal");
        assert_eq!(
            key,
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa:terminal".to_string()
        );
    }
}
