use std::str::Utf8Error;
use thiserror::Error;

pub type StreamResult<T, E> = std::result::Result<T, StreamErr<E>>;

#[derive(Error, Debug)]
pub enum StreamErr<E: std::error::Error> {
    #[error("Timeout has not yet been set")]
    TimeoutNotSet,
    #[error("Producer has already been anchored")]
    AlreadyAnchored,
    #[error("Producer has not yet been anchored")]
    NotAnchored,
    #[error("Consumer group is set; but not expected")]
    ConsumerGroupIsSet,
    #[error("Consumer group has not yet been set")]
    ConsumerGroupNotSet,
    #[error("Consumer has already been assigned")]
    AlreadyAssigned,
    #[error("Utf8Error: {0}")]
    Utf8Error(Utf8Error),
    #[error("Invalid stream key: valid pattern is [a-zA-Z0-9._-]{{1, 249}}")]
    InvalidStreamKey,
    #[error("Unsupported feature: {0}")]
    Unsupported(String),
    #[error("Backend error; please call reveal() to get the details")]
    Backend(E),
}

#[cfg(feature = "json")]
#[derive(Error, Debug)]
pub enum JsonErr {
    #[error("Cannot reach streamer")]
    Utf8Error(#[from] std::str::Utf8Error),
    #[error("Producer has already been anchored")]
    SerdeJson(#[from] serde_json::Error),
}
