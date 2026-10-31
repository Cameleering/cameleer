//! Error type. Camel uses Java exception classes; rust-camel models an exception as
//! a `kind` (the exception "class" name, e.g. `"IOException"`), a message and an
//! optional cause. Exception hierarchy (for `onException` matching) is modelled by
//! [`kind_is_a`].

use std::fmt;

pub type Result<T> = std::result::Result<T, CamelError>;

/// Well known exception kinds (mirroring Camel/Java exception class names).
pub mod kinds {
    pub const EXCEPTION: &str = "Exception";
    pub const RUNTIME: &str = "RuntimeException";
    pub const ILLEGAL_ARGUMENT: &str = "IllegalArgumentException";
    pub const ILLEGAL_STATE: &str = "IllegalStateException";
    pub const IO: &str = "IOException";
    pub const FILE_NOT_FOUND: &str = "FileNotFoundException";
    pub const CONNECT: &str = "ConnectException";
    pub const SOCKET_TIMEOUT: &str = "SocketTimeoutException";
    pub const NUMBER_FORMAT: &str = "NumberFormatException";
    pub const NULL_POINTER: &str = "NullPointerException";
    pub const UNSUPPORTED_OPERATION: &str = "UnsupportedOperationException";
    pub const TIMEOUT: &str = "TimeoutException";
    pub const INTERRUPTED: &str = "InterruptedException";
    pub const CAMEL_EXCHANGE: &str = "CamelExchangeException";
    pub const CAMEL_EXECUTION: &str = "CamelExecutionException";
    pub const NO_SUCH_ENDPOINT: &str = "NoSuchEndpointException";
    pub const RESOLVE_ENDPOINT_FAILED: &str = "ResolveEndpointFailedException";
    pub const NO_SUCH_LANGUAGE: &str = "NoSuchLanguageException";
    pub const NO_SUCH_BEAN: &str = "NoSuchBeanException";
    pub const NO_SUCH_HEADER: &str = "NoSuchHeaderException";
    pub const NO_TYPE_CONVERSION_AVAILABLE: &str = "NoTypeConversionAvailableException";
    pub const TYPE_CONVERSION: &str = "TypeConversionException";
    pub const EXPRESSION_EVALUATION: &str = "ExpressionEvaluationException";
    pub const EXPRESSION_ILLEGAL_SYNTAX: &str = "ExpressionIllegalSyntaxException";
    pub const DIRECT_CONSUMER_NOT_AVAILABLE: &str = "DirectConsumerNotAvailableException";
    pub const EXCHANGE_TIMED_OUT: &str = "ExchangeTimedOutException";
    pub const ROLLBACK_EXCHANGE: &str = "RollbackExchangeException";
    pub const VALIDATION: &str = "ValidationException";
    pub const PREDICATE_VALIDATION: &str = "PredicateValidationException";
    pub const FAILED_TO_CREATE_ROUTE: &str = "FailedToCreateRouteException";
    pub const FAILED_TO_START_ROUTE: &str = "FailedToStartRouteException";
    pub const REJECTED_EXECUTION: &str = "RejectedExecutionException";
    pub const CAMEL_EXCHANGE_FULL: &str = "IllegalStateException";
    pub const HTTP_OPERATION_FAILED: &str = "HttpOperationFailedException";
    pub const CALL_NOT_PERMITTED: &str = "CallNotPermittedException";
    pub const THROTTLER_REJECTED: &str = "ThrottlerRejectedExecutionException";
    pub const CAMEL_AUTHORIZATION: &str = "CamelAuthorizationException";
    pub const NO_MESSAGE_ID: &str = "NoMessageIdException";
    pub const PARSE: &str = "ParseException";
}

