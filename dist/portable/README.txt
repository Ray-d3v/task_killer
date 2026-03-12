# task_killer

Windows task-management TUI MVP with privilege separation.

日本語版は、この英語版 README の後ろにあります。

The project uses a normal-user TUI plus a privileged Windows Service connected through a local Named Pipe. Display and filtering stay in the TUI. Administrative actions are delegated to the service.

## Workspace

- `crates/tasktui-app`
  - `ratatui` TUI and helper CLI binaries
- `crates/tasktui-core`
  - shared IPC types, API version, and errors
- `crates/tasktui-platform-windows`
  - Win32, SCM, shell, and Named Pipe wrappers
- `crates/tasktui-service`
  - privileged Windows Service backend

## Features

- Multi-tab Task Manager style layout
- Processes, Performance, Storage, Services, and Network views
- Search and filter
- Sort by CPU, memory, PID, or name
- High-load row highlighting for CPU and memory
- Process tree view
- TCP port owner list with process names
- Service browser with status and start type
- Summary cards plus CPU and memory history sparklines
- Right-click context menus for process, service, and network actions
- Persistent detail panes per tab
- Graceful close request
- Force kill
- Suspend and resume
- Priority change
- Start, stop, and restart Windows services
- Open selected executable folder

## Requirements

- Windows 10 or Windows 11 x64
- Rust toolchain
- Administrator privileges once for service installation

## Build

```powershell
cargo build --release
```

Artifacts:

- `target/release/tasktui-app.exe`
- `target/release/tasktui-service.exe`
- `target/release/tasktuictl.exe`
- `target/release/updater.exe`
- `target/release/uninstall.exe`

Or build the production distribution folder:

```powershell
.\scripts\build-release.ps1
```

That writes:

- `dist/tasktui-app.exe`
- `dist/tasktui-service.exe`
- `dist/tasktuictl.exe`
- `dist/updater.exe`
- `dist/uninstall.exe`
- `dist/README.txt`

## Build the MSI installer

Install WiX Toolset once:

```powershell
dotnet tool install --global wix
```

Then build the production installer:

```powershell
.\scripts\build-installer.ps1
```

That writes:

- `dist/task_killer-<version>-x64.msi`

The MSI installs the TUI binaries into `Program Files\Task Killer`, creates a Start Menu shortcut, installs `tasktui-service` as an auto-start `LocalSystem` service, and now requires signing configuration for production output.
The installer also closes running `tasktui-app.exe`, `tasktuictl.exe`, and `tasktui-service.exe` before replacing files so upgrades can proceed when an older version is still open.

## Signing requirements for public release

Public release artifacts are expected to be signed with a commercial code-signing certificate. The scripts use `signtool.exe` from the Windows SDK and fail if signing inputs are missing.

Required environment variables:

- `TASK_KILLER_SIGN_CERT_PATH`
- `TASK_KILLER_SIGN_CERT_PASSWORD`
- `TASK_KILLER_SIGN_TIMESTAMP_URL`

Optional environment variables:

- `TASK_KILLER_SIGN_FILE_DIGEST`
  - default: `sha256`
- `TASK_KILLER_SIGN_TIMESTAMP_DIGEST`
  - default: `sha256`

Example:

```powershell
$env:TASK_KILLER_SIGN_CERT_PATH="C:\secure\codesign.pfx"
$env:TASK_KILLER_SIGN_CERT_PASSWORD="secret"
$env:TASK_KILLER_SIGN_TIMESTAMP_URL="http://timestamp.digicert.com"
```

Local development can still use:

```powershell
.\scripts\build-release.ps1
```

That path does not sign artifacts.

## Package GitHub release assets

Build both the MSI and a portable zip, then generate SHA-256 checksums:

```powershell
.\scripts\package-github-release.ps1
```

That writes:

- `dist/task_killer-<version>-x64-portable.zip`
- `dist/task_killer-<version>-x64.msi`
- `dist/SHA256SUMS.txt`

The release script signs the `exe` and `msi` artifacts before creating the portable zip and checksum file.

## Checks

```powershell
cargo test
cargo clippy --workspace --all-targets -- -D warnings
```

