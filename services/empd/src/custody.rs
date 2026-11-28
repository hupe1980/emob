//! Current subject keys and completed erasures live outside the provider journal backup.
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use emob_service::vault::{DataKey, Sealed};
use serde::{Deserialize, Serialize};

use crate::customer::erasure::Receipt;

const PURPOSE: &str = "empd/customer-custody/v1";
const STATE: &str = "subjects.json";
const NEXT: &str = "subjects.next";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Entry {
    Active([u8; 32]),
    Erased(Receipt),
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    id: String,
    subjects: BTreeMap<String, Entry>,
}

struct Disk {
    dir: PathBuf,
    key: Arc<DataKey>,
    // The writer lock spans the lifetime of the live provider, including its HTTP handlers.
    _lock: File,
}

fn private_file() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
}

impl Disk {
    fn open(dir: &Path, key: Arc<DataKey>, bound: Option<&str>) -> Result<(Self, Registry)> {
        if bound.is_none() {
            let mut dirs = fs::DirBuilder::new();
            dirs.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                dirs.mode(0o700);
            }
            dirs.create(dir)
                .context("creating the empd custody directory")?;
        }
        ensure!(dir.is_dir(), "current empd custody directory is missing");
        let lock = private_file()
            .create(true)
            .truncate(false)
            .open(dir.join("owner.lock"))?;
        lock.try_lock()
            .context("empd custody already has an active writer")?;
        let disk = Self {
            dir: dir.to_owned(),
            key,
            _lock: lock,
        };
        // A failed prepare never became authoritative. Remove it before accepting new work.
        match fs::remove_file(dir.join(NEXT)) {
            Ok(()) => File::open(dir)?.sync_all()?,
            Err(e) if e.kind() == ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        let registry = match fs::read(dir.join(STATE)) {
            Ok(raw) => {
                let envelope: Sealed = serde_json::from_slice(&raw)?;
                let plaintext = disk
                    .key
                    .open(PURPOSE, &envelope)
                    .context("authenticating current empd custody")?;
                let registry: Registry = serde_json::from_slice(&plaintext)?;
                ensure!(
                    match bound {
                        Some(id) => id == registry.id,
                        None => registry.subjects.is_empty(),
                    },
                    "empd journal and current custody do not belong together"
                );
                registry
            }
            Err(e) if e.kind() == ErrorKind::NotFound && bound.is_none() => {
                let registry = Registry {
                    id: hex::encode(random_key()?),
                    subjects: BTreeMap::new(),
                };
                disk.save(&registry)?;
                registry
            }
            Err(e) => return Err(e).context("opening the current empd custody state"),
        };
        Ok((disk, registry))
    }

    fn save(&self, registry: &Registry) -> Result<()> {
        let box_ = self.key.seal(PURPOSE, &serde_json::to_vec(registry)?);
        let next = self.dir.join(NEXT);
        let mut temp = private_file().create_new(true).open(&next)?;
        temp.write_all(&serde_json::to_vec(&box_)?)?;
        temp.sync_all()?;
        fs::rename(next, self.dir.join(STATE))?;
        File::open(&self.dir)?.sync_all()?;
        Ok(())
    }
}

fn random_key() -> Result<[u8; 32]> {
    let mut bytes = [0u8; 32];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| anyhow!("subject key generation failed"))?;
    Ok(bytes)
}

struct Owner {
    disk: Option<Disk>,
    state: Mutex<Option<Registry>>,
}

#[derive(Clone)]
pub(crate) struct Custody(Arc<Owner>);

impl core::fmt::Debug for Custody {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Custody").finish_non_exhaustive()
    }
}

impl Custody {
    pub(crate) fn memory() -> Self {
        let registry = Registry {
            id: String::new(),
            subjects: BTreeMap::new(),
        };
        Self(Arc::new(Owner {
            disk: None,
            state: Mutex::new(Some(registry)),
        }))
    }

    pub(crate) fn disk(
        dir: &Path,
        key: Arc<DataKey>,
        bound: Option<&str>,
    ) -> Result<(Self, String)> {
        let (disk, registry) = Disk::open(dir, key, bound)?;
        let id = registry.id.clone();
        Ok((
            Self(Arc::new(Owner {
                disk: Some(disk),
                state: Mutex::new(Some(registry)),
            })),
            id,
        ))
    }

    pub(crate) fn matches(&self, id: &str) -> bool {
        !id.is_empty()
            && self
                .lock()
                .is_ok_and(|registry| registry.as_ref().is_some_and(|state| state.id == id))
    }

