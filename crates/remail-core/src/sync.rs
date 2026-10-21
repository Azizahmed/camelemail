//! Folder synchronisation and server-side mutations for one account.
//!
//! How a folder is kept in step with the server:
//! - first contact with a folder downloads the newest N messages
//!   (100 for INBOX, 20 elsewhere); older mail is paged in on demand;
//! - afterwards each pass fetches UIDs ≥ the stored UIDNEXT, refreshes flags
//!   (CONDSTORE `CHANGEDSINCE` when available, else a full flag fetch of the
//!   local window) and drops local UIDs the server no longer has;
//! - a UIDVALIDITY change rebuilds just that folder.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::imap::{self, FetchedMessage, ImapConn, MailboxStatus};
use crate::model::{labels, Account, LabelKind};
use crate::store::{now_ms, FolderRow, FolderState, IncomingMessage, Location, Store};

pub const INITIAL_INBOX_COUNT: usize = 100;
pub const INITIAL_FOLDER_COUNT: usize = 20;
pub const LOAD_MORE_PAGE: usize = 20;
pub const MAX_USER_FOLDERS: usize = 30;
pub const BATCH_SIZE: usize = 20;
pub const SEARCH_RESULT_LIMIT: usize = 50;
pub const SEARCH_BUDGET: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub struct SyncOutcome {
    /// Rows inserted, updated or deleted.
    pub changed: usize,
    /// Newly arrived unread INBOX messages (not reported on a folder's first sync).
    pub new_inbox: Vec<String>,
    /// New mail brought snoozed conversations back (the snoozes are synced setup).
    pub snoozes_cancelled: bool,
}

/// What a folder looked like on the server when it was last fully checked.
/// While EXISTS and UIDNEXT stay the same nothing was expunged (any arrival
/// raises UIDNEXT, any removal lowers EXISTS); with CONDSTORE an unchanged
/// HIGHESTMODSEQ also means no flag changed.
#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    uid_validity: u32,
    exists: u32,
    uid_next: u32,
    highest_modseq: Option<u64>,
}

impl Snapshot {
    fn of(s: &MailboxStatus) -> Option<Snapshot> {
        (s.uid_validity > 0 && s.uid_next > 0).then_some(Snapshot {
            uid_validity: s.uid_validity,
            exists: s.exists,
            uid_next: s.uid_next,
            highest_modseq: s.highest_modseq,
        })
    }
}

/// Per-account memory of the last sync pass (kept by the account worker), so
/// passes over unchanged folders skip the costly checks. Starting empty only
/// costs one full pass.
#[derive(Debug, Default)]
pub struct SyncCache {
    /// Folder path → state at the end of its last complete sync.
    folders: HashMap<String, Snapshot>,
    /// Folder path → state and local unread count at its last STATUS.
    unread: HashMap<String, (Snapshot, i64)>,
}

/// List folders, map them to labels and persist. Returns the folders to sync
/// in order: INBOX, other system folders, then up to 30 user folders.
pub async fn refresh_folders(conn: &mut ImapConn, store: &Store, account: &Account) -> Result<Vec<FolderRow>> {
    let remote = conn.list_folders().await?;
    let mapped = imap::map_folders(&remote);
    let mut rows = Vec::new();
    let mut user_count = 0;
    for (f, label_id, is_system) in mapped {
        let virtual_folder = matches!(f.special_use, Some(imap::SKIP_FLAGGED) | Some(imap::SKIP_IMPORTANT));
        if virtual_folder {
            continue;
        }
        // "All Mail" duplicates every other folder; keep it as a move target only.
        let synced = if label_id == labels::ALL {
            false
        } else if is_system {
            true
        } else {
            user_count += 1;
            user_count <= MAX_USER_FOLDERS
        };
        let name = if label_id == labels::INBOX { "Inbox".to_string() } else { f.display.clone() };
        let parent_id = if is_system {
            None
        } else {
            f.delimiter.as_deref().and_then(|d| f.path.rsplit_once(d).map(|(p, _)| p.to_string()))
        };
        rows.push(FolderRow {
            label_id,
            path: f.path,
            name,
            kind: if is_system { LabelKind::System } else { LabelKind::User },
            parent_id,
            delimiter: f.delimiter,
            synced,
        });
    }
    // Parents must exist as folders, otherwise show the folder at top level.
    let ids: HashSet<String> = rows.iter().map(|r| r.label_id.clone()).collect();
    for r in &mut rows {
        if r.parent_id.as_ref().is_some_and(|p| !ids.contains(p)) {
            r.parent_id = None;
        }
    }
    if !same_folders(&store.folders(&account.id)?, &rows) {
        store.replace_folders(&account.id, &rows)?;
    }
    let mut order: Vec<FolderRow> = rows.into_iter().filter(|r| r.synced).collect();
    order.sort_by_key(|r| (if r.label_id == labels::INBOX { 0 } else if r.kind == LabelKind::System { 1 } else { 2 }, r.path.clone()));
    Ok(order)
}

fn same_folders(a: &[FolderRow], b: &[FolderRow]) -> bool {
    let key = |f: &FolderRow| {
        (f.label_id.clone(), f.path.clone(), f.name.clone(), f.kind == LabelKind::System, f.parent_id.clone(), f.delimiter.clone(), f.synced)
    };
    let mut a: Vec<_> = a.iter().map(key).collect();
    let mut b: Vec<_> = b.iter().map(key).collect();
    a.sort();
    b.sort();
    a == b
}