/// Parent "class" of a well known kind. Unknown kinds are treated as children of
/// `RuntimeException`. Additional parents can be registered at runtime via
/// [`register_kind_parent`].
fn builtin_parent(kind: &str) -> Option<&'static str> {
    use kinds::*;
    Some(match kind {
        "Throwable" => return None,
        EXCEPTION => "Throwable",
        RUNTIME | IO | TIMEOUT | INTERRUPTED | PARSE => EXCEPTION,
        FILE_NOT_FOUND | CONNECT | SOCKET_TIMEOUT => IO,
        NUMBER_FORMAT => ILLEGAL_ARGUMENT,
        ILLEGAL_ARGUMENT | ILLEGAL_STATE | NULL_POINTER | UNSUPPORTED_OPERATION | REJECTED_EXECUTION => RUNTIME,
        CAMEL_EXCHANGE | NO_SUCH_ENDPOINT | RESOLVE_ENDPOINT_FAILED | ROLLBACK_EXCHANGE | CAMEL_AUTHORIZATION
        | NO_MESSAGE_ID | DIRECT_CONSUMER_NOT_AVAILABLE | HTTP_OPERATION_FAILED => CAMEL_EXCHANGE,
        CAMEL_EXECUTION | NO_SUCH_LANGUAGE | NO_SUCH_BEAN | NO_TYPE_CONVERSION_AVAILABLE | TYPE_CONVERSION
        | EXPRESSION_EVALUATION | FAILED_TO_CREATE_ROUTE | FAILED_TO_START_ROUTE | CALL_NOT_PERMITTED => RUNTIME,
        NO_SUCH_HEADER | EXCHANGE_TIMED_OUT | VALIDATION => CAMEL_EXCHANGE,
        PREDICATE_VALIDATION => VALIDATION,
        EXPRESSION_ILLEGAL_SYNTAX => EXPRESSION_EVALUATION,
        THROTTLER_REJECTED => REJECTED_EXECUTION,
        _ => return None,
    })
}

static EXTRA_PARENTS: std::sync::RwLock<Vec<(String, String)>> = std::sync::RwLock::new(Vec::new());

/// Register a custom exception kind and its parent kind (like subclassing in Java).
pub fn register_kind_parent(kind: impl Into<String>, parent: impl Into<String>) {
    EXTRA_PARENTS.write().unwrap().push((kind.into(), parent.into()));
}

fn parent_of(kind: &str) -> Option<String> {
    if let Some(p) = EXTRA_PARENTS.read().unwrap().iter().rev().find(|(k, _)| k == kind) {
        return Some(p.1.clone());
    }
    match builtin_parent(kind) {
        Some(p) => Some(p.to_string()),
        None if kind == "Throwable" => None,
        None => Some(kinds::RUNTIME.to_string()),
    }
}

/// Returns true if `kind` equals `target` or is a (transitive) child of it.
pub fn kind_is_a(kind: &str, target: &str) -> bool {
    let mut cur = Some(kind.to_string());
    let mut guard = 0;
    while let Some(k) = cur {
        if k == target {
            return true;
        }
        guard += 1;
        if guard > 32 {
            break;
        }
        cur = parent_of(&k);
    }
    false
}

/// Distance in the hierarchy from `kind` up to `target` (0 = exact). None if unrelated.
pub fn kind_distance(kind: &str, target: &str) -> Option<usize> {
    let mut cur = Some(kind.to_string());
    let mut d = 0;
    while let Some(k) = cur {
        if k == target {
            return Some(d);
        }
        d += 1;
        if d > 32 {
            break;
        }
        cur = parent_of(&k);
    }
    None
}

/// The rust-camel error ("exception") type.
#[derive(Clone, Debug, PartialEq)]
pub struct CamelError {
    pub kind: String,
    pub message: String,
    pub cause: Option<Box<CamelError>>,
}

impl CamelError {
    pub fn new(kind: impl Into<String>, message: impl Into<String>) -> Self {
        CamelError { kind: kind.into(), message: message.into(), cause: None }
    }

