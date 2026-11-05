//! Incremental SSE framing. Retained bytes are bounded per event, not per stream.
use super::*;

pub(super) struct Event {
    pub data: Vec<u8>,
    pub id: Option<String>,
    pub retry: Option<u64>,
}
pub(super) struct Decoder {
    line: Vec<u8>,
    data: Vec<u8>,
    id: Option<String>,
    retry: Option<u64>,
    bytes: usize,
    maximum: usize,
    cr: bool,
    first: bool,
}
impl Decoder {
    pub fn finish(self) -> Result<(), RuntimeFailure> {
        if std::str::from_utf8(&self.line).is_err_and(|error| error.error_len().is_some()) {
            Err(fail(FailureCode::McpProtocol))
        } else {
            Ok(())
        }
    }
    pub fn new(maximum: usize) -> Self {
        Self {
            line: vec![],
            data: vec![],
            id: None,
            retry: None,
            bytes: 0,
            maximum,
            cr: false,
            first: true,
        }
    }
    pub fn byte(&mut self, byte: u8) -> Result<Option<Event>, RuntimeFailure> {
        if self.cr && byte == b'\n' {
            self.cr = false;
            return Ok(None);
        }
        self.cr = byte == b'\r';
        self.bytes = self
            .bytes
            .checked_add(1)
            .filter(|n| *n <= self.maximum)
            .ok_or_else(|| fail(FailureCode::McpProtocol))?;
        if byte != b'\r' && byte != b'\n' {
            self.line.push(byte);
            return Ok(None);
        }
        if self.line.is_empty() {
            if self.data.last() == Some(&b'\n') {
                self.data.pop();
            }
            self.bytes = 0;
            return Ok(Some(Event {
                data: std::mem::take(&mut self.data),
                id: self.id.take(),
                retry: self.retry.take(),
            }));
        }
        let line = std::str::from_utf8(&self.line).map_err(|_| fail(FailureCode::McpProtocol))?;
        let line = if self.first {
            self.first = false;
            line.strip_prefix('\u{feff}').unwrap_or(line)
        } else {
            line
        };
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "data" => {
                self.data.extend_from_slice(value.as_bytes());
                self.data.push(b'\n');
            }
            "id" if !value.contains('\0') => self.id = Some(value.into()),
            "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                self.retry = Some(value.parse().map_err(|_| fail(FailureCode::McpProtocol))?);
            }
            _ => {}
        }
        self.line.clear();
        Ok(None)
    }
}