/// One sync pass over every synced folder.
pub async fn sync_account(conn: &mut ImapConn, store: &Store, account: &Account, cache: &mut SyncCache) -> Result<SyncOutcome> {
    let folders = refresh_folders(conn, store, account).await?;
    let mut outcome = SyncOutcome::default();
    let mut first_error: Option<anyhow::Error> = None;
    for folder in &folders {
        match sync_folder(conn, store, account, folder, cache).await {
            Ok(o) => {
                outcome.changed += o.changed;
                outcome.new_inbox.extend(o.new_inbox);
                outcome.snoozes_cancelled |= o.snoozes_cancelled;
            }
            // A dead or desynchronised connection fails every later folder too
            // (or worse, misreads their replies): stop so the worker reconnects.
            Err(e) if imap::is_auth_error(&e) || imap::is_connection_error(&e) => return Err(e),
            Err(e) => {
                tracing::warn!(account = %account.email, folder = %folder.path, "folder sync failed: {e:#}");
                first_error.get_or_insert(e);
            }
        }
    }
    refresh_unread_offsets(conn, store, account, &folders, cache).await;
    match (outcome.changed, first_error) {
        // Every folder failing usually means the connection died.
        (0, Some(e)) if folders.len() == 1 || store.pending_op_count(&account.id).unwrap_or(0) == 0 => {
            Err(e.context("sync failed"))
        }
        _ => Ok(outcome),
    }
}

/// Server unread count minus local unread count for INBOX and user folders,
/// so badges include unread mail older than the local window. Skipped while
/// server mutations are queued (they'd be absorbed into the offset). The
/// STATUS is skipped when neither the folder (by CONDSTORE) nor the local
/// unread count changed since the last one.
async fn refresh_unread_offsets(conn: &mut ImapConn, store: &Store, account: &Account, folders: &[FolderRow], cache: &mut SyncCache) {
    if store.pending_op_count(&account.id).unwrap_or(1) > 0 {
        return;
    }
    for f in folders.iter().filter(|f| f.label_id == labels::INBOX || f.kind == LabelKind::User) {
        let Ok(Some(state)) = store.folder_state(&account.id, &f.path) else { continue };
        let local_unread = store
            .local_flags(&account.id, &f.path, state.uid_validity)
            .map(|m| m.values().filter(|(unread, _)| *unread).count() as i64)
            .unwrap_or(0);
        let key = cache.folders.get(&f.path).filter(|s| s.highest_modseq.is_some()).map(|s| (s.clone(), local_unread));
        if key.is_some() && cache.unread.get(&f.path) == key.as_ref() {
            continue;
        }
        let Ok(server_unseen) = conn.status_unseen(&f.path).await else { continue };
        let _ = store.set_unread_offset(&account.id, &f.path, server_unseen as i64 - local_unread);
        match key {
            Some(k) => cache.unread.insert(f.path.clone(), k),
            None => cache.unread.remove(&f.path),
        };
    }
}

fn incoming(account: &Account, folder: &FolderRow, uid_validity: u32, m: FetchedMessage) -> Option<IncomingMessage> {
    let parsed = crate::mime::parse(&m.raw)?;
    let fallback = format!("{}/{}/{}/{}", account.id, folder.path, uid_validity, m.uid);
    let thread_id = crate::render::thread_id(&parsed, &fallback);
    let date = m.internal_date.or(parsed.date).unwrap_or_else(now_ms);
    let is_draft = m.flags.draft || folder.label_id == labels::DRAFTS;
    Some(IncomingMessage {
        account_id: account.id.clone(),
        thread_id,
        folder: folder.label_id.clone(),
        path: folder.path.clone(),
        uid_validity,
        uid: m.uid,
        date,
        is_unread: !m.flags.seen && !is_draft,
        is_starred: m.flags.flagged,
        is_draft,
        is_answered: m.flags.answered,
        raw: m.complete.then_some(m.raw),
        parsed,
    })
}

/// Join the conversation of a stored message this one replies to, and pull in
/// replies stored before it: a reply naming only its parent (In-Reply-To, no
/// References) would otherwise start a conversation of its own.
fn resolve_thread(store: &Store, inc: &mut IncomingMessage) -> Result<()> {
    let p = &inc.parsed;
    let mut refs = p.references.clone();
    if let Some(irt) = p.in_reply_to.as_ref().filter(|i| !refs.contains(i)) {
        refs.push(irt.clone());
    }
    if let Some(thread) = store.thread_for_references(&inc.account_id, &refs)? {
        inc.thread_id = thread;
    }
    if let Some(mid) = &p.message_id {
        // Replies stored earlier used this message as their root. Only while
        // it's new: a copy (or a forged Message-ID) must not pull a stored
        // conversation into another one.
        let own = crate::render::thread_id(&crate::mime::ParsedMessage { message_id: Some(mid.clone()), ..Default::default() }, "");
        if own != inc.thread_id && !store.message_id_known(&inc.account_id, mid)? {
            store.merge_thread(&inc.account_id, &own, &inc.thread_id)?;
        }
    }
    Ok(())
}

/// What `fetch_and_store` did.
#[derive(Default)]
struct Stored {
    count: usize,
    /// New unread INBOX messages.
    new_ids: Vec<String>,
    min_uid: Option<u32>,
    /// A new message cancelled a snooze.
    unsnoozed: bool,
}

/// Fetch and store `uids` in batches.
async fn fetch_and_store(
    conn: &mut ImapConn,
    store: &Store,
    account: &Account,
    folder: &FolderRow,
    uid_validity: u32,
    uids: &[u32],
    report_new: bool,
) -> Result<Stored> {
    let mut out = Stored::default();
    let now = now_ms();
    for batch in uids.chunks(BATCH_SIZE) {
        let fetched = conn.fetch_messages(&folder.path, batch).await?;
        if let Some(low) = fetched.iter().map(|m| m.uid).min() {
            out.min_uid = Some(out.min_uid.map_or(low, |v| v.min(low)));
        }
        // Parsing (MIME, HTML to text) is CPU work: keep it off the async threads.
        let (acc, fold) = (account.clone(), folder.clone());
        let parsed: Vec<IncomingMessage> = tokio::task::spawn_blocking(move || {
            fetched.into_iter().filter_map(|m| incoming(&acc, &fold, uid_validity, m)).collect()
        })
        .await?;
        for mut inc in parsed {
            resolve_thread(store, &mut inc)?;
            let thread_id = inc.thread_id.clone();
            let unread = inc.is_unread;
            let (id, inserted) = store.upsert_message(&inc)?;
            out.count += 1;
            if inserted && report_new {
                // A new message in a snoozed thread brings it back.
                if store.cancel_snooze_for_new_mail(&account.id, &thread_id, now).unwrap_or(false) {
                    out.unsnoozed = true;
                }
                if folder.label_id == labels::INBOX && unread {
                    out.new_ids.push(id);
                }
            }
        }
    }
    Ok(out)
}