## Install the service

Run from an elevated PowerShell:

```powershell
.\scripts\install-service.ps1
```

Remove it with:

```powershell
.\scripts\uninstall-service.ps1
```

## Run the TUI

After the service is installed:

```powershell
.\target\release\tasktui-app.exe
```

If you installed through the MSI, launch `Task Killer` from the Start Menu or run:

```powershell
"$env:ProgramFiles\Task Killer\tasktui-app.exe"
```

To launch the packaged updater directly:

```powershell
"$env:ProgramFiles\Task Killer\updater.exe" check
```

To remove the installed app using the packaged executable:

```powershell
"$env:ProgramFiles\Task Killer\uninstall.exe"
```

At startup the TUI sends `Ping` and shows service reachability in the status area.
When launched directly in the classic console window, the app also hides the native title bar. This does not remove the surrounding UI of hosts such as Windows Terminal.

## Local service debugging

Run the backend in the foreground without SCM:

```powershell
.\scripts\run-service-console.ps1
```

Or directly:

```powershell
.\target\debug\tasktui-service.exe --console
```

Use `--help` for service CLI help:

```powershell
cargo run -p tasktui-service -- --help
```

## IPC smoke test

Use the helper CLI without opening the TUI:

```powershell
cargo run -p tasktui-app --bin tasktuictl -- ping
```

Or:

```powershell
.\scripts\smoke-ping.ps1
```

Available helper commands:

- `ping`
- `list-ports`
- `open-path <pid>`
- `close-pid <pid>`
- `kill-pid <pid>`
- `restart-pid <pid>`
- `suspend-pid <pid>`
- `resume-pid <pid>`
- `set-priority <pid> <idle|below_normal|normal|above_normal|high>`
- `start-service <service_name>`
- `stop-service <service_name>`
- `restart-service <service_name> [timeout_ms]`

## TUI key bindings

- `Tab` / `Shift+Tab` switch between `Processes`, `Performance`, `Storage`, `Services`, and `Network`
- `/` search the active list tab
- `u` check GitHub Releases for a newer version
- `Shift+U` launch `updater.exe` and install the latest MSI
- `s` cycle process sort mode
- `t` toggle process tree view
- `f` cycle the network state filter
- `Enter` toggle list/detail pane focus on `Processes`, `Services`, and `Network`
- `o` open the executable folder for the selected process or network owner
- `z` suspend the selected process
- `x` resume the selected process
- `r` restart the selected process
- `4/5/6/7/8` open priority confirmation for idle, below_normal, normal, above_normal, high
- `Up` / `Down` move selection
- `k` request graceful close
- `K` open force-kill confirmation
- `1` start the selected service
- `2` stop the selected service
- `3` restart the selected service
- `q` quit
- `Y` / `N` confirm or cancel force kill and service actions
- Mouse:
  - left click selects rows and tabs
  - right click opens a context menu
  - wheel scroll moves the current selection
  - when a context menu is open, background input is blocked until the menu closes

## IPC contract

- Pipe: `\\.\pipe\tasktui.v1`
- Encoding: UTF-8 JSON
- Request: `ApiRequest { request_id, version, command }`
- Response: `ApiResponse { request_id, ok, result, error }`

Allowed administrative commands:

- `Ping`
- `ForceKillProcess { pid }`
- `RequestCloseProcess { pid }`
- `RestartProcess { pid }`
- `SuspendProcess { pid }`
- `ResumeProcess { pid }`
- `SetPriority { pid, priority }`
- `StartService { service_name }`
- `StopService { service_name }`
- `RestartService { service_name, timeout_ms }`

## Current limitations

- In-app update installs require elevation because the updater stops a `LocalSystem` service and launches MSI
- GitHub Releases must contain both `task_killer-<version>-x64.msi` and `SHA256SUMS.txt`
- `watch`, auto-restart rules, `kill-port`, and dump capture are not implemented
- Protected processes and protected services may still be inaccessible
- `RequestCloseProcess` only works for processes with a closable top-level window
- Performance view currently tracks only whole-system CPU and memory
- A valid commercial signing certificate is required to remove `Unknown Publisher` warnings from public release artifacts
- Windows Defender SmartScreen reputation warnings are not addressed by the signing scripts alone for brand new releases

