//! Backport tauri-apps/tray-icon#365 for Tauri 2.11's tray-icon 0.24.
//! macOS 27 swallows primary clicks while NSStatusItem has a resident menu.
use objc2::{rc::Retained, MainThreadMarker};
use objc2_app_kit::NSMenu;
use std::cell::RefCell;

thread_local! {
    static MENU: RefCell<Option<Retained<NSMenu>>> = const { RefCell::new(None) };
}

pub fn install(tray: &tauri::tray::TrayIcon) -> tauri::Result<()> {
    tray.with_inner_tray_icon(|inner| {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("tray install requires main thread"))?;
        inner.set_show_menu_on_right_click(false);
        let item = inner
            .ns_status_item()
            .ok_or_else(|| anyhow::anyhow!("status item missing"))?;
        MENU.with(|saved| saved.replace(item.menu(mtm)));
        item.setMenu(None);
        Ok(())
    })?
    .map_err(tauri::Error::Anyhow)
}

pub fn show_menu(tray: &tauri::tray::TrayIcon) -> tauri::Result<()> {
    tray.with_inner_tray_icon(|inner| {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| anyhow::anyhow!("tray menu requires main thread"))?;
        let item = inner
            .ns_status_item()
            .ok_or_else(|| anyhow::anyhow!("status item missing"))?;
        let button = item.button(mtm)
            .ok_or_else(|| anyhow::anyhow!("status button missing"))?;
        // AppKit runs a nested loop here. Do not hold the RefCell borrow.
        let menu = MENU.with(|saved| saved.borrow().clone())
            .ok_or_else(|| anyhow::anyhow!("tray menu missing"))?;
        unsafe {
            item.setMenu(Some(&menu));
            button.performClick(None);
            item.setMenu(None);
        }
        Ok(())
    })?
    .map_err(tauri::Error::Anyhow)
}
