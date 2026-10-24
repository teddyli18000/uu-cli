# Windows user-scope installer

The package contains uu.exe, uu-background.exe, the uu-cli Codex skill, and the PowerShell installer scripts. It uses Windows PowerShell 5.1 and .NET components already included with supported Windows; it does not require an administrator account, PowerShell 7, Rust, Python, Node.js, or WSL.

Extract the package, open Windows PowerShell in its root folder, and run:

    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\installer\install.ps1

Installation places the executables and skill under %LOCALAPPDATA%\Programs\uu-cli and adds that directory to the current user's PATH. It does not register a startup entry or start a background watcher, and it removes the legacy `uu-cli` value under HKCU\Software\Microsoft\Windows\CurrentVersion\Run during installation or upgrade. There is no discovery watcher. The user changes to the exact desired workspace directory and runs `uu bind`; one available Terminal is authorized directly; multiple candidates are shown with device name, Terminal name and creation time (UTC) for a numbered selection in the same console (Enter cancels). Normal `uu` commands never set up a binding or authorize a newly discovered Terminal. Worker attachments start only when an already-authorized worker is used. Installation does not change official UU or the uu-cli binding config.

Run the same installer from a newer package to upgrade. Windows state is shared under %USERPROFILE%\.uu-cli, outside AppData virtualization by packaged applications. On first use in each legacy context, existing bindings, authorization and quarantine markers are imported once from its visible %LOCALAPPDATA%\uu-cli; originals are preserved. Existing shared bindings take precedence and declined workers stay declined. Active legacy jobs must finish before migration. An explicit UU_CLI_CONFIG path remains unchanged. The installer replaces each program file atomically. If a uu-cli job or persistent attachment is holding a program open, the upgrade reports that it is busy; let the job finish and the attachment's five-minute idle period expire, then retry. Upgrade cleanup stops only legacy watcher processes from this installation. It does not stop foreground uu-cli jobs, worker attachments or official UU processes.

To uninstall:

    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\installer\uninstall.ps1

Uninstall removes only this installation's program files, its user PATH entry, and any remaining legacy `uu-cli` Run entry. It preserves all per-user uu-cli state under %USERPROFILE%\.uu-cli and legacy %LOCALAPPDATA%\uu-cli, including bindings, authorization, config locks/backups and unknown/quarantine markers. If UU_CLI_CONFIG points elsewhere, that file and its adjacent runtime state are preserved there too. This lets a later reinstall keep the same bindings and safely retain quarantined workers. Official UU files, settings, processes, and data are outside this installer's scope.

The hosted installer validation is scripts/test-uu-cli-installer.ps1; it refuses to run outside GitHub Actions.