pub async fn sync_folder(
    conn: &mut ImapConn,
    store: &Store,
    account: &Account,
    folder: &FolderRow,
    cache: &mut SyncCache,
) -> Result<SyncOutcome> {
    let status = conn.select(&folder.path).await?;
    let seen = Snapshot::of(&status);
    // Taken out now and put back only once this sync completes.
    let unchanged = seen.is_some() && cache.folders.remove(&folder.path) == seen;
    let prev = store.folder_state(&account.id, &folder.path)?;
    let mut outcome = SyncOutcome::default();

    if status.uid_validity == 0 {
        // Without UIDVALIDITY we can't tell whether our UIDs are still valid;
        // leave the folder alone rather than risk wiping it.
        anyhow::bail!("server did not report UIDVALIDITY for {}", folder.path);
    }
    let fresh = match &prev {
        None => true,
        Some(p) if p.uid_validity != status.uid_validity => {
            tracing::info!(folder = %folder.path, old = p.uid_validity, new = status.uid_validity, "UIDVALIDITY changed, rebuilding folder");
            outcome.changed += store.delete_path_messages(&account.id, &folder.path)?;
            true
        }
        _ => false,
    };

    if fresh {
        let limit = if folder.label_id == labels::INBOX { INITIAL_INBOX_COUNT } else { INITIAL_FOLDER_COUNT };
        let all = if status.exists == 0 { Vec::new() } else { conn.search(&folder.path, "ALL").await? };
        let newest: Vec<u32> = all.iter().rev().take(limit).copied().collect();
        let Stored { count: stored, min_uid, .. } =
            fetch_and_store(conn, store, account, folder, status.uid_validity, &newest, false).await?;
        outcome.changed += stored;
        let uid_next = if status.uid_next > 0 { status.uid_next } else { all.last().map_or(1, |u| u + 1) };
        store.save_folder_state(
            &account.id,
            &folder.path,
            &FolderState {
                uid_validity: status.uid_validity,
                uid_next,
                highest_modseq: status.highest_modseq,
                oldest_uid: min_uid,
                fully_synced: all.len() <= limit,
            },
        )?;
        if let Some(s) = seen {
            cache.folders.insert(folder.path.clone(), s);
        }
        return Ok(outcome);
    }

    let prev = prev.expect("checked above");
    let mut uid_next = prev.uid_next;

    // 1. New mail.
    if status.uid_next == 0 || status.uid_next > prev.uid_next {
        let found = conn.search(&folder.path, &format!("UID {}:*", prev.uid_next.max(1))).await?;
        // `N:*` always matches the highest UID even when it is below N.
        let new: Vec<u32> = found.into_iter().filter(|u| *u >= prev.uid_next).collect();
        if !new.is_empty() {
            let got = fetch_and_store(conn, store, account, folder, status.uid_validity, &new, true).await?;
            outcome.changed += got.count;
            outcome.new_inbox.extend(got.new_ids);
            outcome.snoozes_cancelled |= got.unsnoozed;
            uid_next = uid_next.max(new.iter().max().copied().unwrap_or(0) + 1);
        }
        if status.uid_next > 0 {
            uid_next = uid_next.max(status.uid_next);
        }
    }

    // 2. Flag changes on the local window.
    let local = store.local_flags(&account.id, &folder.path, status.uid_validity)?;
    if !local.is_empty() {
        let condstore = conn.caps.condstore && prev.highest_modseq.is_some() && status.highest_modseq.is_some();
        let updates = if condstore {
            if prev.highest_modseq != status.highest_modseq {
                conn.fetch_flags(&folder.path, &[], prev.highest_modseq).await?
            } else {
                Vec::new()
            }
        } else {
            let uids: Vec<u32> = local.keys().copied().collect();
            conn.fetch_flags(&folder.path, &uids, None).await?
        };
        let is_draft_folder = folder.label_id == labels::DRAFTS;
        let pending = if updates.is_empty() { HashSet::new() } else { pending_flag_uids(store, account, &folder.path, status.uid_validity)? };
        let changes: Vec<(u32, bool, bool, bool)> = updates
            .into_iter()
            .filter(|(uid, _)| local.contains_key(uid) && !pending.contains(uid))
            .map(|(uid, f)| (uid, !f.seen && !is_draft_folder, f.flagged, f.answered))
            .collect();
        outcome.changed += store.apply_flags(&account.id, &folder.path, status.uid_validity, &changes)?;

        // 3. Deletions / moves away: only check UIDs we hold (nothing can
        // have gone while EXISTS and UIDNEXT are as at the last check).
        if !unchanged {
            let local_uids: Vec<u32> = local.keys().copied().collect();
            let existing: HashSet<u32> = conn.existing_uids(&folder.path, &local_uids).await?.into_iter().collect();
            let gone: Vec<u32> = local_uids.into_iter().filter(|u| !existing.contains(u)).collect();
            if !gone.is_empty() {
                outcome.changed += store.delete_uids(&account.id, &folder.path, status.uid_validity, &gone)?;
            }
        }
    }

    store.save_folder_state(
        &account.id,
        &folder.path,
        &FolderState {
            uid_validity: status.uid_validity,
            uid_next,
            highest_modseq: status.highest_modseq,
            oldest_uid: prev.oldest_uid,
            fully_synced: prev.fully_synced,
        },
    )?;
    if let Some(s) = seen {
        cache.folders.insert(folder.path.clone(), s);
    }
    Ok(outcome)
}

