//! The section sync cycle.
//!
//! Each cycle, under the sync lock:
//! 1. the index (`GET /v1/sections`) says which sections changed on the
//!    server; those are fetched;
//! 2. per section, synchronously: local records are collected and stamped
//!    into the cached document, the server's document is merged in, and what
//!    the merge changed is applied to the store;
//! 3. sections with changes the server hasn't got are written with
//!    `If-Match`; on `412` the current document is merged (and applied) and
//!    the write tried again, up to five times.
//!
//! Merging is the CRDT merge of `remail_proto::sync`, so the order writes
//! reach the server in doesn't matter. Credentials are sections of their own
//! (`cred:<id>`): a token refresh writes only that one, never racing an edit
//! of the accounts.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use reqwest::Method;

use remail_proto::api::{PutSection, Section, SectionIndex};
use remail_proto::sections::{self, account};
use remail_proto::sync::{SectionDoc, Stamp};

use super::cache::{Cache, Cached};
use super::http::PutOutcome;
use super::mapping;
use super::{CloudError, CloudInner, Flags, Phase, Progress};
use crate::engine::{Engine, UI_PREFS};
use crate::store::now_ms;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The first download after signing in: everything is applied, with
    /// progress. The server's setup wins (a new device's defaults mustn't
    /// overwrite the user's choices), unless this computer's setup is being
    /// uploaded (migration).
    First { upload: bool },
    Regular,
}

/// Write attempts per section and cycle when others keep writing first.
const PUT_ATTEMPTS: u32 = 5;

/// Sections applied in full every time (they may wait for mail or accounts
/// that arrive later); the others only where the merge changed something.
fn always_full(section: &str) -> bool {
    matches!(section, sections::SNOOZES | sections::SIGNATURES | sections::LAYOUTS | sections::CALENDARS)
}

impl CloudInner {
    pub(super) fn stamp(&self, now: i64) -> Stamp {
        self.clock.lock().unwrap().stamp(now)
    }

    pub(super) fn observe(&self, ms: i64) {
        self.clock.lock().unwrap().observe(ms);
    }

    /// Every section this device syncs: the fixed ones, and a credential per
    /// account and calendar account (on the server, cached, or here).
    fn section_ids(&self, index: &SectionIndex) -> Result<Vec<String>, CloudError> {
        let store = self.engine.store();
        let mut ids: BTreeSet<String> = sections::FIXED.iter().map(|s| s.to_string()).collect();
        ids.extend(index.sections.iter().map(|s| s.id.clone()).filter(|id| sections::is_valid(id)));
        ids.extend(Cache::ids(store)?.into_iter().filter(|id| id.starts_with(sections::CRED_PREFIX) && sections::is_valid(id)));
        ids.extend(self.engine.accounts()?.into_iter().map(|a| sections::cred_section(&a.id)));
        ids.extend(store.synced_cal_accounts()?.into_iter().filter(|c| c.mail_account_id.is_none()).map(|c| sections::cred_section(&c.id)));
        let mut ids: Vec<String> = ids.into_iter().collect();
        ids.sort_by_key(|id| (mapping::apply_order(id), id.clone()));
        Ok(ids)
    }

