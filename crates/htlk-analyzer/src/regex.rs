//! Shared pinned HTLK literal-regex admission, independent of runtime metering.

/// Finite native regex compiler controls (not JSON Schema pattern semantics).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegexOptions {
    /// Maximum UTF-8 pattern bytes.
    pub max_pattern_bytes: usize,
    /// Native compiled representation and DFA cache ceiling.
    pub max_compiled_bytes: usize,
}
impl Default for RegexOptions {
    fn default() -> Self {
        Self {
            max_pattern_bytes: 64 * 1024,
            max_compiled_bytes: 2 * 1024 * 1024,
        }
    }
}
impl RegexOptions {
    /// Uses the exact regex ceilings from a document's evaluator policy.
    pub fn from_policy(policy: &htlk_executable::EvaluatorLimits) -> Self {
        Self {
            max_pattern_bytes: usize::try_from(policy.max_regex_bytes).unwrap_or(usize::MAX),
            max_compiled_bytes: usize::try_from(policy.max_regex_compiled_bytes)
                .unwrap_or(usize::MAX),
        }
    }
}

/// Redacted literal admission failure; native errors never retain pattern text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegexError {
    /// Unsupported syntax or noncanonical flags.
    Syntax,
    /// Pattern byte ceiling exceeded.
    PatternLimit,
    /// Native compiler representation ceiling exceeded.
    CompiledLimit,
    /// A compiler ceiling is zero.
    InvalidOptions,
}
impl std::fmt::Display for RegexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Syntax => "The regex syntax or flags are not supported by the pinned engine.",
            Self::PatternLimit => "The regex pattern exceeds its byte ceiling.",
            Self::CompiledLimit => "The regex exceeds its compiled representation ceiling.",
            Self::InvalidOptions => "Regex compiler ceilings must be positive.",
        })
    }
}
impl std::error::Error for RegexError {}

/// Validates a literal using the same native construction rules as runtime.
/// Compiled state is private and discarded; callers own work charging/caching.
///
/// # Errors
/// Returns redacted syntax, configuration or compiler-size failures.
pub fn validate_regex(pattern: &str, flags: &str, options: RegexOptions) -> Result<(), RegexError> {
    if options.max_pattern_bytes == 0 || options.max_compiled_bytes == 0 {
        return Err(RegexError::InvalidOptions);
    }
    if pattern.len() > options.max_pattern_bytes {
        return Err(RegexError::PatternLimit);
    }
    if !matches!(flags, "" | "i" | "m" | "s" | "im" | "is" | "ms" | "ims") {
        return Err(RegexError::Syntax);
    }
    regex::RegexBuilder::new(pattern)
        .case_insensitive(flags.contains('i'))
        .multi_line(flags.contains('m'))
        .dot_matches_new_line(flags.contains('s'))
        .size_limit(options.max_compiled_bytes)
        .dfa_size_limit(options.max_compiled_bytes)
        .build()
        .map_err(|error| match error {
            regex::Error::CompiledTooBig(_) => RegexError::CompiledLimit,
            _ => RegexError::Syntax,
        })?;
    Ok(())
}
