//! Deterministic bounded extraction of existing JSON documents from admitted tools.

use std::{collections::BTreeMap, fmt};

use htlk_analyzer::{McpValidationError, NativeSchemaError, NativeSchemas};
use htlk_executable::{
    CandidateSelection, ExtractionPolicy, JsonDocument, JsonError, McpResultError,
    cbor::{self, LimitKind, Limits, Map, Value},
    digest::Digest,
};

/// Operational extraction failure. Ordinary absence/ambiguity/exhaustion is a report.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExtractionError {
    /// The outer protocol result was not admissible; no byte salvage is attempted.
    Protocol(McpValidationError),
    /// Invalid host-selected policy.
    Policy(McpResultError),
    /// Native representation failure.
    Codec(cbor::Error),
    /// Native schema-selection engine failure.
    Schema(NativeSchemaError),
    /// An operational JSON conversion failure, not ordinary invalid display text.
    Json(JsonError),
    /// A schema-selection rule requires explicitly linked schema support.
    SchemaUnavailable,
    /// A fallible allocation failed independently of a configured allowance.
    AllocationFailed,
}
impl fmt::Display for ExtractionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => write!(f, "JSON extraction protocol: {error}"),
            Self::Policy(error) => write!(f, "JSON extraction policy: {error}"),
            Self::Codec(error) => write!(f, "JSON extraction codec: {error}"),
            Self::Schema(error) => write!(f, "JSON extraction selection: {error}"),
            Self::Json(error) => write!(f, "JSON extraction conversion: {error}"),
            Self::SchemaUnavailable => {
                f.write_str("JSON extraction schema implementation unavailable")
            }
            Self::AllocationFailed => f.write_str("JSON extraction allocation failed"),
        }
    }
}
impl std::error::Error for ExtractionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Protocol(error) => Some(error),
            Self::Policy(error) => Some(error),
            Self::Codec(error) => Some(error),
            Self::Schema(error) => Some(error),
            Self::Json(error) => Some(error),
            _ => None,
        }
    }
}
impl From<cbor::Error> for ExtractionError {
    fn from(value: cbor::Error) -> Self {
        Self::Codec(value)
    }
}

/// Strict text discovery boundary; no method interprets prose or executable code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextExtractionMethod {
    /// The complete trimmed text block is one JSON value.
    WholeBlock,
    /// A complete unlabeled/JSON Markdown fence body is one JSON value.
    Fence,
    /// A balanced strict object/array document occurs within display text.
    Embedded,
}
/// Original evidence location. Text offsets are UTF-8 bytes with exclusive ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractionSource {
    /// Original structuredContent, never inserted into the upstream descriptor.
    StructuredContent,
    /// A literal parsed document in one original text block.
    Text {
        /// Protocol content-array index.
        block: u32,
        /// Inclusive original byte start.
        start: usize,
        /// Exclusive original byte end.
        end: usize,
        /// Discovery method.
        method: TextExtractionMethod,
    },
}

/// A distinct canonical value and all retained occurrences.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractedCandidate {
    document: JsonDocument,
    sources: Vec<ExtractionSource>,
}
impl ExtractedCandidate {
    /// Immutable strict JSON value and content identity.
    pub fn document(&self) -> &JsonDocument {
        &self.document
    }
    /// All bounded original locations, in deterministic discovery order.
    pub fn sources(&self) -> &[ExtractionSource] {
        &self.sources
    }
}

/// Completion and selection state, distinct from protocol or tool success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtractionStatus {
    /// One value was selected under the frozen rule.
    Selected,
    /// No candidate matches the frozen rule.
    None,
    /// Several distinct values match, with no justified unique selection.
    Ambiguous,
    /// Inspection was incomplete; no selection is published.
    Exhausted,
}
/// Bounded parse evidence, separate from graph-visible failure classifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtractionDiagnostic {
    /// Protocol text-block index.
    pub block: u32,
    /// Inclusive attempted JSON byte start.
    pub start: usize,
    /// Exclusive attempted JSON byte end.
    pub end: usize,
    /// Stable redacted category, with no submitted text.
    pub category: &'static str,
}
/// Monotonic deterministic resource charges, suitable for bounded diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExtractionUsage {
    /// Original text bytes inspected.
    pub inspected_bytes: usize,
    /// Scan, parse, sorting and value-encoding work units.
    pub work: usize,
    /// Total bytes submitted to strict candidate parsing.
    pub parsed_bytes: usize,
    /// Conservative cumulative derived-allocation charges.
    pub allocation_bytes: usize,
    /// Reserved extracted representation, including selected-data duplication.
    pub output_bytes: usize,
    /// Reserved detailed parse-diagnostic storage.
    pub diagnostic_bytes: usize,
}

