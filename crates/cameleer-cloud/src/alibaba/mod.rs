//! Alibaba Cloud components.
//!
//! | scheme | module | service | auth |
//! |---|---|---|---|
//! | `alibaba-sls` | [`sls`] | Simple Log Service REST API: PutLogs (protobuf `LogGroup`, lz4 or deflate), GetLogs, ListLogStores | SLS signature V1 (HMAC-SHA1) or V4 (`SLS4-HMAC-SHA256`), AccessKey pair with optional STS token |

pub mod sls;
pub mod sls_proto;
pub mod sls_sign;