    /// One sync of every section with the server.
    pub(crate) async fn cycle(self: &Arc<Self>, mode: Mode) -> Result<(), CloudError> {
        let _one = self.sync_lock.lock().await;
        let epoch = self.epoch.load(Ordering::SeqCst);
        let first = matches!(mode, Mode::First { .. });
        if self.org_stale.load(Ordering::SeqCst) || first {
            if let Err(e) = self.refresh_org().await {
                tracing::debug!("organisation settings: {e}");
            }
        }
        let index: SectionIndex = self.authed(Method::GET, "/v1/sections", None::<&()>).await?;
        self.observe(index.server_time);
        let ids = self.section_ids(&index)?;
        let versions: HashMap<&str, i64> = index.sections.iter().map(|s| (s.id.as_str(), s.version)).collect();
        let store = self.engine.store();
        let mut remote: HashMap<String, Section> = HashMap::new();
        for id in &ids {
            let have = self.cache.load(store, id)?.map(|c| c.version).unwrap_or(0);
            let theirs = versions.get(id.as_str()).copied().unwrap_or(0);
            if theirs != have {
                let s = if theirs == 0 { Section { id: id.clone(), version: 0, doc: SectionDoc::default() } } else { self.fetch_section(id).await? };
                remote.insert(id.clone(), s);
            }
        }
        if self.epoch.load(Ordering::SeqCst) != epoch {
            return Err(CloudError::signed_out());
        }
        let accounts_total = if first { remote.get(sections::ACCOUNTS).map(|s| s.doc.live().count()).unwrap_or(0) } else { 0 };
        let now = now_ms();
        for (i, id) in ids.iter().enumerate() {
            if first {
                let label = if id.starts_with(sections::CRED_PREFIX) { "credentials".to_string() } else { id.clone() };
                self.update(|st| st.progress = Some(Progress { done: i, total: ids.len() + accounts_total, label }));
            }
            self.merge_section(id, remote.remove(id), mode, now)?;
        }
        for id in &ids {
            self.push_section(id, epoch).await?;
        }
        Ok(())
    }

    /// A setter changed `section`: fold this device's records into the cached
    /// document now, so the change carries the time it was made. Skipped
    /// while a sync is applying (its own writes, collected right after) and
    /// before the first download (the server's setup wins then).
    pub(super) fn collect_now(&self, section: &str) {
        if self.status().phase != Phase::Ready {
            return;
        }
        let Ok(_cache) = self.cache_lock.try_lock() else { return };
        if let Err(e) = self.stamp_local(section, now_ms()) {
            tracing::debug!(section, "collecting a change: {e}");
        }
    }

    fn stamp_local(&self, id: &str, now: i64) -> Result<(), CloudError> {
        let store = self.engine.store();
        let mut c = self.cache.load(store, id)?.unwrap_or_default();
        let Some(local) = mapping::collect(&self.engine, id)? else { return Ok(()) };
        let before = c.doc.clone();
        let at = self.stamp(now);
        mapping::stamp(&mut c.doc, &local, &c.local, &at);
        if c.doc != before {
            c.local = local.into_keys().collect();
            c.dirty = true;
            self.cache.save(store, id, &c)?;
        }
        Ok(())
    }

    /// Fold local changes and the server's document into the cached one and
    /// apply the result. No awaits: engine writes can't slip in between.
    fn merge_section(&self, id: &str, remote: Option<Section>, mode: Mode, now: i64) -> Result<(), CloudError> {
        let _cache = self.cache_lock.lock().unwrap_or_else(|p| p.into_inner());
        let engine = &self.engine;
        let store = engine.store();
        let cached = self.cache.load(store, id)?.unwrap_or_default();
        let mut doc = cached.doc.clone();
        let collect_first = !matches!(mode, Mode::First { upload: false });
        if let Some(local) = collect_first.then(|| mapping::collect(engine, id)).transpose()?.flatten() {
            let at = self.stamp(now);
            mapping::stamp(&mut doc, &local, &cached.local, &at);
        }
        let local_changed = doc != cached.doc;
        let (merged, version, reset) = match &remote {
            // Offline longer than tombstones live: the server's copy wins.
            Some(r) if cached.version > 0 && SectionDoc::needs_reset(cached.last_sync, now) => (r.doc.clone(), r.version, true),
            Some(r) => {
                self.observe(mapping::latest_ms(&r.doc));
                (doc.clone().merged(&r.doc), r.version, false)
            }
            None => (doc.clone(), cached.version, false),
        };
        let only = (mode == Mode::Regular && !reset && !always_full(id)).then(|| mapping::changed_ids(&doc, &merged));
        if only.as_ref().is_none_or(|o| !o.is_empty()) {
            mapping::apply(engine, id, &merged, only.as_ref(), reset)?;
        }
        let local = match mapping::collect(engine, id)? {
            Some(l) => l.into_keys().collect(),
            None => cached.local.clone(),
        };
        let dirty = match &remote {
            Some(r) => merged != r.doc,
            None => cached.dirty || local_changed,
        };
        self.cache.save(store, id, &Cached { version, doc: merged, local, dirty, last_sync: now })?;
        Ok(())
    }