/// Immutable extraction decision. Exhaustion always withholds selected data.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtractionReport {
    candidates: Vec<ExtractedCandidate>,
    selected: Option<usize>,
    status: ExtractionStatus,
    exhausted_limit: Option<&'static str>,
    usage: ExtractionUsage,
    output_limit: usize,
    diagnostics: Vec<ExtractionDiagnostic>,
}
impl ExtractionReport {
    /// Distinct candidates; index `n` has identifier `candidate_n`.
    pub fn candidates(&self) -> &[ExtractedCandidate] {
        &self.candidates
    }
    /// Complete deterministic selection state.
    pub fn status(&self) -> ExtractionStatus {
        self.status
    }
    /// Exhausted allowance, independently of no JSON being found.
    pub fn exhausted_limit(&self) -> Option<&'static str> {
        self.exhausted_limit
    }
    /// Frozen selected candidate index, if any.
    pub fn selected_index(&self) -> Option<usize> {
        self.selected
    }
    /// Normalized data, preserving present JSON null versus absent selection.
    pub fn data(&self) -> Option<&Value> {
        self.selected
            .map(|index| self.candidates[index].document.value())
    }
    /// Cumulative charges; failed reservations do not perform the rejected work.
    pub fn usage(&self) -> ExtractionUsage {
        self.usage
    }
    /// Protected detailed evidence; callers retain source access restrictions.
    pub fn diagnostics(&self) -> &[ExtractionDiagnostic] {
        &self.diagnostics
    }

    /// Builds the canonical envelope's extraction record under both host ceilings.
    ///
    /// # Errors
    /// Returns codec/representation bounds. An allowance smaller than even an
    /// exhausted report cannot publish that report as an accepted envelope.
    pub fn to_value(&self, limits: &Limits) -> Result<Value, ExtractionError> {
        let mut candidates = Vec::new();
        candidates
            .try_reserve_exact(self.candidates.len())
            .map_err(|_| ExtractionError::AllocationFailed)?;
        for (index, candidate) in self.candidates.iter().enumerate() {
            let mut sources = Vec::new();
            sources
                .try_reserve_exact(candidate.sources.len())
                .map_err(|_| ExtractionError::AllocationFailed)?;
            for source in &candidate.sources {
                sources.push(source_value(*source)?);
            }
            candidates.push(object(vec![
                ("id", Value::Text(format!("candidate_{index}"))),
                ("value", candidate.document.value().clone()),
                ("sources", Value::Array(sources)),
                ("validation", Value::Text("parsed".into())),
            ])?);
        }
        let mut fields = vec![
            (
                "status",
                Value::Text(
                    match self.status {
                        ExtractionStatus::Selected => "selected",
                        ExtractionStatus::None => "none",
                        ExtractionStatus::Ambiguous => "ambiguous",
                        ExtractionStatus::Exhausted => "exhausted",
                    }
                    .into(),
                ),
            ),
            ("candidates", Value::Array(candidates)),
        ];
        if let Some(index) = self.selected {
            fields.push((
                "selected_candidate_id",
                Value::Text(format!("candidate_{index}")),
            ));
        }
        if let Some(limit) = self.exhausted_limit {
            fields.push(("exhausted_limit", Value::Text(limit.into())));
        }
        let value = object(fields)?;
        let effective = Limits {
            max_document_bytes: limits.max_document_bytes.min(self.output_limit),
            max_depth: limits.max_depth,
        };
        cbor::encode(&value, &effective)?;
        Ok(value)
    }
}