    pub fn with_cause(mut self, cause: CamelError) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self::new(kinds::RUNTIME, message)
    }
    pub fn illegal_argument(message: impl Into<String>) -> Self {
        Self::new(kinds::ILLEGAL_ARGUMENT, message)
    }
    pub fn illegal_state(message: impl Into<String>) -> Self {
        Self::new(kinds::ILLEGAL_STATE, message)
    }
    pub fn io(message: impl Into<String>) -> Self {
        Self::new(kinds::IO, message)
    }
    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(kinds::TIMEOUT, message)
    }
    pub fn no_such_endpoint(uri: &str) -> Self {
        Self::new(kinds::NO_SUCH_ENDPOINT, format!("No endpoint could be found for: {}, please check your classpath contains the needed Camel component jar.", uri))
    }
    pub fn resolve_endpoint_failed(uri: &str, reason: impl fmt::Display) -> Self {
        Self::new(kinds::RESOLVE_ENDPOINT_FAILED, format!("Failed to resolve endpoint: {} due to: {}", uri, reason))
    }
    pub fn no_such_language(name: &str) -> Self {
        Self::new(kinds::NO_SUCH_LANGUAGE, format!("No language could be found for: {}", name))
    }
    pub fn no_such_bean(name: &str) -> Self {
        Self::new(kinds::NO_SUCH_BEAN, format!("No bean could be found in the registry for: {}", name))
    }
    pub fn expression(message: impl Into<String>) -> Self {
        Self::new(kinds::EXPRESSION_EVALUATION, message)
    }
    pub fn syntax(message: impl Into<String>) -> Self {
        Self::new(kinds::EXPRESSION_ILLEGAL_SYNTAX, message)
    }

    /// True if this error's kind is `target` or a sub-kind of it.
    pub fn is_a(&self, target: &str) -> bool {
        kind_is_a(&self.kind, target)
    }

    /// Iterate over this error and its causes (outermost first).
    pub fn chain(&self) -> impl Iterator<Item = &CamelError> {
        let mut cur = Some(self);
        std::iter::from_fn(move || {
            let c = cur?;
            cur = c.cause.as_deref();
            Some(c)
        })
    }

    /// The innermost cause.
    pub fn root_cause(&self) -> &CamelError {
        self.chain().last().unwrap()
    }
}

impl fmt::Display for CamelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)?;
        if let Some(c) = &self.cause {
            write!(f, " (caused by {})", c)?;
        }
        Ok(())
    }
}

impl std::error::Error for CamelError {}

impl From<std::io::Error> for CamelError {
    fn from(e: std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        let kind = match e.kind() {
            K::NotFound => kinds::FILE_NOT_FOUND,
            K::ConnectionRefused => kinds::CONNECT,
            K::TimedOut | K::WouldBlock => kinds::SOCKET_TIMEOUT,
            _ => kinds::IO,
        };
        CamelError::new(kind, e.to_string())
    }
}

impl From<String> for CamelError {
    fn from(s: String) -> Self {
        CamelError::runtime(s)
    }
}

impl From<&str> for CamelError {
    fn from(s: &str) -> Self {
        CamelError::runtime(s)
    }
}

impl From<std::num::ParseIntError> for CamelError {
    fn from(e: std::num::ParseIntError) -> Self {
        CamelError::new(kinds::NUMBER_FORMAT, e.to_string())
    }
}

impl From<std::num::ParseFloatError> for CamelError {
    fn from(e: std::num::ParseFloatError) -> Self {
        CamelError::new(kinds::NUMBER_FORMAT, e.to_string())
    }
}

impl From<std::string::FromUtf8Error> for CamelError {
    fn from(e: std::string::FromUtf8Error) -> Self {
        CamelError::new(kinds::TYPE_CONVERSION, e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hierarchy() {
        assert!(kind_is_a("FileNotFoundException", "IOException"));
        assert!(kind_is_a("FileNotFoundException", "Exception"));
        assert!(!kind_is_a("IOException", "RuntimeException"));
        assert!(kind_is_a("MyCustomError", "RuntimeException"));
        register_kind_parent("MyIoError", "IOException");
        assert!(kind_is_a("MyIoError", "IOException"));
        assert_eq!(kind_distance("FileNotFoundException", "Exception"), Some(2));
    }
}
