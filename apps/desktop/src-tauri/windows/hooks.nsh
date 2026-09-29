; Hooks into Tauri's NSIS installer (bundle.windows.nsis.installerHooks).

!macro NSIS_HOOK_POSTINSTALL
  ; Explorer caches each shortcut's icon, pinned taskbar and Start ones
  ; included, and an update leaves the shortcuts alone, so a new icon only
  ; showed after a reinstall. SHCNE_ASSOCCHANGED makes it drop that cache.
  System::Call 'shell32::SHChangeNotify(i 0x08000000, i 0, p 0, p 0)'
!macroend
