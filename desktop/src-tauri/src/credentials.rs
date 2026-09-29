use tauri::AppHandle;

#[cfg(target_os = "android")]
use tauri::Manager;

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

pub async fn read(app: AppHandle, base: String) -> Result<Option<String>, String> {
    if base.is_empty() { return Ok(None); }
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            let _ = app;
            let entry = keyring::Entry::new("com.lich13.sub2ops", &base).map_err(|_| "无法访问钥匙串")?;
            match entry.get_password() {
                Ok(value) => Ok(Some(value)),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err("无法读取钥匙串，请允许访问后重新连接".into()),
            }
        }
        #[cfg(target_os = "android")]
        {
            let result: serde_json::Value = app.state::<SecureStore>().0
                .run_mobile_plugin("read", serde_json::json!({"base": base}))
                .map_err(|_| "无法读取安全存储，请重新连接")?;
            Ok(result["key"].as_str().map(str::to_owned))
        }
    }).await.map_err(|_| "安全存储任务失败".to_string())?
}

pub async fn write(app: AppHandle, base: String, key: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            let _ = app;
            keyring::Entry::new("com.lich13.sub2ops", &base)
                .map_err(|_| "无法访问钥匙串")?.set_password(&key)
                .map_err(|_| "无法将 Key 保存到钥匙串；连接未保存".into())
        }
        #[cfg(target_os = "android")]
        {
            app.state::<SecureStore>().0.run_mobile_plugin::<serde_json::Value>(
                "write", serde_json::json!({"base": base, "key": key}))
                .map(|_| ()).map_err(|_| "无法加密保存 Key；连接未保存".into())
        }
    }).await.map_err(|_| "安全存储任务失败".to_string())?
}

pub async fn delete(app: AppHandle, base: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        #[cfg(target_os = "macos")]
        {
            let _ = app;
            let entry = keyring::Entry::new("com.lich13.sub2ops", &base).map_err(|_| "无法访问钥匙串")?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(_) => Err("无法删除钥匙串中的 Key".into()),
            }
        }
        #[cfg(target_os = "android")]
        {
            app.state::<SecureStore>().0.run_mobile_plugin::<serde_json::Value>(
                "delete", serde_json::json!({"base": base}))
                .map(|_| ()).map_err(|_| "无法删除安全存储中的 Key".into())
        }
    }).await.map_err(|_| "安全存储任务失败".to_string())?
}