/// Extracts existing JSON only after complete protocol admission, without effects.
/// All budgets are cumulative across structured content and every text block.
///
/// # Errors
/// Returns invalid protocol/policy, unavailable schema selection, native engine,
/// or allocation failures. Configured extraction exhaustion is an explicit report.
pub fn extract_mcp_json(
    document: &JsonDocument,
    policy: &ExtractionPolicy,
    schemas: Option<&NativeSchemas>,
    limits: &Limits,
) -> Result<ExtractionReport, ExtractionError> {
    policy.to_value(limits).map_err(ExtractionError::Policy)?;
    crate::validate_mcp_tool_result(document, limits).map_err(ExtractionError::Protocol)?;
    if let CandidateSelection::Schema(schema) = &policy.selection {
        let schemas = schemas.ok_or(ExtractionError::SchemaUnavailable)?;
        if !schemas.has_schema_type(schema) {
            return Err(ExtractionError::Schema(NativeSchemaError::UnknownRoot));
        }
    }
    let mut state = State {
        policy,
        codec: limits,
        ids: BTreeMap::new(),
        report: ExtractionReport {
            candidates: Vec::new(),
            selected: None,
            status: ExtractionStatus::None,
            exhausted_limit: None,
            usage: ExtractionUsage::default(),
            output_limit: policy.limits.output_bytes,
            diagnostics: Vec::new(),
        },
    };
    if !state.reserve("output_bytes", 512) {
        return Ok(state.report);
    }
    let Value::Map(protocol) = document.value() else {
        unreachable!("admitted protocol")
    };
    if let Some(value) = protocol.get("structuredContent") {
        if !state.reserve_original(value, 0) {
            return Ok(state.report);
        }
        let value = JsonDocument::from_value(value, limits).map_err(ExtractionError::Json)?;
        state.retain(value, ExtractionSource::StructuredContent)?;
    }
    let Some(Value::Array(blocks)) = protocol.get("content") else {
        unreachable!("admitted protocol")
    };
    for (index, block) in blocks.iter().enumerate() {
        if state.exhausted() {
            break;
        }
        let Value::Map(block) = block else {
            unreachable!("admitted content")
        };
        if block.get("type") != Some(&Value::Text("text".into())) {
            continue;
        }
        let Some(Value::Text(text)) = block.get("text") else {
            unreachable!("admitted text")
        };
        if !state.reserve("inspected_bytes", text.len()) {
            break;
        }
        let index = u32::try_from(index)
            .map_err(|_| ExtractionError::Policy(McpResultError::Shape("content block index")))?;
        state.text(text, index)?;
    }
    if !state.exhausted() {
        state.select(schemas)?;
    }
    Ok(state.report)
}

