//! A small file-backed session store, so signing in survives a service restart.
//!
//! The default `MemoryStore` loses every session when the process exits — and this process is a
//! SYSTEM service that restarts on failure, on upgrade, and on every reboot. For a parent whose
//! browser also shows a self-signed-certificate warning, that made each restart cost *two*
//! annoyances: click through the warning again, then retype a long passphrase on a phone
//! keyboard. Persisting sessions turns signing in into a one-time cost per device.
//!
//! **Reads never touch the disk.** The map is the source of truth and is loaded once at startup;
//! only mutations (login, logout, pairing, id rotation) write. Those are rare — `tower-sessions`
//! saves lazily, so ordinary authenticated requests don't.
//!
//! **Persistence is best-effort.** A failed write is logged, never fatal: losing durability is
//! strictly better than refusing to serve the dashboard. The file lives in the ACL-locked data
//! dir alongside the password hash and TLS key, so a standard user can't read the session ids
//! out of it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use tower_sessions::SessionStore;
use tower_sessions::cookie::time::OffsetDateTime;
use tower_sessions::session::{Id, Record};
use tower_sessions::session_store;

/// Cheap to clone (shared inner state), so it can live on `AppState` *and* be handed to the
/// session layer without two stores disagreeing about who's signed in.
#[derive(Debug, Clone)]
pub struct FileSessionStore {
    inner: std::sync::Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    /// `None` in tests — behaves exactly like an in-memory store.
    path: Option<PathBuf>,
    map: Mutex<HashMap<Id, Record>>,
}

impl FileSessionStore {
    /// Load any previously persisted sessions from `path` (missing or unreadable → start empty:
    /// the worst case is everyone signs in again, which is the old behavior).
    pub fn new(path: PathBuf) -> Self {
        let records = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Vec<Record>>(&raw).ok())
            .unwrap_or_default();

        let now = OffsetDateTime::now_utc();
        let inner: HashMap<Id, Record> = records
            .into_iter()
            .filter(|r| r.expiry_date > now)
            .map(|r| (r.id, r))
            .collect();

