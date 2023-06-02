use std::{collections::BTreeMap, path::Path};

use sea_streamer_types::{
    export::futures::{future::BoxFuture, FutureExt},
    Buffer, Message as MessageTrait, MessageHeader, OwnedMessage, SeqNo, SeqPos, ShardId,
    SharedMessage, StreamKey, Timestamp, SEA_STREAMER_INTERNAL,
};

use crate::{
    format::{Beacon, Checksum, FormatErr, Header, Marker, Message, RunningChecksum},
    ByteBuffer, ByteSource, Bytes, DynFileSource, FileErr, FileId, FileSink, FileSourceType,
    StreamMode, WriteFrom,
};

pub const END_OF_STREAM: &str = "EOS";

/// A high level file reader that demux messages and beacon
pub struct MessageSource {
    source: DynFileSource,
    buffer: ByteBuffer,
    offset: u64,
    beacon_interval: u32,
    beacon: (u32, Vec<Marker>),
}

/// A high level file writer that mux messages and beacon
pub struct MessageSink {
    sink: FileSink,
    offset: u64,
    beacon_interval: u32,
    beacon: BTreeMap<(StreamKey, ShardId), BeaconState>,
    beacon_count: u32,
    message_count: u32,
}

pub enum SeekTarget {
    SeqNo(SeqNo),
    Timestamp(Timestamp),
}

struct BeaconState {
    seq_no: SeqNo,
    ts: Timestamp,
    running_checksum: RunningChecksum,
}

impl MessageSource {
    /// Creates a new message source. First, the stream Header is read
    /// from the file's beginning.
    ///
    /// If StreamMode is `Live`, it will fast forward to the file's end.
    /// Thanks to SeaStreamer's Beacon system, this is pretty efficient.
    pub async fn new(file_id: FileId, mode: StreamMode) -> Result<Self, FileErr> {
        let mut source = DynFileSource::new(
            file_id,
            match mode {
                StreamMode::Live | StreamMode::LiveReplay => FileSourceType::FileSource,
                StreamMode::Replay => FileSourceType::FileReader,
            },
        )
        .await?;
        let header = Header::read_from(&mut source).await?;
        assert!(Header::size() < header.beacon_interval as usize);
        let mut stream = Self::new_with(source, Header::size() as u64, header.beacon_interval);
        if mode == StreamMode::Live {
            stream.rewind(SeqPos::End).await?;
        }
        Ok(stream)
    }

    /// Creates a new message source with the header read and skipped.
    pub fn new_with(source: DynFileSource, offset: u64, beacon_interval: u32) -> Self {
        Self {
            source,
            buffer: ByteBuffer::new(),
            offset,
            beacon_interval,
            beacon: (0, Vec::new()),
        }
    }