/// UIDs in a folder whose queued flag changes haven't reached the server yet:
/// the server still has the old flags, which mustn't overwrite ours.
fn pending_flag_uids(store: &Store, account: &Account, path: &str, uid_validity: u32) -> Result<HashSet<u32>> {
    let mut out = HashSet::new();
    for id in store.pending_flag_message_ids(&account.id)? {
        if let Some(loc) = store.location(&id)? {
            if loc.path == path && loc.uid_validity == uid_validity && loc.uid > 0 {
                out.insert(loc.uid);
            }
        }
    }
    Ok(out)
}

/// Where "load more" continues: below the synced window. The lowest stored
/// UID is only a fallback, since search results and Unread paging store mail
/// far below the window.
fn load_more_floor(oldest_uid: Option<u32>, lowest_stored: Option<u32>) -> Option<u32> {
    oldest_uid.or(lowest_stored)
}

/// The next page of older mail from `found` (ascending UIDs): the newest
/// `LOAD_MORE_PAGE` below `floor` not stored yet, and whether more remain.
fn older_page(found: &[u32], floor: u32, known: &HashSet<u32>) -> (Vec<u32>, bool) {
    let older: Vec<u32> = found.iter().copied().filter(|u| *u < floor && !known.contains(u)).collect();
    let page: Vec<u32> = older.iter().rev().take(LOAD_MORE_PAGE).copied().collect();
    let has_more = older.len() > page.len();
    (page, has_more)
}

/// Page in older mail for a label. Returns `(stored, has_more)`.
pub async fn load_more(conn: &mut ImapConn, store: &Store, account: &Account, label_id: &str) -> Result<(usize, bool)> {
    // Unread pages through unread INBOX mail without moving the INBOX window.
    let unread_only = label_id == labels::UNREAD;
    let folder_label = if unread_only { labels::INBOX } else { label_id };
    let folders = store.folders(&account.id)?;
    let Some(folder) = folders.into_iter().find(|f| f.label_id == folder_label) else {
        return Ok((0, false));
    };
    let status = conn.select(&folder.path).await?;
    let Some(mut state) = store.folder_state(&account.id, &folder.path)? else {
        let o = sync_folder(conn, store, account, &folder, &mut SyncCache::default()).await?;
        return Ok((o.changed, true));
    };
    if state.uid_validity != status.uid_validity {
        let o = sync_folder(conn, store, account, &folder, &mut SyncCache::default()).await?;
        return Ok((o.changed, true));
    }
    let lowest = if state.oldest_uid.is_none() { store.min_uid(&account.id, &folder.path, status.uid_validity)? } else { None };
    let Some(floor) = load_more_floor(state.oldest_uid, lowest) else { return Ok((0, false)) };
    if floor <= 1 {
        if !unread_only {
            state.fully_synced = true;
            store.save_folder_state(&account.id, &folder.path, &state)?;
        }
        return Ok((0, false));
    }
    let unseen = if unread_only { " UNSEEN" } else { "" };
    let older = conn.search(&folder.path, &format!("UID 1:{}{}", floor - 1, unseen)).await?;
    let known: HashSet<u32> = store.local_uids(&account.id, &folder.path, status.uid_validity)?.into_iter().collect();
    let (page, has_more) = older_page(&older, floor, &known);
    let stored = fetch_and_store(conn, store, account, &folder, status.uid_validity, &page, false).await?.count;
    if !unread_only {
        state.oldest_uid = page.iter().min().copied().or(state.oldest_uid);
        state.fully_synced = !has_more;
        store.save_folder_state(&account.id, &folder.path, &state)?;
    }
    Ok((stored, has_more))
}

/// Run a search on the server and store matching messages locally so the
/// normal (local) search shows them. Searches every synced folder except
/// Trash/Spam unless the query has `in:`; keeps the newest 50 matches.
pub async fn server_search(conn: &mut ImapConn, store: &Store, account: &Account, query: &str) -> Result<usize> {
    let parsed = crate::search::parse(query, now_ms());
    if parsed.is_empty() {
        return Ok(0);
    }
    let criteria = crate::search::to_imap(&parsed);
    let folders = store.folders(&account.id)?;
    let scope: Vec<FolderRow> = match &parsed.folder {
        Some(name) => {
            let label = crate::store::resolve_label_name(name);
            folders
                .into_iter()
                .filter(|f| f.label_id == label || f.path.eq_ignore_ascii_case(name) || f.name.eq_ignore_ascii_case(name))
                .collect()
        }
        None => folders
            .into_iter()
            .filter(|f| f.synced && f.label_id != labels::TRASH && f.label_id != labels::SPAM)
            .collect(),
    };
    let deadline = Instant::now() + SEARCH_BUDGET;
    let mut candidates: Vec<(FolderRow, u32, u32)> = Vec::new();
    for f in scope {
        if Instant::now() >= deadline {
            break;
        }
        let Ok(status) = conn.select(&f.path).await else { continue };
        match conn.search(&f.path, &criteria).await {
            Ok(uids) => {
                for uid in uids.into_iter().rev().take(SEARCH_RESULT_LIMIT) {
                    candidates.push((f.clone(), status.uid_validity, uid));
                }
            }
            Err(e) => tracing::warn!(folder = %f.path, "server search failed: {e:#}"),
        }
    }
    // Skip what we already have.
    let mut known: HashMap<(String, u32), HashSet<u32>> = HashMap::new();
    let mut by_folder: HashMap<String, (FolderRow, u32, Vec<u32>)> = HashMap::new();
    for (f, uidv, uid) in candidates {
        let key = (f.path.clone(), uidv);
        if !known.contains_key(&key) {
            known.insert(key.clone(), store.local_uids(&account.id, &f.path, uidv)?.into_iter().collect());
        }
        if known[&key].contains(&uid) {
            continue;
        }
        by_folder.entry(f.path.clone()).or_insert_with(|| (f.clone(), uidv, Vec::new())).2.push(uid);
    }
    let mut stored = 0;
    for (_, (f, uidv, mut uids)) in by_folder {
        uids.sort_unstable_by(|a, b| b.cmp(a));
        uids.truncate(SEARCH_RESULT_LIMIT);
        if Instant::now() >= deadline {
            break;
        }
        let n = fetch_and_store(conn, store, account, &f, uidv, &uids, false).await?.count;
        stored += n;
    }
    Ok(stored)
}

