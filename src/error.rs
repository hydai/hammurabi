use thiserror::Error;

#[derive(Debug, Error)]
pub enum HammurabiError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("database error: {0}")]
    Database(String),

    #[error("GitHub API error: {0}")]
    GitHub(String),

    #[error("AI agent error: {0}")]
    Ai(String),

    #[error("AI agent timeout: {0}")]
    AiTimeout(String),

    #[error("worktree error: {0}")]
    Worktree(String),

    #[error("state machine error: {0}")]
    #[allow(dead_code)]
    StateMachine(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
