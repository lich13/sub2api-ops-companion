use tauri::{AppHandle, Manager};

#[cfg(not(target_os = "android"))]
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[cfg(target_os = "android")]
pub struct SecureStore(pub tauri::plugin::PluginHandle<tauri::Wry>);

#[cfg(target_os = "android")]
pub fn plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    tauri::plugin::Builder::new("secure-store")
        .setup(|app, api| {
            let handle = api.register_android_plugin("com.lich13.sub2ops", "SecureStorePlugin")?;
            app.manage(SecureStore(handle));
            Ok(())
        })
        .build()
}

#[cfg(not(target_os = "android"))]
pub struct CredentialStore {
    path: PathBuf,
    cache: Mutex<Option<BTreeMap<String, String>>>,
}

#[cfg(not(target_os = "android"))]
impl CredentialStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            cache: Mutex::new(None),
        }
    }

    fn ensure_loaded<'a>(
        &self,
        guard: &'a mut Option<BTreeMap<String, String>>,
    ) -> Result<&'a mut BTreeMap<String, String>, String> {
        if guard.is_none() {
            let loaded = match std::fs::read(&self.path) {
                Ok(bytes) => serde_json::from_slice::<BTreeMap<String, String>>(&bytes)
                    .map_err(|_| "本地凭据文件已损坏，请检查或移除后重新连接".to_string())?,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
                Err(_) => return Err("无法读取本地凭据文件".into()),
            };
            *guard = Some(loaded);
        }
        Ok(guard.as_mut().expect("credential map initialized"))
    }

    pub fn read_sync(&self, base: &str) -> Result<Option<String>, String> {
        if base.is_empty() {
            return Ok(None);
        }
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let map = self.ensure_loaded(&mut guard)?;
        Ok(map.get(base).cloned())
    }

    pub fn write_sync(&self, base: &str, key: &str) -> Result<(), String> {
        if base.is_empty() {
            return Err("服务地址无效".into());
        }
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let map = self.ensure_loaded(&mut guard)?;
        let mut next = map.clone();
        next.insert(base.to_owned(), key.to_owned());
        let mut bytes = serde_json::to_vec(&next).map_err(|_| "凭据格式无效".to_string())?;
        bytes.push(b'\n');
        match crate::write_private_atomic(&self.path, &bytes) {
            Ok(()) => {
                *map = next;
                Ok(())
            }
            Err(crate::AtomicWriteError::BeforeRename(_)) => {
                Err("无法保存本地凭据；连接未保存".to_string())
            }
            Err(crate::AtomicWriteError::AfterRename(_)) => {
                *guard = None;
                Err("无法保存本地凭据；连接未保存".to_string())
            }
        }
    }

    pub fn delete_sync(&self, base: &str) -> Result<(), String> {
        if base.is_empty() {
            return Ok(());
        }
        let mut guard = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let map = self.ensure_loaded(&mut guard)?;
        if !map.contains_key(base) {
            return Ok(());
        }
        let mut next = map.clone();
        next.remove(base);
        let mut bytes = serde_json::to_vec(&next).map_err(|_| "凭据格式无效".to_string())?;
        bytes.push(b'\n');
        match crate::write_private_atomic(&self.path, &bytes) {
            Ok(()) => {
                *map = next;
                Ok(())
            }
            Err(crate::AtomicWriteError::BeforeRename(_)) => {
                Err("无法删除本地凭据中的 Key".to_string())
            }
            Err(crate::AtomicWriteError::AfterRename(_)) => {
                *guard = None;
                Err("无法删除本地凭据中的 Key".to_string())
            }
        }
    }
}

pub async fn read(app: AppHandle, base: String) -> Result<Option<String>, String> {
    if base.is_empty() {
        return Ok(None);
    }
    #[cfg(not(target_os = "android"))]
    let store = app.state::<Arc<CredentialStore>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(not(target_os = "android"))]
        {
            store.read_sync(&base)
        }
        #[cfg(target_os = "android")]
        {
            let result: serde_json::Value = app
                .state::<SecureStore>()
                .0
                .run_mobile_plugin("read", serde_json::json!({"base": base}))
                .map_err(|_| "无法读取安全存储，请重新连接")?;
            Ok(result["key"].as_str().map(str::to_owned))
        }
    })
    .await
    .map_err(|_| "安全存储任务失败".to_string())?
}

pub async fn write(app: AppHandle, base: String, key: String) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    let store = app.state::<Arc<CredentialStore>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(not(target_os = "android"))]
        {
            store.write_sync(&base, &key)
        }
        #[cfg(target_os = "android")]
        {
            app.state::<SecureStore>()
                .0
                .run_mobile_plugin::<serde_json::Value>(
                    "write",
                    serde_json::json!({"base": base, "key": key}),
                )
                .map(|_| ())
                .map_err(|_| "无法加密保存 Key；连接未保存".into())
        }
    })
    .await
    .map_err(|_| "安全存储任务失败".to_string())?
}