/// Fetch the full source of a message that was synced headers-only.
pub async fn fetch_body(conn: &mut ImapConn, store: &Store, message_id: &str) -> Result<Option<Vec<u8>>> {
    let Some(loc) = store.location(message_id)? else { return Ok(None) };
    let uid = match resolve_uid(conn, store, &loc).await? {
        Some(uid) => uid,
        None => return Ok(None),
    };
    let raw = conn.fetch_raw(&loc.path, uid).await?;
    if let Some(raw) = &raw {
        if let Some(parsed) = crate::mime::parse(raw) {
            store.store_body(message_id, raw, &parsed)?;
        }
    }
    Ok(raw)
}

/// A row's current UID (see `current_uids`).
async fn resolve_uid(conn: &mut ImapConn, store: &Store, loc: &Location) -> Result<Option<u32>> {
    Ok(current_uids(conn, store, &loc.path, vec![loc.clone()]).await?.into_iter().next().map(|(_, uid)| uid))
}

/// SELECT `path` and pair each stored row in it with its UID there. A UID
/// from an older UIDVALIDITY (the server renumbered the folder) or one a
/// move left unknown is looked up by Message-ID; rows that can't be found
/// are left out, so a stale UID is never acted on.
async fn current_uids(conn: &mut ImapConn, store: &Store, path: &str, locs: Vec<Location>) -> Result<Vec<(Location, u32)>> {
    let status = conn.select(path).await?;
    if status.uid_validity == 0 {
        anyhow::bail!("server did not report UIDVALIDITY for {path}");
    }
    let (valid, stale): (Vec<Location>, Vec<Location>) =
        locs.into_iter().partition(|l| l.uid > 0 && l.uid_validity == status.uid_validity);
    let mut out: Vec<(Location, u32)> = valid
        .into_iter()
        .map(|l| {
            let uid = l.uid;
            (l, uid)
        })
        .collect();
    let mids: Vec<String> = stale.iter().filter_map(|l| l.message_id.clone()).collect();
    let found = if mids.is_empty() { HashMap::new() } else { conn.find_by_message_ids(path, &mids).await? };
    for loc in stale {
        match loc.message_id.as_ref().and_then(|m| found.get(m)) {
            Some(&uid) => {
                store.set_location(&loc.id, &loc.folder, &loc.path, status.uid_validity, uid)?;
                out.push((loc, uid));
            }
            None => tracing::info!(folder = path, "message not found on the server; leaving it alone"),
        }
    }
    Ok(out)
}

/// Stored rows by IMAP folder.
fn locations_by_path(store: &Store, message_ids: &[String], skip: impl Fn(&Location) -> bool) -> Result<HashMap<String, Vec<Location>>> {
    let mut groups: HashMap<String, Vec<Location>> = HashMap::new();
    for id in message_ids {
        if let Some(loc) = store.location(id)?.filter(|l| !skip(l)) {
            groups.entry(loc.path.clone()).or_default().push(loc);
        }
    }
    Ok(groups)
}

// ----------------------------------------------------------------------------
// Server operations (executed from the persistent op queue)
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ServerOp {
    /// Move messages to the folder backing `dest_label`.
    #[serde(rename_all = "camelCase")]
    Move { message_ids: Vec<String>, dest_label: String },
    /// Set or clear \Seen / \Flagged / \Answered.
    #[serde(rename_all = "camelCase")]
    Flags { message_ids: Vec<String>, seen: Option<bool>, flagged: Option<bool>, answered: Option<bool> },
    /// Permanently delete (Trash/Spam "delete forever", replaced drafts):
    /// `(path, uid_validity, uid)`, plus `(path, Message-ID)` for messages
    /// whose UID was unknown.
    #[serde(rename_all = "camelCase")]
    Expunge {
        locations: Vec<(String, u32, u32)>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        by_message_id: Vec<(String, String)>,
    },
    /// Delete every message in a folder (empty Trash/Spam).
    #[serde(rename_all = "camelCase")]
    EmptyFolder { label: String },
    /// Store a copy of a sent message in the folder backing `label` (once,
    /// even when retried).
    #[serde(rename_all = "camelCase")]
    Append { label: String, flags: String, message_id: String, raw_base64: String },
}

/// Destination folder for a label, creating a conventional one if missing.
/// Archiving on Gmail moves to "All Mail", which removes the Inbox label.
pub async fn destination_path(conn: &mut ImapConn, store: &Store, account: &Account, label: &str) -> Result<String> {
    if let Some(p) = store.folder_path(&account.id, label)? {
        return Ok(p);
    }
    if label == labels::ARCHIVE {
        if let Some(p) = store.folder_path(&account.id, labels::ALL)? {
            return Ok(p);
        }
    }
    let name = match label {
        labels::ARCHIVE => "Archive",
        labels::TRASH => "Trash",
        labels::SPAM => "Junk",
        labels::SENT => "Sent",
        labels::DRAFTS => "Drafts",
        other => other,
    };
    conn.create_folder(name).await?;
    refresh_folders(conn, store, account).await?;
    Ok(store.folder_path(&account.id, label)?.unwrap_or_else(|| name.to_string()))
}

