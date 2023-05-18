use sea_streamer_types::StreamResult;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum FileErr {
    #[error("IO Error: {0}")]
    IoError(#[source] std::io::Error),
    #[error("Watch Error: {0}")]
    WatchError(#[source] notify::Error),
    // #[error("Flume RecvError: {0}")]
    // RecvError(#[source] flume::RecvError),
    #[error("File Removed")]
    FileRemoved,
    #[error("File Limit Exceeded")]
    FileLimitExceeded,
    #[error("Watch Dead")]
    WatchDead,
}

pub type FileResult<T> = StreamResult<T, FileErr>;