pub async fn delete(app: AppHandle, base: String) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    let store = app.state::<Arc<CredentialStore>>().inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(not(target_os = "android"))]
        {
            store.delete_sync(&base)
        }
        #[cfg(target_os = "android")]
        {
            app.state::<SecureStore>()
                .0
                .run_mobile_plugin::<serde_json::Value>("delete", serde_json::json!({"base": base}))
                .map(|_| ())
                .map_err(|_| "无法删除安全存储中的 Key".into())
        }
    })
    .await
    .map_err(|_| "安全存储任务失败".to_string())?
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty_and_multi_instance_crud_enforces_0600_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = CredentialStore::new(path.clone());

        assert_eq!(store.read_sync("").unwrap(), None);
        assert_eq!(
            store.read_sync("https://alpha.example.invalid").unwrap(),
            None
        );
        assert!(!path.exists());
        store
            .delete_sync("https://alpha.example.invalid")
            .expect("deleting missing entry is a no-op");
        assert!(!path.exists());

        store
            .write_sync("https://alpha.example.invalid", "fixture-key-alpha")
            .unwrap();
        store
            .write_sync("https://beta.example.invalid", "fixture-key-beta")
            .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // No temporary files remain after atomic writes
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["credentials.json"]);

        // Reload in a fresh store instance
        let reloaded = CredentialStore::new(path.clone());
        assert_eq!(
            reloaded.read_sync("https://alpha.example.invalid").unwrap(),
            Some("fixture-key-alpha".into())
        );
        assert_eq!(
            reloaded.read_sync("https://beta.example.invalid").unwrap(),
            Some("fixture-key-beta".into())
        );

        // Overwrite one key and delete the other
        reloaded
            .write_sync("https://alpha.example.invalid", "fixture-key-alpha-v2")
            .unwrap();
        reloaded
            .delete_sync("https://beta.example.invalid")
            .unwrap();

        let third = CredentialStore::new(path);
        assert_eq!(
            third.read_sync("https://alpha.example.invalid").unwrap(),
            Some("fixture-key-alpha-v2".into())
        );
        assert_eq!(
            third.read_sync("https://beta.example.invalid").unwrap(),
            None
        );
    }

    #[test]
    fn corrupt_credentials_file_errors_explicitly_and_is_never_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");

        for bad in [
            &b"{corrupt-json"[..],
            &b"[\"array\"]"[..],
            &b"{\"https://a.example.invalid\":123}"[..],
        ] {
            std::fs::write(&path, bad).unwrap();
            let store = CredentialStore::new(path.clone());
            assert!(store.read_sync("https://a.example.invalid").is_err());
            assert!(store
                .write_sync("https://b.example.invalid", "fixture-key-b")
                .is_err());
            assert!(store.delete_sync("https://a.example.invalid").is_err());
            assert_eq!(std::fs::read(&path).unwrap(), bad);
        }
    }

    #[cfg(unix)]
    #[test]
    fn disk_write_failure_preserves_existing_file_and_in_memory_cache() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = CredentialStore::new(path.clone());
        store
            .write_sync("https://alpha.example.invalid", "fixture-key-alpha")
            .unwrap();
        let original_bytes = std::fs::read(&path).unwrap();

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
        let write_denied = store.write_sync("https://beta.example.invalid", "fixture-key-beta");
        let delete_denied = store.delete_sync("https://alpha.example.invalid");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

        // Only assert failure when running as an unprivileged user (non-root)
        if unsafe { libc_geteuid() } != 0 {
            assert!(write_denied.is_err());
            assert!(delete_denied.is_err());
            assert_eq!(std::fs::read(&path).unwrap(), original_bytes);
            assert_eq!(
                store.read_sync("https://alpha.example.invalid").unwrap(),
                Some("fixture-key-alpha".into())
            );
            assert_eq!(
                store.read_sync("https://beta.example.invalid").unwrap(),
                None
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn post_rename_directory_sync_failure_invalidates_cache_and_never_resurrects_deleted_keys() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc_geteuid() } == 0 {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        let store = CredentialStore::new(path.clone());
        store
            .write_sync("https://alpha.example.invalid", "fixture-key-alpha")
            .unwrap();
        store
            .write_sync("https://beta.example.invalid", "fixture-key-beta")
            .unwrap();

        // Mode 0o300 (-wx------) permits creating temp files, renaming onto credentials.json,
        // and opening credentials.json by exact path, while failing File::open(parent) (O_RDONLY)
        // right after std::fs::rename succeeds.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o300)).unwrap();
        let delete_after_rename = store.delete_sync("https://alpha.example.invalid");
        let write_after_rename = store.write_sync("https://gamma.example.invalid", "fixture-key-c");
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(delete_after_rename.is_err());
        assert!(write_after_rename.is_err());

        // Cache was invalidated on post-rename error, so reads reflect actual disk state
        // and subsequent writes never resurrect the deleted alpha key.
        assert_eq!(
            store.read_sync("https://alpha.example.invalid").unwrap(),
            None
        );
        assert_eq!(
            store.read_sync("https://beta.example.invalid").unwrap(),
            Some("fixture-key-beta".into())
        );
        assert_eq!(
            store.read_sync("https://gamma.example.invalid").unwrap(),
            Some("fixture-key-c".into())
        );

        store
            .write_sync("https://delta.example.invalid", "fixture-key-d")
            .unwrap();
        let reloaded = CredentialStore::new(path);
        assert_eq!(
            reloaded.read_sync("https://alpha.example.invalid").unwrap(),
            None
        );
        assert_eq!(
            reloaded.read_sync("https://delta.example.invalid").unwrap(),
            Some("fixture-key-d".into())
        );
    }

    #[cfg(unix)]
    unsafe fn libc_geteuid() -> u32 {
        extern "C" {
            fn geteuid() -> u32;
        }
        unsafe { geteuid() }
    }
}
