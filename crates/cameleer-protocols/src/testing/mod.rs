//! Embedded servers for tests.
pub mod ftp_server;
pub mod lumberjack_client;
pub mod mail_server;
pub use ftp_server::{FtpServer, FtpsMode};
pub use lumberjack_client::LumberjackClient;
pub use mail_server::{ImapServer, Mailbox, Pop3Server, SmtpServer, StoredMail};