    /// Rewind the message stream to a coarse position.
    /// SeqNo is regarded as the N-th beacon.
    /// Returns the current location in terms of N-th beacon.
    ///
    /// Warning: This future must not be canceled.
    pub async fn rewind(&mut self, target: SeqPos) -> Result<u32, FileErr> {
        let pos = match target {
            SeqPos::Beginning | SeqPos::At(0) => SeqPos::At(Header::size() as u64),
            SeqPos::End => SeqPos::End,
            SeqPos::At(nth) => {
                let at = nth * self.beacon_interval as u64;
                if at < self.known_size() {
                    SeqPos::At(at)
                } else {
                    SeqPos::End
                }
            }
        };
        self.offset = self.source.seek(pos).await?;

        // Align at a beacon
        if pos == SeqPos::End {
            let max = self.known_size() - (self.known_size() % self.beacon_interval as u64);
            let max = std::cmp::max(max, Header::size() as u64);
            let pos = match target {
                SeqPos::Beginning | SeqPos::At(0) => unreachable!(),
                SeqPos::End => max,
                SeqPos::At(nth) => {
                    let at = nth * self.beacon_interval as u64;
                    if at < self.known_size() {
                        at
                    } else {
                        max
                    }
                }
            };
            self.offset = self.source.seek(SeqPos::At(pos)).await?;
        }

        self.buffer.clear();
        self.clear_beacon();

        // Read until the start of the next message
        while let Some(i) = self.has_beacon() {
            let beacon = Beacon::read_from(&mut self.source).await?;
            let beacon_size = beacon.size();
            self.offset += beacon_size as u64;
            self.beacon = (i, beacon.items);

            let bytes = self
                .source
                .request_bytes(std::cmp::min(
                    beacon.remaining_messages_bytes as usize,
                    self.beacon_interval as usize - beacon_size,
                ))
                .await?;
            self.offset += bytes.len() as u64;
        }

        // Now we are at the first message after the last beacon,
        // we want to consume all messages up to known size
        if matches!(target, SeqPos::End) && self.offset < self.known_size() {
            let mut next = self.offset;
            let bytes = self
                .source
                .request_bytes((self.known_size() - self.offset) as usize)
                .await?;
            let mut buffer = ByteBuffer::one(bytes);
            while let Ok(message) = Message::read_from(&mut buffer).await {
                next += message.size() as u64;
            }
            self.offset = self.source.seek(SeqPos::At(next)).await?;
        }

        Ok((self.offset / self.beacon_interval as u64) as u32)
    }

    /// Warning: This future must not be canceled.
    pub async fn seek(&mut self, target: SeekTarget) -> Result<(), FileErr> {
        todo!()
    }

    fn has_beacon(&self) -> Option<u32> {
        if self.offset > 0 && self.offset % self.beacon_interval as u64 == 0 {
            Some((self.offset / self.beacon_interval as u64) as u32)
        } else {
            None
        }
    }

    async fn request_bytes(&mut self, size: usize) -> Result<Bytes, FileErr> {
        loop {
            if let Some(i) = self.has_beacon() {
                let beacon = Beacon::read_from(&mut self.source).await?;
                self.offset += beacon.size() as u64;
                self.beacon = (i, beacon.items);
            }

            let chunk = std::cmp::min(
                size - self.buffer.size(), // remaining size
                self.beacon_interval as usize
                    - (self.offset % self.beacon_interval as u64) as usize, // should not read past the next beacon
            );
            let bytes = self.source.request_bytes(chunk).await?;
            self.offset += chunk as u64;
            self.buffer.append(bytes); // these are message bytes

            debug_assert!(!self.buffer.size() > size, "we should never over-read");
            if self.buffer.size() == size {
                return Ok(self.buffer.consume(size));
            }
        }
    }

    /// Switch the file source type.
    ///
    /// Warning: This future must not be canceled.
    pub async fn switch_to(&mut self, stype: FileSourceType) -> Result<(), FileErr> {
        let source = std::mem::replace(&mut self.source, DynFileSource::Dead);
        self.source = source.switch_to(stype).await?;
        Ok(())
    }

    /// Read the next message.
    pub async fn next(&mut self) -> Result<Message, FileErr> {
        let message = Message::read_from(self).await?;
        let computed = message.compute_checksum();
        if message.checksum != computed {
            Err(FileErr::FormatErr(FormatErr::ChecksumErr {
                received: message.checksum,
                computed,
            }))
        } else {
            Ok(message)
        }
    }

    /// Get the most recent Beacon and it's index. Note that it is cleared (rather than carry-over)
    /// on each Beacon point.
    ///
    /// Beacon index starts from 1 (don't wary, because 0 is the header), and we have the following
    /// equation:
    ///
    /// ```ignore
    /// file offset = beacon index * beacon interval
    /// ```
    pub fn beacon(&self) -> (u32, &[Marker]) {
        (self.beacon.0, &self.beacon.1)
    }

    fn clear_beacon(&mut self) {
        self.beacon.0 = 0;
        self.beacon.1.clear();
    }

    #[inline]
    fn known_size(&self) -> u64 {
        self.source.file_size()
    }
}