    /// Write a section the server hasn't got yet (`If-Match`), merging and
    /// trying again when someone else wrote first.
    async fn push_section(self: &Arc<Self>, id: &str, epoch: u64) -> Result<(), CloudError> {
        let store = self.engine.store();
        for attempt in 0..PUT_ATTEMPTS {
            let Some(loaded) = self.cache.load(store, id)? else { return Ok(()) };
            if !loaded.dirty {
                return Ok(());
            }
            let mut sent = loaded.doc.clone();
            sent.gc(now_ms());
            let server = self.session_server()?;
            let token = self.access_token().await?;
            let body = PutSection { doc: sent.clone() };
            let r = match self.api.put_section(&server, &token, id, loaded.version, &body).await {
                Err(e) if e.code == "unauthorized" => {
                    self.forget_access(&token).await;
                    let token = self.access_token().await?;
                    self.api.put_section(&server, &token, id, loaded.version, &body).await
                }
                r => r,
            };
            self.note(&r);
            if self.epoch.load(Ordering::SeqCst) != epoch {
                return Err(CloudError::signed_out());
            }
            match r {
                Ok(PutOutcome::Saved(version)) => {
                    let _cache = self.cache_lock.lock().unwrap_or_else(|p| p.into_inner());
                    // A change made here while the write was on its way is written next time.
                    let now = self.cache.load(store, id)?.unwrap_or_default();
                    let c = if now.doc == loaded.doc {
                        Cached { version, doc: sent, local: now.local, dirty: false, last_sync: now_ms() }
                    } else {
                        Cached { version, dirty: true, last_sync: now_ms(), ..now }
                    };
                    self.cache.save(store, id, &c)?;
                    return Ok(());
                }
                Ok(PutOutcome::Conflict(current)) => {
                    self.observe(mapping::latest_ms(&current.doc));
                    let _cache = self.cache_lock.lock().unwrap_or_else(|p| p.into_inner());
                    let base = self.cache.load(store, id)?.unwrap_or(loaded);
                    let merged = base.doc.clone().merged(&current.doc);
                    let only = mapping::changed_ids(&base.doc, &merged);
                    if !only.is_empty() {
                        mapping::apply(&self.engine, id, &merged, Some(&only), false)?;
                    }
                    let local = match mapping::collect(&self.engine, id)? {
                        Some(l) => l.into_keys().collect(),
                        None => base.local.clone(),
                    };
                    let dirty = merged != current.doc;
                    self.cache.save(store, id, &Cached { version: current.version, doc: merged, local, dirty, last_sync: now_ms() })?;
                    drop(_cache);
                    if !dirty {
                        return Ok(());
                    }
                    let jitter = 50 + now_ms().rem_euclid(200) as u64;
                    tokio::time::sleep(Duration::from_millis(jitter * u64::from(attempt + 1))).await;
                }
                // The server won't take it (a provider the org turned off, too
                // big): keep the change here and say so; other sections go on.
                // A server that doesn't know the section yet (a newer app than the
                // server) is the same: the data stays here until it's updated.
                Err(e) if matches!(e.code.as_str(), "provider_not_allowed" | "too_large" | "bad_request" | "not_found") => {
                    tracing::warn!(section = id, "the Remail server refused this section: {e}");
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
        }
        tracing::info!(section = id, "the section kept changing on the server; trying again later");
        Ok(())
    }

    /// After signing in: bring everything down, start mail, and wait (with
    /// progress) until every mailbox has started its first sync.
    pub(crate) async fn first_sync(self: Arc<Self>) {
        let mut wait = Duration::from_secs(2);
        let upload = self.flags().upload;
        loop {
            match self.cycle(Mode::First { upload }).await {
                Ok(()) => break,
                Err(e) => {
                    if !self.signed_in() {
                        return;
                    }
                    tracing::info!("first sync with the Remail server: {e}");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(Duration::from_secs(60));
                }
            }
        }
        self.set_flags(&Flags { first_sync_done: true, ..Default::default() });
        if let Err(e) = self.engine.open_gate() {
            tracing::warn!("couldn't start mail: {e:#}");
        }
        let accounts = self.engine.accounts().unwrap_or_default();
        let base = self.status().progress.map(|p| p.total.saturating_sub(accounts.len())).unwrap_or(0);
        let deadline = tokio::time::Instant::now() + self.opts.first_sync_wait;
        loop {
            let started: Vec<&crate::model::Account> = accounts.iter().filter(|a| self.engine.sync_started(&a.id)).collect();
            let waiting = accounts.iter().find(|a| !self.engine.sync_started(&a.id));
            let label = waiting.map(|a| a.email.clone()).unwrap_or_default();
            self.update(|st| st.progress = Some(Progress { done: base + started.len(), total: base + accounts.len(), label }));
            if waiting.is_none() || tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        self.update(|st| {
            st.phase = Some(Phase::Ready);
            st.progress = None;
        });
        self.start_loops();
    }

    /// Sync whenever setup changes here or a notice says it changed there,
    /// and every so often regardless.
    pub(crate) async fn sync_loop(self: Arc<Self>) {
        let mut first = true;
        loop {
            if !first {
                let connected = self.ws_connected.load(Ordering::SeqCst);
                let mut every = if connected { self.opts.poll_with_notices } else { self.opts.poll_without_notices };
                if self.status().offline {
                    every = every.min(Duration::from_secs(30));
                }
                tokio::select! {
                    _ = self.wake.notified() => {
                        // A burst of edits makes one sync.
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                    _ = tokio::time::sleep(every) => {
                        if !connected {
                            self.org_stale.store(true, Ordering::SeqCst);
                        }
                    }
                }
            }
            first = false;
            if let Err(e) = self.cycle(Mode::Regular).await {
                if !e.is_unavailable() {
                    tracing::warn!("syncing settings with the Remail server failed: {e}");
                }
            }
        }
    }
}

/// Migration: local accounts the server already has (same address) take the
/// server's ids, with everything that refers to them. Returns how many.
pub(crate) fn adopt_server_ids(engine: &Engine, server: &SectionDoc) -> Result<usize, CloudError> {
    let store = engine.store();
    let secrets = engine.secrets();
    let mut moved = 0;
    for acc in engine.accounts()? {
        let target = server.live().find(|(_, r)| r.get(account::EMAIL).and_then(|v| v.as_str()).is_some_and(|e| e.eq_ignore_ascii_case(&acc.email)));
        let Some((new_id, _)) = target else { continue };
        if *new_id == acc.id || store.account(new_id)?.is_some() {
            continue;
        }
        store.remap_account_id(&acc.id, new_id)?;
        if let Some(secret) = secrets.get(&acc.id)? {
            engine.set_credential(new_id, &secret)?;
            secrets.delete(&acc.id)?;
        }
        let mut ui = engine.ui_prefs();
        let (old_prefix, new_prefix) = (format!("folder:{}:", acc.id), format!("folder:{new_id}:"));
        if let Some(h) = ui.sidebar_hidden.as_mut() {
            for e in h.iter_mut() {
                if let Some(rest) = e.strip_prefix(&old_prefix) {
                    *e = format!("{new_prefix}{rest}");
                }
            }
        }
        if ui.last_compose_account.as_deref() == Some(acc.id.as_str()) {
            ui.last_compose_account = Some(new_id.clone());
        }
        store.set_setting(UI_PREFS, &ui)?;
        let mut off: Vec<String> = store.setting(crate::calendar::OUTLOOK_OFF).unwrap_or_default();
        if off.contains(&acc.id) {
            for m in off.iter_mut().filter(|m| **m == acc.id) {
                *m = new_id.clone();
            }
            store.set_setting(crate::calendar::OUTLOOK_OFF, &off)?;
        }
        moved += 1;
    }
    Ok(moved)
}
