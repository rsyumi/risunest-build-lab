use risunest_ir::{Budget, Charge, Error as CompatError};
use async_compression::{codecs::{self, DecodeV2}, core::util::{PartialBuffer, WriteBuffer}};
use async_trait::async_trait;
use reqwest::header::{HeaderMap, CONTENT_ENCODING};
use std::{mem::size_of, sync::{Arc, atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering}}};
use tokio::sync::Notify;

#[derive(Debug)]
pub(crate) enum DecodeError {
    Cancelled,
    Limit(&'static str),
    Allocation,
    Compat(CompatError),
    Reader(String),
    Codec { coding: Coding, diagnostic: String, window_log_max: Option<u32> },
    InvalidLimits,
    NoProgress(Coding),
}
impl From<CompatError> for DecodeError {
    fn from(value: CompatError) -> Self {
        match value {
            CompatError::Cancelled => Self::Cancelled,
            CompatError::Limit(kind) => Self::Limit(kind),
            CompatError::Allocation => Self::Allocation,
            other => Self::Compat(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CancelOrigin { Caller = 1, Owner = 2 }
#[derive(Clone)]
pub(crate) struct Cancellation { flag: Arc<AtomicBool>, origin: Arc<AtomicU8>, notify: Arc<Notify> }
impl Cancellation {
    pub fn new() -> Self { Self { flag: Arc::new(AtomicBool::new(false)), origin: Arc::new(AtomicU8::new(0)), notify: Arc::new(Notify::new()) } }
    pub fn flag(&self) -> Arc<AtomicBool> { self.flag.clone() }
    pub fn cancel(&self) { self.cancel_as(CancelOrigin::Caller); }
    pub fn cancel_as(&self, origin: CancelOrigin) {
        let _ = self.origin.compare_exchange(0, origin as u8, Ordering::AcqRel, Ordering::Acquire);
        self.flag.store(true, Ordering::Release); self.notify.notify_waiters();
    }
    pub fn origin(&self) -> CancelOrigin {
        if self.origin.load(Ordering::Acquire) == CancelOrigin::Owner as u8 { CancelOrigin::Owner } else { CancelOrigin::Caller }
    }
    pub fn is_cancelled(&self) -> bool { self.flag.load(Ordering::Acquire) }
    pub async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() { return; }
            notified.await;
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DecodeLimits {
    pub input_quantum: usize,
    pub output_quantum: usize,
    pub max_encoded_bytes: u64,
    pub max_layer_decoded_bytes: u64,
    pub max_total_decoded_bytes: u64,
    pub max_layers: usize,
    pub max_header_bytes: usize,
    pub max_window_sum: u64,
    pub zstd_window_log_max: u32,
}
impl DecodeLimits {
    pub fn validate(&self) -> Result<(), DecodeError> {
        let highest = if usize::BITS == 32 { 30 } else { 31 };
        if self.input_quantum == 0 || self.output_quantum == 0 || self.input_quantum > 65536 || self.output_quantum > 65536
            || self.max_encoded_bytes == 0 || self.max_layer_decoded_bytes == 0 || self.max_total_decoded_bytes == 0
            || self.max_layers == 0 || self.max_layers > 4 || self.max_header_bytes == 0
            || !(10..=highest).contains(&self.zstd_window_log_max) {
            return Err(DecodeError::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coding { Gzip, Deflate, Brotli, Zstd }
fn coding(bytes: &[u8]) -> Option<Coding> {
    if bytes.eq_ignore_ascii_case(b"gzip") || bytes.eq_ignore_ascii_case(b"x-gzip") { Some(Coding::Gzip) }
    else if bytes.eq_ignore_ascii_case(b"deflate") { Some(Coding::Deflate) }
    else if bytes.eq_ignore_ascii_case(b"br") { Some(Coding::Brotli) }
    else if bytes.eq_ignore_ascii_case(b"zstd") { Some(Coding::Zstd) }
    else { None }
}
fn trim_lws(mut value: &[u8]) -> &[u8] {
    while matches!(value.first(), Some(b' ' | b'\t')) { value = &value[1..]; }
    while matches!(value.last(), Some(b' ' | b'\t')) { value = &value[..value.len() - 1]; }
    value
}

pub(crate) struct EncodingChain { codings: [Coding; 4], length: usize }
impl EncodingChain { fn values(&self) -> &[Coding] { &self.codings[..self.length] } }
pub(crate) fn encoding_chain(headers: &HeaderMap, limits: DecodeLimits, budget: &Budget) -> Result<EncodingChain, DecodeError> {
    limits.validate()?;
    let mut bytes = 0usize;
    let mut count = 0usize;
    let mut passthrough = false;
    for value in headers.get_all(CONTENT_ENCODING).iter() {
        bytes = bytes.checked_add(value.as_bytes().len()).ok_or(DecodeError::Limit("encoding-header"))?;
        if bytes > limits.max_header_bytes { return Err(DecodeError::Limit("encoding-header")); }
        budget.work(value.as_bytes().len())?;
        for token in value.as_bytes().split(|byte| *byte == b',') {
            if coding(trim_lws(token)).is_none() { passthrough = true; }
            else { count = count.checked_add(1).ok_or(DecodeError::Limit("codec-layers"))?; }
        }
    }
    if passthrough { count = 0; }
    if count > limits.max_layers { return Err(DecodeError::Limit("codec-layers")); }
    let mut codings = [Coding::Gzip; 4];
    let mut index = 0;
    if count != 0 {
        for value in headers.get_all(CONTENT_ENCODING).iter() {
            for token in value.as_bytes().split(|byte| *byte == b',') {
                budget.work(token.len())?;
                codings[index] = coding(trim_lws(token)).expect("first encoding pass admitted known token");
                index += 1;
            }
        }
    }
    Ok(EncodingChain { codings, length: count })
}

struct Allocation { data: Vec<u8>, _charge: Charge, _extra: Option<Charge> }
pub(crate) struct MutableChunk { data: Vec<u8>, charge: Charge, extra: Option<Charge> }
impl MutableChunk {
    fn new(capacity: usize, budget: &Budget) -> Result<Self, DecodeError> {
        let fixed = size_of::<Allocation>() + 2 * size_of::<usize>();
        let charge = budget.reserve_owned(fixed.checked_add(capacity).ok_or(DecodeError::Limit("owned-bytes"))?)?;
        let mut data = Vec::new();
        data.try_reserve_exact(capacity).map_err(|_| DecodeError::Allocation)?;
        let extra = if data.capacity() > capacity { Some(budget.reserve_owned(data.capacity() - capacity)?) } else { None };
        budget.work(capacity)?;
        data.resize(capacity, 0);
        Ok(Self { data, charge, extra })
    }
    fn finish(mut self, length: usize) -> Chunk {
        self.data.truncate(length);
        Chunk { data: Arc::new(Allocation { data: self.data, _charge: self.charge, _extra: self.extra }), first: 0, last: length }
    }
}
#[derive(Clone)]
pub(crate) struct Chunk { data: Arc<Allocation>, first: usize, last: usize }
impl std::fmt::Debug for Chunk {
    fn fmt(&self, value: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { value.debug_struct("Chunk").field("length", &self.len()).finish() }
}
impl Chunk {
    pub fn copy(bytes: &[u8], budget: &Budget) -> Result<Self, DecodeError> {
        let mut output = MutableChunk::new(bytes.len(), budget)?;
        budget.work(bytes.len())?;
        output.data.copy_from_slice(bytes);
        Ok(output.finish(bytes.len()))
    }
    pub fn bytes(&self) -> &[u8] { &self.data.data[self.first..self.last] }
    pub fn len(&self) -> usize { self.last - self.first }
    pub fn take(&mut self, maximum: usize) -> Self {
        let end = self.first + self.len().min(maximum);
        let part = Self { data: self.data.clone(), first: self.first, last: end };
        self.first = end;
        part
    }
}

#[async_trait]
pub(crate) trait ChunkSource: Send {
    // An empty chunk is not EOF. Only None denotes a clean upstream end.
    async fn pull(&mut self, maximum: usize, cancellation: &Cancellation, budget: &Budget) -> Result<Option<Chunk>, DecodeError>;
    #[cfg(test)]
    fn poll_native_finish(&mut self, _cx: &mut std::task::Context<'_>, _cancel: &Cancellation, _budget: &Budget)
        -> std::task::Poll<Result<(), DecodeError>>
    {
        // Sources without raw HTTP frame ownership cannot qualify native body completion.
        std::task::Poll::Ready(Err(DecodeError::Reader(String::new())))
    }
}

struct Context {
    limits: DecodeLimits,
    budget: Budget,
    decoded: AtomicU64,
    _charge: Charge,
}
impl Context {
    fn produced(&self, length: usize, layer: &mut u64) -> Result<(), DecodeError> {
        *layer = layer.checked_add(length as u64).filter(|value| *value <= self.limits.max_layer_decoded_bytes).ok_or(DecodeError::Limit("layer-decoded-bytes"))?;
        self.decoded.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value|
            value.checked_add(length as u64).filter(|next| *next <= self.limits.max_total_decoded_bytes))
            .map_err(|_| DecodeError::Limit("total-decoded-bytes"))?;
        Ok(())
    }
}

struct MeteredWire { upstream: Box<dyn ChunkSource>, context: Arc<Context>, read: u64, _charge: Charge }
#[async_trait]
impl ChunkSource for MeteredWire {
    async fn pull(&mut self, maximum: usize, cancel: &Cancellation, budget: &Budget) -> Result<Option<Chunk>, DecodeError> {
        loop {
            budget.work(1)?;
            let part = tokio::select! { biased;
                _ = cancel.cancelled() => return Err(DecodeError::Cancelled),
                value = self.upstream.pull(maximum, cancel, budget) => value?,
            };
            let Some(part) = part else { return Ok(None) };
            if part.len() == 0 { continue; }
            if part.len() > maximum { return Err(DecodeError::Limit("source-quantum")); }
            self.read = self.read.checked_add(part.len() as u64).filter(|value| *value <= self.context.limits.max_encoded_bytes).ok_or(DecodeError::Limit("encoded-bytes"))?;
            return Ok(Some(part));
        }
    }
    #[cfg(test)]
    fn poll_native_finish(&mut self, cx: &mut std::task::Context<'_>, cancel: &Cancellation, budget: &Budget)
        -> std::task::Poll<Result<(), DecodeError>>
    { self.upstream.poll_native_finish(cx, cancel, budget) }
}

enum Engine { Gzip(codecs::GzipDecoder), Deflate(codecs::ZlibDecoder), Brotli(codecs::BrotliDecoder), Zstd(codecs::ZstdDecoder) }
impl Engine {
    fn new(coding: Coding, limits: DecodeLimits) -> Self {
        match coding {
            Coding::Gzip => Self::Gzip(codecs::GzipDecoder::new_http_chromium()),
            Coding::Deflate => Self::Deflate(codecs::ZlibDecoder::new_http_chromium()),
            Coding::Brotli => Self::Brotli(codecs::BrotliDecoder::new_http_chromium()),
            Coding::Zstd => Self::Zstd(codecs::ZstdDecoder::new_http_chromium_with_params(&[codecs::zstd::params::DParameter::window_log_max(limits.zstd_window_log_max)])),
        }
    }
    fn decoder(&mut self) -> &mut dyn DecodeV2 {
        match self { Self::Gzip(value) => value, Self::Deflate(value) => value, Self::Brotli(value) => value, Self::Zstd(value) => value }
    }
    fn replay_work(&self) -> usize { match self { Self::Deflate(value) => value.http_replay_work_bound(), _ => 0 } }
}
#[derive(Clone, Copy)]
enum PumpState { Input, Drain, FrameBoundary, Finish, Done, Failed,
    #[cfg(test)]
    NativeFinish,
}
struct Layer {
    upstream: Box<dyn ChunkSource>, engine: Engine, coding: Coding, context: Arc<Context>,
    input: Option<Chunk>, state: PumpState, eof: bool, decoded: u64, frame_input: u64, _charge: Charge, _replay: Option<Charge>,
    #[cfg(test)]
    native_gzip: bool,
    #[cfg(test)]
    native_error: Option<DecodeError>,
}
impl Layer {
    fn failure(&mut self, error: std::io::Error) -> DecodeError {
        self.state = PumpState::Failed;
        #[cfg(test)]
        if self.native_gzip { return DecodeError::Codec { coding: self.coding, diagnostic: String::new(), window_log_max: None }; }
        DecodeError::Codec { coding: self.coding, diagnostic: error.to_string(),
            window_log_max: (self.coding == Coding::Zstd).then_some(self.context.limits.zstd_window_log_max) }
    }
}
#[async_trait]
impl ChunkSource for Layer {
    async fn pull(&mut self, maximum: usize, cancel: &Cancellation, budget: &Budget) -> Result<Option<Chunk>, DecodeError> {
        loop {
            budget.work(1)?;
            #[cfg(test)]
            if let Some(error) = self.native_error.take() { return Err(error); }
            #[cfg(test)]
            if matches!(self.state, PumpState::NativeFinish) {
                self.input = None;
                let result = tokio::select! { biased;
                    _ = cancel.cancelled() => Err(DecodeError::Cancelled),
                    value = std::future::poll_fn(|cx| self.upstream.poll_native_finish(cx, cancel, budget)) => value,
                };
                let result = match budget.check() { Ok(()) => result, Err(error) => Err(error.into()) };
                self.state = if result.is_ok() { PumpState::Done } else { PumpState::Failed };
                result?;
                return Ok(None);
            }
            if matches!(self.state, PumpState::Done) { return Ok(None); }
            if matches!(self.state, PumpState::Failed) { return Err(DecodeError::NoProgress(self.coding)); }
            if matches!(self.state, PumpState::Input | PumpState::FrameBoundary) && self.input.as_ref().is_none_or(|part| part.len() == 0) && !self.eof {
                self.input = tokio::select! { biased;
                    _ = cancel.cancelled() => return Err(DecodeError::Cancelled),
                    value = self.upstream.pull(self.context.limits.input_quantum, cancel, budget) => match value {
                        Ok(part) => part,
                        Err(error) => { self.state = PumpState::Failed; return Err(error); }
                    },
                };
                self.eof = self.input.is_none();
                if self.eof { self.state = PumpState::Drain; }
            }
            if matches!(self.state, PumpState::FrameBoundary) {
                if self.eof { self.state = PumpState::Drain; }
                else {
                    self.engine.decoder().reinit().map_err(|error| self.failure(error))?;
                    self.frame_input = 0;
                    self.state = PumpState::Input;
                }
            }
            let remaining = self.context.limits.max_layer_decoded_bytes.saturating_sub(self.decoded).saturating_add(1);
            let total_remaining = self.context.limits.max_total_decoded_bytes.saturating_sub(self.context.decoded.load(Ordering::Relaxed)).saturating_add(1);
            let size = maximum.min(self.context.limits.output_quantum).min(usize::try_from(remaining.min(total_remaining)).unwrap_or(usize::MAX));
            if size == 0 { return Err(DecodeError::Limit("output-quantum")); }
            let input_len = self.input.as_ref().map_or(0, Chunk::len);
            budget.work(input_len.checked_add(size).and_then(|value| value.checked_add(self.engine.replay_work())).ok_or(DecodeError::Limit("work"))?)?;
            let mut output = MutableChunk::new(size, budget)?;
            let mut buffer = WriteBuffer::new_initialized(&mut output.data);
            let mut used = 0;
            let old_state = self.state;
            let result = match old_state {
                PumpState::Input => {
                    let source = self.input.as_ref().map_or(&[][..], Chunk::bytes);
                    let mut input = PartialBuffer::new(source);
                    let result = self.engine.decoder().decode(&mut input, &mut buffer);
                    used = input.written_len();
                    result
                }
                PumpState::Drain => self.engine.decoder().flush(&mut buffer),
                PumpState::Finish => self.engine.decoder().finish(&mut buffer),
                _ => unreachable!("pump transition handled before codec operation"),
            };
            #[cfg(test)]
            let result = if self.native_gzip {
                match result {
                    Err(error) if buffer.written_len() > 0 => {
                        let written = buffer.written_len();
                        drop(buffer);
                        self.native_error = Some(self.failure(error));
                        self.input = None;
                        self.context.produced(written, &mut self.decoded)?;
                        return Ok(Some(output.finish(written)));
                    }
                    result => result,
                }
            } else { result };
            let done = result.map_err(|error| self.failure(error))?;
            let written = buffer.written_len();
            drop(buffer);
            if let Some(input) = self.input.as_mut() { input.first += used; }
            self.frame_input = self.frame_input.checked_add(used as u64).ok_or(DecodeError::Limit("frame-input"))?;
            #[cfg(test)]
            if self.native_gzip && matches!(old_state, PumpState::Input) && done {
                self.state = PumpState::NativeFinish;
                if written > 0 {
                    self.context.produced(written, &mut self.decoded)?;
                    return Ok(Some(output.finish(written)));
                }
                continue;
            }
            match old_state {
                PumpState::Input => {
                    if done {
                        if self.coding != Coding::Zstd || (self.frame_input == 0 && written == 0) {
                            self.state = PumpState::Failed;
                            return Err(DecodeError::NoProgress(self.coding));
                        }
                        self.state = PumpState::FrameBoundary;
                    } else if written == size { self.state = PumpState::Drain; }
                    else if written == 0 && used == 0 && input_len != 0 {
                        self.state = PumpState::Failed; return Err(DecodeError::NoProgress(self.coding));
                    }
                }
                PumpState::Drain => if done { self.state = if self.eof { PumpState::Finish } else { PumpState::Input }; },
                PumpState::Finish => if done { self.state = PumpState::Done; },
                _ => unreachable!(),
            }
            #[cfg(test)]
            if self.native_gzip && matches!(self.state, PumpState::Done) { self.state = PumpState::NativeFinish; }
            if written > 0 {
                self.context.produced(written, &mut self.decoded)?;
                return Ok(Some(output.finish(written)));
            }
            if matches!(old_state, PumpState::Drain | PumpState::Finish) && !done {
                self.state = PumpState::Failed; return Err(DecodeError::NoProgress(self.coding));
            }
        }
    }
}

pub(crate) struct Pipeline { source: Box<dyn ChunkSource>, context: Arc<Context>, raw: bool, decoded: u64 }
impl Pipeline {
    pub fn new(wire: Box<dyn ChunkSource>, chain: EncodingChain, limits: DecodeLimits, budget: Budget) -> Result<Self, DecodeError> {
        limits.validate()?;
        let mut windows = 0u64;
        for coding in chain.values() {
            windows = windows.checked_add(match coding { Coding::Gzip | Coding::Deflate => 32768,
                Coding::Brotli => 1 << 24, Coding::Zstd => 1u64 << limits.zstd_window_log_max }).ok_or(DecodeError::Limit("codec-windows"))?;
        }
        if windows > limits.max_window_sum { return Err(DecodeError::Limit("codec-windows")); }
        let charge = budget.reserve_owned(size_of::<Context>() + 2 * size_of::<usize>())?;
        let context = Arc::new(Context { limits, budget: budget.clone(), decoded: AtomicU64::new(0), _charge: charge });
        let metered_charge = budget.reserve_owned(size_of::<MeteredWire>())?;
        let mut source: Box<dyn ChunkSource> = Box::new(MeteredWire { upstream: wire, context: context.clone(), read: 0, _charge: metered_charge });
        for coding in chain.values().iter().rev() {
            let layer_charge = budget.reserve_owned(size_of::<Layer>())?;
            let replay = if *coding == Coding::Deflate { Some(budget.reserve_owned(codecs::ZlibDecoder::http_context_bytes())?) } else { None };
            source = Box::new(Layer { upstream: source, engine: Engine::new(*coding, limits), coding: *coding, context: context.clone(),
                input: None, state: PumpState::Input, eof: false, decoded: 0, frame_input: 0, _charge: layer_charge, _replay: replay,
                #[cfg(test)] native_gzip: false, #[cfg(test)] native_error: None });
        }
        Ok(Self { source, context, raw: chain.length == 0, decoded: 0 })
    }
    #[cfg(test)]
    pub(super) fn native_plugin(wire: Box<dyn ChunkSource>, gzip: bool, limits: DecodeLimits, budget: Budget) -> Result<Self, DecodeError> {
        limits.validate()?;
        if gzip && limits.max_window_sum < 32768 { return Err(DecodeError::Limit("codec-windows")); }
        let charge = budget.reserve_owned(size_of::<Context>() + 2 * size_of::<usize>())?;
        let context = Arc::new(Context { limits, budget: budget.clone(), decoded: AtomicU64::new(0), _charge: charge });
        let metered_charge = budget.reserve_owned(size_of::<MeteredWire>())?;
        let mut source: Box<dyn ChunkSource> = Box::new(MeteredWire { upstream: wire, context: context.clone(), read: 0, _charge: metered_charge });
        if gzip {
            let layer_charge = budget.reserve_owned(size_of::<Layer>())?;
            source = Box::new(Layer { upstream: source, engine: Engine::Gzip(codecs::GzipDecoder::new()), coding: Coding::Gzip,
                context: context.clone(), input: None, state: PumpState::Input, eof: false, decoded: 0, frame_input: 0,
                _charge: layer_charge, _replay: None, native_gzip: true, native_error: None });
        }
        Ok(Self { source, context, raw: !gzip, decoded: 0 })
    }
    pub async fn pull(&mut self, cancel: &Cancellation) -> Result<Option<Chunk>, DecodeError> {
        let result = self.source.pull(self.context.limits.output_quantum, cancel, &self.context.budget).await?;
        if self.raw {
            if let Some(value) = &result { self.context.produced(value.len(), &mut self.decoded)?; }
        }
        Ok(result)
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use async_compression::{codecs::EncodeV2, core::Level};
    use std::{collections::VecDeque, sync::atomic::AtomicUsize};

    pub fn limits() -> DecodeLimits {
        DecodeLimits { input_quantum: 4096, output_quantum: 4096, max_encoded_bytes: 65536,
            max_layer_decoded_bytes: 65536, max_total_decoded_bytes: 262144, max_layers: 4,
            max_header_bytes: 4096, max_window_sum: 64 * 1024 * 1024, zstd_window_log_max: 23 }
    }
    pub fn budget(cancel: &Cancellation) -> Budget {
        Budget::new(risunest_ir::Limits { max_owned_bytes: 8 * 1024 * 1024, max_json_bytes: 65536,
            max_text_units: 65536, max_values: 4096, max_depth: 32, max_work: 16 * 1024 * 1024 }, cancel.flag()).unwrap()
    }
    pub fn encode(coding: Coding, input: &[u8]) -> Vec<u8> {
        encode_with(match coding {
            Coding::Gzip => Box::new(codecs::GzipEncoder::new(Level::Fastest.into())),
            Coding::Deflate => Box::new(codecs::ZlibEncoder::new(Level::Fastest.into())),
            Coding::Brotli => Box::new(codecs::BrotliEncoder::new(codecs::brotli::params::EncoderParams::default().quality(Level::Fastest))),
            Coding::Zstd => Box::new(codecs::ZstdEncoder::new(1)),
        }, input)
    }
    // Streamed encoding leaves the content size unknown, so a zstd frame declares
    // the encoder's window rather than a single-segment size.
    pub fn encode_with(mut encoder: Box<dyn EncodeV2>, input: &[u8]) -> Vec<u8> {
        let mut input = PartialBuffer::new(input);
        let mut output = Vec::new();
        for _ in 0..4096 {
            let mut block = [0; 128];
            let mut buffer = WriteBuffer::new_initialized(&mut block);
            if input.unwritten().is_empty() {
                let done = encoder.finish(&mut buffer).unwrap();
                output.extend_from_slice(buffer.written());
                if done { return output; }
            } else {
                encoder.encode(&mut input, &mut buffer).unwrap();
                output.extend_from_slice(buffer.written());
            }
        }
        panic!("bounded synthetic encoder failed to finish");
    }
    pub enum Action { Data(Vec<u8>), Error, Hold(Arc<Notify>) }
    pub struct Scripted {
        actions: VecDeque<Action>, pending: Option<Chunk>, pub pulls: Arc<AtomicUsize>, pub closed: Arc<AtomicBool>,
    }
    impl Scripted {
        pub fn new(actions: Vec<Action>) -> Self {
            Self { actions: actions.into(), pending: None, pulls: Arc::new(AtomicUsize::new(0)), closed: Arc::new(AtomicBool::new(false)) }
        }
    }
    impl Drop for Scripted { fn drop(&mut self) { self.closed.store(true, Ordering::Release); } }
    #[async_trait]
    impl ChunkSource for Scripted {
        async fn pull(&mut self, maximum: usize, cancel: &Cancellation, budget: &Budget) -> Result<Option<Chunk>, DecodeError> {
            if let Some(pending) = &mut self.pending {
                if pending.len() != 0 { return Ok(Some(pending.take(maximum))); }
                self.pending = None;
            }
            self.pulls.fetch_add(1, Ordering::Relaxed);
            match self.actions.pop_front() {
                None => Ok(None),
                Some(Action::Error) => Err(DecodeError::Reader("synthetic reader failure".into())),
                Some(Action::Hold(entered)) => { entered.notify_one(); cancel.cancelled().await; Err(DecodeError::Cancelled) }
                Some(Action::Data(data)) => {
                    let mut pending = Chunk::copy(&data, budget)?;
                    let part = pending.take(maximum);
                    self.pending = Some(pending);
                    Ok(Some(part))
                }
            }
        }
    }
    pub fn pipeline(actions: Vec<Action>, header: &str, limits: DecodeLimits, budget: &Budget) -> (Pipeline, Arc<AtomicBool>, Arc<AtomicUsize>) {
        let source = Scripted::new(actions);
        let closed = source.closed.clone(); let pulls = source.pulls.clone();
        let mut headers = HeaderMap::new();
        if !header.is_empty() { headers.insert(CONTENT_ENCODING, header.parse().unwrap()); }
        let chain = encoding_chain(&headers, limits, budget).unwrap();
        (Pipeline::new(Box::new(source), chain, limits, budget.clone()).unwrap(), closed, pulls)
    }
    fn name(coding: Coding) -> &'static str { match coding { Coding::Gzip => "gzip", Coding::Deflate => "deflate", Coding::Brotli => "br", Coding::Zstd => "zstd" } }

    #[tokio::test]
    async fn all_four_codecs_preserve_one_byte_input_output_and_clean_flush() {
        let payload = b"synthetic one-byte transitions";
        for coding in [Coding::Gzip, Coding::Deflate, Coding::Brotli, Coding::Zstd] {
            let cancel = Cancellation::new(); let budget = budget(&cancel);
            let mut tiny = limits(); tiny.input_quantum = 1; tiny.output_quantum = 1;
            let (mut pipeline, closed, _) = pipeline(vec![Action::Data(encode(coding, payload))], name(coding), tiny, &budget);
            let mut result = Vec::new();
            while let Some(part) = pipeline.pull(&cancel).await.unwrap() { assert_eq!(part.len(), 1); result.extend_from_slice(part.bytes()); }
            assert_eq!(result, payload, "{coding:?}");
            drop(pipeline); assert!(closed.load(Ordering::Acquire));
            assert_eq!(budget.owned_bytes(), Budget::context_bytes());
        }
    }

    #[tokio::test]
    async fn positive_output_precedes_ready_tail_errors_for_all_codecs_and_one_stack() {
        let payload = b"{\"message\":{\"content\":\"synthetic\"},\"done\":true}\n";
        for header in ["gzip", "deflate", "br", "zstd", "gzip, br"] {
            let coding = match header { "deflate" => Coding::Deflate, "br" => Coding::Brotli, "zstd" => Coding::Zstd, _ => Coding::Gzip };
            let mut encoded = encode(coding, payload);
            if header == "gzip, br" { encoded = encode(Coding::Brotli, &encoded); }
            let tail = if header == "zstd" { vec![0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0] } else { b"ignored tail".to_vec() };
            let cancel = Cancellation::new(); let budget = budget(&cancel);
            let (mut pipeline, closed, pulls) = pipeline(vec![Action::Data(encoded), Action::Data(tail), Action::Error], header, limits(), &budget);
            let first = pipeline.pull(&cancel).await.unwrap().unwrap();
            assert_eq!(first.bytes(), payload, "{header}");
            assert_eq!(pulls.load(Ordering::Relaxed), 1, "terminal output must precede another upstream read");
            drop(first);
            assert!(matches!(pipeline.pull(&cancel).await, Err(DecodeError::Reader(_))), "{header}");
            drop(pipeline); assert!(closed.load(Ordering::Acquire));
            assert_eq!(budget.owned_bytes(), Budget::context_bytes());
        }
    }

    #[tokio::test]
    async fn short_output_precedes_held_open_body_and_drop_releases_every_codec() {
        for coding in [Coding::Gzip, Coding::Deflate, Coding::Brotli, Coding::Zstd] {
            let cancel = Cancellation::new(); let budget = budget(&cancel); let entered = Arc::new(Notify::new());
            let (mut pipeline, closed, pulls) = pipeline(vec![Action::Data(encode(coding, b"terminal\n")), Action::Hold(entered.clone())], name(coding), limits(), &budget);
            let first = tokio::time::timeout(std::time::Duration::from_secs(5), pipeline.pull(&cancel)).await.expect("positive output must not await body EOF").unwrap().unwrap();
            assert_eq!(first.bytes(), b"terminal\n"); assert_eq!(pulls.load(Ordering::Relaxed), 1); drop(first);
            {
            let pending = pipeline.pull(&cancel); tokio::pin!(pending);
            tokio::select! { result = &mut pending => panic!("held body settled early: {result:?}"), _ = entered.notified() => () }
            cancel.cancel_as(CancelOrigin::Owner);
            assert!(matches!(pending.await, Err(DecodeError::Cancelled)));
            assert_eq!(cancel.origin(), CancelOrigin::Owner);
            }
            drop(pipeline); assert!(closed.load(Ordering::Acquire));
        }
    }

    #[test]
    fn committed_bad_adler_preserves_prior_output_and_http_reinit_restores_raw_mode() {
        let encoded = encode(Coding::Deflate, b"already accepted synthetic output");
        let body_end = encoded.len() - 4;
        let mut decoder = codecs::ZlibDecoder::new_http_chromium();
        let mut bytes = [0; 256]; let mut output = WriteBuffer::new_initialized(&mut bytes);
        decoder.decode(&mut PartialBuffer::new(&encoded[..body_end]), &mut output).unwrap();
        assert_eq!(output.written(), b"already accepted synthetic output");
        assert_eq!(decoder.http_replay_work_bound(), 0, "payload commits sniffing before bad checksum");
        let accepted = output.written_len();
        assert!(decoder.decode(&mut PartialBuffer::new(&[0xff; 4][..]), &mut output).is_err());
        assert_eq!(output.written_len(), accepted);
        assert!(decoder.flush(&mut output).is_err()); assert!(decoder.finish(&mut output).is_err());
        decoder.reinit().unwrap();
        let mut bytes = [0; 256]; let mut output = WriteBuffer::new_initialized(&mut bytes);
        decoder.decode(&mut PartialBuffer::new(&encoded[2..body_end]), &mut output).unwrap();
        assert_eq!(output.written(), b"already accepted synthetic output");
        assert!(decoder.finish(&mut output).unwrap());
        let mut strict = codecs::ZlibDecoder::new();
        let mut bytes = [0; 256]; let mut output = WriteBuffer::new_initialized(&mut bytes);
        assert!(strict.decode(&mut PartialBuffer::new(&encoded[2..body_end]), &mut output).is_err());
    }

    #[test]
    fn real_codec_errors_do_not_advance_failed_output_or_become_clean_eof() {
        let limits = limits();
        for coding in [Coding::Gzip, Coding::Deflate, Coding::Brotli, Coding::Zstd] {
            let mut engine = Engine::new(coding, limits);
            let mut bytes = [0; 256]; let mut output = WriteBuffer::new_initialized(&mut bytes);
            let before = output.written_len();
            assert!(engine.decoder().decode(&mut PartialBuffer::new(&[0xff; 32][..]), &mut output).is_err(), "{coding:?}");
            assert_eq!(output.written_len(), before);
            assert!(engine.decoder().finish(&mut output).is_err(), "{coding:?}");
            engine.decoder().reinit().unwrap();
            let encoded = encode(coding, b"reset synthetic data");
            let body = if coding == Coding::Deflate { encoded.as_slice() } else { &encoded[..encoded.len() - 1] };
            engine.decoder().decode(&mut PartialBuffer::new(body), &mut output).unwrap();
            assert!(b"reset synthetic data".starts_with(output.written()), "{coding:?}");
            assert!(engine.decoder().finish(&mut output).unwrap());
        }
    }

    #[test]
    fn unknown_or_identity_anywhere_selects_whole_raw_chain_before_layer_limit() {
        let cancel = Cancellation::new(); let budget = budget(&cancel); let mut limits = limits(); limits.max_layers = 1;
        for header in ["gzip, gzip, unknown", "unknown, br", "gzip, identity", "identity", "gzip,,br"] {
            let mut headers = HeaderMap::new(); headers.insert(CONTENT_ENCODING, header.parse().unwrap());
            assert!(encoding_chain(&headers, limits, &budget).unwrap().values().is_empty());
        }
        let mut headers = HeaderMap::new(); headers.insert(CONTENT_ENCODING, "gzip, br".parse().unwrap());
        assert!(matches!(encoding_chain(&headers, limits, &budget), Err(DecodeError::Limit("codec-layers"))));
        for cap in [0, 9, u32::MAX, 32] {
            limits.zstd_window_log_max = cap;
            assert!(matches!(limits.validate(), Err(DecodeError::InvalidLimits)));
        }
        // Every configured layer window counts toward one operation-wide sum.
        let mut limits = super::tests::limits(); limits.zstd_window_log_max = 23;
        for (header, windows) in [("gzip, br", 32768 + (1 << 24)), ("zstd, deflate", (1 << 23) + 32768)] {
            for (sum, accepted) in [(windows, true), (windows - 1, false)] {
                limits.max_window_sum = sum;
                let mut headers = HeaderMap::new(); headers.insert(CONTENT_ENCODING, header.parse().unwrap());
                let chain = encoding_chain(&headers, limits, &budget).unwrap();
                match Pipeline::new(Box::new(Scripted::new(Vec::new())), chain, limits, budget.clone()) {
                    Ok(_) => assert!(accepted, "{header} at {sum}"),
                    Err(error) => { assert!(!accepted, "{header} at {sum}"); assert!(matches!(error, DecodeError::Limit("codec-windows"))); }
                }
            }
        }
    }

    #[tokio::test]
    async fn zstd_window_cap_persists_across_joined_and_skippable_members() {
        use codecs::zstd::params::CParameter;
        let small = encode_with(Box::new(codecs::ZstdEncoder::new_with_params(1, &[CParameter::window_log(10)])), b"first member;");
        let large = encode(Coding::Zstd, b"second member");
        let skippable = [0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0];
        let joined = [small.as_slice(), &skippable, &large].concat();
        // Low and high caps match the browser controls: the later member declares
        // a window above 2^10, so only the low cap rejects it, with typed cap context.
        for (cap, body, accepted) in [(10, &large, &b""[..]), (23, &large, &b"second member"[..]),
            (10, &joined, &b"first member;"[..]), (23, &joined, &b"first member;second member"[..])] {
            let cancel = Cancellation::new(); let budget = budget(&cancel);
            let mut capped = limits(); capped.zstd_window_log_max = cap;
            let (mut pipeline, _, _) = pipeline(vec![Action::Data(body.clone())], "zstd", capped, &budget);
            let mut output = Vec::new();
            let failure = loop {
                match pipeline.pull(&cancel).await {
                    Ok(Some(part)) => output.extend_from_slice(part.bytes()),
                    Ok(None) => break None,
                    Err(error) => break Some(error),
                }
            };
            assert_eq!(output, accepted, "cap {cap}, {} bytes", body.len());
            if cap == 10 {
                assert!(matches!(failure, Some(DecodeError::Codec { coding: Coding::Zstd, window_log_max: Some(10), .. })), "{failure:?}");
            } else { assert!(failure.is_none(), "{failure:?}"); }
        }
    }

    #[tokio::test]
    async fn encoded_decoded_layer_and_aggregate_caps_are_operation_wide() {
        for kind in ["encoded-bytes", "layer-decoded-bytes", "total-decoded-bytes"] {
            let cancel = Cancellation::new(); let budget = budget(&cancel); let mut tiny = limits();
            let (bytes, header) = if kind == "total-decoded-bytes" {
                let inner = encode(Coding::Gzip, b"synthetic bounded payload");
                tiny.max_total_decoded_bytes = inner.len() as u64;
                (encode(Coding::Brotli, &inner), "gzip, br")
            } else {
                if kind == "encoded-bytes" { tiny.max_encoded_bytes = 1; } else { tiny.max_layer_decoded_bytes = 1; }
                (encode(Coding::Gzip, b"synthetic bounded payload"), "gzip")
            };
            let (mut pipeline, _, _) = pipeline(vec![Action::Data(bytes)], header, tiny, &budget);
            loop {
                match pipeline.pull(&cancel).await {
                    Err(DecodeError::Limit(actual)) => { assert_eq!(actual, kind); break; }
                    Ok(Some(_)) => (), other => panic!("missing {kind} rejection: {other:?}"),
                }
            }
        }
    }
}