impl ByteSource for MessageSource {
    /// Too complex to unroll by hand. Let's just box it.
    type Future<'a> = BoxFuture<'a, Result<Bytes, FileErr>>;

    /// Although this is exposed as public. Do not call this directly,
    /// this will interfere the Message Stream.
    fn request_bytes(&mut self, size: usize) -> Self::Future<'_> {
        self.request_bytes(size).boxed()
    }
}

impl MessageSink {
    pub async fn new(file_id: FileId, beacon_interval: u32, limit: u64) -> Result<Self, FileErr> {
        let path: &Path = file_id.path().as_ref();
        let file_name = path.file_name().unwrap().to_str().unwrap().to_owned();
        let mut sink = FileSink::new(file_id, WriteFrom::Beginning, limit).await?;
        let header = Header {
            file_name,
            created_at: Timestamp::now_utc(),
            beacon_interval,
        };
        let size = header.write_to(&mut sink)?;
        let message_count = 0;
        sink.flush(message_count).await?;

        Ok(Self {
            sink,
            offset: size as u64,
            beacon_interval,
            beacon: Default::default(),
            beacon_count: 0,
            message_count,
        })
    }

    /// This future is cancel safe. If it's canceled after polled once, the message
    /// will have been written. Otherwise it will be dropped.
    pub async fn write(&mut self, message: OwnedMessage) -> Result<Checksum, FileErr> {
        let key = (message.stream_key(), message.shard_id());
        let (seq_no, ts) = (message.sequence(), message.timestamp());
        let message = Message {
            message,
            checksum: 0,
        };
        let mut buffer = ByteBuffer::new();
        let (_, checksum) = message.write_to(&mut buffer)?;
        let entry = self.beacon.entry(key).or_insert(BeaconState {
            seq_no,
            ts,
            running_checksum: RunningChecksum::new(),
        });
        entry.seq_no = std::cmp::max(seq_no, entry.seq_no);
        entry.ts = std::cmp::max(ts, entry.ts);
        entry.running_checksum.update(checksum);

        while !buffer.is_empty() {
            let chunk = self.beacon_interval as usize
                - (self.offset % self.beacon_interval as u64) as usize;
            let chunk: ByteBuffer = buffer.consume(std::cmp::min(chunk, buffer.size()));
            self.offset += chunk.write_to(&mut self.sink)? as u64;

            if self.offset > 0 && self.offset % self.beacon_interval as u64 == 0 {
                let num_markers = Beacon::num_markers(self.beacon_interval as usize);
                let mut items = Vec::new();
                // We may not have enough space to fit in all beacon for every stream.
                // In which case, we'll round-robin among them.
                for ((key, sid), beacon) in self
                    .beacon
                    .iter()
                    .skip(self.beacon_count as usize % self.beacon.len())
                    .chain(self.beacon.iter())
                    .take(std::cmp::min(self.beacon.len(), num_markers))
                {
                    items.push(Marker {
                        header: MessageHeader::new(key.to_owned(), *sid, beacon.seq_no, beacon.ts),
                        running_checksum: beacon.running_checksum.crc(),
                    });
                }
                let beacon_count = items.len();
                let beacon = Beacon {
                    remaining_messages_bytes: buffer.size() as u32,
                    items,
                };
                self.offset += beacon.write_to(&mut self.sink)? as u64;
                self.beacon_count += beacon_count as u32;
            }
        }

        self.message_count += 1;
        self.sink.flush(self.message_count).await?;

        Ok(checksum)
    }
}

pub fn end_of_stream() -> OwnedMessage {
    let header = MessageHeader::new(
        StreamKey::new(SEA_STREAMER_INTERNAL).unwrap(),
        ShardId::new(0),
        0,
        Timestamp::now_utc(),
    );
    OwnedMessage::new(header, END_OF_STREAM.into_bytes())
}

pub fn is_end_of_stream(mess: &SharedMessage) -> bool {
    mess.header().stream_key().name() == SEA_STREAMER_INTERNAL
        && mess.message().as_bytes() == END_OF_STREAM.as_bytes()
}