## References

- [How User Account Control works](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works)
- [Named Pipe security and access rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
- [ControlService](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-controlservice)
- [Process Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights)
- [ShellExecuteW](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecutew)

---

# task_killer

権限分離を前提にした Windows 向けタスク管理 TUI の MVP です。

このプロジェクトは、通常ユーザー権限で動作する TUI と、ローカル Named Pipe で接続された特権 Windows Service で構成されています。表示やフィルタリングは TUI 側で行い、管理者権限が必要な操作はサービス側へ委譲します。

## ワークスペース構成

- `crates/tasktui-app`
  - `ratatui` ベースの TUI と補助 CLI バイナリ
- `crates/tasktui-core`
  - 共有 IPC 型、API バージョン、エラー定義
- `crates/tasktui-platform-windows`
  - Win32、SCM、shell、Named Pipe のラッパー
- `crates/tasktui-service`
  - 特権 Windows Service バックエンド

## 機能

- マルチタブの Task Manager 風レイアウト
- `Processes`、`Performance`、`Storage`、`Services`、`Network` ビュー
- 検索とフィルタリング
- CPU、メモリ、PID、名前でのソート
- CPU とメモリの高負荷行ハイライト
- プロセスツリー表示
- プロセス名付き TCP ポート所有者一覧
- 状態と開始種別を確認できるサービスブラウザ
- サマリーカードと CPU / メモリ履歴スパークライン
- プロセス、サービス、ネットワーク操作向け右クリックコンテキストメニュー
- タブごとに詳細ペイン状態を保持
- 通常終了要求
- 強制終了
- 一時停止と再開
- 優先度変更
- Windows サービスの開始、停止、再起動
- 選択した実行ファイルのフォルダを開く

## 要件

- Windows 10 または Windows 11 x64
- Rust ツールチェーン
- サービスインストール時に 1 回だけ管理者権限が必要

## ビルド

```powershell
cargo build --release
```

生成物:

- `target/release/tasktui-app.exe`
- `target/release/tasktui-service.exe`
- `target/release/tasktuictl.exe`

本番配布用フォルダを作る場合:

```powershell
.\scripts\build-release.ps1
```

出力先:

- `dist/tasktui-app.exe`
- `dist/tasktui-service.exe`
- `dist/tasktuictl.exe`
- `dist/README.txt`

## MSI インストーラーのビルド

最初に WiX Toolset をインストールします:

```powershell
dotnet tool install --global wix
```

その後、本番向けインストーラーをビルドします:

```powershell
.\scripts\build-installer.ps1
```

出力先:

- `dist/task_killer-<version>-x64.msi`

MSI は TUI バイナリを `Program Files\Task Killer` にインストールし、スタートメニューショートカットを作成し、`tasktui-service` を自動起動の `LocalSystem` サービスとして登録します。

## GitHub リリース用アセットの作成

MSI とポータブル zip をまとめて作成し、その後 SHA-256 チェックサムを生成します:

```powershell
.\scripts\package-github-release.ps1
```

出力先:

- `dist/task_killer-<version>-x64-portable.zip`
- `dist/task_killer-<version>-x64.msi`
- `dist/SHA256SUMS.txt`

## チェック

```powershell
cargo test
cargo clippy --workspace --all-targets -- -D warnings
```

## サービスのインストール

管理者権限の PowerShell で実行します:

```powershell
.\scripts\install-service.ps1
```

削除する場合:

```powershell
.\scripts\uninstall-service.ps1
```

## TUI の起動

サービスインストール後に実行します:

```powershell
.\target\release\tasktui-app.exe
```

MSI 経由でインストールした場合は、スタートメニューから `Task Killer` を起動するか、次を実行します:

```powershell
"$env:ProgramFiles\Task Killer\tasktui-app.exe"
```

起動時に TUI は `Ping` を送信し、サービスへの到達性をステータス領域に表示します。
また、クラシックなコンソールウィンドウから直接起動した場合は、ネイティブのタイトルバーも非表示にします。Windows Terminal のようなホスト側 UI までは除去しません。

## ローカルサービスデバッグ

SCM を介さずにバックエンドをフォアグラウンドで起動します:

```powershell
.\scripts\run-service-console.ps1
```

または直接実行します:

```powershell
.\target\debug\tasktui-service.exe --console
```

サービス CLI のヘルプは `--help` を使います:

```powershell
cargo run -p tasktui-service -- --help
```

## IPC スモークテスト

TUI を開かずに補助 CLI を使います:

```powershell
cargo run -p tasktui-app --bin tasktuictl -- ping
```

または:

```powershell
.\scripts\smoke-ping.ps1
```

利用可能な補助コマンド:

- `ping`
- `list-ports`
- `open-path <pid>`
- `close-pid <pid>`
- `kill-pid <pid>`
- `restart-pid <pid>`
- `suspend-pid <pid>`
- `resume-pid <pid>`
- `set-priority <pid> <idle|below_normal|normal|above_normal|high>`
- `start-service <service_name>`
- `stop-service <service_name>`
- `restart-service <service_name> [timeout_ms]`

## TUI キーバインド

- `Tab` / `Shift+Tab` で `Processes`、`Performance`、`Storage`、`Services`、`Network` を切り替え
- `/` で現在アクティブなリストタブを検索
- `s` でプロセスのソートモードを切り替え
- `t` でプロセスツリー表示を切り替え
- `f` でネットワーク状態フィルタを切り替え
- `Enter` で `Processes`、`Services`、`Network` のリスト / 詳細ペインのフォーカスを切り替え
- `o` で選択中プロセスまたはネットワーク所有者の実行ファイルフォルダを開く
- `z` で選択中プロセスを一時停止
- `x` で選択中プロセスを再開
- `r` で選択中プロセスを再起動
- `4/5/6/7/8` で idle、below_normal、normal、above_normal、high の優先度確認を開く
- `Up` / `Down` で選択移動
- `k` で通常終了要求
- `K` で強制終了確認を開く
- `1` で選択中サービスを開始
- `2` で選択中サービスを停止
- `3` で選択中サービスを再起動
- `q` で終了
- `Y` / `N` で強制終了とサービス操作を確定またはキャンセル
- マウス:
  - 左クリックで行とタブを選択
  - 右クリックでコンテキストメニューを開く
  - ホイールスクロールで現在の選択を移動
  - コンテキストメニュー表示中は閉じるまで背後の入力を受け付けない

## IPC 契約

- パイプ: `\\.\pipe\tasktui.v1`
- エンコーディング: UTF-8 JSON
- リクエスト: `ApiRequest { request_id, version, command }`
- レスポンス: `ApiResponse { request_id, ok, result, error }`

許可される管理コマンド:

- `Ping`
- `ForceKillProcess { pid }`
- `RequestCloseProcess { pid }`
- `RestartProcess { pid }`
- `SuspendProcess { pid }`
- `ResumeProcess { pid }`
- `SetPriority { pid, priority }`
- `StartService { service_name }`
- `StopService { service_name }`
- `RestartService { service_name, timeout_ms }`

## 現在の制限事項

- 一般的なプロセス再起動はベストエフォートです
  - 同じ実行ファイルパスの再起動は行いますが、元の引数や作業ディレクトリまでは完全に復元しません
- `watch`、自動再起動ルール、`kill-port`、ダンプ取得は未実装です
- 保護されたプロセスや保護されたサービスには引き続きアクセスできない場合があります
- `RequestCloseProcess` は閉じられるトップレベルウィンドウを持つプロセスでのみ機能します
- Performance ビューは現在、システム全体の CPU とメモリのみを追跡します

## 参考資料

- [How User Account Control works](https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works)
- [Named Pipe security and access rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
- [ControlService](https://learn.microsoft.com/en-us/windows/win32/api/winsvc/nf-winsvc-controlservice)
- [Process Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights)
- [ShellExecuteW](https://learn.microsoft.com/en-us/windows/win32/api/shellapi/nf-shellapi-shellexecutew)