        if !inner.is_empty() {
            tracing::debug!("restored {} session(s) from disk", inner.len());
        }
        Self {
            inner: std::sync::Arc::new(Shared {
                path: Some(path),
                map: Mutex::new(inner),
            }),
        }
    }

    /// An in-memory-only store (tests), so the suite never touches the real data dir.
    pub fn ephemeral() -> Self {
        Self {
            inner: std::sync::Arc::new(Shared {
                path: None,
                map: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Recover from a poisoned lock rather than panicking, mirroring [`crate::state::recover_read`].
    /// The critical sections here can't panic, and the release build aborts on panic anyway.
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<Id, Record>> {
        self.inner.map.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Drop expired entries, then write the survivors. Pruning here (rather than on a timer)
    /// keeps the file bounded without a background task: it can only grow between two writes.
    ///
    /// Takes the already-held guard so the caller can't accidentally persist a stale snapshot.
    /// `consequence` describes what a *failed* write means for this particular operation — it
    /// differs by caller and getting it backwards is actively misleading. For `save`/`create` a
    /// lost write means the sign-in doesn't survive a restart; for `delete` it means the
    /// sign-*out* doesn't, and the session comes back.
    fn persist(&self, map: &mut HashMap<Id, Record>, consequence: &str) {
        let Some(path) = &self.inner.path else { return };
        let now = OffsetDateTime::now_utc();
        map.retain(|_, r| r.expiry_date > now);

        let records: Vec<&Record> = map.values().collect();
        match serde_json::to_vec(&records) {
            Ok(bytes) => {
                if let Err(e) = crate::config::write_atomic(path, &bytes) {
                    tracing::warn!(
                        "could not persist sessions to {} ({e}) — {consequence}",
                        path.display()
                    );
                }
            }
            Err(e) => tracing::warn!("could not serialize sessions: {e}"),
        }
    }

    /// Sign every device out. Used on password change: the parent's mental model is that changing
    /// the password locks everyone else out, and with sessions now surviving restarts there is
    /// otherwise **no** way to revoke a leaked cookie for its full 30-day life.
    pub fn clear_all(&self) {
        let mut map = self.map();
        map.clear();
        self.persist(
            &mut map,
            "some devices may stay signed in until the next restart",
        );
    }
}

#[async_trait::async_trait]
impl SessionStore for FileSessionStore {
    /// Insert a brand-new session, regenerating the id on the (astronomically unlikely) collision
    /// rather than silently clobbering someone else's session. Implemented explicitly because the
    /// provided default delegates to `save` and logs a warning on every call.
    async fn create(&self, record: &mut Record) -> session_store::Result<()> {
        let mut map = self.map();
        while map.contains_key(&record.id) {
            record.id = Id::default();
        }
        map.insert(record.id, record.clone());
        self.persist(&mut map, "you'll be signed out on restart");
        Ok(())
    }

    async fn save(&self, record: &Record) -> session_store::Result<()> {
        let mut map = self.map();
        // Skip the write when nothing actually changed. `cycle_id` and repeated saves of an
        // identical record are common, and each write is a full serialize + fsync + rename.
        if map.get(&record.id) == Some(record) {
            return Ok(());
        }
        map.insert(record.id, record.clone());
        self.persist(&mut map, "you'll be signed out on restart");
        Ok(())
    }

    async fn load(&self, id: &Id) -> session_store::Result<Option<Record>> {
        let now = OffsetDateTime::now_utc();
        // Memory-only: this runs on every authenticated request.
        Ok(self.map().get(id).filter(|r| r.expiry_date > now).cloned())
    }

    async fn delete(&self, id: &Id) -> session_store::Result<()> {
        let mut map = self.map();
        // Only write if something was actually removed. `POST /logout` is unauthenticated and
        // unthrottled, and a made-up cookie reaches here — so without this check, anyone on the
        // LAN could force a serialize + fsync + rename per request (holding the map lock every
        // authenticated request needs) just by looping curl at /logout.
        if map.remove(id).is_none() {
            return Ok(());
        }
        self.persist(&mut map, "this sign-out may be undone by a restart");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower_sessions::cookie::time::Duration;

    fn record(offset: Duration) -> Record {
        Record {
            id: Id::default(),
            data: Default::default(),
            expiry_date: OffsetDateTime::now_utc() + offset,
        }
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("nw-sess-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("sessions.json")
    }

    #[tokio::test]
    async fn sessions_survive_a_restart() {
        let path = tmp("restart");
        let rec = record(Duration::days(30));

        let store = FileSessionStore::new(path.clone());
        store.save(&rec).await.unwrap();
        drop(store); // simulate the service exiting

        let reopened = FileSessionStore::new(path.clone());
        let loaded = reopened.load(&rec.id).await.unwrap();
        assert!(loaded.is_some(), "a saved session must outlive the process");
        assert_eq!(loaded.unwrap().id, rec.id);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The startup filter in `new()` must drop expired records.
    ///
    /// Writing an expired record via `save` can't test this: `persist` prunes *before* writing,
    /// so the file ends up `[]` and the assertion passes even with the filter deleted. The record
    /// has to be planted on disk directly.
    #[tokio::test]
    async fn expired_sessions_on_disk_are_not_restored() {
        let path = tmp("expired");
        let live = record(Duration::days(30));
        let dead = record(Duration::seconds(-1));
        std::fs::write(&path, serde_json::to_vec(&vec![&live, &dead]).unwrap()).unwrap();

        let store = FileSessionStore::new(path.clone());
        assert!(
            store.load(&dead.id).await.unwrap().is_none(),
            "an expired record on disk must not be restored"
        );
        assert!(
            store.load(&live.id).await.unwrap().is_some(),
            "a live record on disk must still load"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// `delete` of an id that isn't present must not write. `POST /logout` is unauthenticated and
    /// unthrottled, so a bogus cookie reaching a write would let anyone on the LAN force an fsync
    /// per request while holding the lock every authenticated request needs.
    #[tokio::test]
    async fn deleting_an_unknown_session_writes_nothing() {
        let path = tmp("nowrite");
        let store = FileSessionStore::new(path.clone());
        store.delete(&Id::default()).await.unwrap();
        assert!(
            !path.exists(),
            "an unknown session id must not cause a disk write"
        );

        // And with a real session present, the file must not be rewritten either.
        let rec = record(Duration::days(30));
        store.save(&rec).await.unwrap();
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        store.delete(&Id::default()).await.unwrap();
        let after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before, after, "unknown-id delete must leave the file alone");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Password change relies on this to revoke a possibly-leaked cookie.
    #[tokio::test]
    async fn clear_all_revokes_every_session_including_on_disk() {
        let path = tmp("clearall");
        let store = FileSessionStore::new(path.clone());
        let a = record(Duration::days(30));
        let b = record(Duration::days(30));
        store.save(&a).await.unwrap();
        store.save(&b).await.unwrap();

        store.clear_all();
        assert!(store.load(&a.id).await.unwrap().is_none());
        assert!(store.load(&b.id).await.unwrap().is_none());

        let reopened = FileSessionStore::new(path.clone());
        assert!(
            reopened.load(&a.id).await.unwrap().is_none(),
            "revocation must survive a restart"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn delete_signs_out_everywhere_including_on_disk() {
        let path = tmp("delete");
        let rec = record(Duration::days(30));

        let store = FileSessionStore::new(path.clone());
        store.save(&rec).await.unwrap();
        store.delete(&rec.id).await.unwrap();
        assert!(store.load(&rec.id).await.unwrap().is_none());

        // A logout must not come back after a restart.
        let reopened = FileSessionStore::new(path.clone());
        assert!(reopened.load(&rec.id).await.unwrap().is_none());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn create_assigns_a_free_id_and_persists() {
        let path = tmp("create");
        let store = FileSessionStore::new(path.clone());
        let mut rec = record(Duration::days(30));
        store.create(&mut rec).await.unwrap();
        assert!(store.load(&rec.id).await.unwrap().is_some());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A record that was live at one write and expired by the next must be pruned by that next
    /// write — otherwise the file grows forever.
    ///
    /// (Saving an already-expired record proves nothing: `persist` prunes before writing, so it
    /// never reaches disk in the first place.)
    #[tokio::test]
    async fn a_write_prunes_records_that_have_since_expired() {
        let path = tmp("prune");
        let store = FileSessionStore::new(path.clone());

        let soon = record(Duration::milliseconds(150));
        let live = record(Duration::days(30));
        store.save(&soon).await.unwrap();
        let on_disk: Vec<Record> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk.len(), 1, "it was still live at the first write");

        // Let it lapse, then trigger another write.
        std::thread::sleep(std::time::Duration::from_millis(250));
        store.save(&live).await.unwrap();

        let on_disk: Vec<Record> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk.len(), 1, "the lapsed record must be pruned");
        assert_eq!(on_disk[0].id, live.id);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn ephemeral_store_writes_nothing() {
        let store = FileSessionStore::ephemeral();
        let rec = record(Duration::days(30));
        store.save(&rec).await.unwrap();
        assert!(store.load(&rec.id).await.unwrap().is_some());
    }
}
