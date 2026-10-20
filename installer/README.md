# Windows user-scope installer

The package contains uu.exe, uu-background.exe, the uu-cli Codex skill, and the PowerShell installer scripts. It uses Windows PowerShell 5.1 and .NET components already included with supported Windows; it does not require an administrator account, PowerShell 7, Rust, Python, Node.js, or WSL.

Extract the package, open Windows PowerShell in its root folder, and run:

    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\installer\install.ps1

Installation places the executables and skill under %LOCALAPPDATA%\Programs\uu-cli, adds that directory to the current user's PATH, and registers a hidden watcher entry under HKCU\Software\Microsoft\Windows\CurrentVersion\Run. It also starts the watcher hidden for the current logon. It does not change UU or the uu-cli binding config.

Run the same installer from a newer package to upgrade. Existing bindings remain under %LOCALAPPDATA%\uu-cli\config.json (or the configured UU_CLI_CONFIG path). The installer replaces each program file atomically. If an active uu-cli job is holding uu.exe open, the upgrade reports that it is busy; let the job finish and retry. The installer stops only this installation's watcher processes. It does not stop foreground uu-cli jobs or official UU processes.

To uninstall:

    powershell.exe -NoProfile -ExecutionPolicy Bypass -File .\installer\uninstall.ps1

Uninstall removes only this installation's program files and its user PATH and Run entries. It preserves all per-user uu-cli state, including workspace bindings and authorization in %LOCALAPPDATA%\uu-cli\config.json, config lock/backup files, worker runtime state, and unknown/quarantine markers under %LOCALAPPDATA%\uu-cli\workers. If UU_CLI_CONFIG points elsewhere, that file and its adjacent runtime state are preserved there too. This lets a later reinstall keep the same bindings and safely retain quarantined workers. Official UU files, settings, processes, and data are outside this installer's scope.

The hosted installer validation is scripts/test-uu-cli-installer.ps1; it refuses to run outside GitHub Actions.