pub async fn execute_op(conn: &mut ImapConn, store: &Store, account: &Account, op: &ServerOp) -> Result<()> {
    match op {
        ServerOp::Flags { message_ids, seen, flagged, answered } => {
            for (path, locs) in locations_by_path(store, message_ids, |_| false)? {
                let uids: Vec<u32> = current_uids(conn, store, &path, locs).await?.into_iter().map(|(_, uid)| uid).collect();
                for (value, flag) in [(seen, "\\Seen"), (flagged, "\\Flagged"), (answered, "\\Answered")] {
                    if let Some(v) = value {
                        conn.store_flags(&path, &uids, *v, flag).await?;
                    }
                }
            }
        }
        ServerOp::Move { message_ids, dest_label } => {
            let dest = destination_path(conn, store, account, dest_label).await?;
            for (path, locs) in locations_by_path(store, message_ids, |l| l.path == dest && l.uid > 0)? {
                let items = current_uids(conn, store, &path, locs).await?;
                if items.is_empty() {
                    continue;
                }
                let uids: Vec<u32> = items.iter().map(|(_, u)| *u).collect();
                conn.move_uids(&path, &uids, &dest).await?;
                // Find the new UIDs so the rows stay addressable. The move is
                // done either way: unknown UIDs are looked up again when needed.
                let status = conn.select(&dest).await?;
                let mids: Vec<String> = items.iter().filter_map(|(l, _)| l.message_id.clone()).collect();
                let found = conn.find_by_message_ids(&dest, &mids).await.unwrap_or_else(|e| {
                    tracing::debug!("finding moved messages failed: {e:#}");
                    HashMap::new()
                });
                let folder = store.label_for_path(&account.id, &dest)?.unwrap_or_else(|| dest_label.clone());
                // A Gmail archive lands in All Mail, but we show it under Archive.
                let folder = if folder == labels::ALL { labels::ARCHIVE.to_string() } else { folder };
                for (loc, _) in items {
                    let new_uid = loc.message_id.as_ref().and_then(|m| found.get(m)).copied().unwrap_or(0);
                    store.set_location(&loc.id, &folder, &dest, status.uid_validity, new_uid)?;
                }
            }
        }
        ServerOp::Expunge { locations, by_message_id } => {
            let mut by_uid: HashMap<&str, Vec<(u32, u32)>> = HashMap::new();
            for (path, uidv, uid) in locations {
                by_uid.entry(path).or_default().push((*uidv, *uid));
            }
            let mut by_mid: HashMap<&str, Vec<String>> = HashMap::new();
            for (path, mid) in by_message_id {
                by_mid.entry(path).or_default().push(mid.clone());
            }
            let paths: HashSet<&str> = by_uid.keys().chain(by_mid.keys()).copied().collect();
            for path in paths {
                let uids = by_uid.remove(path).unwrap_or_default();
                let mids = by_mid.remove(path).unwrap_or_default();
                let status = conn.select(path).await?;
                if status.uid_validity == 0 {
                    anyhow::bail!("server did not report UIDVALIDITY for {path}");
                }
                // UIDs from before a UIDVALIDITY change may now name other messages.
                let mut doomed: Vec<u32> = uids.iter().filter(|(v, _)| *v == status.uid_validity).map(|(_, u)| *u).collect();
                if doomed.len() < uids.len() {
                    tracing::info!(folder = path, "UIDVALIDITY changed; not deleting by old UIDs");
                }
                if !mids.is_empty() {
                    doomed.extend(conn.find_by_message_ids(path, &mids).await?.into_values());
                }
                conn.delete_uids(path, &doomed).await?;
            }
        }
        ServerOp::EmptyFolder { label } => {
            let Some(path) = store.folder_path(&account.id, label)? else { return Ok(()) };
            let status = conn.select(&path).await?;
            if status.exists > 0 {
                let all = conn.search(&path, "ALL").await?;
                conn.delete_uids(&path, &all).await?;
            }
            store.delete_path_messages(&account.id, &path)?;
        }
        ServerOp::Append { label, flags, message_id, raw_base64 } => {
            use base64::Engine as _;
            let raw = base64::engine::general_purpose::STANDARD.decode(raw_base64).context("stored message is damaged")?;
            append_and_record(conn, store, account, label, flags, &raw, message_id).await?;
        }
    }
    Ok(())
}

/// Revert the optimistic local state of an op that failed for good.
pub fn revert_op(store: &Store, account: &Account, op: &ServerOp) -> Result<()> {
    if let ServerOp::Move { message_ids, .. } = op {
        for id in message_ids {
            if let Some(loc) = store.location(id)? {
                if let Some(label) = store.label_for_path(&account.id, &loc.path)? {
                    store.set_location(&loc.id, &label, &loc.path, loc.uid_validity, loc.uid)?;
                }
            }
        }
    }
    Ok(())
}