    fn lock(&self) -> Result<MutexGuard<'_, Option<Registry>>> {
        self.0
            .state
            .lock()
            .map_err(|_| anyhow!("empd custody unavailable"))
    }

    fn save(&self, state: &mut Option<Registry>) -> Result<()> {
        if let Some(disk) = &self.0.disk
            && let Err(error) = disk.save(state.as_ref().context("empd custody unavailable")?)
        {
            // A rename can succeed before its directory sync fails. Reopen before another decision.
            *state = None;
            return Err(error).context("empd custody commit unacknowledged");
        }
        Ok(())
    }

    pub(crate) fn ensure_key(&self, tag: &str) -> Result<DataKey> {
        let mut guard = self.lock()?;
        let registry = guard.as_mut().context("empd custody unavailable")?;
        match registry.subjects.get(tag) {
            Some(Entry::Active(key)) => return Ok(DataKey::from_bytes(key)),
            Some(Entry::Erased(_)) => bail!("customer access was erased"),
            None => (),
        }
        let key = random_key()?;
        registry.subjects.insert(tag.to_owned(), Entry::Active(key));
        self.save(&mut guard)?;
        Ok(DataKey::from_bytes(&key))
    }

    pub(crate) fn key(&self, tag: &str) -> Result<Option<DataKey>> {
        let guard = self.lock()?;
        match guard
            .as_ref()
            .context("empd custody unavailable")?
            .subjects
            .get(tag)
        {
            Some(Entry::Active(key)) => Ok(Some(DataKey::from_bytes(key))),
            Some(Entry::Erased(_)) => Ok(None),
            None => bail!("customer grant has no current custody entry"),
        }
    }

    pub(crate) fn receipt(&self, tag: &str) -> Result<Option<Receipt>> {
        let guard = self.lock()?;
        Ok(
            match guard
                .as_ref()
                .context("empd custody unavailable")?
                .subjects
                .get(tag)
            {
                Some(Entry::Erased(receipt)) => Some(receipt.clone()),
                _ => None,
            },
        )
    }

    pub(crate) fn erase(&self, tag: &str, receipt: &Receipt) -> Result<Receipt> {
        let mut guard = self.lock()?;
        let registry = guard.as_mut().context("empd custody unavailable")?;
        if let Some(Entry::Erased(original)) = registry.subjects.get(tag) {
            ensure!(
                receipt == original,
                "customer access already erased by another decision"
            );
            return Ok(original.clone());
        }
        registry
            .subjects
            .insert(tag.to_owned(), Entry::Erased(receipt.clone()));
        self.save(&mut guard)?;
        Ok(receipt.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emob_service::Actor;

    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!(
            "empd-custody-{}",
            hex::encode(random_key().unwrap())
        ))
    }

    fn receipt() -> Receipt {
        serde_json::from_value(serde_json::json!({
            "operation":"a".repeat(64),"actor":Actor::operator("privacy").unwrap(),
            "policy":"portal_access_v1","erased_at":"2026-11-27T08:00:00Z",
            "revision":3,"accounts":1,"agreements":1
        }))
        .unwrap()
    }

    #[test]
    fn an_older_grant_backup_opens_only_for_subjects_whose_keys_remain() {
        let path = temp();
        let master = Arc::new(DataKey::from_bytes(&[1; 32]));
        let (store, id) = Custody::disk(&path, Arc::clone(&master), None).unwrap();
        let (first, second) = ("01".repeat(32), "02".repeat(32));
        let purpose = "test/portal-binding";
        let archived = store
            .ensure_key(&first)
            .unwrap()
            .seal_str(purpose, "first private grant");
        let kept = store
            .ensure_key(&second)
            .unwrap()
            .seal_str(purpose, "second private grant");
        assert!(master.open(purpose, &archived).is_err());
        let decision = receipt();
        assert_eq!(store.erase(&first, &decision).unwrap(), decision);
        drop(store);
        let (restored, _) = Custody::disk(&path, Arc::clone(&master), Some(&id)).unwrap();
        assert!(restored.key(&first).unwrap().is_none());
        assert!(restored.ensure_key(&first).is_err());
        assert_eq!(restored.receipt(&first).unwrap(), Some(decision.clone()));
        assert_eq!(restored.erase(&first, &decision).unwrap(), decision);
        let mut conflict = decision;
        conflict.operation = "b".repeat(64);
        assert!(restored.erase(&first, &conflict).is_err());
        let survivor = restored.key(&second).unwrap().unwrap();
        assert!(survivor.open(purpose, &archived).is_err());
        assert_eq!(
            survivor.open(purpose, &kept).unwrap(),
            b"second private grant"
        );
        assert!(restored.key("unrecorded").is_err());
        drop(restored);
        assert!(Custody::disk(&path, Arc::clone(&master), Some("another-store")).is_err());
        assert!(Custody::disk(&path, Arc::new(DataKey::from_bytes(&[2; 32])), Some(&id)).is_err());
        fs::remove_file(path.join(STATE)).unwrap();
        assert!(Custody::disk(&path, Arc::clone(&master), Some(&id)).is_err());
        assert!(!path.join(STATE).exists());
        fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn a_second_writer_and_an_unacknowledged_key_update_fail_closed() {
        let path = temp();
        let master = Arc::new(DataKey::from_bytes(&[5; 32]));
        let (store, id) = Custody::disk(&path, Arc::clone(&master), None).unwrap();
        assert!(Custody::disk(&path, Arc::clone(&master), Some(&id)).is_err());
        let tag = "03".repeat(32);
        let saved = store.ensure_key(&tag).unwrap().seal_str("scope", "grant");
        // A prepared file that never reached rename must not become authoritative.
        fs::write(path.join(NEXT), b"interrupted prepare").unwrap();
        assert!(store.erase(&tag, &receipt()).is_err());
        assert!(store.key(&tag).is_err());
        assert!(store.receipt(&tag).is_err());
        drop(store);
        let (restarted, _) = Custody::disk(&path, Arc::clone(&master), Some(&id)).unwrap();
        assert!(!path.join(NEXT).exists());
        assert_eq!(
            restarted
                .key(&tag)
                .unwrap()
                .unwrap()
                .open("scope", &saved)
                .unwrap(),
            b"grant"
        );
        restarted.erase(&tag, &receipt()).unwrap();
        drop(restarted);
        fs::remove_dir_all(path).unwrap();
    }
}
