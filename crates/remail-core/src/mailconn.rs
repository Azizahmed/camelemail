//! One account's mail connection: IMAP, or Microsoft Graph for Microsoft
//! mailboxes that refuse IMAP (see `graphmail`). Sync talks to either the
//! same way.

use std::collections::HashMap;

use anyhow::Result;

use crate::graphmail::GraphMail;
use crate::imap::{self, FetchedMessage, ImapConn, MailboxStatus, MsgFlags, RemoteFolder};

pub enum MailConn {
    Imap(ImapConn),
    Graph(GraphMail),
}

impl MailConn {
    /// Changes since a MODSEQ can be asked for.
    pub fn condstore(&self) -> bool {
        match self {
            MailConn::Imap(c) => c.caps.condstore,
            MailConn::Graph(_) => true,
        }
    }

    pub async fn logout(self) {
        if let MailConn::Imap(c) = self {
            c.logout().await;
        }
    }

    pub async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>> {
        match self {
            MailConn::Imap(c) => c.list_folders().await,
            MailConn::Graph(g) => g.list_folders().await,
        }
    }

    pub async fn select(&mut self, path: &str) -> Result<MailboxStatus> {
        match self {
            MailConn::Imap(c) => c.select(path).await,
            MailConn::Graph(g) => g.select(path).await,
        }
    }

    pub async fn status_unseen(&mut self, path: &str) -> Result<u32> {
        match self {
            MailConn::Imap(c) => c.status_unseen(path).await,
            MailConn::Graph(g) => g.status_unseen(path).await,
        }
    }

    pub async fn search(&mut self, path: &str, criteria: &str) -> Result<Vec<u32>> {
        match self {
            MailConn::Imap(c) => c.search(path, criteria).await,
            MailConn::Graph(g) => g.search(path, criteria).await,
        }
    }

    pub async fn fetch_messages(&mut self, path: &str, uids: &[u32]) -> Result<Vec<FetchedMessage>> {
        match self {
            MailConn::Imap(c) => c.fetch_messages(path, uids).await,
            MailConn::Graph(g) => g.fetch_messages(path, uids).await,
        }
    }

    pub async fn fetch_raw(&mut self, path: &str, uid: u32) -> Result<Option<Vec<u8>>> {
        match self {
            MailConn::Imap(c) => c.fetch_raw(path, uid).await,
            MailConn::Graph(g) => g.fetch_raw(path, uid).await,
        }
    }

    pub async fn fetch_flags(&mut self, path: &str, uids: &[u32], changed_since: Option<u64>) -> Result<Vec<(u32, MsgFlags)>> {
        match self {
            MailConn::Imap(c) => c.fetch_flags(path, uids, changed_since).await,
            MailConn::Graph(g) => g.fetch_flags(path, uids, changed_since).await,
        }
    }

    pub async fn existing_uids(&mut self, path: &str, uids: &[u32]) -> Result<Vec<u32>> {
        match self {
            MailConn::Imap(c) => c.existing_uids(path, uids).await,
            MailConn::Graph(g) => g.existing_uids(path, uids).await,
        }
    }

    pub async fn store_flags(&mut self, path: &str, uids: &[u32], add: bool, flag: &str) -> Result<()> {
        match self {
            MailConn::Imap(c) => c.store_flags(path, uids, add, flag).await,
            MailConn::Graph(g) => g.store_flags(path, uids, add, flag).await,
        }
    }

    pub async fn move_uids(&mut self, path: &str, uids: &[u32], dest: &str) -> Result<()> {
        match self {
            MailConn::Imap(c) => c.move_uids(path, uids, dest).await,
            MailConn::Graph(g) => g.move_uids(path, uids, dest).await,
        }
    }

    pub async fn delete_uids(&mut self, path: &str, uids: &[u32]) -> Result<()> {
        match self {
            MailConn::Imap(c) => c.delete_uids(path, uids).await,
            MailConn::Graph(g) => g.delete_uids(path, uids).await,
        }
    }

    pub async fn append(&mut self, path: &str, flags: &str, raw: &[u8]) -> Result<()> {
        match self {
            MailConn::Imap(c) => c.append(path, flags, raw).await,
            MailConn::Graph(g) => g.append(path, flags, raw).await,
        }
    }

    pub async fn find_by_message_ids(&mut self, path: &str, message_ids: &[String]) -> Result<HashMap<String, u32>> {
        match self {
            MailConn::Imap(c) => c.find_by_message_ids(path, message_ids).await,
            MailConn::Graph(g) => g.find_by_message_ids(path, message_ids).await,
        }
    }

    /// `name` as the user sees it.
    pub async fn create_folder(&mut self, name: &str) -> Result<()> {
        match self {
            MailConn::Imap(c) => c.create_folder(&imap::encode_utf7(name)).await,
            MailConn::Graph(g) => g.create_folder(name).await,
        }
    }
}