struct State<'a> {
    policy: &'a ExtractionPolicy,
    codec: &'a Limits,
    ids: BTreeMap<Digest, usize>,
    report: ExtractionReport,
}
impl State<'_> {
    fn reserve_original(&mut self, value: &Value, depth: usize) -> bool {
        if depth > self.policy.limits.depth.min(self.codec.max_depth) {
            self.exhaust("depth");
            return false;
        }
        if !self.reserve("work", 1) || !self.reserve("allocation_bytes", 128) {
            return false;
        }
        match value {
            Value::Text(value) => {
                self.reserve("work", value.len())
                    && self.reserve("allocation_bytes", value.len().saturating_mul(16))
            }
            Value::Array(values) => values
                .iter()
                .all(|value| self.reserve_original(value, depth + 1)),
            Value::Map(values) => values.iter().all(|(key, value)| {
                self.reserve("work", key.len())
                    && self.reserve(
                        "allocation_bytes",
                        key.len().saturating_mul(16).saturating_add(128),
                    )
                    && self.reserve_original(value, depth + 1)
            }),
            _ => true,
        }
    }
    fn exhausted(&self) -> bool {
        self.report.exhausted_limit.is_some()
    }
    fn exhaust(&mut self, limit: &'static str) {
        if !self.exhausted() {
            self.report.exhausted_limit = Some(limit);
        }
        self.report.selected = None;
        self.report.status = ExtractionStatus::Exhausted;
    }
    fn reserve(&mut self, name: &'static str, amount: usize) -> bool {
        if self.exhausted() {
            return false;
        }
        let l = &self.policy.limits;
        let (used, maximum) = match name {
            "inspected_bytes" => (&mut self.report.usage.inspected_bytes, l.inspected_bytes),
            "work" => (&mut self.report.usage.work, l.work),
            "parsed_bytes" => (&mut self.report.usage.parsed_bytes, l.parsed_bytes),
            "allocation_bytes" => (&mut self.report.usage.allocation_bytes, l.allocation_bytes),
            "output_bytes" => (&mut self.report.usage.output_bytes, l.output_bytes),
            "diagnostic_bytes" => (&mut self.report.usage.diagnostic_bytes, l.diagnostic_bytes),
            _ => unreachable!("fixed extraction counter"),
        };
        if let Some(next) = used.checked_add(amount).filter(|next| *next <= maximum) {
            *used = next;
            true
        } else {
            self.exhaust(name);
            false
        }
    }
    fn parse(
        &mut self,
        text: &str,
        block: u32,
        start: usize,
        end: usize,
        method: TextExtractionMethod,
    ) -> Result<bool, ExtractionError> {
        if !self.reserve("work", end - start) {
            return Ok(false);
        }
        let (start, end) = trim_span(text, start, end);
        if start == end || self.exhausted() {
            return Ok(false);
        }
        let length = end - start;
        if !self.reserve("parsed_bytes", length)
            || !self.reserve("work", length.saturating_mul(4))
            || !self.reserve(
                "allocation_bytes",
                length.saturating_mul(64).saturating_add(256),
            )
        {
            return Ok(false);
        }
        let limits = Limits {
            max_document_bytes: self
                .codec
                .max_document_bytes
                .min(self.policy.limits.output_bytes),
            max_depth: self.codec.max_depth.min(self.policy.limits.depth),
        };
        match JsonDocument::new(&text.as_bytes()[start..end], &limits) {
            Ok(document) => self.retain(
                document,
                ExtractionSource::Text {
                    block,
                    start,
                    end,
                    method,
                },
            ),
            Err(JsonError::LimitExceeded {
                limit: LimitKind::Depth,
                ..
            }) => {
                self.exhaust("depth");
                Ok(false)
            }
            Err(JsonError::LimitExceeded { .. }) => {
                self.exhaust("output_bytes");
                Ok(false)
            }
            Err(JsonError::AllocationFailed) => Err(ExtractionError::AllocationFailed),
            Err(JsonError::Codec(error)) => Err(error.into()),
            Err(
                error @ (JsonError::Syntax { .. }
                | JsonError::DuplicateKey { .. }
                | JsonError::UnsafeNumber
                | JsonError::InvalidUtf8 { .. }),
            ) => {
                let category = match error {
                    JsonError::DuplicateKey { .. } => "duplicate_key",
                    JsonError::UnsafeNumber => "unsafe_number",
                    _ => "invalid_json",
                };
                if self.reserve("diagnostic_bytes", 128) && self.reserve("allocation_bytes", 256) {
                    self.report
                        .diagnostics
                        .try_reserve(1)
                        .map_err(|_| ExtractionError::AllocationFailed)?;
                    self.report.diagnostics.push(ExtractionDiagnostic {
                        block,
                        start,
                        end,
                        category,
                    });
                }
                Ok(false)
            }
            Err(error) => Err(ExtractionError::Json(error)),
        }
    }
    fn retain(
        &mut self,
        document: JsonDocument,
        source: ExtractionSource,
    ) -> Result<bool, ExtractionError> {
        if self.exhausted() {
            return Ok(false);
        }
        if !self.reserve("work", document.as_bytes().len()) {
            return Ok(false);
        }
        let id = document.digest();
        if !self.reserve(
            "work",
            (usize::BITS - self.ids.len().leading_zeros()).max(1) as usize,
        ) {
            return Ok(false);
        }
        if let Some(index) = self.ids.get(&id).copied() {
            if !self.reserve("work", self.report.candidates[index].sources.len()) {
                return Ok(false);
            }
            if self.report.candidates[index].document.as_bytes() != document.as_bytes() {
                return Err(ExtractionError::Policy(McpResultError::Shape(
                    "candidate digest collision",
                )));
            }
            if self.report.candidates[index].sources.contains(&source) {
                return Ok(true);
            }
            if !self.reserve("allocation_bytes", 512) || !self.reserve("output_bytes", 256) {
                return Ok(false);
            }
            self.report.candidates[index]
                .sources
                .try_reserve(1)
                .map_err(|_| ExtractionError::AllocationFailed)?;
            self.report.candidates[index].sources.push(source);
            return Ok(true);
        }
        if self.report.candidates.len() >= self.policy.limits.candidates {
            self.exhaust("candidates");
            return Ok(false);
        }
        if !self.reserve(
            "allocation_bytes",
            document
                .as_bytes()
                .len()
                .saturating_mul(64)
                .saturating_add(1024),
        ) || !self.reserve(
            "output_bytes",
            document
                .as_bytes()
                .len()
                .saturating_mul(8)
                .saturating_add(512),
        ) {
            return Ok(false);
        }
        let index = self.report.candidates.len();
        self.report
            .candidates
            .try_reserve(1)
            .map_err(|_| ExtractionError::AllocationFailed)?;
        self.report.candidates.push(ExtractedCandidate {
            document,
            sources: vec![source],
        });
        self.ids.insert(id, index);
        Ok(true)
    }
    fn text(&mut self, text: &str, block: u32) -> Result<(), ExtractionError> {
        if self.parse(text, block, 0, text.len(), TextExtractionMethod::WholeBlock)?
            || self.exhausted()
        {
            return Ok(());
        }
        let mut claimed = Vec::new();
        let mut fence: Option<(u8, usize, usize)> = None;
        let mut offset = 0;
        if !self.reserve("work", text.len().saturating_mul(6)) {
            return Ok(());
        }
        for line in text.split_inclusive('\n') {
            let trimmed = line.trim_matches(|c| matches!(c, ' ' | '\t' | '\r' | '\n'));
            if let Some((character, count, start)) = fence {
                let closing = trimmed
                    .bytes()
                    .take_while(|byte| *byte == character)
                    .count();
                if closing >= count && trimmed[closing..].is_empty() {
                    if self.parse(text, block, start, offset, TextExtractionMethod::Fence)? {
                        if !self.reserve("allocation_bytes", 32) {
                            return Ok(());
                        }
                        claimed
                            .try_reserve(1)
                            .map_err(|_| ExtractionError::AllocationFailed)?;
                        claimed.push((start, offset));
                    }
                    fence = None;
                }
            } else if let Some(character @ (b'`' | b'~')) = trimmed.as_bytes().first().copied() {
                let count = trimmed
                    .bytes()
                    .take_while(|byte| *byte == character)
                    .count();
                let info = trimmed[count..].trim();
                if count >= 3 && (info.is_empty() || info.eq_ignore_ascii_case("json")) {
                    fence = Some((character, count, offset + line.len()));
                }
            }
            offset += line.len();
            if self.exhausted() {
                return Ok(());
            }
        }
        self.embedded(text, block, &claimed)
    }
    fn embedded(
        &mut self,
        text: &str,
        block: u32,
        claimed: &[(usize, usize)],
    ) -> Result<(), ExtractionError> {
        let mut stack = Vec::new();
        let mut spans = Vec::new();
        let mut quoted = false;
        let mut escaped = false;
        let mut region = 0;
        let mut position = 0;
        while position < text.len() {
            if region < claimed.len() && position >= claimed[region].0 {
                position = claimed[region].1;
                region += 1;
                quoted = false;
                escaped = false;
                stack.clear();
                continue;
            }
            if !self.reserve("work", 1) {
                return Ok(());
            }
            let byte = text.as_bytes()[position];
            if quoted {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quoted = false;
                }
            } else {
                match byte {
                    b'"' => quoted = true,
                    b'{' | b'[' => {
                        if stack.len() > self.policy.limits.depth.min(self.codec.max_depth) {
                            self.exhaust("depth");
                            return Ok(());
                        }
                        if !self.reserve("allocation_bytes", 32) {
                            return Ok(());
                        }
                        stack
                            .try_reserve(1)
                            .map_err(|_| ExtractionError::AllocationFailed)?;
                        stack.push((byte, position));
                    }
                    b'}' | b']' => {
                        if let Some((open, start)) = stack.pop() {
                            if (open == b'{' && byte == b'}') || (open == b'[' && byte == b']') {
                                if !self.reserve("allocation_bytes", 32) {
                                    return Ok(());
                                }
                                spans
                                    .try_reserve(1)
                                    .map_err(|_| ExtractionError::AllocationFailed)?;
                                spans.push((start, position + 1));
                            } else {
                                stack.clear();
                            }
                        }
                    }
                    _ => (),
                }
            }
            position += 1;
        }
        let sorting_work = spans
            .len()
            .saturating_mul((usize::BITS - spans.len().leading_zeros()) as usize)
            .saturating_mul(2);
        if !self.reserve("work", sorting_work) {
            return Ok(());
        }
        spans.sort_unstable_by(|left, right| {
            left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1))
        });
        let mut enclosing_end = 0;
        for (start, end) in spans {
            if start < enclosing_end {
                continue;
            }
            if self.parse(text, block, start, end, TextExtractionMethod::Embedded)? {
                enclosing_end = end;
            }
            if self.exhausted() {
                break;
            }
        }
        Ok(())
    }
    fn select(&mut self, schemas: Option<&NativeSchemas>) -> Result<(), ExtractionError> {
        let mut selected = None;
        let mut count = 0;
        for index in 0..self.report.candidates.len() {
            if !self.reserve(
                "work",
                self.report.candidates[index]
                    .sources
                    .len()
                    .saturating_add(1),
            ) {
                return Ok(());
            }
            if matches!(self.policy.selection, CandidateSelection::Schema(_))
                && !self.reserve(
                    "work",
                    self.report.candidates[index]
                        .document
                        .as_bytes()
                        .len()
                        .saturating_mul(4),
                )
            {
                return Ok(());
            }
            let candidate = &self.report.candidates[index];
            let matches = match &self.policy.selection {
                CandidateSelection::Unique => {
                    if candidate.sources.contains(&ExtractionSource::StructuredContent) {
                        self.report.selected = Some(index); self.report.status = ExtractionStatus::Selected; return Ok(());
                    }
                    true
                }
                CandidateSelection::Candidate(id) => *id == format!("candidate_{index}"),
                CandidateSelection::Block(block) => candidate.sources.iter().any(|source| matches!(source, ExtractionSource::Text { block: origin, .. } if origin == block)),
                CandidateSelection::Schema(schema) => schemas.ok_or(ExtractionError::SchemaUnavailable)?
                    .validate_schema_document(schema, &candidate.document, self.codec).map_err(ExtractionError::Schema)?,
            };
            if matches {
                selected = Some(index);
                count += 1;
            }
        }
        self.report.status = match count {
            0 => ExtractionStatus::None,
            1 => ExtractionStatus::Selected,
            _ => ExtractionStatus::Ambiguous,
        };
        self.report.selected = if count == 1 { selected } else { None };
        Ok(())
    }
}

