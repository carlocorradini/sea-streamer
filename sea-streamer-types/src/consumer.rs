use crate::{Message, SequencePos, ShardId, StreamResult, Timestamp};
use async_trait::async_trait;
use futures::{Future, Stream};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ConsumerMode {
    /// This is the 'vanilla' stream consumer. It does not auto-commit, and thus only consumes messages from now on.
    RealTime,
    /// When the process restarts, it will resume the stream from the previous committed sequence.
    Resumable,
    /// You should assign a consumer group manually. The load-balancing mechanism is implementation-specific.
    LoadBalanced,
}

impl Default for ConsumerMode {
    fn default() -> Self {
        Self::RealTime
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConsumerGroup {
    name: String,
}

pub trait ConsumerOptions: Default + Clone + Send {
    type Error: std::error::Error;

    fn new(mode: ConsumerMode) -> Self;

    /// Get currently set ConsumerMode
    fn mode(&self) -> StreamResult<&ConsumerMode, Self::Error>;

    /// Get currently set consumer group; may return [`StreamErr::ConsumerGroupNotSet`].
    fn consumer_group(&self) -> StreamResult<&ConsumerGroup, Self::Error>;

    /// Set consumer group for this consumer. Note the semantic is implementation-specific.
    fn set_consumer_group(
        &mut self,
        group_id: ConsumerGroup,
    ) -> StreamResult<&mut Self, Self::Error>;
}

#[async_trait]
pub trait Consumer: Sized + Send + Sync {
    type Error: std::error::Error;

    type Message<'a>: Message
    where
        Self: 'a;
    type NextFuture<'a>: Future<Output = StreamResult<Self::Message<'a>, Self::Error>>
    where
        Self: 'a;
    type Stream<'a>: Stream<Item = StreamResult<Self::Message<'a>, Self::Error>>
    where
        Self: 'a;

    /// Seek to an arbitrary point in time. If will start consuming from the earliest message
    /// with a timestamp later than `to`.
    ///
    /// If the consumer is not already assigned, shard ZERO will be used.
    async fn seek(&mut self, to: Timestamp) -> StreamResult<(), Self::Error>;

    /// Rewind the stream to a particular sequence number.
    ///
    /// If the consumer is not already assigned, shard ZERO will be used.
    fn rewind(&mut self, offset: SequencePos) -> StreamResult<(), Self::Error>;

    /// Assign this consumer to a particular shard.
    ///
    /// It will only take effect on the next [`Consumer::seek`] or [`Consumer::rewind`].
    fn assign(&mut self, shard: ShardId) -> StreamResult<(), Self::Error>;

    /// Poll and receive one message: it awaits until there are new messages
    fn next(&self) -> Self::NextFuture<'_>;

    /// Returns an async stream. You cannot create multiple streams from the same consumer,
    /// nor perform any operation while streaming.
    fn stream<'a, 'b: 'a>(&'b mut self) -> Self::Stream<'a>;
}

impl ConsumerGroup {
    pub fn new(name: String) -> Self {
        Self { name }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}