/// Store a copy of a sent message (or draft) in a folder via APPEND and
/// record it locally.
pub async fn append_and_record(
    conn: &mut ImapConn,
    store: &Store,
    account: &Account,
    label: &str,
    flags: &str,
    raw: &[u8],
    message_id: &str,
) -> Result<Option<String>> {
    let path = destination_path(conn, store, account, label).await.context("finding folder")?;
    let mid = [message_id.to_string()];
    // A retry after the APPEND went through must not store a second copy.
    conn.select(&path).await?;
    if conn.find_by_message_ids(&path, &mid).await?.is_empty() {
        conn.append(&path, flags, raw).await?;
    }
    let status = conn.select(&path).await?;
    let uid = conn.find_by_message_ids(&path, &mid).await?.get(message_id).copied().unwrap_or(0);
    let folder = FolderRow {
        label_id: store.label_for_path(&account.id, &path)?.unwrap_or_else(|| label.to_string()),
        path: path.clone(),
        name: path.clone(),
        kind: LabelKind::System,
        parent_id: None,
        delimiter: None,
        synced: true,
    };
    if uid == 0 {
        return Ok(None);
    }
    let fetched = FetchedMessage {
        uid,
        flags: imap::MsgFlags { seen: flags.contains("\\Seen"), draft: flags.contains("\\Draft"), ..Default::default() },
        internal_date: Some(now_ms()),
        size: Some(raw.len() as u32),
        raw: raw.to_vec(),
        complete: true,
    };
    let Some(mut inc) = incoming(account, &folder, status.uid_validity, fetched) else { return Ok(None) };
    resolve_thread(store, &mut inc)?;
    // The next sync sees this UID as new; the upsert keyed on location makes
    // that a no-op rather than a duplicate.
    let (id, _) = store.upsert_message(&inc)?;
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{account, put, raw_message};

    fn folder(label: &str, path: &str) -> FolderRow {
        FolderRow {
            label_id: label.into(),
            path: path.into(),
            name: path.into(),
            kind: LabelKind::System,
            parent_id: None,
            delimiter: Some("/".into()),
            synced: true,
        }
    }

    fn fetched(uid: u32, raw: &[u8], complete: bool, internal_date: Option<i64>) -> FetchedMessage {
        FetchedMessage { uid, flags: imap::MsgFlags::default(), internal_date, size: Some(raw.len() as u32), raw: raw.to_vec(), complete }
    }

    #[test]
    fn queued_ops_keep_deserializing() {
        // Shapes already stored in op_queue by earlier versions.
        let op: ServerOp = serde_json::from_str(r#"{"kind":"move","messageIds":["a"],"destLabel":"TRASH"}"#).unwrap();
        assert!(matches!(&op, ServerOp::Move { message_ids, dest_label } if message_ids == &["a"] && dest_label == "TRASH"));
        let op: ServerOp = serde_json::from_str(r#"{"kind":"expunge","locations":[["INBOX",7,42]]}"#).unwrap();
        assert!(matches!(&op, ServerOp::Expunge { locations, by_message_id }
            if locations == &[("INBOX".to_string(), 7, 42)] && by_message_id.is_empty()));
        // ...and still written that way when there is nothing new to say.
        assert_eq!(serde_json::to_string(&op).unwrap(), r#"{"kind":"expunge","locations":[["INBOX",7,42]]}"#);
        let op: ServerOp =
            serde_json::from_str(r#"{"kind":"flags","messageIds":["a"],"seen":true,"flagged":null,"answered":null}"#).unwrap();
        assert!(matches!(op, ServerOp::Flags { seen: Some(true), flagged: None, answered: None, .. }));
        let op: ServerOp = serde_json::from_str(r#"{"kind":"emptyFolder","label":"TRASH"}"#).unwrap();
        assert!(matches!(op, ServerOp::EmptyFolder { label } if label == "TRASH"));
        // `Store::pending_flag_message_ids` finds flag ops by this prefix.
        let json = serde_json::to_string(&ServerOp::Flags { message_ids: vec!["a".into()], seen: None, flagged: Some(true), answered: None }).unwrap();
        assert!(json.starts_with(r#"{"kind":"flags","messageIds":["a"]"#), "{json}");
        let append = ServerOp::Append { label: "SENT".into(), flags: "(\\Seen)".into(), message_id: "m@x".into(), raw_base64: "QQ==".into() };
        let back: ServerOp = serde_json::from_str(&serde_json::to_string(&append).unwrap()).unwrap();
        assert!(matches!(back, ServerOp::Append { message_id, raw_base64, .. } if message_id == "m@x" && raw_base64 == "QQ=="));
    }

    #[test]
    fn incoming_rows() {
        let store = Store::open_in_memory().unwrap();
        let acc = account(&store, "me@example.com");
        let inbox = folder(labels::INBOX, "INBOX");
        let raw = raw_message("Sara <sara@x.test>", "me@example.com", "Hi", "", "m1@x.test");
        // Drafts are never unread.
        let inc = incoming(&acc, &folder(labels::DRAFTS, "Drafts"), 9, fetched(3, &raw, true, None)).unwrap();
        assert!(!inc.is_unread && inc.is_draft);
        assert!(inc.raw.is_some());
        // Headers only: no body is stored. INTERNALDATE wins over Date:.
        let inc = incoming(&acc, &inbox, 9, fetched(3, &raw, false, Some(1234))).unwrap();
        assert!(inc.raw.is_none() && inc.is_unread);
        assert_eq!(inc.date, 1234);
        assert_eq!(inc.thread_id, "t:m1@x.test");
        let inc = incoming(&acc, &inbox, 9, fetched(3, &raw, true, None)).unwrap();
        assert_eq!(inc.date, chrono::DateTime::parse_from_rfc2822("Mon, 19 Oct 2026 09:00:00 +0000").unwrap().timestamp_millis());
        // No Message-ID: the conversation is just this message, by location.
        let inc = incoming(&acc, &inbox, 9, fetched(5, b"From: a@x.test\r\nSubject: s\r\n\r\nbody", true, None)).unwrap();
        assert_eq!(inc.thread_id, format!("t:{}/INBOX/9/5", acc.id));
    }

    #[test]
    fn failed_move_is_undone_locally() {
        let store = Store::open_in_memory().unwrap();
        let acc = account(&store, "me@example.com");
        store.replace_folders(&acc.id, &[folder(labels::INBOX, "INBOX"), folder(labels::TRASH, "Trash")]).unwrap();
        let id = put(&store, &acc, "INBOX", 1, raw_message("a@x.test", "me@example.com", "Hi", "", "m@x"), 1000);
        store.set_folder_local(std::slice::from_ref(&id), labels::TRASH).unwrap();
        revert_op(&store, &acc, &ServerOp::Move { message_ids: vec![id.clone()], dest_label: labels::TRASH.into() }).unwrap();
        let loc = store.location(&id).unwrap().unwrap();
        assert_eq!((loc.folder.as_str(), loc.path.as_str(), loc.uid), (labels::INBOX, "INBOX", 1));
        // Nothing to undo locally for other ops.
        store.set_folder_local(std::slice::from_ref(&id), labels::TRASH).unwrap();
        revert_op(&store, &acc, &ServerOp::Flags { message_ids: vec![id.clone()], seen: Some(true), flagged: None, answered: None }).unwrap();
        assert_eq!(store.location(&id).unwrap().unwrap().folder, labels::TRASH);
    }

    #[test]
    fn load_more_continues_below_the_window() {
        // Mail stored far below the window (search results, Unread paging) doesn't move the floor.
        assert_eq!(load_more_floor(Some(900), Some(5)), Some(900));
        assert_eq!(load_more_floor(None, Some(5)), Some(5));
        assert_eq!(load_more_floor(None, None), None);
        let found: Vec<u32> = (1..900).collect();
        let known: HashSet<u32> = [5, 898].into_iter().collect();
        let (page, more) = older_page(&found, 900, &known);
        assert_eq!(page.len(), LOAD_MORE_PAGE);
        assert_eq!(page[0], 899);
        assert!(!page.contains(&898) && more);
        let (page, more) = older_page(&[3, 5, 7], 900, &known);
        assert_eq!((page, more), (vec![7, 3], false));
        // `UID 1:N` can answer with UIDs at or above N.
        assert!(older_page(&[950], 900, &HashSet::new()).0.is_empty());
    }

    #[test]
    fn replies_join_their_parents_conversation() {
        let store = Store::open_in_memory().unwrap();
        let acc = account(&store, "me@example.com");
        let inbox = folder(labels::INBOX, "INBOX");
        let save = |uid: u32, extra: &str, mid: &str| {
            let raw = raw_message("a@x.test", "me@example.com", "Plans", extra, mid);
            let mut inc = incoming(&acc, &inbox, 1, fetched(uid, &raw, true, None)).unwrap();
            resolve_thread(&store, &mut inc).unwrap();
            store.upsert_message(&inc).unwrap().0
        };
        let thread = |id: &str| store.location(id).unwrap().unwrap().thread_id;
        let a = save(1, "", "A@x");
        let b = save(2, "In-Reply-To: <A@x>\r\nReferences: <A@x>\r\n", "b@x");
        // Only In-Reply-To, naming a reply rather than the root.
        let c = save(3, "In-Reply-To: <b@x>\r\n", "c@x");
        assert_eq!([thread(&a), thread(&b), thread(&c)], ["t:a@x", "t:a@x", "t:a@x"]);
        // A reply stored before its parent joins once the parent arrives, with
        // its snooze and auto-clean exemption.
        let z = save(5, "In-Reply-To: <y@x>\r\n", "z@x");
        assert_eq!(thread(&z), "t:y@x");
        store.snooze(&acc.id, "t:y@x", 4_000_000_000_000).unwrap();
        store.autoclean_exempt(&[(acc.id.clone(), "t:y@x".into())]).unwrap();
        let y = save(4, "In-Reply-To: <x@x>\r\nReferences: <x@x>\r\n", "y@x");
        assert_eq!([thread(&y), thread(&z)], ["t:x@x", "t:x@x"]);
        assert_eq!(store.snoozed_until(&acc.id, "t:x@x").unwrap(), Some(4_000_000_000_000));
        assert_eq!(store.snoozed_until(&acc.id, "t:y@x").unwrap(), None);
        // A message claiming a stored root's Message-ID doesn't drag that conversation along.
        let forged = save(6, "In-Reply-To: <z@x>\r\n", "A@x");
        assert_eq!(thread(&forged), "t:x@x");
        assert_eq!([thread(&a), thread(&b), thread(&c)], ["t:a@x", "t:a@x", "t:a@x"]);
        // Re-syncing a message keeps it where it is.
        save(3, "In-Reply-To: <b@x>\r\n", "c@x");
        assert_eq!(thread(&c), "t:a@x");
    }

    #[test]
    fn pending_flag_changes_are_not_overwritten() {
        let store = Store::open_in_memory().unwrap();
        let acc = account(&store, "me@example.com");
        let id = put(&store, &acc, "INBOX", 7, raw_message("a@x.test", "me@example.com", "Hi", "", "m@x"), 1000);
        put(&store, &acc, "INBOX", 8, raw_message("a@x.test", "me@example.com", "Hi", "", "n@x"), 1000);
        let op = ServerOp::Flags { message_ids: vec![id], seen: Some(true), flagged: None, answered: None };
        store.enqueue_op(&acc.id, &serde_json::to_string(&op).unwrap()).unwrap();
        store.enqueue_op(&acc.id, r#"{"kind":"emptyFolder","label":"TRASH"}"#).unwrap();
        assert_eq!(pending_flag_uids(&store, &acc, "INBOX", 1).unwrap(), HashSet::from([7]));
        assert!(pending_flag_uids(&store, &acc, "INBOX", 2).unwrap().is_empty(), "other UIDVALIDITY");
        assert!(pending_flag_uids(&store, &acc, "Archive", 1).unwrap().is_empty());
    }

    #[test]
    fn folder_list_comparison() {
        let a = vec![folder(labels::INBOX, "INBOX"), folder(labels::TRASH, "Trash")];
        let b = vec![folder(labels::TRASH, "Trash"), folder(labels::INBOX, "INBOX")];
        assert!(same_folders(&a, &b));
        let mut c = b.clone();
        c[0].name = "Bin".into();
        assert!(!same_folders(&a, &c));
        assert!(!same_folders(&a, &a[..1]));
    }

    #[test]
    fn snapshots_need_uidnext() {
        let st = |uid_next| MailboxStatus { uid_validity: 3, uid_next, exists: 10, highest_modseq: None };
        assert!(Snapshot::of(&st(0)).is_none(), "can't tell arrivals apart without UIDNEXT");
        assert_eq!(Snapshot::of(&st(11)), Snapshot::of(&st(11)));
        assert_ne!(Snapshot::of(&st(11)), Snapshot::of(&st(12)));
    }
}
