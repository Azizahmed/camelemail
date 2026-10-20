//! Add an account to a Remail profile from the command line and sync it once.
//!
//! Usage: add_account <data-dir> <account.json>
//! (credentials go to <data-dir>/credentials.json, as with REMAIL_DATA_DIR)

use std::sync::Arc;

use remail_core::secrets::FileStore;
use remail_core::{Engine, EngineEvent, EventSink, NewAccount};

struct Log;
impl EventSink for Log {
    fn emit(&self, e: EngineEvent) {
        if let EngineEvent::SyncStatus { error: Some(err), .. } = e {
            eprintln!("sync error: {err}");
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("data dir"));
    let account: NewAccount = serde_json::from_str(&std::fs::read_to_string(args.next().expect("account json"))?)?;
    let secrets = Arc::new(FileStore::new(dir.join("credentials.json")));
    let engine = Engine::start(dir, secrets, Arc::new(Log))?;
    let acc = engine.add_account(account).await?;
    engine.sync_and_wait(&acc.id).await?;
    println!("added {} ({})", acc.email, acc.id);
    Ok(())
}
