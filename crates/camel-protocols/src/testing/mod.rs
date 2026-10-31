//! Embedded servers for tests.
pub mod ftp_server;
pub mod mail_server;
pub use ftp_server::FtpServer;
pub use mail_server::{ImapServer, Mailbox, Pop3Server, SmtpServer, StoredMail};