fn trim_span(text: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    while start < end && matches!(text.as_bytes()[start], b' ' | b'\t' | b'\r' | b'\n') {
        start += 1;
    }
    while end > start && matches!(text.as_bytes()[end - 1], b' ' | b'\t' | b'\r' | b'\n') {
        end -= 1;
    }
    (start, end)
}
fn source_value(source: ExtractionSource) -> Result<Value, ExtractionError> {
    Ok(match source {
        ExtractionSource::StructuredContent => object(vec![
            ("origin", Value::Text("structured_content".into())),
            ("method", Value::Text("structured_content".into())),
        ])?,
        ExtractionSource::Text {
            block,
            start,
            end,
            method,
        } => {
            object(vec![
                ("origin", Value::Text("text".into())),
                (
                    "method",
                    Value::Text(
                        match method {
                            TextExtractionMethod::WholeBlock => "whole_block",
                            TextExtractionMethod::Fence => "fence",
                            TextExtractionMethod::Embedded => "embedded",
                        }
                        .into(),
                    ),
                ),
                ("content_block_index", Value::Integer(i64::from(block))),
                (
                    "byte_start",
                    Value::Integer(i64::try_from(start).map_err(|_| {
                        ExtractionError::Policy(McpResultError::Shape("source span"))
                    })?),
                ),
                (
                    "byte_end",
                    Value::Integer(i64::try_from(end).map_err(|_| {
                        ExtractionError::Policy(McpResultError::Shape("source span"))
                    })?),
                ),
            ])?
        }
    })
}
fn object(fields: Vec<(&str, Value)>) -> Result<Value, ExtractionError> {
    Ok(Value::Map(Map::try_from_entries(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
    )?))
}
